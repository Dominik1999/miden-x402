//! Guardian-flow facilitator module.
//!
//! Implements the verify-before-prove + nullifier-reservation pattern from
//! `ideas/GUARDIAN.md`. The agent hands the facilitator a signed-but-unproven
//! `TransactionInputs`; the facilitator:
//!
//! 1. Looks up the challenge by `serial_num` ([`challenge`]).
//! 2. Extracts the buyer's Falcon `PublicKey` from `TransactionInputs.account`
//!    ([`auth`]).
//! 3. Reconstructs the signed message (`TransactionSummary::to_commitment()`)
//!    and offline-verifies the signature.
//! 4. Reserves the input nullifiers in [`reservation`].
//! 5. (Settle path only) Submits to the configured remote prover; on success
//!    promotes the reservations to "consumed" and returns the post-prove
//!    `ProvenTransaction` id; on failure releases the reservations.
//!
//! The flow is private-only — public notes don't gain anything from the
//! Guardian path. Public-payload requests to `/guardian/*` return
//! `BadRequest`.

pub mod auth;
pub mod challenge;
pub mod reservation;
pub mod settle;
pub mod verify;

pub use auth::{read_falcon_auth, verify_signature};
pub use challenge::{ChallengeStore, IssuedChallenge};
pub use reservation::{AlreadyReserved, Reservation, ReservedNullifierSet};
pub use settle::settle_and_submit;
pub use verify::{VerifiedGuardianTx, verify_unproven};
