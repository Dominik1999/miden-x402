//! Per-agent persistence for the x402 facilitator.
//!
//! v1 is filesystem-only: one directory per registered agent under a
//! configurable root. Layout:
//!
//! ```text
//! root/
//!   agents/
//!     <agent_id>/
//!       agent.json           registration record (account_id, hot_key, scheme, pubkey)
//!       mandate.json         the AP2-minimal mandate
//!       pending_state.json   {committed, pending, last_accepted_seq}
//!       nullifiers.wal       append-only: {nullifier, seq, ts} reservations
//!       nullifiers.spent     append-only: {nullifier, seq, ts} after batch commit
//!       pending_queue/
//!         <seq>.json         one accepted-but-not-proven tx per file
//!       committed/<seq>.json after batch commit
//!       failed/<seq>.json    after batch failure
//!       payments/<nullifier> sentinel file pointing to its seq (for fast status lookup)
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{FacilitatorError, Result};
use crate::types::{AgentMandate, AgenticPayload, PaymentStatus};
use guardian_shared::SignatureScheme;

/// Static registration record we persist after `POST /agents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_id: String,
    pub account_id: String,
    pub hot_key_commitment: String,
    pub hot_key_scheme: SignatureScheme,
    pub hot_key_pubkey_hex: Option<String>,
    pub registered_at_unix_secs: u64,
}

/// Per-agent pending state — the load-bearing piece DESIGN.md calls the
/// "single serialization authority" view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingState {
    pub committed_state_commitment: String,
    pub pending_state_commitment: String,
    pub last_accepted_seq: u64,
}

/// One entry in the per-agent pending queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTx {
    pub seq: u64,
    pub accepted_at_unix_micros: u64,
    pub nullifiers: Vec<String>,
    pub status: PaymentStatus,
    pub payload: AgenticPayload,
    /// When the batch worker first picked this tx up.
    #[serde(default)]
    pub t_batch_started_unix_micros: Option<u64>,
    /// When the batch worker called `submit_transaction` for this tx.
    #[serde(default)]
    pub t_submitted_unix_micros: Option<u64>,
    /// When on-chain inclusion was confirmed.
    #[serde(default)]
    pub t_committed_unix_micros: Option<u64>,
    /// Last error string if the tx failed at any stage.
    #[serde(default)]
    pub error: Option<String>,
}

/// Thin wrapper holding the configurable root path.
#[derive(Debug, Clone)]
pub struct FilesystemX402Store {
    root: Arc<PathBuf>,
}

