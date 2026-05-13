//! x402 v2 facilitator for the Miden network.
//!
//! Exposes a small set of HTTP endpoints (`POST /verify`, `POST /settle`,
//! `GET /supported`, `GET /health`) that resource servers can call when
//! gating routes with x402 on Miden testnet.
//!
//! Verification is read-only — the facilitator only queries node state via
//! [`miden_client::rpc::GrpcClient`] and never custodies keys. Settlement
//! semantics under this implementation are "settled-at-commit": once the
//! buyer's P2ID note is in a committed block, the payment is considered
//! settled and `/settle` is idempotent with `/verify`.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod handlers;
pub mod node;
pub mod verifier;

pub use config::FacilitatorConfig;
pub use error::FacilitatorError;
pub use handlers::{AppState, build_router};
pub use node::{GrpcMidenNode, MidenNode, NoteSnapshot};
