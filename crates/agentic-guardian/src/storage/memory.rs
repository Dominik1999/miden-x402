//! In-memory backend impls — for tests and local development.
//!
//! Production deployments should use the [`super::postgres`] backend.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    AgentRecord, AgentRegistryRepo, BatchQueueEntry, BatchQueueRepo, ChallengeRecord,
    ChallengeRepo, MandateCounterRepo, MandateRecord, MandateRepo, PendingState, PendingStateRepo,
    ReservationRepo, StorageError, StorageResult,
};

// ---------- agents ----------

#[derive(Default, Clone)]
pub struct MemoryAgentRegistry {
    inner: Arc<Mutex<HashMap<String, AgentRecord>>>,
}

#[async_trait]
impl AgentRegistryRepo for MemoryAgentRegistry {
    async fn register(&self, agent: AgentRecord) -> StorageResult<()> {
        self.inner
            .lock()
            .await
            .insert(agent.agent_account_id.as_str().to_owned(), agent);
        Ok(())
    }
    async fn get(&self, agent_account_id: &str) -> StorageResult<Option<AgentRecord>> {
        Ok(self.inner.lock().await.get(agent_account_id).cloned())
    }
}

// ---------- mandates ----------

#[derive(Default, Clone)]
pub struct MemoryMandateRepo {
    by_id: Arc<Mutex<HashMap<String, MandateRecord>>>,
}

#[async_trait]
impl MandateRepo for MemoryMandateRepo {
    async fn put(&self, mandate: MandateRecord) -> StorageResult<()> {
        let id = mandate.signed.mandate.mandate_id.clone();
        self.by_id.lock().await.insert(id, mandate);
        Ok(())
    }
    async fn get(&self, mandate_id: &str) -> StorageResult<Option<MandateRecord>> {
        Ok(self.by_id.lock().await.get(mandate_id).cloned())
    }
    async fn list_for_agent(
        &self,
        agent_account_id: &str,
    ) -> StorageResult<Vec<MandateRecord>> {
        Ok(self
            .by_id
            .lock()
            .await
            .values()
            .filter(|m| m.signed.mandate.agent_account_id.as_str() == agent_account_id)
            .cloned()
            .collect())
    }
}

// ---------- pending state ----------

#[derive(Default, Clone)]
pub struct MemoryPendingStateRepo {
    inner: Arc<Mutex<HashMap<String, PendingState>>>,
}

#[async_trait]
impl PendingStateRepo for MemoryPendingStateRepo {
    async fn get(&self, agent_account_id: &str) -> StorageResult<Option<PendingState>> {
        Ok(self.inner.lock().await.get(agent_account_id).cloned())
    }

    async fn try_advance(
        &self,
        agent_account_id: &str,
        expected_prev: &str,
        new_commitment: &str,
        new_nonce: u64,
    ) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        match g.get_mut(agent_account_id) {
            Some(state) => {
                if state.current_commitment_hex != expected_prev {
                    return Err(StorageError::Conflict(format!(
                        "pending state advance: have {}, claimed {}",
                        state.current_commitment_hex, expected_prev
                    )));
                }
                state.current_commitment_hex = new_commitment.to_owned();
                state.nonce = new_nonce;
                state.last_advanced_at_unix_secs = unix_now();
                Ok(())
            }
            None => Err(StorageError::NotFound),
        }
    }

    async fn rollback(
        &self,
        agent_account_id: &str,
        rolled_back_commitment: &str,
        previous_commitment: &str,
        previous_nonce: u64,
    ) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        if let Some(state) = g.get_mut(agent_account_id) {
            if state.current_commitment_hex == rolled_back_commitment {
                state.current_commitment_hex = previous_commitment.to_owned();
                state.nonce = previous_nonce;
                state.last_advanced_at_unix_secs = unix_now();
            }
        }
        Ok(())
    }
}

// ---------- reservations ----------

#[derive(Default, Clone)]
pub struct MemoryReservationRepo {
    /// `nullifier_hex -> (owning_queued_id, expires_at)`
    inner: Arc<Mutex<HashMap<String, (String, u64)>>>,
}

#[async_trait]
impl ReservationRepo for MemoryReservationRepo {
    async fn try_reserve_all(
        &self,
        nullifiers: &[String],
        ttl: Duration,
        owning_queued_id: &str,
    ) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        let now = unix_now();
        // pre-check
        for n in nullifiers {
            if let Some((_, expires)) = g.get(n) {
                if *expires > now {
                    return Err(StorageError::Conflict(format!("nullifier reserved: {n}")));
                }
            }
        }
        let expires = now + ttl.as_secs();
        for n in nullifiers {
            g.insert(n.clone(), (owning_queued_id.to_owned(), expires));
        }
        Ok(())
    }

    async fn release_for_tx(&self, owning_queued_id: &str) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        g.retain(|_, (owner, _)| owner != owning_queued_id);
        Ok(())
    }

    async fn promote_for_tx(&self, _owning_queued_id: &str) -> StorageResult<()> {
        // In-memory backend doesn't distinguish promoted from reserved;
        // the entry stays in the set until release / sweep.
        Ok(())
    }

    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize> {
        let mut g = self.inner.lock().await;
        let before = g.len();
        g.retain(|_, (_, expires)| *expires > now_unix_secs);
        Ok(before - g.len())
    }
}

