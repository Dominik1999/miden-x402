//! High-level agentic client entry point.

use crate::AgenticClientResult;
use crate::coordinator::CoordinatorClient;
use crate::pending_state::PendingStateTracker;

/// Bundles a [`CoordinatorClient`] (HTTP to the agentic-guardian) with
/// a local [`PendingStateTracker`].
pub struct AgenticClient {
    pub coordinator: CoordinatorClient,
    pub pending: PendingStateTracker,
}

impl AgenticClient {
    pub fn new(guardian_url: impl Into<String>) -> AgenticClientResult<Self> {
        Ok(Self {
            coordinator: CoordinatorClient::new(guardian_url.into()),
            pending: PendingStateTracker::default(),
        })
    }
}
