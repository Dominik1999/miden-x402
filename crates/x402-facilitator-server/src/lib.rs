//! x402 facilitator server library.
//!
//! Provides the x402 facilitator endpoints and per-agent pending state
//! tracking on top of the vendored OpenZeppelin Guardian server.
//!
//! See `ideas/DESIGN.md` for the architecture.

pub mod api;
pub mod error;
pub mod jobs;
pub mod key;
pub mod state;
pub mod store;
pub mod submitter;
pub mod types;
