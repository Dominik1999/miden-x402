//! Wire types for the x402 facilitator API.

use guardian_shared::{DeltaSignature, SignatureScheme};
use serde::{Deserialize, Serialize};

/// What the agentic client sends to `POST /agents/{agent_id}/payments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticPayload {
    /// JSON form of `miden_protocol::transaction::TransactionSummary`.
    pub tx_summary: serde_json::Value,
    /// Hot-key signature over `TransactionSummary::to_commitment()`.
    pub hot_key_signature: DeltaSignature,
    /// x402 context as agreed with the merchant.
    pub x402_context: X402Context,
    /// Hex Word — the agent's view of the latest pending state when it built this tx.
    pub built_on_state_commitment: String,
    /// Hex Word — the agent's claimed new pending state after applying this tx.
    /// (v1 trusts this; later phases derive it server-side from `tx_summary`.)
    pub new_state_commitment: String,
    /// Hex strings — the agent's claimed output nullifiers consumed by this tx.
    /// (v1 trusts this; later phases extract them from `tx_summary`.)
    pub claimed_nullifiers: Vec<String>,
}

/// The merchant-bound payment context the agent and merchant agreed on
/// during the x402 `402 → accepts → PAYMENT-SIGNATURE` handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X402Context {
    pub merchant_account_id: String,
    pub asset_faucet_id: String,
    /// String to preserve big-integer precision.
    pub amount: String,
    pub deadline_unix_secs: u64,
    /// Hash of the merchant's `accepts` entry the client picked.
    pub payment_requirements_digest: String,
}

/// AP2-style mandate stored per registered agent. v1 enforces a minimal
/// subset (per-tx cap, merchant allowlist, expiry); daily totals and
/// signed AP2 mandates are out of scope.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentMandate {
    pub per_tx_amount_cap: String,
    pub merchant_allowlist: Vec<String>,
    pub expires_at_unix_secs: u64,
}

/// Body of `POST /agents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentRequest {
    pub agent_id: String,
    pub account_id: String,
    /// Hex Word commitment of the agent's hot key (Falcon public key commitment).
    pub hot_key_commitment: String,
    pub hot_key_scheme: SignatureScheme,
    /// Optional full hex-encoded public key. Required for off-chain
    /// signature verification (commitments alone don't let us verify
    /// Falcon signatures without the underlying public key).
    #[serde(default)]
    pub hot_key_pubkey_hex: Option<String>,
    /// Initial on-chain state commitment, used as the starting pending
    /// state. Hex Word.
    pub initial_state_commitment: String,
    pub mandate: AgentMandate,
    /// Optional base64-encoded `miden_protocol::account::Account` snapshot.
    /// When present the facilitator mirrors this account into its own
    /// `miden-client` store so the batch worker can later prove and
    /// submit transactions against it. Set in real-Miden mode; omitted
    /// for placeholder-only integration tests.
    #[serde(default)]
    pub account_snapshot_b64: Option<String>,
}

/// Response of `POST /agents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentResponse {
    pub agent_id: String,
    pub facilitator_pubkey_commitment: String,
}

/// Successful ack for `POST /agents/{agent_id}/payments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckResponse {
    pub accepted_at_unix_micros: u64,
    pub new_pending_state_commitment: String,
    pub reserved_nullifiers: Vec<String>,
    /// Sequence number assigned in the agent's queue; useful for ordering.
    pub seq: u64,
    /// Facilitator's Falcon signature over
    /// `(accepted_at, new_state_commitment, nullifiers)`. Hex.
    pub facilitator_ack_signature: String,
}

/// Response of `GET /agents/{agent_id}/state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateResponse {
    pub agent_id: String,
    pub committed_state_commitment: String,
    pub pending_state_commitment: String,
    pub last_accepted_seq: u64,
    pub in_flight_count: u64,
}

/// Payment lifecycle status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Accepted,
    Proving,
    Submitted,
    Committed,
    Failed,
}

/// Response of `GET /agents/{agent_id}/payments/{nullifier}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentStatusResponse {
    pub agent_id: String,
    pub nullifier: String,
    pub seq: u64,
    pub status: PaymentStatus,
    pub accepted_at_unix_micros: u64,
    #[serde(default)]
    pub t_batch_started_unix_micros: Option<u64>,
    #[serde(default)]
    pub t_submitted_unix_micros: Option<u64>,
    #[serde(default)]
    pub t_committed_unix_micros: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}
