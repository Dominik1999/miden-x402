//! Shared types for ADN batch-settlement services.

use serde::{Deserialize, Serialize};

/// POST /verify request
#[derive(Serialize, Deserialize, Debug)]
pub struct VerifyRequest {
    pub note_file_hex: String,
    pub merchant_id_hex: String,
}

/// POST /verify response
#[derive(Serialize, Deserialize, Debug)]
pub struct VerifyResponse {
    pub is_valid: bool,
    pub note_balance: u64,
    pub user_pub_key_hex: String,
    pub reclaim_block_height: u64,
    pub error: Option<String>,
}

/// POST /settle request
#[derive(Serialize, Deserialize, Debug)]
pub struct SettleRequest {
    pub note_file_hex: String,
    pub agent_sk_hex: String,
    pub serial_num: [String; 4],
    pub cumulative_amount: u64,
    pub merchant_id_hex: String,
}

/// POST /settle response
#[derive(Serialize, Deserialize, Debug)]
pub struct SettleResponse {
    pub success: bool,
    pub tx_hash: String,
    pub settled_amount: u64,
    pub remainder_balance: u64,
    pub p2id_note_file_hex: Option<String>,
    pub error: Option<String>,
}

/// Merchant → Agent: 402 payment required
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaymentRequired {
    pub facilitator_url: String,
    pub merchant_id_hex: String,
    pub amount_per_request: u64,
}

/// Agent → Merchant: payment with voucher
#[derive(Serialize, Deserialize, Debug)]
pub struct PaymentRequest {
    /// First request: hex-encoded NoteFile. Subsequent: None.
    pub note_file_hex: Option<String>,
    pub agent_sk_hex: String,
    pub serial_num: [String; 4],
    pub cumulative_amount: u64,
    pub signature_hex: String,
}

/// Merchant → Agent: resource or error
#[derive(Serialize, Deserialize, Debug)]
pub struct PaymentResponse {
    pub success: bool,
    pub resource: Option<String>,
    pub error: Option<String>,
}
