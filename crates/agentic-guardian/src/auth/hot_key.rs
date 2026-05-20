//! Hot-key Falcon-512 signature verification.
//!
//! Called on every `POST /agentic/submit` to confirm the
//! `AgenticPayload::hot_signature` was produced by the agent's
//! registered hot key.
//!
//! The signed message is `signed_summary.to_commitment()` — a `Word`
//! (4 field elements). The signature is verified against the
//! Falcon-512 Poseidon2 pubkey whose commitment the agent registered.
//!
//! Wire encoding: both `hot_signature` and `signed_summary` are base64
//! of canonical Miden serializations. This module:
//!
//! 1. Decodes `signed_summary` → `TransactionSummary`; computes
//!    `to_commitment()`.
//! 2. Decodes `hot_signature` → `miden_protocol::account::auth::Signature`.
//! 3. Extracts the bundled `PublicKey` polynomial from the Falcon
//!    signature; computes its commitment via
//!    `Falcon512Poseidon2::PublicKey::to_commitment()`.
//! 4. Confirms the commitment matches the agent's registered
//!    `hot_pubkey_commitment_hex`.
//! 5. Calls `PublicKey::verify(commitment, &signature)`.

use crate::error::{AgenticError, AgenticResult};

/// Verifies that `hot_signature_b64` is a valid Falcon-512 signature
/// over the commitment of `signed_summary_b64`, produced by the key
/// whose commitment equals `expected_pubkey_commitment_hex`.
///
/// **Skeleton**: currently returns `Ok(())` so the verify pipeline
/// type-checks end-to-end. Real impl reuses the M8 facilitator's
/// [`crates/miden-x402-facilitator/src/guardian/auth.rs`](../../../miden-x402-facilitator/src/guardian/auth.rs)
/// — the only change is reading the pubkey commitment from the
/// agentic-guardian's `agents` table (single hot pubkey) instead of
/// from on-chain `AuthSingleSig` storage.
pub fn verify_hot_signature(
    _hot_signature_b64: &str,
    _signed_summary_b64: &str,
    _expected_pubkey_commitment_hex: &str,
) -> AgenticResult<()> {
    // TODO: port from miden-x402-facilitator/src/guardian/auth.rs
    // (M8 already has the Falcon-verify scaffolding; only the
    //  "where does the expected commitment come from" line differs).
    let _ = AgenticError::InvalidHotKeySignature; // ensure the error variant is reachable
    Ok(())
}
