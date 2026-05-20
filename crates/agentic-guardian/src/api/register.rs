//! `POST /agentic/register` — agent + AP2 mandate registration.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use miden_x402_types::{AccountIdHex, Ap2SignedMandate};

use super::AppState;
use crate::auth::cold_key::verify_mandate_signature;
use crate::error::{AgenticError, AgenticResult};
use crate::storage::{AgentRecord, MandateRecord, memory::unix_now};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub agent_account_id: AccountIdHex,
    pub hot_pubkey_commitment_hex: String,
    pub cold_pubkey_commitment_hex: String,
    pub signed_mandate: Ap2SignedMandate,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub agent_account_id: AccountIdHex,
    pub mandate_id: String,
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> AgenticResult<Json<RegisterResponse>> {
    // 1. Verify the user's cold-key signature on the mandate.
    verify_mandate_signature(&req.signed_mandate, &req.cold_pubkey_commitment_hex)?;

    // 2. Sanity check: mandate's agent_account_id matches body.
    if req.signed_mandate.mandate.agent_account_id != req.agent_account_id {
        return Err(AgenticError::BadRequest(
            "signed mandate's agent_account_id does not match request".to_owned(),
        ));
    }

    // 3. Persist the agent + mandate.
    let now = unix_now();
    state
        .agents
        .register(AgentRecord {
            agent_account_id: req.agent_account_id.clone(),
            hot_pubkey_commitment_hex: req.hot_pubkey_commitment_hex,
            cold_pubkey_commitment_hex: req.cold_pubkey_commitment_hex,
            registered_at_unix_secs: now,
        })
        .await
        .map_err(|e| AgenticError::Storage(e.to_string()))?;
    let mandate_id = req.signed_mandate.mandate.mandate_id.clone();
    state
        .mandates
        .put(MandateRecord {
            signed: req.signed_mandate,
            stored_at_unix_secs: now,
        })
        .await
        .map_err(|e| AgenticError::Storage(e.to_string()))?;

    Ok(Json(RegisterResponse {
        agent_account_id: req.agent_account_id,
        mandate_id,
    }))
}
