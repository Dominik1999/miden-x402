//! Guardian-as-x402-facilitator — module bolted onto an OZ Guardian server.
//!
//! See [`ideas/DESIGN.md`] for the design and `docs/protocol.md` for the
//! wire contract.
//!
//! The crate exposes the building blocks the `guardian-facilitator` binary
//! composes:
//!
//! - [`config`] — env-driven facilitator config.
//! - [`storage`] — pluggable repos for challenges, reservations, batch
//!   queue, receipt key.
//! - [`mandate`] — `MandatePolicy` trait + `AllowAll` default.
//! - [`buyer_auth`] — buyer cosigner pubkey lookup (wired to Guardian
//!   metadata in the binary).
//! - [`balance`] — buyer balance lookup against Guardian's persisted state.
//! - [`verify`] — verify-before-prove pipeline.
//! - [`settle`] — enqueue + sign receipt; the prove + submit half lives
//!   in [`batch`].
//! - [`batch`] — async batch worker.
//! - [`receipt`] — facilitator-owned Falcon receipt signer.
//! - [`handlers`] — axum router for `/x402/*`.

#![forbid(unsafe_code)]

pub mod balance;
pub mod batch;
pub mod buyer_auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod mandate;
pub mod receipt;
pub mod settle;
pub mod storage;
pub mod verify;

pub use config::{BatchSettleConfig, ConfigError, FacilitatorConfig, MandatePolicyConfig};
pub use error::{ErrorBody, FacilitatorError};
pub use handlers::{X402AppState, x402_router};
pub use mandate::{AllowAll, ArcMandatePolicy, MandateContext, MandateError, MandatePolicy};
pub use receipt::{ReceiptError, ReceiptSigner, receipt_digest};
pub use settle::{X402SettleSuccess, compute_queued_id};
pub use verify::{NullifierBackstop, NullifierCheckError, VerifiedX402Tx, verify_unproven};
