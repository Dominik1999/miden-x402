//! Merchant-facing `/x402/*` routes.
//!
//! Thin wrappers — the actual verify pipeline lives in
//! [`super::submit`]. `verify` and `settle` re-shape the incoming
//! body (which carries the merchant's offered requirements + the
//! buyer's signed payload) into the agentic-submit shape.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use miden_x402_types::{
    AgenticPayload, MidenPaymentPayload, MidenPaymentRequirements, MidenExactPayload,
};

use super::AppState;
use super::submit::{SubmitAck, SubmitRequest};
use crate::error::{AgenticError, AgenticResult};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest {
    #[serde(default)]
    pub x402_version: serde_json::Value, // accept any version literal
    pub payment_payload: MidenPaymentPayload,
    pub payment_requirements: MidenPaymentRequirements,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticSettleResponse {
    pub success: bool,
    pub queued_id: String,
    pub network: String,
    pub new_pending_state_commitment: String,
}

pub async fn verify(
    State(state): State<AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> AgenticResult<Json<serde_json::Value>> {
    let agentic = pick_agentic_payload(&req)?;
    let ack = super::submit::submit(
        State(state),
        Json(SubmitRequest {
            payment_requirements: req.payment_requirements,
            payload: agentic,
        }),
    )
    .await?;
    Ok(Json(serde_json::json!({
        "isValid": true,
        "payer": ack.0.queued_id, // placeholder — verify response in NEW_DESIGN flow returns the payer
    })))
}

pub async fn settle(
    State(state): State<AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> AgenticResult<Json<AgenticSettleResponse>> {
    let network = req.payment_requirements.network.to_string();
    let agentic = pick_agentic_payload(&req)?;
    let ack: Json<SubmitAck> = super::submit::submit(
        State(state),
        Json(SubmitRequest {
            payment_requirements: req.payment_requirements,
            payload: agentic,
        }),
    )
    .await?;
    Ok(Json(AgenticSettleResponse {
        success: true,
        queued_id: ack.0.queued_id,
        network,
        new_pending_state_commitment: ack.0.new_pending_state_commitment,
    }))
}

fn pick_agentic_payload(req: &FacilitatorRequest) -> AgenticResult<AgenticPayload> {
    match &req.payment_payload.payload {
        MidenExactPayload::Agentic(p) => Ok(p.clone()),
        _ => Err(AgenticError::BadRequest(
            "agentic-guardian /x402 endpoints require an Agentic payload".to_owned(),
        )),
    }
}

#[derive(Debug, Serialize)]
struct HealthBody { status: &'static str }

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportedKind {
    x402_version: u8,
    scheme: String,
    network: String,
    settlement: String,
}

#[derive(Debug, Serialize)]
struct SupportedBody {
    kinds: Vec<SupportedKind>,
    extensions: Vec<String>,
}

pub async fn supported(State(state): State<AppState>) -> impl IntoResponse {
    let body = SupportedBody {
        kinds: vec![SupportedKind {
            x402_version: 2,
            scheme: "exact".to_owned(),
            network: state.config.app.network_id.clone(),
            settlement: "agentic".to_owned(),
        }],
        extensions: vec!["miden-agentic-guardian".to_owned()],
    };
    (StatusCode::OK, Json(body))
}

#[derive(Debug, Serialize)]
struct PubkeyBody { todo: &'static str }

pub async fn pubkey() -> impl IntoResponse {
    // In the agentic flow the merchant's trust anchor is the Guardian
    // operator's ack key (signed receipts come from this endpoint).
    // Skeleton: not yet wired.
    (
        StatusCode::OK,
        Json(PubkeyBody {
            todo: "agentic-guardian pubkey endpoint — wire up once the receipt-signer is implemented",
        }),
    )
}
