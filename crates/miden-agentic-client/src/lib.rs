//! Agent-paced sign-without-prove client for x402 payments on Miden.
//!
//! See `ideas/DESIGN.md` (per-payment hot-path section).

pub mod client;
pub mod error;
pub mod key;
pub mod miden_integration;
pub mod transport;
pub mod types;

pub use client::{AgenticClient, AgenticClientBuilder};
pub use error::{AgenticError, Result};
pub use types::{
    AckResponse, AgentMandate, AgentStateResponse, AgenticPayload, PayTimings, PaymentReceipt,
    PaymentStatus, PaymentStatusResponse, RegisterAgentRequest, RegisterAgentResponse, X402Context,
};
