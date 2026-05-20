//! Batch settle worker.
//!
//! Per DESIGN.md, settle is **async** — the merchant gets back a receipt
//! immediately and the actual prove + submit happens later, batched across
//! multiple buyers. This module implements that worker.
//!
//! Lifecycle of a queued tx:
//!
//! 1. `/x402/settle` calls [`BatchSettleQueue::enqueue`] with a
//!    [`crate::storage::BatchQueueEntry`].
//! 2. [`BatchSettleWorker`] wakes every `tick_interval` and drains:
//!    - if the queue has ≥ `max_batch_size` entries, or
//!    - if the oldest entry is older than `max_batch_age`.
//! 3. For each drained entry:
//!    - prove via [`ProvenTxProver`]
//!    - submit via [`ProvenTxSubmitter`]
//!    - on success: `mark_submitted` + `promote_to_consumed`
//!    - on failure: `release` the reservations, drop the entry (DESIGN.md
//!      doesn't specify a retry policy; we choose drop-and-log to keep
//!      the v1 surface minimal).
//!
//! The "inclusion bridge" that flips promoted reservations to consumed
//! once the on-chain block lands is out of scope for this module — it's
//! the binary's job to wire that up against Guardian's canonicalization
//! worker (see [`docs/UPSTREAM_WISHLIST.md`] for what would simplify
//! this; today the promotion lifetime is effectively the reservation TTL).

use std::sync::Arc;

use async_trait::async_trait;
use miden_protocol::transaction::TransactionInputs;
use thiserror::Error;
use tokio::sync::Notify;

use crate::config::BatchSettleConfig;
use crate::storage::{BatchQueueEntry, BatchQueueRepo, ReservationRepo, unix_now};

/// Errors from the prover backend.
#[derive(Debug, Error)]
pub enum ProverError {
    #[error("remote prover failed: {0}")]
    Backend(String),
}

/// Errors from the node-submit backend.
#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("node rejected tx: {0}")]
    Backend(String),
}

/// Proves a signed-unproven `TransactionInputs` into a serialised
/// proven-tx blob (plus the canonical post-prove tx id).
#[async_trait]
pub trait ProvenTxProver: Send + Sync + 'static {
    async fn prove(
        &self,
        tx_inputs: TransactionInputs,
    ) -> Result<ProvenTx, ProverError>;
}

/// Submits a proven tx to the Miden node.
#[async_trait]
pub trait ProvenTxSubmitter: Send + Sync + 'static {
    async fn submit(&self, proven: ProvenTx) -> Result<(), SubmitError>;
}

/// Opaque proven-tx blob (the wire-serialised `ProvenTransaction` plus
/// its hex id). Kept as a struct so backends can be swapped without
/// changing call sites.
#[derive(Debug, Clone)]
pub struct ProvenTx {
    pub id_hex: String,
    pub bytes: Vec<u8>,
}

/// Light handle around a [`BatchQueueRepo`] — adds a `Notify` so the
/// worker can be woken when a new entry lands and the queue reaches the
/// size threshold mid-tick.
#[derive(Clone)]
pub struct BatchSettleQueue {
    repo: Arc<dyn BatchQueueRepo>,
    wake: Arc<Notify>,
}

impl BatchSettleQueue {
    pub fn new(repo: Arc<dyn BatchQueueRepo>) -> Self {
        Self { repo, wake: Arc::new(Notify::new()) }
    }

    pub async fn enqueue(&self, entry: BatchQueueEntry) -> Result<(), crate::storage::StorageError> {
        self.repo.enqueue(entry).await?;
        self.wake.notify_one();
        Ok(())
    }

    pub async fn len(&self) -> Result<usize, crate::storage::StorageError> {
        self.repo.len().await
    }

    pub async fn lookup(
        &self,
        queued_id: &str,
    ) -> Result<Option<BatchQueueEntry>, crate::storage::StorageError> {
        self.repo.lookup(queued_id).await
    }

    pub fn repo(&self) -> Arc<dyn BatchQueueRepo> { self.repo.clone() }
}

/// Background worker.
pub struct BatchSettleWorker {
    queue: BatchSettleQueue,
    reservations: Arc<dyn ReservationRepo>,
    prover: Arc<dyn ProvenTxProver>,
    submitter: Arc<dyn ProvenTxSubmitter>,
    config: BatchSettleConfig,
    /// Optional force-drain trigger for tests. Set high (or `None`) in
    /// production. The worker checks this notifier alongside the ticker.
    test_hook: Arc<Notify>,
}

impl BatchSettleWorker {
    pub fn new(
        queue: BatchSettleQueue,
        reservations: Arc<dyn ReservationRepo>,
        prover: Arc<dyn ProvenTxProver>,
        submitter: Arc<dyn ProvenTxSubmitter>,
        config: BatchSettleConfig,
    ) -> Self {
        Self {
            queue,
            reservations,
            prover,
            submitter,
            config,
            test_hook: Arc::new(Notify::new()),
        }
    }

    /// Returns a handle that test code can use to force an immediate
    /// drain, regardless of `max_batch_size` / `max_batch_age` thresholds.
    pub fn test_hook(&self) -> Arc<Notify> { self.test_hook.clone() }

