use base64::Engine;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 402 Payment Required response (merchant → agent)
// ---------------------------------------------------------------------------

/// Top-level `PAYMENT-REQUIRED` header payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    pub x402_version: u32,
    pub accepts: Vec<PaymentRequirements>,
}

/// A single accepted payment scheme/network combination.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    /// Always `"batch-settlement"` for this binding.
    pub scheme: String,
    /// CAIP-2 identifier, e.g. `"miden:testnet"`.
    pub network: String,
    /// Minimum amount per request (stringified u64).
    pub amount: String,
    /// Faucet account id (hex).
    pub asset: String,
    /// Merchant account id (hex).
    pub pay_to: String,
    /// Maximum allowed timeout in seconds.
    pub max_timeout_seconds: u32,
    /// Miden-specific extra parameters.
    pub extra: Option<MidenExtra>,
}

/// Miden-specific extension fields for `PaymentRequirements`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MidenExtra {
    pub min_reclaim_block_buffer: u64,
}

// ---------------------------------------------------------------------------
// Payment Signature header — Session Setup (agent → merchant)
// ---------------------------------------------------------------------------

/// `PAYMENT-SIGNATURE` header payload sent during session setup.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PaymentSignatureSetup {
    pub x402_version: u32,
    pub scheme: String,
    pub network: String,
    pub payer_account_id: String,
    /// Block height at which the agent may reclaim unused funds.
    pub reclaim_block_height: u64,
    /// Hex-encoded note commitment.
    pub note_commitment: String,
    pub note_details: NoteDetails,
}

/// On-chain note details transmitted alongside the session-setup header.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NoteDetails {
    /// Hex-encoded serial number.
    pub serial_num: String,
    /// Hex-encoded compiled script bytes.
    pub script: String,
    /// Storage items as hex strings.
    pub inputs: Vec<String>,
    /// Assets locked inside the note.
    pub assets: Vec<AssetEntry>,
}

/// A single asset entry inside a note.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AssetEntry {
    pub faucet_id: String,
    pub amount: String,
}

// ---------------------------------------------------------------------------
// Cumulative Voucher — Per Request (agent → merchant, off-chain)
// ---------------------------------------------------------------------------

/// Off-chain cumulative voucher sent with each API request.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CumulativeVoucherPayload {
    pub scheme: String,
    pub network: String,
    pub note_commitment: String,
    /// Stringified cumulative total authorised so far.
    pub cumulative_amount: String,
    /// Block height after which the voucher expires.
    pub block_height_expiry: u64,
    pub merchant_account_id: String,
    /// Hex-encoded Falcon signature over the voucher fields.
    pub signature: String,
}

// ---------------------------------------------------------------------------
// Payment Response header (merchant → agent)
// ---------------------------------------------------------------------------

/// `PAYMENT-RESPONSE` header payload returned after settlement.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PaymentResponse {
    pub success: bool,
    pub tx_hash: Option<String>,
    pub settled_amount: Option<String>,
    pub remainder_note_commitment: Option<String>,
    pub remainder_balance: Option<String>,
}

// ---------------------------------------------------------------------------
// Base64 encode / decode helpers
// ---------------------------------------------------------------------------

/// Serialize `value` to JSON and base64-encode it for use as an HTTP header.
pub fn encode_header<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_string(value).expect("wire type must be serialisable");
    base64::engine::general_purpose::STANDARD.encode(json.as_bytes())
}

/// Decode a base64-encoded JSON header back into `T`.
pub fn decode_header<T: for<'de> Deserialize<'de>>(b64: &str) -> Result<T, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("base64 decode: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("json decode: {e}"))
}
