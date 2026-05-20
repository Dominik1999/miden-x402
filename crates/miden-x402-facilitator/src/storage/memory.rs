//! In-memory implementations of the four storage traits.
//!
//! Suitable for tests, local development, and the single-process reference
//! deployment. For multi-instance facilitators, swap in a different impl of
//! the same traits (filesystem + checksumming, or a network-backed store).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    BatchQueueEntry, BatchQueueRepo, ChallengeRepo, FacilitatorKeyStore, IssuedChallenge,
    Reservation, ReservationRepo, StorageError, StorageResult, unix_now,
};

// ---------- Challenge ----------

#[derive(Debug, Default, Clone)]
pub struct MemoryChallengeRepo {
    inner: Arc<Mutex<HashMap<String, IssuedChallenge>>>,
}

impl MemoryChallengeRepo {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl ChallengeRepo for MemoryChallengeRepo {
    async fn put(&self, challenge: IssuedChallenge) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        g.insert(challenge.serial_num_hex.as_str().to_owned(), challenge);
        Ok(())
    }

    async fn consume(&self, serial_num_hex: &str) -> StorageResult<IssuedChallenge> {
        let mut g = self.inner.lock().await;
        g.remove(serial_num_hex).ok_or(StorageError::NotFound)
    }

    async fn peek(&self, serial_num_hex: &str) -> StorageResult<Option<IssuedChallenge>> {
        let g = self.inner.lock().await;
        Ok(g.get(serial_num_hex).cloned())
    }

    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize> {
        let mut g = self.inner.lock().await;
        let before = g.len();
        g.retain(|_, c| !c.is_expired(now_unix_secs));
        Ok(before - g.len())
    }
}

// ---------- Reservation ----------

#[derive(Debug, Default, Clone)]
pub struct MemoryReservationRepo {
    inner: Arc<Mutex<HashMap<String, Reservation>>>,
}

impl MemoryReservationRepo {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl ReservationRepo for MemoryReservationRepo {
    async fn try_reserve_all(&self, nullifiers: &[String], ttl: Duration) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        // Pre-check: if any key is already reserved (and not expired), fail
        // without writing anything.
        let now = unix_now();
        for n in nullifiers {
            if let Some(r) = g.get(n) {
                if r.expires_at_unix_secs > now {
                    return Err(StorageError::Conflict(format!(
                        "nullifier already reserved: {n}"
                    )));
                }
            }
        }
        let expires_at = now + ttl.as_secs();
        for n in nullifiers {
            g.insert(
                n.clone(),
                Reservation {
                    reserved_at_unix_secs: now,
                    expires_at_unix_secs: expires_at,
                    promoted: false,
                },
            );
        }
        Ok(())
    }

    async fn promote_to_consumed(&self, nullifiers: &[String]) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        for n in nullifiers {
            if let Some(r) = g.get_mut(n) {
                r.promoted = true;
            }
        }
        Ok(())
    }

    async fn release(&self, nullifiers: &[String]) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        for n in nullifiers {
            g.remove(n);
        }
        Ok(())
    }

    async fn get(&self, nullifier: &str) -> StorageResult<Option<Reservation>> {
        let g = self.inner.lock().await;
        Ok(g.get(nullifier).cloned())
    }

    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize> {
        let mut g = self.inner.lock().await;
        let before = g.len();
        g.retain(|_, r| r.expires_at_unix_secs > now_unix_secs);
        Ok(before - g.len())
    }
}

// ---------- Batch queue ----------

#[derive(Debug, Default, Clone)]
pub struct MemoryBatchQueueRepo {
    // BTreeMap keyed by `(enqueued_at_unix_secs, queued_id)` so we drain
    // in FIFO order. We store a copy of the queued_id as the value key in
    // a sibling `HashMap` for O(1) lookups.
    inner: Arc<Mutex<BatchQueueInner>>,
}

#[derive(Debug, Default)]
struct BatchQueueInner {
    entries: HashMap<String, BatchQueueEntry>,
    fifo: BTreeMap<(u64, String), ()>,
}

impl MemoryBatchQueueRepo {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl BatchQueueRepo for MemoryBatchQueueRepo {
    async fn enqueue(&self, entry: BatchQueueEntry) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        if g.entries.contains_key(&entry.queued_id) {
            return Ok(()); // idempotent
        }
        g.fifo
            .insert((entry.enqueued_at_unix_secs, entry.queued_id.clone()), ());
        g.entries.insert(entry.queued_id.clone(), entry);
        Ok(())
    }

    async fn drain_batch(
        &self,
        max_batch_size: usize,
        max_batch_age: Duration,
        now_unix_secs: u64,
    ) -> StorageResult<Vec<BatchQueueEntry>> {
        let mut g = self.inner.lock().await;

        let oldest_drainable_age = max_batch_age.as_secs();
        let mut taken: Vec<BatchQueueEntry> = Vec::new();
        let mut to_remove: Vec<(u64, String)> = Vec::new();

        for (key, _) in g.fifo.iter() {
            if taken.len() >= max_batch_size {
                break;
            }
            let age = now_unix_secs.saturating_sub(key.0);
            // Drain everything older than the threshold; if we hit the
            // size cap first that's also fine.
            if age < oldest_drainable_age && taken.len() < max_batch_size && taken.is_empty() {
                // Nothing old enough yet and we haven't started taking.
                break;
            }
            if let Some(entry) = g.entries.get(&key.1).cloned() {
                taken.push(entry);
                to_remove.push(key.clone());
            }
        }
        for key in &to_remove {
            g.fifo.remove(key);
            g.entries.remove(&key.1);
        }
        Ok(taken)
    }

