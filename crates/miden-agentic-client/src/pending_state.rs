//! Client-side mirror of the agent's pending state.

use std::collections::HashMap;

use crate::AgenticClientError;

/// Tracks the agent's view of "the next tx must build on this
/// commitment + nonce." Per NEW_DESIGN §110-112, the client advances
/// after a successful ack from the agentic-guardian, and rolls back if
/// a queued tx is later reported as failed.
#[derive(Debug, Default, Clone)]
pub struct PendingStateTracker {
    current_commitment_hex: String,
    nonce: u64,
    in_flight: HashMap<String, (String, u64)>,
}

impl PendingStateTracker {
    pub fn new(initial_commitment_hex: String, initial_nonce: u64) -> Self {
        Self {
            current_commitment_hex: initial_commitment_hex,
            nonce: initial_nonce,
            in_flight: HashMap::new(),
        }
    }

    pub fn current_commitment_hex(&self) -> &str { &self.current_commitment_hex }
    pub fn nonce(&self) -> u64 { self.nonce }

    /// Called after the agentic-guardian acks a submission.
    pub fn advance_after_ack(
        &mut self,
        queued_id: String,
        new_commitment_hex: String,
        new_nonce: u64,
    ) {
        self.in_flight
            .insert(queued_id, (self.current_commitment_hex.clone(), self.nonce));
        self.current_commitment_hex = new_commitment_hex;
        self.nonce = new_nonce;
    }

    /// Roll back to the snapshot we held just before this queued tx
    /// advanced us. Used when the guardian reports a settle failure.
    pub fn rollback(&mut self, queued_id: &str) -> Result<(), AgenticClientError> {
        match self.in_flight.remove(queued_id) {
            Some((prev_commitment, prev_nonce)) => {
                self.current_commitment_hex = prev_commitment;
                self.nonce = prev_nonce;
                Ok(())
            }
            None => Err(AgenticClientError::StateMismatch(format!(
                "queued_id {queued_id} not in flight"
            ))),
        }
    }

    pub fn mark_committed(&mut self, queued_id: &str) {
        self.in_flight.remove(queued_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_then_rollback() {
        let mut t = PendingStateTracker::new("0xaaa".into(), 1);
        t.advance_after_ack("q1".into(), "0xbbb".into(), 2);
        assert_eq!(t.current_commitment_hex(), "0xbbb");
        t.rollback("q1").unwrap();
        assert_eq!(t.current_commitment_hex(), "0xaaa");
        assert_eq!(t.nonce(), 1);
    }

    #[test]
    fn mark_committed_clears_in_flight() {
        let mut t = PendingStateTracker::new("0xaaa".into(), 1);
        t.advance_after_ack("q1".into(), "0xbbb".into(), 2);
        t.mark_committed("q1");
        // Rolling back now must fail.
        assert!(t.rollback("q1").is_err());
    }
}
