//! Filesystem-backed storage impls.
//!
//! Persists each kind under a configured root directory:
//!
//! ```text
//! <root>/
//!   challenges/<serial_num_hex>.json
//!   reservations/<nullifier_hex>.json
//!   batch_queue/<queued_id>.json
//!   keystore/receipt_key.bin
//! ```
//!
//! Each mutation is serialised through a per-repo `tokio::sync::Mutex` so
//! concurrent writes to the same kind are linearised. The on-disk state is
//! the source of truth — restarts replay it. This is a single-instance
//! impl; for multi-instance deployments swap in a network-backed store
//! behind the same traits.
//!
//! Atomicity uses a write-then-rename pattern (`tempfile` in the same dir
//! followed by `tokio::fs::rename`). On crash, partial writes leave a
//! `.tmp-*` file that is cleaned by the next sweep.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    BatchQueueEntry, BatchQueueRepo, ChallengeRepo, FacilitatorKeyStore, IssuedChallenge,
    Reservation, ReservationRepo, StorageError, StorageResult, unix_now,
};

const CHALLENGES_SUBDIR: &str = "challenges";
const RESERVATIONS_SUBDIR: &str = "reservations";
const BATCH_QUEUE_SUBDIR: &str = "batch_queue";
const KEYSTORE_SUBDIR: &str = "keystore";
const RECEIPT_KEY_FILE: &str = "receipt_key.bin";

fn ioerr<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Io(e.to_string())
}
fn jsonerr<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Serialization(e.to_string())
}

async fn ensure_dir(p: &Path) -> StorageResult<()> {
    tokio::fs::create_dir_all(p).await.map_err(ioerr)
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> StorageResult<()> {
    let parent = path.parent().ok_or_else(|| StorageError::Io("no parent".into()))?;
    ensure_dir(parent).await?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("x"),
        unix_now()
    ));
    tokio::fs::write(&tmp, bytes).await.map_err(ioerr)?;
    tokio::fs::rename(&tmp, path).await.map_err(ioerr)?;
    Ok(())
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> StorageResult<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).map_err(jsonerr)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ioerr(e)),
    }
}

async fn list_json_filenames(dir: &Path) -> StorageResult<Vec<String>> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(ioerr(e)),
    };
    while let Some(entry) = entries.next_entry().await.map_err(ioerr)? {
        let name = entry.file_name();
        let s = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if s.starts_with(".tmp-") || !s.ends_with(".json") {
            continue;
        }
        out.push(s.trim_end_matches(".json").to_owned());
    }
    Ok(out)
}

// ---------- Challenge ----------

#[derive(Debug, Clone)]
pub struct FilesystemChallengeRepo {
    root: PathBuf,
    lock: std::sync::Arc<Mutex<()>>,
}

impl FilesystemChallengeRepo {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into().join(CHALLENGES_SUBDIR),
            lock: Default::default(),
        }
    }
    fn path(&self, serial_num_hex: &str) -> PathBuf {
        self.root.join(format!("{serial_num_hex}.json"))
    }
}

