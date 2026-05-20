//! `GET /agentic/pending_state/{agent_id}` — agent's currently-tracked
//! pending state. Useful for clients recovering from a crash.

use axum::{Json, extract::{Path, State}};
use serde::Serialize;

use super::AppState;
use crate::error::AgenticResult;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingStateResponse {
    pub agent_account_id: String,
    pub current_commitment_hex: String,
    pub nonce: u64,
    pub last_advanced_at_unix_secs: u64,
}

pub async fn pending(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> AgenticResult<Json<PendingStateResponse>> {
    let s = state.pending_state.current(&agent_id).await?;
    Ok(Json(PendingStateResponse {
        agent_account_id: s.agent_account_id.as_str().to_owned(),
        current_commitment_hex: s.current_commitment_hex,
        nonce: s.nonce,
        last_advanced_at_unix_secs: s.last_advanced_at_unix_secs,
    }))
}
