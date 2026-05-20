//! Storage abstractions for the agentic-guardian.
//!
//! The agentic-guardian persists six kinds of records:
//!
//! - **Agents** — `(agent_account_id, hot_pubkey_commitment, cold_pubkey_commitment)`
//!   from `POST /agentic/register`.
//! - **Mandates** — `Ap2SignedMandate` per agent, keyed by `mandate_id`.
//! - **Pending states** — per-agent `(current_commitment, nonce, last_advanced_at)`.
//! - **Reservations** — nullifier locks held during verify-before-prove.
//! - **Batch queue** — verified-but-unsubmitted txs waiting on the batch
//!   worker.
//! - **Mandate counters** — per-`(agent, window_start)` rolling totals.
//!
//! Each kind has a trait so impls can be swapped. The default impl in
//! [`postgres`] uses Diesel + Postgres (matching the inicio-labs
//! coordinator-server pattern); [`memory`] provides an in-memory backend
//! for tests and local development.

pub mod memory;
pub mod postgres;
pub mod schema;
pub mod types;

pub use types::*;

use async_trait::async_trait;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O / backend error: {0}")]
    Backend(String),
    #[error("serialisation error: {0}")]
    Serialisation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found")]
    NotFound,
}

pub type StorageResult<T> = Result<T, StorageError>;

#[async_trait]
pub trait AgentRegistryRepo: Send + Sync + 'static {
    async fn register(&self, agent: AgentRecord) -> StorageResult<()>;
    async fn get(&self, agent_account_id: &str) -> StorageResult<Option<AgentRecord>>;
}

#[async_trait]
pub trait MandateRepo: Send + Sync + 'static {
    async fn put(&self, mandate: MandateRecord) -> StorageResult<()>;
    async fn get(&self, mandate_id: &str) -> StorageResult<Option<MandateRecord>>;
    async fn list_for_agent(&self, agent_account_id: &str) -> StorageResult<Vec<MandateRecord>>;
}

#[async_trait]
pub trait PendingStateRepo: Send + Sync + 'static {
    async fn get(&self, agent_account_id: &str) -> StorageResult<Option<PendingState>>;
    /// Atomic CAS-like advance: succeeds only if the stored
    /// `current_commitment` equals `expected_prev`; otherwise returns
    /// `Conflict`. Required to keep multiple in-flight submissions from
    /// racing past each other (NEW_DESIGN §111).
    async fn try_advance(
        &self,
        agent_account_id: &str,
        expected_prev: &str,
        new_commitment: &str,
        new_nonce: u64,
    ) -> StorageResult<()>;
    async fn rollback(
        &self,
        agent_account_id: &str,
        rolled_back_commitment: &str,
        previous_commitment: &str,
        previous_nonce: u64,
    ) -> StorageResult<()>;
}

#[async_trait]
pub trait ReservationRepo: Send + Sync + 'static {
    /// Atomically reserves a batch of nullifier hex strings. All-or-nothing.
    async fn try_reserve_all(
        &self,
        nullifiers: &[String],
        ttl: Duration,
        owning_queued_id: &str,
    ) -> StorageResult<()>;
    async fn release_for_tx(&self, owning_queued_id: &str) -> StorageResult<()>;
    async fn promote_for_tx(&self, owning_queued_id: &str) -> StorageResult<()>;
    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize>;
}

#[async_trait]
pub trait BatchQueueRepo: Send + Sync + 'static {
    /// Idempotent on `queued_id`.
    async fn enqueue(&self, entry: BatchQueueEntry) -> StorageResult<()>;
    async fn drain_batch(
        &self,
        max_batch_size: usize,
        max_batch_age: Duration,
        now_unix_secs: u64,
    ) -> StorageResult<Vec<BatchQueueEntry>>;
    async fn drain_all(&self) -> StorageResult<Vec<BatchQueueEntry>>;
    async fn mark_submitted(
        &self,
        queued_id: &str,
        on_chain_tx_id: &str,
    ) -> StorageResult<()>;
    async fn delete(&self, queued_id: &str) -> StorageResult<()>;
    async fn lookup(&self, queued_id: &str) -> StorageResult<Option<BatchQueueEntry>>;
    async fn len(&self) -> StorageResult<usize>;
}

#[async_trait]
pub trait ChallengeRepo: Send + Sync + 'static {
    async fn put(&self, challenge: ChallengeRecord) -> StorageResult<()>;
    async fn consume(&self, serial_num_hex: &str) -> StorageResult<ChallengeRecord>;
    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize>;
}

#[async_trait]
pub trait MandateCounterRepo: Send + Sync + 'static {
    /// Adds `amount` to the bucket for `(agent_account_id,
    /// window_start_unix_secs)`. Used after a successful mandate
    /// evaluation.
    async fn add(
        &self,
        agent_account_id: &str,
        window_start_unix_secs: u64,
        amount: u64,
    ) -> StorageResult<()>;
    /// Sums all amounts for `agent_account_id` over the last `lookback`
    /// seconds.
    async fn sum_recent(
        &self,
        agent_account_id: &str,
        lookback_secs: u64,
        now_unix_secs: u64,
    ) -> StorageResult<u64>;
}