impl FilesystemX402Store {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root: PathBuf = root.into();
        fs::create_dir_all(root.join("agents")).map_err(io_err)?;
        Ok(Self { root: Arc::new(root) })
    }

    fn agent_dir(&self, agent_id: &str) -> PathBuf {
        self.root.join("agents").join(agent_id)
    }

    fn agent_file(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("agent.json")
    }
    fn mandate_file(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("mandate.json")
    }
    fn pending_state_file(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("pending_state.json")
    }
    fn nullifiers_wal(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("nullifiers.wal")
    }
    fn nullifiers_spent(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("nullifiers.spent")
    }
    fn queue_dir(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("pending_queue")
    }
    fn committed_dir(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("committed")
    }
    fn failed_dir(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("failed")
    }
    fn payments_dir(&self, agent_id: &str) -> PathBuf {
        self.agent_dir(agent_id).join("payments")
    }

    pub fn agent_exists(&self, agent_id: &str) -> bool {
        self.agent_file(agent_id).is_file()
    }

    pub fn register_agent(
        &self,
        record: &AgentRecord,
        mandate: &AgentMandate,
        initial_state_commitment: &str,
    ) -> Result<()> {
        let dir = self.agent_dir(&record.agent_id);
        if dir.exists() {
            return Err(FacilitatorError::AgentAlreadyRegistered(record.agent_id.clone()));
        }
        fs::create_dir_all(&dir).map_err(io_err)?;
        fs::create_dir_all(self.queue_dir(&record.agent_id)).map_err(io_err)?;
        fs::create_dir_all(self.committed_dir(&record.agent_id)).map_err(io_err)?;
        fs::create_dir_all(self.failed_dir(&record.agent_id)).map_err(io_err)?;
        fs::create_dir_all(self.payments_dir(&record.agent_id)).map_err(io_err)?;

        atomic_write_json(&self.agent_file(&record.agent_id), record)?;
        atomic_write_json(&self.mandate_file(&record.agent_id), mandate)?;
        atomic_write_json(
            &self.pending_state_file(&record.agent_id),
            &PendingState {
                committed_state_commitment: initial_state_commitment.to_string(),
                pending_state_commitment: initial_state_commitment.to_string(),
                last_accepted_seq: 0,
            },
        )?;
        // Touch WAL files so later append-opens never error on "no such file".
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.nullifiers_wal(&record.agent_id))
            .map_err(io_err)?;
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.nullifiers_spent(&record.agent_id))
            .map_err(io_err)?;
        Ok(())
    }

    pub fn load_agent(&self, agent_id: &str) -> Result<AgentRecord> {
        read_json(&self.agent_file(agent_id))
            .map_err(|_| FacilitatorError::AgentNotRegistered(agent_id.to_string()))
    }
    pub fn load_mandate(&self, agent_id: &str) -> Result<AgentMandate> {
        read_json(&self.mandate_file(agent_id))
            .map_err(|_| FacilitatorError::AgentNotRegistered(agent_id.to_string()))
    }
    pub fn load_pending_state(&self, agent_id: &str) -> Result<PendingState> {
        read_json(&self.pending_state_file(agent_id))
            .map_err(|_| FacilitatorError::AgentNotRegistered(agent_id.to_string()))
    }

    /// Replay both WAL files to rebuild the live reservation set.
    /// Reserved set = (entries in `nullifiers.wal`) MINUS (entries in `nullifiers.spent`).
    pub fn nullifier_view(&self, agent_id: &str) -> Result<NullifierView> {
        let reserved = read_wal_set(&self.nullifiers_wal(agent_id))?;
        let spent = read_wal_set(&self.nullifiers_spent(agent_id))?;
        Ok(NullifierView { reserved, spent })
    }

    /// Append nullifier reservations to the WAL with fsync.
    pub fn reserve_nullifiers(&self, agent_id: &str, entries: &[WalEntry]) -> Result<()> {
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.nullifiers_wal(agent_id))
            .map_err(io_err)?;
        for e in entries {
            let line = serde_json::to_string(e).map_err(|err| {
                FacilitatorError::Internal(format!("serialize WAL entry: {err}"))
            })?;
            f.write_all(line.as_bytes()).map_err(io_err)?;
            f.write_all(b"\n").map_err(io_err)?;
        }
        f.sync_all().map_err(io_err)?;
        Ok(())
    }

    pub fn mark_spent(&self, agent_id: &str, entries: &[WalEntry]) -> Result<()> {
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.nullifiers_spent(agent_id))
            .map_err(io_err)?;
        for e in entries {
            let line = serde_json::to_string(e).map_err(|err| {
                FacilitatorError::Internal(format!("serialize spent entry: {err}"))
            })?;
            f.write_all(line.as_bytes()).map_err(io_err)?;
            f.write_all(b"\n").map_err(io_err)?;
        }
        f.sync_all().map_err(io_err)?;
        Ok(())
    }

    pub fn advance_pending_state(&self, agent_id: &str, new_state: &PendingState) -> Result<()> {
        atomic_write_json(&self.pending_state_file(agent_id), new_state)
    }

    pub fn enqueue(&self, agent_id: &str, tx: &QueuedTx) -> Result<()> {
        atomic_write_json(&self.queue_dir(agent_id).join(format!("{}.json", tx.seq)), tx)?;
        for n in &tx.nullifiers {
            let sentinel = self.payments_dir(agent_id).join(n);
            atomic_write_json(&sentinel, &PaymentLookup { seq: tx.seq })?;
        }
        Ok(())
    }

    /// Find a queued tx by agent + seq across all status directories
    /// (pending_queue / committed / failed) and overwrite it in place
    /// with the new state. Used by the batch worker to record
    /// timestamps and lifecycle transitions.
    pub fn upsert_queued(&self, agent_id: &str, tx: &QueuedTx) -> Result<()> {
        for sub in ["pending_queue", "committed", "failed"] {
            let p = self.agent_dir(agent_id).join(sub).join(format!("{}.json", tx.seq));
            if p.is_file() {
                return atomic_write_json(&p, tx);
            }
        }
        atomic_write_json(&self.queue_dir(agent_id).join(format!("{}.json", tx.seq)), tx)
    }

    pub fn list_queued(&self, agent_id: &str) -> Result<Vec<QueuedTx>> {
        let dir = self.queue_dir(agent_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            if !entry.file_type().map_err(io_err)?.is_file() {
                continue;
            }
            let tx: QueuedTx = read_json(&entry.path())?;
            out.push(tx);
        }
        out.sort_by_key(|t| t.seq);
        Ok(out)
    }

    pub fn list_agents(&self) -> Result<Vec<String>> {
        let dir = self.root.join("agents");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            if entry.file_type().map_err(io_err)?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        Ok(out)
    }

    pub fn move_to_committed(&self, agent_id: &str, seq: u64) -> Result<()> {
        let src = self.queue_dir(agent_id).join(format!("{seq}.json"));
        let dst = self.committed_dir(agent_id).join(format!("{seq}.json"));
        fs::rename(&src, &dst).map_err(io_err)
    }

    pub fn move_to_failed(&self, agent_id: &str, seq: u64) -> Result<()> {
        let src = self.queue_dir(agent_id).join(format!("{seq}.json"));
        let dst = self.failed_dir(agent_id).join(format!("{seq}.json"));
        fs::rename(&src, &dst).map_err(io_err)
    }

    pub fn lookup_payment(&self, agent_id: &str, nullifier: &str) -> Result<QueuedTx> {
        let sentinel = self.payments_dir(agent_id).join(nullifier);
        let lookup: PaymentLookup = read_json(&sentinel)
            .map_err(|_| FacilitatorError::NotFound(format!("payment {nullifier}")))?;
        for sub in ["pending_queue", "committed", "failed"] {
            let p = self.agent_dir(agent_id).join(sub).join(format!("{}.json", lookup.seq));
            if p.is_file() {
                return read_json(&p);
            }
        }
        Err(FacilitatorError::NotFound(format!("payment {nullifier} record missing")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEntry {
    pub nullifier: String,
    pub seq: u64,
    pub ts_unix_micros: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaymentLookup {
    seq: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NullifierView {
    pub reserved: HashSet<String>,
    pub spent: HashSet<String>,
}

impl NullifierView {
    pub fn contains(&self, nullifier: &str) -> bool {
        self.reserved.contains(nullifier) || self.spent.contains(nullifier)
    }
}

fn read_wal_set(path: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    if !path.exists() {
        return Ok(out);
    }
    let file = File::open(path).map_err(io_err)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(io_err)?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: WalEntry = serde_json::from_str(&line)
            .map_err(|e| FacilitatorError::Internal(format!("corrupt WAL entry: {e}")))?;
        out.insert(entry.nullifier);
    }
    Ok(out)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| FacilitatorError::Internal(format!("path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent).map_err(io_err)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("write")
    ));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| FacilitatorError::Internal(format!("serialize: {e}")))?;
    {
        let mut f = File::create(&tmp).map_err(io_err)?;
        f.write_all(&bytes).map_err(io_err)?;
        f.sync_all().map_err(io_err)?;
    }
    fs::rename(&tmp, path).map_err(io_err)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(io_err)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| FacilitatorError::Internal(format!("read {}: {e}", path.display())))
}

fn io_err(e: std::io::Error) -> FacilitatorError {
    FacilitatorError::Internal(format!("io: {e}"))
}
