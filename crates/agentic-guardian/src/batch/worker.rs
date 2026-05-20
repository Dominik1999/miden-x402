//! Batch settle worker — drain → parallel-prove → SubmitProvenBatch loop.
//!
//! **Skeleton**: drain + reservation lifecycle work; the prove +
//! submit calls forward to the `MidenRuntimeHandle` (which itself is a
//! skeleton). Wiring the runtime to a real Miden client unblocks
//! prove + submit without touching this module.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::config::BatchConfig;
use crate::error::AgenticError;
use crate::runtime::{MidenRuntimeHandle, MidenRuntimeMsg, msg::SubmitBatchResult};
use crate::storage::{BatchQueueEntry, BatchQueueRepo, ReservationRepo, memory::unix_now};

/// Light handle around a [`BatchQueueRepo`] with a `Notify` so the
/// worker can wake on enqueue.
#[derive(Clone)]
pub struct BatchSettleQueue {
    repo: Arc<dyn BatchQueueRepo>,
    wake: Arc<Notify>,
}

impl BatchSettleQueue {
    pub fn new(repo: Arc<dyn BatchQueueRepo>) -> Self {
        Self { repo, wake: Arc::new(Notify::new()) }
    }

    pub async fn enqueue(&self, entry: BatchQueueEntry) -> Result<(), AgenticError> {
        self.repo
            .enqueue(entry)
            .await
            .map_err(|e| AgenticError::Storage(e.to_string()))?;
        self.wake.notify_one();
        Ok(())
    }

    pub fn repo(&self) -> Arc<dyn BatchQueueRepo> { self.repo.clone() }
}

/// Background worker.
pub struct BatchSettleWorker {
    queue: BatchSettleQueue,
    reservations: Arc<dyn ReservationRepo>,
    runtime: MidenRuntimeHandle,
    config: BatchConfig,
}

impl BatchSettleWorker {
    pub fn new(
        queue: BatchSettleQueue,
        reservations: Arc<dyn ReservationRepo>,
        runtime: MidenRuntimeHandle,
        config: BatchConfig,
    ) -> Self {
        Self { queue, reservations, runtime, config }
    }

    /// Spawns the worker on the multi-threaded tokio runtime. Returns
    /// the join handle.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    async fn run(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.tick_interval) => {},
                _ = self.queue.wake.notified() => {},
            }
            self.drain_once().await;
        }
    }

    async fn drain_once(&self) {
        let now = unix_now();
        let drained = match self
            .queue
            .repo
            .drain_batch(self.config.max_batch_size, self.config.max_batch_age, now)
            .await
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "batch worker: drain failed");
                return;
            }
        };
        if drained.is_empty() {
            return;
        }
        self.process_batch(drained).await;
    }

    async fn process_batch(&self, batch: Vec<BatchQueueEntry>) {
        // TODO: parallel prove via `RemoteTransactionProver::prove` per
        // entry, then assemble `TransactionBatch`, then submit via
        // `MidenRuntimeMsg::SubmitProvenBatch`. On per-tx failure
        // release that entry's reservations + notify the client; on
        // partial-batch reject, halve-and-retry to isolate the
        // offender (NEW_DESIGN §83-87).
        for entry in batch {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let send_result = self.runtime.send(MidenRuntimeMsg::SubmitProvenBatch {
                proven_txs_b64: vec![/* TODO: real proven_tx for this entry */],
                reply: tx,
            });
            if let Err(e) = send_result {
                tracing::warn!(error = %e, queued_id = %entry.queued_id, "runtime closed");
                let _ = self.reservations.release_for_tx(&entry.queued_id).await;
                let _ = self.queue.repo.delete(&entry.queued_id).await;
                continue;
            }
            let reply = rx.await;
            match reply {
                Ok(crate::runtime::msg::RuntimeResponse::Ok(SubmitBatchResult { block_num })) => {
                    tracing::info!(queued_id = %entry.queued_id, block_num, "tx submitted");
                    let _ = self
                        .queue
                        .repo
                        .mark_submitted(&entry.queued_id, &format!("blk-{block_num}"))
                        .await;
                    let _ = self.reservations.promote_for_tx(&entry.queued_id).await;
                }
                Ok(crate::runtime::msg::RuntimeResponse::Err(e)) => {
                    tracing::warn!(error = %e, queued_id = %entry.queued_id,
                        "runtime returned error; releasing reservation");
                    let _ = self.reservations.release_for_tx(&entry.queued_id).await;
                    let _ = self.queue.repo.delete(&entry.queued_id).await;
                }
                Err(_) => {
                    tracing::warn!(queued_id = %entry.queued_id, "runtime reply channel dropped");
                    let _ = self.reservations.release_for_tx(&entry.queued_id).await;
                    let _ = self.queue.repo.delete(&entry.queued_id).await;
                }
            }
        }
    }

    /// Test-only hook: force a drain regardless of size/age thresholds.
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn drain_now_for_testing(&self) {
        if let Ok(drained) = self.queue.repo.drain_all().await {
            if !drained.is_empty() {
                self.process_batch(drained).await;
            }
        }
    }

    pub fn config(&self) -> &BatchConfig { &self.config }
    pub fn config_max_age(&self) -> Duration { self.config.max_batch_age }
}
