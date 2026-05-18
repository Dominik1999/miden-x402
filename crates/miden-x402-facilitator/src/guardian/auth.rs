//! Off-chain Falcon signature verification for the Guardian flow.
//!
//! The Guardian receives a buyer-signed but not-yet-proven transaction. To
//! verify the signature without re-executing the transaction inside the VM,
//! it needs three things:
//!
//! 1. The on-chain `PublicKeyCommitment` for the buyer's account, read from
//!    the `AuthSingleSig` standard-component storage slot inside
//!    `PartialAccount.storage()`.
//! 2. The high-level `Signature` enum (sent on the wire as a separate base64
//!    field — the advice-map carries a stack-reversed prepared form that
//!    can't be inverted).
//! 3. The signed message `Word` — for Phase B this is the canonical
//!    `TransactionSummary::to_commitment()` digest, computed by the verifier
//!    from `TransactionInputs`.
//!
//! The flow is:
//! - Decode the wire `Signature`.
//! - Extract the underlying `falcon512_poseidon2::PublicKey` polynomial from
//!   the signature (it is bundled with the signature for Falcon — see
//!   `miden-crypto-0.23.0/src/dsa/falcon512_poseidon2/signature.rs`).
//! - Compute the commitment of that polynomial and require it to match the
//!   on-chain commitment. This binds the wire signature to the account
//!   identity.
//! - Run the cryptographic verification via `PublicKey::verify`.
//!
//! Only `AuthScheme::Falcon512Poseidon2` is supported in Phase B. ECDSA
//! K256/Keccak accounts are out of scope for the initial Guardian
//! deployment; they would need an analogous `ecdsa_k256_keccak` branch.

use miden_protocol::Word;
use miden_protocol::account::PartialAccount;
use miden_protocol::account::auth::{AuthScheme, PublicKey, PublicKeyCommitment, Signature};
use miden_standards::account::auth::AuthSingleSig;

use crate::error::FacilitatorError;

/// Reads the `(public_key_commitment, auth_scheme)` pair from the canonical
/// `AuthSingleSig` storage slots of the given partial account. Returns
/// `UnsupportedAuthScheme` if either slot is missing, the scheme byte is
/// not in the registered set, or the scheme is not `Falcon512Poseidon2`
/// (the only scheme the Phase B Guardian verifies).
pub fn read_falcon_auth(
    partial_account: &PartialAccount,
) -> Result<PublicKeyCommitment, FacilitatorError> {
    let header = partial_account.storage().header();

    let pubkey_slot_name = AuthSingleSig::public_key_slot();
    let scheme_slot_name = AuthSingleSig::scheme_id_slot();

    let pubkey_slot = header.find_slot_header_by_name(pubkey_slot_name).ok_or_else(|| {
        FacilitatorError::UnsupportedAuthScheme(format!(
            "account has no `{}` storage slot — not an AuthSingleSig account",
            pubkey_slot_name.as_str()
        ))
    })?;

    let scheme_slot = header.find_slot_header_by_name(scheme_slot_name).ok_or_else(|| {
        FacilitatorError::UnsupportedAuthScheme(format!(
            "account has no `{}` storage slot — not an AuthSingleSig account",
            scheme_slot_name.as_str()
        ))
    })?;

    // The scheme slot is `Word::from([scheme_byte, 0, 0, 0])` per
    // `AuthSingleSig`'s storage layout. Decode the byte from the first
    // field element.
    let scheme_word: Word = scheme_slot.value();
    let scheme_byte = scheme_word.as_elements()[0].as_canonical_u64() as u8;

    let scheme = AuthScheme::try_from(scheme_byte).map_err(|e| {
        FacilitatorError::UnsupportedAuthScheme(format!("scheme byte {scheme_byte}: {e}"))
    })?;

    if scheme != AuthScheme::Falcon512Poseidon2 {
        return Err(FacilitatorError::UnsupportedAuthScheme(format!(
            "auth scheme is {scheme:?}; Phase B Guardian only supports Falcon512Poseidon2"
        )));
    }

    Ok(PublicKeyCommitment::from(pubkey_slot.value()))
}

/// Verifies `signature` over `message`, binding it to the buyer's on-chain
/// `PublicKeyCommitment`. Returns `BadSignature` on any failure (commitment
/// mismatch or crypto-level failure) and `UnsupportedAuthScheme` if the
/// signature variant is not Falcon.
pub fn verify_signature(
    on_chain_commitment: &PublicKeyCommitment,
    signature: &Signature,
    message: Word,
) -> Result<(), FacilitatorError> {
    let falcon_sig = match signature {
        Signature::Falcon512Poseidon2(sig) => sig,
        Signature::EcdsaK256Keccak(_) => {
            return Err(FacilitatorError::UnsupportedAuthScheme(
                "wire signature is EcdsaK256Keccak; Phase B Guardian only supports Falcon"
                    .to_owned(),
            ));
        }
    };

    // The Falcon signature carries the public-key polynomial `h` inline.
    // Compute its commitment and require it to match the on-chain value —
    // this is the bind between the buyer's claimed public key and the
    // account identity. Without this check the buyer could substitute any
    // (signature, public key) pair.
    let claimed_pk = falcon_sig.public_key().clone();
    let claimed_pk_high = PublicKey::Falcon512Poseidon2(claimed_pk);
    let claimed_commitment = claimed_pk_high.to_commitment();
    if Word::from(claimed_commitment) != Word::from(on_chain_commitment.clone()) {
        return Err(FacilitatorError::BadSignature);
    }

    // Cryptographic verification. `Signature` is moved, so we clone.
    let ok = claimed_pk_high.verify(message, signature.clone());
    if !ok {
        return Err(FacilitatorError::BadSignature);
    }
    Ok(())
}