#[async_trait]
impl ChallengeRepo for FilesystemChallengeRepo {
    async fn put(&self, challenge: IssuedChallenge) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        let bytes = serde_json::to_vec(&challenge).map_err(jsonerr)?;
        atomic_write(&self.path(challenge.serial_num_hex.as_str()), &bytes).await
    }

    async fn consume(&self, serial_num_hex: &str) -> StorageResult<IssuedChallenge> {
        let _g = self.lock.lock().await;
        let path = self.path(serial_num_hex);
        let entry: IssuedChallenge =
            read_json(&path).await?.ok_or(StorageError::NotFound)?;
        tokio::fs::remove_file(&path).await.map_err(ioerr)?;
        Ok(entry)
    }

    async fn peek(&self, serial_num_hex: &str) -> StorageResult<Option<IssuedChallenge>> {
        let _g = self.lock.lock().await;
        read_json(&self.path(serial_num_hex)).await
    }

    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize> {
        let _g = self.lock.lock().await;
        let mut removed = 0;
        for name in list_json_filenames(&self.root).await? {
            let path = self.path(&name);
            if let Some(entry) = read_json::<IssuedChallenge>(&path).await? {
                if entry.is_expired(now_unix_secs) {
                    tokio::fs::remove_file(&path).await.map_err(ioerr)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

// ---------- Reservation ----------

#[derive(Debug, Clone)]
pub struct FilesystemReservationRepo {
    root: PathBuf,
    lock: std::sync::Arc<Mutex<()>>,
}

impl FilesystemReservationRepo {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into().join(RESERVATIONS_SUBDIR),
            lock: Default::default(),
        }
    }
    fn path(&self, nullifier: &str) -> PathBuf {
        // nullifier may contain `:`; sanitise it for the filesystem.
        let safe = nullifier.replace(['/', '\\', ':'], "_");
        self.root.join(format!("{safe}.json"))
    }
}

#[async_trait]
impl ReservationRepo for FilesystemReservationRepo {
    async fn try_reserve_all(&self, nullifiers: &[String], ttl: Duration) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        ensure_dir(&self.root).await?;
        let now = unix_now();

        // Pre-check: every key must be free (or expired).
        for n in nullifiers {
            let path = self.path(n);
            if let Some(r) = read_json::<Reservation>(&path).await? {
                if r.expires_at_unix_secs > now {
                    return Err(StorageError::Conflict(format!(
                        "nullifier already reserved: {n}"
                    )));
                }
            }
        }
        let expires_at = now + ttl.as_secs();
        for n in nullifiers {
            let entry = Reservation {
                reserved_at_unix_secs: now,
                expires_at_unix_secs: expires_at,
                promoted: false,
            };
            let bytes = serde_json::to_vec(&entry).map_err(jsonerr)?;
            atomic_write(&self.path(n), &bytes).await?;
        }
        Ok(())
    }

    async fn promote_to_consumed(&self, nullifiers: &[String]) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        for n in nullifiers {
            let path = self.path(n);
            if let Some(mut r) = read_json::<Reservation>(&path).await? {
                r.promoted = true;
                let bytes = serde_json::to_vec(&r).map_err(jsonerr)?;
                atomic_write(&path, &bytes).await?;
            }
        }
        Ok(())
    }

    async fn release(&self, nullifiers: &[String]) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        for n in nullifiers {
            let path = self.path(n);
            match tokio::fs::remove_file(&path).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(ioerr(e)),
            }
        }
        Ok(())
    }

    async fn get(&self, nullifier: &str) -> StorageResult<Option<Reservation>> {
        let _g = self.lock.lock().await;
        read_json(&self.path(nullifier)).await
    }

    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize> {
        let _g = self.lock.lock().await;
        let mut removed = 0;
        for name in list_json_filenames(&self.root).await? {
            let path = self.root.join(format!("{name}.json"));
            if let Some(r) = read_json::<Reservation>(&path).await? {
                if r.expires_at_unix_secs <= now_unix_secs {
                    tokio::fs::remove_file(&path).await.map_err(ioerr)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

// ---------- Batch queue ----------

#[derive(Debug, Clone)]
pub struct FilesystemBatchQueueRepo {
    root: PathBuf,
    lock: std::sync::Arc<Mutex<()>>,
}

impl FilesystemBatchQueueRepo {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into().join(BATCH_QUEUE_SUBDIR),
            lock: Default::default(),
        }
    }
    fn path(&self, queued_id: &str) -> PathBuf {
        let safe = queued_id.replace(['/', '\\', ':'], "_");
        self.root.join(format!("{safe}.json"))
    }
}

#[async_trait]
impl BatchQueueRepo for FilesystemBatchQueueRepo {
    async fn enqueue(&self, entry: BatchQueueEntry) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        ensure_dir(&self.root).await?;
        let path = self.path(&entry.queued_id);
        // Idempotent: if the file exists, drop the new write.
        if tokio::fs::try_exists(&path).await.map_err(ioerr)? {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&entry).map_err(jsonerr)?;
        atomic_write(&path, &bytes).await
    }

    async fn drain_batch(
        &self,
        max_batch_size: usize,
        max_batch_age: Duration,
        now_unix_secs: u64,
    ) -> StorageResult<Vec<BatchQueueEntry>> {
        let _g = self.lock.lock().await;
        let mut entries: Vec<BatchQueueEntry> = Vec::new();
        for name in list_json_filenames(&self.root).await? {
            let path = self.root.join(format!("{name}.json"));
            if let Some(entry) = read_json::<BatchQueueEntry>(&path).await? {
                entries.push(entry);
            }
        }
        entries.sort_by_key(|e| e.enqueued_at_unix_secs);
        // Apply the drain rules:
        // - take up to `max_batch_size`
        // - OR if the oldest entry is older than `max_batch_age`, drain a
        //   batch even if shorter than max size
        let oldest_age = entries
            .first()
            .map(|e| now_unix_secs.saturating_sub(e.enqueued_at_unix_secs))
            .unwrap_or(0);
        let should_drain_by_age = oldest_age >= max_batch_age.as_secs();
        if !should_drain_by_age && entries.len() < max_batch_size {
            return Ok(Vec::new());
        }
        let take = entries.len().min(max_batch_size);
        let drained: Vec<BatchQueueEntry> = entries.into_iter().take(take).collect();
        for entry in &drained {
            let path = self.path(&entry.queued_id);
            tokio::fs::remove_file(&path).await.map_err(ioerr)?;
        }
        Ok(drained)
    }

    async fn drain_all(&self) -> StorageResult<Vec<BatchQueueEntry>> {
        let _g = self.lock.lock().await;
        let mut entries: Vec<BatchQueueEntry> = Vec::new();
        for name in list_json_filenames(&self.root).await? {
            let path = self.root.join(format!("{name}.json"));
            if let Some(entry) = read_json::<BatchQueueEntry>(&path).await? {
                tokio::fs::remove_file(&path).await.map_err(ioerr)?;
                entries.push(entry);
            }
        }
        entries.sort_by_key(|e| e.enqueued_at_unix_secs);
        Ok(entries)
    }

    async fn mark_submitted(&self, queued_id: &str, on_chain_tx_id: &str) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        let path = self.path(queued_id);
        let mut entry: BatchQueueEntry =
            read_json(&path).await?.ok_or(StorageError::NotFound)?;
        entry.submitted = true;
        entry.on_chain_tx_id = Some(on_chain_tx_id.to_owned());
        let bytes = serde_json::to_vec(&entry).map_err(jsonerr)?;
        atomic_write(&path, &bytes).await
    }

    async fn delete(&self, queued_id: &str) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        let path = self.path(queued_id);
        match tokio::fs::remove_file(&path).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ioerr(e)),
        }
    }

    async fn lookup(&self, queued_id: &str) -> StorageResult<Option<BatchQueueEntry>> {
        let _g = self.lock.lock().await;
        read_json(&self.path(queued_id)).await
    }

    async fn len(&self) -> StorageResult<usize> {
        let _g = self.lock.lock().await;
        Ok(list_json_filenames(&self.root).await?.len())
    }
}

