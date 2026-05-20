//! Wire types mirrored from `x402-facilitator-server`.
//!
//! Kept here (rather than importing from the facilitator crate) so the
//! client doesn't pull in axum and the rest of the server's
//! dependency tree.

use guardian_shared::{DeltaSignature, SignatureScheme};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X402Context {
    pub merchant_account_id: String,
    pub asset_faucet_id: String,
    pub amount: String,
    pub deadline_unix_secs: u64,
    pub payment_requirements_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentMandate {
    pub per_tx_amount_cap: String,
    pub merchant_allowlist: Vec<String>,
    pub expires_at_unix_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticPayload {
    pub tx_summary: serde_json::Value,
    pub hot_key_signature: DeltaSignature,
    pub x402_context: X402Context,
    pub built_on_state_commitment: String,
    pub new_state_commitment: String,
    pub claimed_nullifiers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentRequest {
    pub agent_id: String,
    pub account_id: String,
    pub hot_key_commitment: String,
    pub hot_key_scheme: SignatureScheme,
    #[serde(default)]
    pub hot_key_pubkey_hex: Option<String>,
    pub initial_state_commitment: String,
    pub mandate: AgentMandate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAgentResponse {
    pub agent_id: String,
    pub facilitator_pubkey_commitment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckResponse {
    pub accepted_at_unix_micros: u64,
    pub new_pending_state_commitment: String,
    pub reserved_nullifiers: Vec<String>,
    pub seq: u64,
    pub facilitator_ack_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateResponse {
    pub agent_id: String,
    pub committed_state_commitment: String,
    pub pending_state_commitment: String,
    pub last_accepted_seq: u64,
    pub in_flight_count: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Accepted,
    Proving,
    Submitted,
    Committed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentStatusResponse {
    pub agent_id: String,
    pub nullifier: String,
    pub seq: u64,
    pub status: PaymentStatus,
    pub accepted_at_unix_micros: u64,
}

/// What the agent's surrounding code receives after `pay()` returns.
#[derive(Debug, Clone)]
pub struct PaymentReceipt {
    pub agent_id: String,
    pub seq: u64,
    pub reserved_nullifiers: Vec<String>,
    pub new_pending_state_commitment: String,
    pub facilitator_ack_signature: String,
    pub accepted_at_unix_micros: u64,
}

/// Per-call client-side timing breakdown. All values are
/// unix-epoch microseconds taken from `SystemTime::now()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PayTimings {
    pub t_pay_start: u64,
    pub t_sign_start: u64,
    pub t_sign_end: u64,
    pub t_send_facilitator: u64,
    pub t_ack_received: u64,
    /// Number of stale-base retries that occurred during this `pay()`.
    pub retries: u32,
}
