//! axum routes for the agentic-guardian.
//!
//! - `/agentic/*` — agent-client facing (register, submit, status, pending)
//! - `/x402/*` — merchant-facing (verify, settle, challenge, supported, health, pubkey)

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::batch::BatchSettleQueue;
use crate::config::Config;
use crate::mandate::Ap2Policy;
use crate::runtime::MidenRuntimeHandle;
use crate::state::pending::PendingStateTracker;
use crate::storage::{
    AgentRegistryRepo, BatchQueueRepo, ChallengeRepo, MandateRepo, ReservationRepo,
};

pub mod challenge;
pub mod pending;
pub mod register;
pub mod status;
pub mod submit;
pub mod x402;

/// State threaded through every handler. Cloneable handles around Arcs.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub agents: Arc<dyn AgentRegistryRepo>,
    pub mandates: Arc<dyn MandateRepo>,
    pub pending_state: PendingStateTracker,
    pub reservations: Arc<dyn ReservationRepo>,
    pub queue: BatchSettleQueue,
    pub challenges: Arc<dyn ChallengeRepo>,
    pub policy: Ap2Policy,
    pub runtime: MidenRuntimeHandle,
}

/// Builds the merged `/agentic/*` + `/x402/*` router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // agentic API (client-facing)
        .route("/agentic/register", post(register::register))
        .route("/agentic/submit", post(submit::submit))
        .route(
            "/agentic/status/{queued_id}",
            get(status::status),
        )
        .route(
            "/agentic/pending_state/{agent_id}",
            get(pending::pending),
        )
        // x402 API (merchant-facing)
        .route("/x402/challenge", post(challenge::challenge))
        .route("/x402/verify", post(x402::verify))
        .route("/x402/settle", post(x402::settle))
        .route("/x402/supported", get(x402::supported))
        .route("/x402/health", get(x402::health))
        .route("/x402/pubkey", get(x402::pubkey))
        // BatchQueueRepo trait-imported here for the build_router compile
        .with_state(state)
}

// Keep the storage trait import alive (suppresses unused warnings when
// the file is included via the module tree without an explicit use).
#[doc(hidden)]
#[allow(dead_code)]
fn _import_traits(repo: Arc<dyn BatchQueueRepo>) -> Arc<dyn BatchQueueRepo> {
    repo
}
