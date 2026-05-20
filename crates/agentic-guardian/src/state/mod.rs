//! Per-agent pending-state tracking — NEW_DESIGN.md §47-50.
//!
//! The agentic-guardian is the single serialization authority for an
//! agent's transactions. After verifying a tx, it advances the agent's
//! pending state commitment + nonce. The agent's next tx must build
//! against this new commitment; the agentic-guardian rejects any tx
//! whose `pending_state_commitment` doesn't match its tracked value
//! (preventing forks).
//!
//! Storage: [`crate::storage::PendingStateRepo`]. Atomicity: the
//! `try_advance` method is a CAS — succeeds only if the stored prev
//! commitment matches what the caller claims.

pub mod pending;
