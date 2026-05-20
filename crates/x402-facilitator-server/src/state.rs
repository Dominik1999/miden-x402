//! Shared application state passed to every handler.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dashmap::DashMap;
use tokio::sync::Mutex;

use crate::key::FacilitatorKey;
use crate::store::FilesystemX402Store;
use crate::submitter::SubmitterHandle;

/// Axum extractor state for all x402 handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: FilesystemX402Store,
    pub facilitator_key: FacilitatorKey,
    pub locks: PerAgentLocks,
    /// `None` when running without a Miden RPC endpoint configured
    /// (e.g. integration tests). When set, the batch worker uses it
    /// to prove + submit accepted payments. The `SubmitterHandle` is
    /// a `Send + Sync` proxy that communicates with the underlying
    /// non-`Send` `miden-client` instance via a channel-backed actor.
    pub submitter: Option<SubmitterHandle>,
    /// Set true by the binary when MIDEN_RPC_ENDPOINT is configured.
    pub submitter_available: Arc<AtomicBool>,
}

impl AppState {
    pub fn submitter_available(&self) -> bool {
        self.submitter_available.load(Ordering::Relaxed)
    }
}

/// Per-agent serial mutex so steps 1..9 of the verify chain run
/// atomically against pending state. Cross-agent throughput is
/// unbounded; this only serializes within a single agent.
#[derive(Default, Clone)]
pub struct PerAgentLocks {
    inner: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl PerAgentLocks {
    pub fn for_agent(&self, agent_id: &str) -> Arc<Mutex<()>> {
        if let Some(m) = self.inner.get(agent_id) {
            return m.clone();
        }
        self.inner
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}
