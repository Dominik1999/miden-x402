//! Cold-key Falcon-512 signature verification, used at agent
//! registration time to confirm the user actually signed the AP2
//! mandate.
//!
//! `POST /agentic/register` carries an `Ap2SignedMandate { mandate,
//! user_signature_b64, user_pubkey_b64 }`. The agentic-guardian:
//!
//! 1. Computes `commitment = Rpo256::hash(mandate.canonical_bytes())`.
//! 2. Decodes `user_signature_b64` and `user_pubkey_b64`.
//! 3. Verifies the signature against `commitment` using the pubkey.
//! 4. Computes the pubkey's commitment and confirms it matches the
//!    `cold_pubkey_commitment_hex` baked into the registered agent
//!    account (so a substituted pubkey is rejected).

use miden_x402_types::Ap2SignedMandate;

use crate::error::{AgenticError, AgenticResult};

/// Verifies the user's signature on a submitted `Ap2SignedMandate` and
/// confirms the embedded pubkey matches the agent's registered cold
/// pubkey commitment.
///
/// **Skeleton**: currently returns `Ok(())`. Real impl uses
/// `miden-crypto::dsa::falcon512_poseidon2` to compute the commitment
/// (via `Rpo256::hash` over `mandate.canonical_bytes()`) and to verify
/// the signature.
pub fn verify_mandate_signature(
    _signed: &Ap2SignedMandate,
    _expected_cold_pubkey_commitment_hex: &str,
) -> AgenticResult<()> {
    // TODO: real Falcon verify using miden-crypto::dsa::falcon512_poseidon2.
    let _ = AgenticError::InvalidMandateSignature;
    Ok(())
}
