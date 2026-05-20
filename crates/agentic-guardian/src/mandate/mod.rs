//! AP2 mandate enforcement — NEW_DESIGN.md §42-44 bullets.
//!
//! Schema lives in [`miden_x402_types::Ap2Mandate`]. This module
//! implements the runtime evaluator: per-tx + counter-backed.

pub mod ap2;

pub use ap2::Ap2Policy;