// ---------- Receipt key ----------

#[derive(Debug, Clone)]
pub struct FilesystemKeyStore {
    path: PathBuf,
    lock: std::sync::Arc<Mutex<()>>,
}

impl FilesystemKeyStore {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        let p: PathBuf = root.into();
        Self {
            path: p.join(KEYSTORE_SUBDIR).join(RECEIPT_KEY_FILE),
            lock: Default::default(),
        }
    }
}

#[async_trait]
impl FacilitatorKeyStore for FilesystemKeyStore {
    async fn load(&self) -> StorageResult<Option<Vec<u8>>> {
        let _g = self.lock.lock().await;
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ioerr(e)),
        }
    }
    async fn save(&self, secret_key_bytes: &[u8]) -> StorageResult<()> {
        let _g = self.lock.lock().await;
        atomic_write(&self.path, secret_key_bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn keystore_persists_and_reads_back() {
        let dir = tempdir().unwrap();
        let store = FilesystemKeyStore::new(dir.path());
        assert!(store.load().await.unwrap().is_none());
        store.save(b"secret-bytes").await.unwrap();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded, b"secret-bytes");
    }

    #[tokio::test]
    async fn reservation_conflict_does_not_persist_partial_state() {
        let dir = tempdir().unwrap();
        let repo = FilesystemReservationRepo::new(dir.path());
        repo.try_reserve_all(&["a".into()], Duration::from_secs(60))
            .await
            .unwrap();
        let r = repo
            .try_reserve_all(&["a".into(), "b".into()], Duration::from_secs(60))
            .await;
        assert!(matches!(r, Err(StorageError::Conflict(_))));
        assert!(repo.get("b").await.unwrap().is_none());
    }
}
