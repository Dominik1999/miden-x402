//! Settle handler logic: enqueue + sign receipt.
//!
//! Per DESIGN.md, settle is **async** — the merchant receives a Guardian-
//! signed receipt immediately and the actual prove + submit happens in the
//! background batch worker. This module composes the verify path with the
//! queue + receipt signer; it has no state of its own.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;

use crate::batch::BatchSettleQueue;
use crate::error::FacilitatorError;
use crate::receipt::ReceiptSigner;
use crate::storage::{BatchQueueEntry, unix_now};
use crate::verify::VerifiedX402Tx;
use miden_crypto::utils::Serializable;

/// Successful settle response shape.
///
/// Distinct from `x402_types::SettleResponse::Success` because the x402 v1
/// shape doesn't carry our receipt fields. The header value still uses the
/// upstream `SettleResponse` for wire compatibility (see
/// [`crate::handlers`]); this struct is the *facilitator's* richer body.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct X402SettleSuccess {
    pub success: bool,
    pub payer: String,
    /// Deterministic queued id (`blake3(serial_num || tx_summary_commitment)`).
    /// Becomes the on-chain `ProvenTransaction.id()` once the batch worker
    /// settles — clients can poll `GET /x402/lookup?queued_id=<...>` to
    /// resolve the post-prove id.
    pub transaction: String,
    /// CAIP-2 network id (echoed from `MIDEN_X402_NETWORK`).
    pub network: String,
    /// Base64-encoded Falcon-512 signature over
    /// `RPO256([payer, queued_id, network_hash])`. Verifiable against
    /// `receipt_pubkey_commitment` (which the merchant also caches from
    /// `GET /x402/pubkey`).
    pub receipt_sig: String,
    /// Hex commitment of the facilitator's receipt-signing pubkey.
    pub receipt_pubkey_commitment: String,
}

/// Composes a queued-id from the wire serial_num + signed_summary commitment.
/// Stable across processes (same inputs → same id) so a retried `/x402/settle`
/// is naturally idempotent.
pub fn compute_queued_id(serial_num_hex: &str, signed_summary_commitment_hex: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(serial_num_hex.as_bytes());
    hasher.update(b"|");
    hasher.update(signed_summary_commitment_hex.as_bytes());
    let hash = hasher.finalize();
    // 32 bytes → 64-char hex with `0x` prefix.
    format!("0x{}", hex::encode(hash.as_bytes()))
}

/// Drives a settle call. Assumes [`crate::verify::verify_unproven`] has
/// already produced `verified`; enqueues onto the batch worker and signs
/// the receipt.
pub async fn settle(
    verified: VerifiedX402Tx,
    queue: &BatchSettleQueue,
    signer: &ReceiptSigner,
    network: &str,
) -> Result<X402SettleSuccess, FacilitatorError> {
    let queued_id = compute_queued_id(
        verified.payer.as_str(),
        &verified.signed_summary_commitment.to_hex(),
    );

    let entry = BatchQueueEntry {
        queued_id: queued_id.clone(),
        payer: verified.payer.clone(),
        tx_inputs_b64: verified.tx_inputs_b64,
        reserved_nullifiers: verified.reserved_nullifiers,
        network: network.to_owned(),
        enqueued_at_unix_secs: unix_now(),
        submitted: false,
        on_chain_tx_id: None,
    };
    queue.enqueue(entry).await?;

    let sig = signer
        .sign_receipt(verified.payer.as_str(), &queued_id, network)
        .map_err(|e| FacilitatorError::ReceiptSigning(e.to_string()))?;
    let sig_b64 = BASE64.encode(sig.to_bytes());

    Ok(X402SettleSuccess {
        success: true,
        payer: verified.payer.as_str().to_owned(),
        transaction: queued_id,
        network: network.to_owned(),
        receipt_sig: sig_b64,
        receipt_pubkey_commitment: signer.pubkey_commitment_hex(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_id_is_deterministic() {
        let a = compute_queued_id("0xc", "0xff");
        let b = compute_queued_id("0xc", "0xff");
        assert_eq!(a, b);
        let c = compute_queued_id("0xc", "0xee");
        assert_ne!(a, c);
    }
}
