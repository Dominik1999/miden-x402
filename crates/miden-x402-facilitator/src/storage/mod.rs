//! Storage abstractions for the Guardian-facilitator's mutable state.
//!
//! The Guardian-facilitator persists three short-lived kinds of records:
//!
//! - **Challenges** — server-issued `serial_num` values handed to merchants
//!   on `POST /x402/challenge`. Consumed on the corresponding
//!   `POST /x402/verify` call.
//! - **Reservations** — nullifier locks held during the verify-before-prove
//!   window. Promoted to `Consumed` when the batch worker submits the tx;
//!   released on failure; swept on TTL expiry.
//! - **Batch queue entries** — verified-but-unsubmitted txs waiting for the
//!   `BatchSettleWorker` to drain them.
//!
//! Plus one long-lived record:
//!
//! - **Facilitator receipt key** — the Falcon keypair the facilitator uses
//!   to sign `SettleResponse::Success { receipt_sig, .. }`. Persistent across
//!   restarts so merchants don't have to re-fetch the pubkey after every
//!   bounce.
//!
//! The traits in this module isolate those four storage concerns so the
//! impls can be swapped. The default is filesystem-backed (single-process,
//! good for the reference deployment); the trait shape is also compatible
//! with a future Postgres impl for horizontally-scaled facilitator
//! operators.
//!
//! Why not reuse OZ Guardian's `StorageBackend`? Guardian's trait is
//! account-scoped (`pull_state`, `submit_delta_proposal`, ...) and has no
//! notion of arbitrary TTL'd key-value records. Defining our own traits
//! keeps the facilitator concerns isolated from Guardian's account-state
//! storage.

pub mod filesystem;
pub mod memory;

use std::time::{Duration, Instant};

use async_trait::async_trait;
use miden_protocol::Word;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use miden_x402_types::{AccountIdHex, MidenPaymentRequirements, NoteIdHex};

/// Storage-layer error.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found")]
    NotFound,
    #[error("backend error: {0}")]
    Backend(String),
}

pub type StorageResult<T> = Result<T, StorageError>;

// ---------- Challenge store ----------

/// A challenge issued at `POST /x402/challenge` and consumed at
/// `POST /x402/verify` / `POST /x402/settle`.
///
/// The `serial_num` doubles as both the future P2ID note's `serial_num` and
/// as the challenge identifier — one issue, one consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuedChallenge {
    /// The 4-felt `Word` (serialised as canonical hex on the wire).
    pub serial_num: SerializableWord,
    /// Same value as `NoteIdHex` for wire echo.
    pub serial_num_hex: NoteIdHex,
    /// Snapshot of the merchant's requirements at issue time.
    pub requirements: MidenPaymentRequirements,
    /// Issue timestamp (`SystemTime::UNIX_EPOCH` seconds — serialisable
    /// across the storage boundary, unlike `Instant`).
    pub issued_at_unix_secs: u64,
    /// Expiry timestamp (`UNIX_EPOCH` seconds).
    pub expires_at_unix_secs: u64,
}

impl IssuedChallenge {
    /// Returns `true` if the challenge has expired relative to `now_unix_secs`.
    pub fn is_expired(&self, now_unix_secs: u64) -> bool {
        now_unix_secs >= self.expires_at_unix_secs
    }
}

#[async_trait]
pub trait ChallengeRepo: Send + Sync + 'static {
    /// Inserts a new challenge keyed by its `serial_num_hex`.
    async fn put(&self, challenge: IssuedChallenge) -> StorageResult<()>;
    /// Removes and returns a challenge by its `serial_num_hex`. Returns
    /// `StorageError::NotFound` if it does not exist (or was already consumed).
    async fn consume(&self, serial_num_hex: &str) -> StorageResult<IssuedChallenge>;
    /// Read-only lookup; does not consume. Returns `Ok(None)` if not present.
    async fn peek(&self, serial_num_hex: &str) -> StorageResult<Option<IssuedChallenge>>;
    /// Removes expired entries relative to the current wall-clock time.
    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize>;
}

// ---------- Reservation set ----------

/// State of a single reserved nullifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub reserved_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
    /// `true` once the corresponding tx has been submitted to the network
    /// by the batch worker; the entry is kept until the
    /// inclusion-bridge marks it consumed.
    pub promoted: bool,
}

#[async_trait]
pub trait ReservationRepo: Send + Sync + 'static {
    /// Atomically reserves every nullifier in `nullifiers`. Either every key
    /// is inserted or the call returns `StorageError::Conflict` (no partial
    /// state).
    async fn try_reserve_all(
        &self,
        nullifiers: &[String],
        ttl: Duration,
    ) -> StorageResult<()>;
    /// Flips `promoted = true` on each entry that exists; ignores missing
    /// entries.
    async fn promote_to_consumed(&self, nullifiers: &[String]) -> StorageResult<()>;
    /// Removes each entry that exists; ignores missing entries.
    async fn release(&self, nullifiers: &[String]) -> StorageResult<()>;
    /// Returns the current reservation, or `None` if the nullifier is not
    /// reserved.
    async fn get(&self, nullifier: &str) -> StorageResult<Option<Reservation>>;
    /// Removes expired entries (those whose `expires_at_unix_secs` is in the
    /// past relative to `now_unix_secs`).
    async fn sweep(&self, now_unix_secs: u64) -> StorageResult<usize>;
}

