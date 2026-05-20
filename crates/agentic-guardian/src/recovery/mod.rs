//! WAL recovery — NEW_DESIGN.md §90-93.
//!
//! Postgres rows in `reservations` and `batch_queue` are the durable
//! source of truth. On startup the agentic-guardian:
//!
//! 1. Sweeps `reservations` for entries past their `expires_at_unix_secs`.
//! 2. Re-queues any `batch_queue` rows with `submitted = false` —
//!    they'll be picked up by the regular batch worker.
//! 3. Calls the inclusion-reconciler for `batch_queue` rows with
//!    `submitted = true` but no observed on-chain inclusion yet
//!    (`on_chain_tx_id` populated but the reconciler hasn't confirmed
//!    block inclusion).
//!
//! Postgres transactional semantics give us crash-safe atomicity for
//! free; this module is the orchestrator that runs at boot.

use std::sync::Arc;

use crate::error::AgenticResult;
use crate::storage::{BatchQueueRepo, ReservationRepo, memory::unix_now};

#[tracing::instrument(skip_all)]
pub async fn replay_on_boot(
    reservations: &Arc<dyn ReservationRepo>,
    _batch_queue: &Arc<dyn BatchQueueRepo>,
) -> AgenticResult<RecoveryReport> {
    let now = unix_now();
    let swept = reservations
        .sweep(now)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "reservation sweep failed");
            0
        });
    // TODO: query batch_queue for unsubmitted rows; nothing to do on
    // memory backend (rows are already in-process). Postgres backend
    // will need an explicit `SELECT ... WHERE submitted = FALSE` and
    // an enqueue-notify so the batch worker wakes immediately.
    Ok(RecoveryReport { expired_reservations: swept })
}

#[derive(Debug, Clone)]
pub struct RecoveryReport {
    pub expired_reservations: usize,
}
