//! Pending-state tracker — wraps the [`PendingStateRepo`] with the
//! verify-path semantics from NEW_DESIGN §47-50.

use std::sync::Arc;

use crate::error::{AgenticError, AgenticResult};
use crate::storage::{PendingState, PendingStateRepo};

/// Convenience wrapper around the repo. Holds the trait object so call
/// sites don't carry generic params.
#[derive(Clone)]
pub struct PendingStateTracker {
    repo: Arc<dyn PendingStateRepo>,
}

impl PendingStateTracker {
    pub fn new(repo: Arc<dyn PendingStateRepo>) -> Self { Self { repo } }

    /// Reads the agent's currently-acknowledged pending state. Returns
    /// `AgenticError::AgentNotRegistered` if no pending state exists.
    pub async fn current(&self, agent_account_id: &str) -> AgenticResult<PendingState> {
        self.repo
            .get(agent_account_id)
            .await
            .map_err(|e| AgenticError::Storage(e.to_string()))?
            .ok_or(AgenticError::AgentNotRegistered)
    }

    /// Atomically advances the agent's pending state from
    /// `expected_prev` → `new_commitment`. Fails with
    /// `PendingStateMismatch` if the stored prev doesn't match
    /// (concurrent submission won the race) — the caller can then
    /// retry the verify pipeline against the fresh pending state.
    pub async fn try_advance(
        &self,
        agent_account_id: &str,
        expected_prev: &str,
        new_commitment: &str,
        new_nonce: u64,
    ) -> AgenticResult<()> {
        match self
            .repo
            .try_advance(agent_account_id, expected_prev, new_commitment, new_nonce)
            .await
        {
            Ok(_) => Ok(()),
            Err(crate::storage::StorageError::Conflict(_)) => {
                let current = self
                    .repo
                    .get(agent_account_id)
                    .await
                    .map_err(|e| AgenticError::Storage(e.to_string()))?;
                Err(AgenticError::PendingStateMismatch {
                    claimed: expected_prev.to_owned(),
                    actual: current
                        .map(|s| s.current_commitment_hex)
                        .unwrap_or_else(|| "<absent>".into()),
                })
            }
            Err(e) => Err(AgenticError::Storage(e.to_string())),
        }
    }

    /// Rolls back the agent's pending state after a tx fails on the
    /// batch worker (NEW_DESIGN §86). Idempotent.
    pub async fn rollback(
        &self,
        agent_account_id: &str,
        rolled_back: &str,
        previous: &str,
        previous_nonce: u64,
    ) -> AgenticResult<()> {
        self.repo
            .rollback(agent_account_id, rolled_back, previous, previous_nonce)
            .await
            .map_err(|e| AgenticError::Storage(e.to_string()))
    }
}