    /// Spawns the background task and returns immediately. Loops until
    /// the process exits.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    async fn run(self: Arc<Self>) {
        let tick = self.config.tick_interval;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick) => {},
                _ = self.queue.wake.notified() => {},
                _ = self.test_hook.notified() => {
                    self.drain_force().await;
                    continue;
                }
            }
            self.drain_threshold().await;
        }
    }

    /// Drains by the configured thresholds (size or age).
    async fn drain_threshold(&self) {
        let now = unix_now();
        let drained = match self
            .queue
            .repo
            .drain_batch(self.config.max_batch_size, self.config.max_batch_age, now)
            .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "batch worker: drain_batch failed");
                return;
            }
        };
        if drained.is_empty() {
            return;
        }
        self.process_batch(drained).await;
    }

    /// Force-drain everything regardless of thresholds. Used by the
    /// `drain_now_for_testing` hook.
    pub async fn drain_force(&self) {
        let drained = match self.queue.repo.drain_all().await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "batch worker: drain_all failed");
                return;
            }
        };
        if drained.is_empty() {
            return;
        }
        self.process_batch(drained).await;
    }

    async fn process_batch(&self, batch: Vec<BatchQueueEntry>) {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as BASE64;
        use miden_client::Deserializable;

        for entry in batch {
            let tx_inputs_bytes = match BASE64.decode(entry.tx_inputs_b64.trim()) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(queued_id = %entry.queued_id, error = %e,
                        "batch worker: tx_inputs base64 decode failed; releasing reservation");
                    let _ = self.reservations.release(&entry.reserved_nullifiers).await;
                    continue;
                }
            };
            let tx_inputs = match TransactionInputs::read_from_bytes(&tx_inputs_bytes) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(queued_id = %entry.queued_id, error = %e,
                        "batch worker: tx_inputs decode failed; releasing reservation");
                    let _ = self.reservations.release(&entry.reserved_nullifiers).await;
                    continue;
                }
            };

            let proven = match self.prover.prove(tx_inputs).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(queued_id = %entry.queued_id, error = %e,
                        "batch worker: prove failed; releasing reservation");
                    let _ = self.reservations.release(&entry.reserved_nullifiers).await;
                    continue;
                }
            };

            let on_chain_tx_id = proven.id_hex.clone();
            match self.submitter.submit(proven).await {
                Ok(_) => {
                    if let Err(e) = self
                        .queue
                        .repo
                        .mark_submitted(&entry.queued_id, &on_chain_tx_id)
                        .await
                    {
                        tracing::warn!(queued_id = %entry.queued_id, error = %e,
                            "batch worker: mark_submitted failed");
                    }
                    if let Err(e) = self
                        .reservations
                        .promote_to_consumed(&entry.reserved_nullifiers)
                        .await
                    {
                        tracing::warn!(queued_id = %entry.queued_id, error = %e,
                            "batch worker: promote_to_consumed failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(queued_id = %entry.queued_id, error = %e,
                        "batch worker: node submit failed; releasing reservation");
                    let _ = self.reservations.release(&entry.reserved_nullifiers).await;
                    let _ = self.queue.repo.delete(&entry.queued_id).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory::{MemoryBatchQueueRepo, MemoryReservationRepo};
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct MockProver {
        ids: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl ProvenTxProver for MockProver {
        async fn prove(&self, _: TransactionInputs) -> Result<ProvenTx, ProverError> {
            let mut ids = self.ids.lock().unwrap();
            let next = format!("0x{:0>64}", ids.len());
            ids.push(next.clone());
            Ok(ProvenTx { id_hex: next, bytes: vec![1, 2, 3] })
        }
    }

    #[derive(Default)]
    struct MockSubmitter {
        submitted: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl ProvenTxSubmitter for MockSubmitter {
        async fn submit(&self, proven: ProvenTx) -> Result<(), SubmitError> {
            self.submitted.lock().unwrap().push(proven.id_hex);
            Ok(())
        }
    }

    fn entry(queued_id: &str, nullifiers: Vec<String>, age_secs: u64) -> BatchQueueEntry {
        let now = unix_now();
        BatchQueueEntry {
            queued_id: queued_id.into(),
            payer: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
            tx_inputs_b64: "AAA=".into(), // base64 of [0] — decode succeeds, TxInputs decode will fail in tests; OK for shape
            reserved_nullifiers: nullifiers,
            network: "miden:testnet".into(),
            enqueued_at_unix_secs: now.saturating_sub(age_secs),
            submitted: false,
            on_chain_tx_id: None,
        }
    }

    #[tokio::test]
    async fn drain_threshold_skips_when_too_young_and_too_few() {
        let repo: Arc<dyn BatchQueueRepo> = Arc::new(MemoryBatchQueueRepo::new());
        let queue = BatchSettleQueue::new(repo);
        queue.enqueue(entry("q1", vec!["n1".into()], 0)).await.unwrap();

        let reservations: Arc<dyn ReservationRepo> = Arc::new(MemoryReservationRepo::new());
        let prover: Arc<dyn ProvenTxProver> = Arc::new(MockProver::default());
        let submitter: Arc<dyn ProvenTxSubmitter> = Arc::new(MockSubmitter::default());
        let cfg = BatchSettleConfig {
            max_batch_size: 10,
            max_batch_age: Duration::from_secs(60),
            tick_interval: Duration::from_millis(100),
        };
        let worker = BatchSettleWorker::new(queue.clone(), reservations, prover, submitter, cfg);

        // Nothing drains: queue has 1, threshold is 10, age is 0.
        worker.drain_threshold().await;
        assert_eq!(queue.len().await.unwrap(), 1);
    }
}