    async fn drain_all(&self) -> StorageResult<Vec<BatchQueueEntry>> {
        let mut g = self.inner.lock().await;
        let mut taken: Vec<BatchQueueEntry> = g.entries.values().cloned().collect();
        // Stable ordering for tests.
        taken.sort_by_key(|e| e.enqueued_at_unix_secs);
        g.entries.clear();
        g.fifo.clear();
        Ok(taken)
    }

    async fn mark_submitted(&self, queued_id: &str, on_chain_tx_id: &str) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.entries.get_mut(queued_id) {
            entry.submitted = true;
            entry.on_chain_tx_id = Some(on_chain_tx_id.to_owned());
            Ok(())
        } else {
            Err(StorageError::NotFound)
        }
    }

    async fn delete(&self, queued_id: &str) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.entries.remove(queued_id) {
            g.fifo.remove(&(entry.enqueued_at_unix_secs, entry.queued_id));
        }
        Ok(())
    }

    async fn lookup(&self, queued_id: &str) -> StorageResult<Option<BatchQueueEntry>> {
        let g = self.inner.lock().await;
        Ok(g.entries.get(queued_id).cloned())
    }

    async fn len(&self) -> StorageResult<usize> {
        let g = self.inner.lock().await;
        Ok(g.entries.len())
    }
}

// ---------- Receipt key ----------

#[derive(Debug, Default, Clone)]
pub struct MemoryFacilitatorKeyStore {
    inner: Arc<Mutex<Option<Vec<u8>>>>,
}

impl MemoryFacilitatorKeyStore {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl FacilitatorKeyStore for MemoryFacilitatorKeyStore {
    async fn load(&self) -> StorageResult<Option<Vec<u8>>> {
        Ok(self.inner.lock().await.clone())
    }
    async fn save(&self, secret_key_bytes: &[u8]) -> StorageResult<()> {
        *self.inner.lock().await = Some(secret_key_bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reservation_conflict_rolls_back_partial_writes() {
        let repo = MemoryReservationRepo::new();
        repo.try_reserve_all(&["a".into()], Duration::from_secs(60))
            .await
            .unwrap();
        // Trying to reserve [a, b] must NOT leave b reserved.
        let result = repo
            .try_reserve_all(&["a".into(), "b".into()], Duration::from_secs(60))
            .await;
        assert!(matches!(result, Err(StorageError::Conflict(_))));
        assert!(repo.get("b").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn promote_keeps_entry_in_set() {
        let repo = MemoryReservationRepo::new();
        repo.try_reserve_all(&["a".into()], Duration::from_secs(60))
            .await
            .unwrap();
        repo.promote_to_consumed(&["a".into()]).await.unwrap();
        let r = repo.get("a").await.unwrap().unwrap();
        assert!(r.promoted);
        // A concurrent reserve still fails.
        let result = repo.try_reserve_all(&["a".into()], Duration::from_secs(60)).await;
        assert!(matches!(result, Err(StorageError::Conflict(_))));
    }

    #[tokio::test]
    async fn release_makes_nullifier_available_again() {
        let repo = MemoryReservationRepo::new();
        repo.try_reserve_all(&["a".into()], Duration::from_secs(60))
            .await
            .unwrap();
        repo.release(&["a".into()]).await.unwrap();
        repo.try_reserve_all(&["a".into()], Duration::from_secs(60))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn batch_queue_is_idempotent_on_queued_id() {
        let repo = MemoryBatchQueueRepo::new();
        let entry = BatchQueueEntry {
            queued_id: "q1".into(),
            payer: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
            tx_inputs_b64: "AAA=".into(),
            reserved_nullifiers: vec!["n1".into()],
            network: "miden:testnet".into(),
            enqueued_at_unix_secs: 1,
            submitted: false,
            on_chain_tx_id: None,
        };
        repo.enqueue(entry.clone()).await.unwrap();
        repo.enqueue(entry.clone()).await.unwrap();
        assert_eq!(repo.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn batch_queue_drain_all_returns_fifo_order() {
        let repo = MemoryBatchQueueRepo::new();
        for i in 0..3u64 {
            repo.enqueue(BatchQueueEntry {
                queued_id: format!("q{i}"),
                payer: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
                tx_inputs_b64: "AAA=".into(),
                reserved_nullifiers: vec![],
                network: "miden:testnet".into(),
                enqueued_at_unix_secs: i,
                submitted: false,
                on_chain_tx_id: None,
            })
            .await
            .unwrap();
        }
        let drained = repo.drain_all().await.unwrap();
        assert_eq!(drained.iter().map(|e| e.queued_id.clone()).collect::<Vec<_>>(),
            vec!["q0".to_string(), "q1".to_string(), "q2".to_string()]);
        assert_eq!(repo.len().await.unwrap(), 0);
    }
}
