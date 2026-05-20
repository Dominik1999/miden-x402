//! Storage-layer DTO types shared across backends.

use serde::{Deserialize, Serialize};

use miden_x402_types::{AccountIdHex, Ap2SignedMandate, MidenPaymentRequirements, NoteIdHex};

/// One row in the `agents` table — the result of `POST /agentic/register`.
///
/// Per NEW_DESIGN: the agent account on-chain is a 3-cosigner Falcon
/// multisig (`hot`, `cold`, `guardian`) with threshold 1. The agentic-guardian
/// enforces the hot-vs-cold split off-chain by gating its co-sig:
/// in-mandate → it signs alongside hot; out-of-mandate → it refuses and
/// the user must drive the cold-key path via the (separately-deployed)
/// OZ Guardian.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_account_id: AccountIdHex,
    /// Canonical-hex commitment of the agent's hot Falcon-512 pubkey.
    pub hot_pubkey_commitment_hex: String,
    /// Canonical-hex commitment of the user's cold Falcon-512 pubkey
    /// (used to verify the mandate signature at register time).
    pub cold_pubkey_commitment_hex: String,
    pub registered_at_unix_secs: u64,
}

/// One row in the `mandates` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MandateRecord {
    /// The full signed mandate as the user submitted it.
    pub signed: Ap2SignedMandate,
    pub stored_at_unix_secs: u64,
}

/// One row in the `pending_states` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingState {
    pub agent_account_id: AccountIdHex,
    /// Canonical-hex commitment of the agent's account state as the
    /// Guardian has acknowledged.
    pub current_commitment_hex: String,
    /// Account nonce as the Guardian has acknowledged.
    pub nonce: u64,
    pub last_advanced_at_unix_secs: u64,
}

/// One row in the `batch_queue` table — a verified-but-unsubmitted tx.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchQueueEntry {
    /// Deterministic id: `blake3(serial_num || signed_summary_commitment)`.
    pub queued_id: String,
    pub agent_account_id: AccountIdHex,
    pub mandate_id: String,
    pub serial_num: NoteIdHex,
    pub payer: AccountIdHex,
    /// Base64 of canonical `TransactionInputs`.
    pub tx_inputs_b64: String,
    /// Base64 of the hot-key `Signature` over `signed_summary.to_commitment()`.
    pub hot_signature_b64: String,
    /// Base64 of canonical `TransactionSummary`.
    pub signed_summary_b64: String,
    /// CAIP-2 network id echoed into `SettleResponse::Success.network`.
    pub network: String,
    pub enqueued_at_unix_secs: u64,
    pub submitted: bool,
    pub on_chain_tx_id: Option<String>,
}

/// One row in the `challenges` table — a server-issued `serial_num`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeRecord {
    pub serial_num: NoteIdHex,
    /// Snapshot of the merchant's requirements at issuance time.
    pub requirements: MidenPaymentRequirements,
    pub issued_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
}

impl ChallengeRecord {
    pub fn is_expired(&self, now_unix_secs: u64) -> bool {
        now_unix_secs >= self.expires_at_unix_secs
    }
}
