//! Postgres + Diesel backend impls.
//!
//! **Skeleton only** in this branch. The schema (`schema.rs`) and
//! migrations (`migrations/`) are committed; the trait implementations
//! are wired up but currently delegate to the in-memory backend with a
//! TODO marker, so the binary compiles and `cargo test` passes against
//! the trait surface. Follow-up work: implement each trait against
//! `diesel_async::AsyncPgConnection` using the patterns in the inicio-labs
//! [`coordinator/store/src/persistence/store.rs`](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/crates/coordinator/store/src/persistence/store.rs).
//!
//! Pattern reference:
//!
//! - Connection pool: `diesel_async::pooled_connection::deadpool::Pool<AsyncPgConnection>`.
//! - Transactions: `conn.transaction::<_, Error, _>(|conn| async move { ... })`.
//! - Atomic CAS for `try_advance`: `UPDATE pending_states SET current_commitment = $new, nonce = $nonce WHERE agent_account_id = $aid AND current_commitment = $prev` — `rows_affected = 0` means conflict.
//! - Atomic batch reserve for nullifiers: insert with `ON CONFLICT (nullifier_hex) DO NOTHING RETURNING nullifier_hex` — count of returned rows must equal input length.

#![allow(unused)]

use std::time::Duration;

use async_trait::async_trait;

use super::{
    AgentRecord, AgentRegistryRepo, BatchQueueEntry, BatchQueueRepo, ChallengeRecord,
    ChallengeRepo, MandateCounterRepo, MandateRecord, MandateRepo, PendingState, PendingStateRepo,
    ReservationRepo, StorageError, StorageResult,
    memory::{
        MemoryAgentRegistry, MemoryBatchQueueRepo, MemoryChallengeRepo, MemoryMandateCounterRepo,
        MemoryMandateRepo, MemoryPendingStateRepo, MemoryReservationRepo,
    },
};

/// Postgres backend. Currently a thin wrapper that delegates to memory
/// impls so the binary boots and tests pass; real Diesel impls land in
/// a follow-up commit.
#[derive(Clone, Default)]
pub struct PostgresStorage {
    pub agents: MemoryAgentRegistry,
    pub mandates: MemoryMandateRepo,
    pub pending: MemoryPendingStateRepo,
    pub reservations: MemoryReservationRepo,
    pub batch_queue: MemoryBatchQueueRepo,
    pub challenges: MemoryChallengeRepo,
    pub counters: MemoryMandateCounterRepo,
}

impl PostgresStorage {
    /// Will be `pub async fn connect(url: &str, max_conn: u32) -> Result<Self>`
    /// once the Diesel impls land.
    pub fn in_memory_fallback() -> Self { Self::default() }
}

// All trait impls just forward to the in-memory components for now —
// signalled by `// TODO: diesel impl` comments so reviewers see the gap.
// The trait surface is locked; the swap to real Diesel is mechanical.
#[async_trait]
impl AgentRegistryRepo for PostgresStorage {
    async fn register(&self, agent: AgentRecord) -> StorageResult<()> {
        // TODO: diesel impl — INSERT INTO agents
        self.agents.register(agent).await
    }
    async fn get(&self, id: &str) -> StorageResult<Option<AgentRecord>> {
        self.agents.get(id).await
    }
}

#[async_trait]
impl MandateRepo for PostgresStorage {
    async fn put(&self, m: MandateRecord) -> StorageResult<()> { self.mandates.put(m).await }
    async fn get(&self, id: &str) -> StorageResult<Option<MandateRecord>> {
        self.mandates.get(id).await
    }
    async fn list_for_agent(&self, aid: &str) -> StorageResult<Vec<MandateRecord>> {
        self.mandates.list_for_agent(aid).await
    }
}

#[async_trait]
impl PendingStateRepo for PostgresStorage {
    async fn get(&self, aid: &str) -> StorageResult<Option<PendingState>> {
        self.pending.get(aid).await
    }
    async fn try_advance(
        &self,
        aid: &str,
        expected_prev: &str,
        new_commitment: &str,
        new_nonce: u64,
    ) -> StorageResult<()> {
        // TODO: diesel impl — UPDATE pending_states ... WHERE current_commitment = $prev
        self.pending.try_advance(aid, expected_prev, new_commitment, new_nonce).await
    }
    async fn rollback(
        &self,
        aid: &str,
        rolled_back: &str,
        prev: &str,
        prev_nonce: u64,
    ) -> StorageResult<()> {
        self.pending.rollback(aid, rolled_back, prev, prev_nonce).await
    }
}

#[async_trait]
impl ReservationRepo for PostgresStorage {
    async fn try_reserve_all(
        &self,
        nullifiers: &[String],
        ttl: Duration,
        owner: &str,
    ) -> StorageResult<()> {
        // TODO: diesel impl — bulk INSERT ... ON CONFLICT DO NOTHING RETURNING
        self.reservations.try_reserve_all(nullifiers, ttl, owner).await
    }
    async fn release_for_tx(&self, owner: &str) -> StorageResult<()> {
        self.reservations.release_for_tx(owner).await
    }
    async fn promote_for_tx(&self, owner: &str) -> StorageResult<()> {
        self.reservations.promote_for_tx(owner).await
    }
    async fn sweep(&self, now: u64) -> StorageResult<usize> {
        self.reservations.sweep(now).await
    }
}

#[async_trait]
impl BatchQueueRepo for PostgresStorage {
    async fn enqueue(&self, e: BatchQueueEntry) -> StorageResult<()> {
        self.batch_queue.enqueue(e).await
    }
    async fn drain_batch(
        &self,
        max: usize,
        age: Duration,
        now: u64,
    ) -> StorageResult<Vec<BatchQueueEntry>> {
        self.batch_queue.drain_batch(max, age, now).await
    }
    async fn drain_all(&self) -> StorageResult<Vec<BatchQueueEntry>> {
        self.batch_queue.drain_all().await
    }
    async fn mark_submitted(&self, qid: &str, txid: &str) -> StorageResult<()> {
        self.batch_queue.mark_submitted(qid, txid).await
    }
    async fn delete(&self, qid: &str) -> StorageResult<()> { self.batch_queue.delete(qid).await }
    async fn lookup(&self, qid: &str) -> StorageResult<Option<BatchQueueEntry>> {
        self.batch_queue.lookup(qid).await
    }
    async fn len(&self) -> StorageResult<usize> { self.batch_queue.len().await }
}

#[async_trait]
impl ChallengeRepo for PostgresStorage {
    async fn put(&self, c: ChallengeRecord) -> StorageResult<()> {
        self.challenges.put(c).await
    }
    async fn consume(&self, sn: &str) -> StorageResult<ChallengeRecord> {
        self.challenges.consume(sn).await
    }
    async fn sweep(&self, now: u64) -> StorageResult<usize> {
        self.challenges.sweep(now).await
    }
}

#[async_trait]
impl MandateCounterRepo for PostgresStorage {
    async fn add(
        &self,
        agent_id: &str,
        window_start: u64,
        amount: u64,
    ) -> StorageResult<()> {
        self.counters.add(agent_id, window_start, amount).await
    }
    async fn sum_recent(
        &self,
        agent_id: &str,
        lookback: u64,
        now: u64,
    ) -> StorageResult<u64> {
        self.counters.sum_recent(agent_id, lookback, now).await
    }
}