// ---------- Batch settle queue ----------

/// A verified-but-unsubmitted transaction waiting for the
/// `BatchSettleWorker` to prove + submit it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchQueueEntry {
    /// Deterministic per-tx id: `blake3(serial_num || tx_summary_commitment)`.
    pub queued_id: String,
    /// Buyer account id, echoed into `SettleResponse::Success { payer }`.
    pub payer: AccountIdHex,
    /// Base64 of canonical `TransactionInputs` — fed to the remote prover.
    pub tx_inputs_b64: String,
    /// Reserved nullifier hex strings — promoted to consumed on submit
    /// success, released on submit failure.
    pub reserved_nullifiers: Vec<String>,
    /// `CAIP-2` network id (e.g. `"miden:testnet"`) — echoed into the
    /// settle response.
    pub network: String,
    /// Enqueue timestamp; the batch worker uses it to honour the
    /// `max_batch_age_ms` config knob.
    pub enqueued_at_unix_secs: u64,
    /// `true` once submitted; the inclusion-bridge then removes the entry.
    pub submitted: bool,
    /// Post-prove `ProvenTransaction.id()` once available (hex).
    pub on_chain_tx_id: Option<String>,
}

#[async_trait]
pub trait BatchQueueRepo: Send + Sync + 'static {
    /// Enqueues a verified tx. Idempotent on `queued_id`: a duplicate enqueue
    /// is a no-op (so a retried `/x402/settle` doesn't enqueue twice).
    async fn enqueue(&self, entry: BatchQueueEntry) -> StorageResult<()>;
    /// Removes up to `max_batch_size` entries that are old enough to drain
    /// (older than `max_batch_age` from `now_unix_secs`). If the queue is
    /// shorter than `max_batch_size` but the oldest entry is older than
    /// `max_batch_age`, returns whatever's available.
    async fn drain_batch(
        &self,
        max_batch_size: usize,
        max_batch_age: Duration,
        now_unix_secs: u64,
    ) -> StorageResult<Vec<BatchQueueEntry>>;
    /// Force-drain all entries regardless of age — used by the
    /// `drain_now_for_testing` hook and by `max_batch_size` fast path.
    async fn drain_all(&self) -> StorageResult<Vec<BatchQueueEntry>>;
    /// After successful prove+submit, the worker writes back the on-chain
    /// tx id and flips `submitted = true`. The inclusion-bridge later
    /// removes the row.
    async fn mark_submitted(
        &self,
        queued_id: &str,
        on_chain_tx_id: &str,
    ) -> StorageResult<()>;
    /// Removes a fully-resolved entry from the queue.
    async fn delete(&self, queued_id: &str) -> StorageResult<()>;
    /// Looks up the on-chain tx id for a queued id, once available. Returns
    /// `Ok(None)` if the entry is still in flight.
    async fn lookup(&self, queued_id: &str) -> StorageResult<Option<BatchQueueEntry>>;
    /// Returns the current queue length (useful for metrics + the batch
    /// trigger).
    async fn len(&self) -> StorageResult<usize>;
}

// ---------- Facilitator receipt key ----------

/// Persistent key material the facilitator uses to sign settle receipts.
///
/// On first boot, the binary generates a fresh Falcon keypair and persists it
/// through this trait. On subsequent boots it loads the same key, so the
/// `receipt_pubkey_commitment` returned by `GET /x402/pubkey` is stable.
#[async_trait]
pub trait FacilitatorKeyStore: Send + Sync + 'static {
    /// Loads the persisted Falcon secret-key bytes. Returns `Ok(None)` if no
    /// key has been persisted yet.
    async fn load(&self) -> StorageResult<Option<Vec<u8>>>;
    /// Persists the Falcon secret-key bytes.
    async fn save(&self, secret_key_bytes: &[u8]) -> StorageResult<()>;
}

// ---------- Helpers ----------

/// Wall-clock seconds since UNIX_EPOCH. Returns 0 if the clock is before
/// the epoch (impossible in practice).
pub fn unix_now() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `Instant` is not Serialize. Use `unix_now()` + offsets for stored times.
pub fn elapsed_since_unix(t_unix_secs: u64) -> Duration {
    let now = unix_now();
    if now > t_unix_secs {
        Duration::from_secs(now - t_unix_secs)
    } else {
        Duration::ZERO
    }
}

/// Convenience for tests / sweepers that prefer `Instant`-style code.
pub fn approx_now_as_instant() -> Instant {
    Instant::now()
}

/// JSON-serialisable wrapper around `miden_protocol::Word`. We serialise as
/// the canonical hex string so stored records survive across upgrades that
/// might change `Word`'s internal layout.
#[derive(Debug, Clone)]
pub struct SerializableWord(pub Word);

impl From<Word> for SerializableWord {
    fn from(w: Word) -> Self { Self(w) }
}

impl From<SerializableWord> for Word {
    fn from(s: SerializableWord) -> Self { s.0 }
}

impl Serialize for SerializableWord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_hex())
    }
}

impl<'de> Deserialize<'de> for SerializableWord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        let word = Word::try_from(hex.as_str()).map_err(serde::de::Error::custom)?;
        Ok(Self(word))
    }
}