// ---------- batch queue ----------

#[derive(Default, Clone)]
pub struct MemoryBatchQueueRepo {
    inner: Arc<Mutex<BatchQueueInner>>,
}

#[derive(Default)]
struct BatchQueueInner {
    entries: HashMap<String, BatchQueueEntry>,
    fifo: BTreeMap<(u64, String), ()>,
}

#[async_trait]
impl BatchQueueRepo for MemoryBatchQueueRepo {
    async fn enqueue(&self, entry: BatchQueueEntry) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        if g.entries.contains_key(&entry.queued_id) {
            return Ok(());
        }
        g.fifo.insert((entry.enqueued_at_unix_secs, entry.queued_id.clone()), ());
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
        let mut taken = Vec::new();
        let mut to_remove = Vec::new();
        let oldest_age = match g.fifo.iter().next() {
            Some((k, _)) => now_unix_secs.saturating_sub(k.0),
            None => 0,
        };
        let should_drain_by_age = oldest_age >= max_batch_age.as_secs();
        if !should_drain_by_age && g.fifo.len() < max_batch_size {
            return Ok(taken);
        }
        for (key, _) in g.fifo.iter() {
            if taken.len() >= max_batch_size {
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
        let mut taken: Vec<_> = g.entries.values().cloned().collect();
        taken.sort_by_key(|e| e.enqueued_at_unix_secs);
        g.entries.clear();
        g.fifo.clear();
        Ok(taken)
    }

    async fn mark_submitted(
        &self,
        queued_id: &str,
        on_chain_tx_id: &str,
    ) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        match g.entries.get_mut(queued_id) {
            Some(e) => {
                e.submitted = true;
                e.on_chain_tx_id = Some(on_chain_tx_id.to_owned());
                Ok(())
            }
            None => Err(StorageError::NotFound),
        }
    }

    async fn delete(&self, queued_id: &str) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.entries.remove(queued_id) {
            g.fifo.remove(&(entry.enqueued_at_unix_secs, entry.queued_id));
        }
        Ok(())
    }

    async fn lookup(
        &self,
        queued_id: &str,
    ) -> StorageResult<Option<BatchQueueEntry>> {
        Ok(self.inner.lock().await.entries.get(queued_id).cloned())
    }

    async fn len(&self) -> StorageResult<usize> {
        Ok(self.inner.lock().await.entries.len())
    }
}

// ---------- challenges ----------

#[derive(Default, Clone)]
pub struct MemoryChallengeRepo {
    inner: Arc<Mutex<HashMap<String, ChallengeRecord>>>,
}

#[async_trait]
impl ChallengeRepo for MemoryChallengeRepo {
    async fn put(&self, challenge: ChallengeRecord) -> StorageResult<()> {
        let key = challenge.serial_num.as_str().to_owned();
        self.inner.lock().await.insert(key, challenge);
        Ok(())
    }
    async fn consume(&self, serial_num_hex: &str) -> StorageResult<ChallengeRecord> {
        self.inner
            .lock()
            .await
            .remove(serial_num_hex)
            .ok_or(StorageError::NotFound)
    }
    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize> {
        let mut g = self.inner.lock().await;
        let before = g.len();
        g.retain(|_, c| !c.is_expired(now_unix_secs));
        Ok(before - g.len())
    }
}

// ---------- mandate counters ----------

#[derive(Default, Clone)]
pub struct MemoryMandateCounterRepo {
    inner: Arc<Mutex<HashMap<(String, u64), u64>>>,
}

#[async_trait]
impl MandateCounterRepo for MemoryMandateCounterRepo {
    async fn add(
        &self,
        agent_account_id: &str,
        window_start_unix_secs: u64,
        amount: u64,
    ) -> StorageResult<()> {
        let mut g = self.inner.lock().await;
        *g.entry((agent_account_id.to_owned(), window_start_unix_secs)).or_insert(0) += amount;
        Ok(())
    }
    async fn sum_recent(
        &self,
        agent_account_id: &str,
        lookback_secs: u64,
        now_unix_secs: u64,
    ) -> StorageResult<u64> {
        let cutoff = now_unix_secs.saturating_sub(lookback_secs);
        let g = self.inner.lock().await;
        Ok(g.iter()
            .filter(|((a, t), _)| a == agent_account_id && *t >= cutoff)
            .map(|(_, v)| *v)
            .sum())
    }
}

pub fn unix_now() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
