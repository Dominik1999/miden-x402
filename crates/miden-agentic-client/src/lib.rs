//! Agentic client SDK for x402 on Miden, per [`ideas/NEW_DESIGN.md`].
//!
//! Two responsibilities:
//!
//! 1. **Sign-without-prove.** Build a `TransactionRequest` for a P2ID
//!    payment, dry-run via `miden-client` to obtain a
//!    `TransactionSummary`, sign that summary with the agent's **hot
//!    key**, and package into an [`miden_x402_types::AgenticPayload`].
//!    This mirrors
//!    [`MultisigClient::propose_multisig_transaction`](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/crates/miden-multisig-client/src/lib.rs)
//!    adapted for single-signer high-rate use.
//!
//! 2. **Pending-state tracking + coordinator handoff.** Track the
//!    agent's pending-state commitment locally; reconcile with the
//!    agentic-guardian over HTTP; roll back on failure
//!    notifications.

#![forbid(unsafe_code)]

pub mod account_setup;
pub mod client;
pub mod coordinator;
pub mod p2id;
pub mod pending_state;
pub mod sign;

pub use client::AgenticClient;
pub use pending_state::PendingStateTracker;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgenticClientError {
    #[error("http error: {0}")]
    Http(String),

    #[error("invalid response from agentic-guardian: {0}")]
    BadResponse(String),

    #[error("local state mismatch: {0}")]
    StateMismatch(String),

    #[error("signing error: {0}")]
    Sign(String),

    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),
}

pub type AgenticClientResult<T> = Result<T, AgenticClientError>;
