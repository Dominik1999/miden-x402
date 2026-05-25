use crate::wire::{
    CumulativeVoucherPayload, MidenExtra, NoteDetails, PaymentRequired, PaymentRequirements,
    PaymentSignatureSetup,
};

/// Build a `PAYMENT-REQUIRED` header value for a Miden batch-settlement resource.
///
/// Returns the base64-encoded JSON string ready for use as an HTTP header.
pub fn build_payment_required(
    merchant_id: &str,
    asset_faucet_id: &str,
    amount_per_request: u64,
    network: &str,
) -> String {
    let pr = PaymentRequired {
        x402_version: 1,
        accepts: vec![PaymentRequirements {
            scheme: "batch-settlement".into(),
            network: network.into(),
            amount: amount_per_request.to_string(),
            asset: asset_faucet_id.into(),
            pay_to: merchant_id.into(),
            max_timeout_seconds: 3600,
            extra: Some(MidenExtra {
                min_reclaim_block_buffer: 100,
            }),
        }],
    };
    crate::wire::encode_header(&pr)
}

/// Build a `PAYMENT-SIGNATURE` header for session setup.
///
/// Returns the base64-encoded JSON string ready for use as an HTTP header.
pub fn build_session_setup_header(
    note_commitment: &str,
    note_details: NoteDetails,
    payer_id: &str,
    reclaim_block: u64,
    network: &str,
) -> String {
    let setup = PaymentSignatureSetup {
        x402_version: 1,
        scheme: "batch-settlement".into(),
        network: network.into(),
        payer_account_id: payer_id.into(),
        reclaim_block_height: reclaim_block,
        note_commitment: note_commitment.into(),
        note_details,
    };
    crate::wire::encode_header(&setup)
}

/// Build a cumulative voucher struct (not base64-encoded).
pub fn build_voucher(
    note_commitment: &str,
    merchant_id: &str,
    cumulative_amount: u64,
    block_height_expiry: u64,
    signature_hex: &str,
    network: &str,
) -> CumulativeVoucherPayload {
    CumulativeVoucherPayload {
        scheme: "batch-settlement".into(),
        network: network.into(),
        note_commitment: note_commitment.into(),
        cumulative_amount: cumulative_amount.to_string(),
        block_height_expiry,
        merchant_account_id: merchant_id.into(),
        signature: signature_hex.into(),
    }
}

/// Encode a voucher as a `PAYMENT-SIGNATURE` header value (base64).
pub fn encode_voucher_header(voucher: &CumulativeVoucherPayload) -> String {
    crate::wire::encode_header(voucher)
}
