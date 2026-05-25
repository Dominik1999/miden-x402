//! Cumulative voucher — off-chain signed intent for batch-settlement.
//!
//! The agent signs cumulative vouchers that the merchant verifies locally.
//! No facilitator involvement per-request. Settlement happens when the
//! merchant sends the latest voucher to the facilitator.

use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_poseidon2::{PublicKey, SecretKey, Signature};
use miden_protocol::utils::serde::Serializable;

use crate::message::debit_message;

/// Compute the voucher message to sign.
///
/// This matches the MASM consume path message:
/// `merge(serial_num, [merchant_suffix, merchant_prefix, cumulative_amount, 0])`
pub fn voucher_message(
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
) -> Word {
    debit_message(note_serial_num, merchant_account_id, cumulative_amount)
}

/// Sign a cumulative voucher.
pub fn sign_voucher(
    secret_key: &SecretKey,
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
) -> Signature {
    let message = voucher_message(note_serial_num, merchant_account_id, cumulative_amount);
    secret_key.sign(message)
}

/// Verify a cumulative voucher signature.
pub fn verify_voucher(
    public_key: &PublicKey,
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
    signature: &Signature,
) -> bool {
    let message = voucher_message(note_serial_num, merchant_account_id, cumulative_amount);
    public_key.verify(message, signature)
}

/// Serialize a Falcon signature to hex string.
pub fn signature_to_hex(sig: &Signature) -> String {
    format!("0x{}", hex::encode(sig.to_bytes()))
}
