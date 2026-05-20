//! Batch settle worker — NEW_DESIGN.md §60-87.
//!
//! The worker:
//!
//! 1. Drains up to `BATCH_MAX_SIZE` verified txs from the
//!    [`crate::storage::BatchQueueRepo`] (also drains when the oldest
//!    is older than `BATCH_MAX_AGE_MS`).
//! 2. Proves each tx **in parallel** via per-tx
//!    `RemoteTransactionProver::prove` (NEW_DESIGN §68: "Generate
//!    STARK proofs in parallel, per tx").
//! 3. Assembles the proven txs into a `TransactionBatch` and submits
//!    via the Miden node's `SubmitProvenBatch` RPC.
//! 4. On success: `mark_submitted` + `promote_for_tx` for the
//!    reservations.
//! 5. On per-tx failure: `release_for_tx` for the offender +
//!    pending-state rollback + client notification; resubmit the
//!    remaining batch (NEW_DESIGN §83-87).

pub mod worker;

pub use worker::{BatchSettleQueue, BatchSettleWorker};
