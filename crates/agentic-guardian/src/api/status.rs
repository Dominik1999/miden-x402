//! `GET /agentic/status/{queued_id}` — current state of a queued tx.

use axum::{Json, extract::{Path, State}};
use serde::Serialize;

use super::AppState;
use crate::error::{AgenticError, AgenticResult};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum StatusResponse {
    Queued { queued_id: String, enqueued_at_unix_secs: u64 },
    Submitted { queued_id: String, on_chain_tx_id: String },
}

pub async fn status(
    State(state): State<AppState>,
    Path(queued_id): Path<String>,
) -> AgenticResult<Json<StatusResponse>> {
    let entry = state
        .queue
        .repo()
        .lookup(&queued_id)
        .await
        .map_err(|e| AgenticError::Storage(e.to_string()))?
        .ok_or_else(|| AgenticError::QueuedTxNotFound(queued_id.clone()))?;
    if entry.submitted {
        Ok(Json(StatusResponse::Submitted {
            queued_id,
            on_chain_tx_id: entry.on_chain_tx_id.unwrap_or_default(),
        }))
    } else {
        Ok(Json(StatusResponse::Queued {
            queued_id,
            enqueued_at_unix_secs: entry.enqueued_at_unix_secs,
        }))
    }
}
