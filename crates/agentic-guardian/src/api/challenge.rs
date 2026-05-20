//! `POST /x402/challenge` — issue a server-generated `serial_num`.
//!
//! The merchant calls this before emitting the 402 so the buyer's
//! signed unproven tx can be validated against a known
//! pre-issued serial.

use axum::{Json, extract::State};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use miden_x402_types::{MidenPaymentRequirements, NoteIdHex};

use super::AppState;
use crate::error::{AgenticError, AgenticResult};
use crate::storage::ChallengeRecord;
use crate::storage::memory::unix_now;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    pub payment_requirements: MidenPaymentRequirements,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    pub serial_num: String,
    pub expires_in_seconds: u64,
}

pub async fn challenge(
    State(state): State<AppState>,
    Json(req): Json<ChallengeRequest>,
) -> AgenticResult<Json<ChallengeResponse>> {
    if !miden_x402_types::network::is_miden(&req.payment_requirements.network) {
        return Err(AgenticError::UnsupportedNetwork);
    }
    let serial_hex = generate_serial_num()?;
    let now = unix_now();
    let ttl = state.config.mandate.challenge_ttl;
    state
        .challenges
        .put(ChallengeRecord {
            serial_num: serial_hex.clone(),
            requirements: req.payment_requirements,
            issued_at_unix_secs: now,
            expires_at_unix_secs: now + ttl.as_secs(),
        })
        .await
        .map_err(|e| AgenticError::Storage(e.to_string()))?;

    Ok(Json(ChallengeResponse {
        serial_num: serial_hex.as_str().to_owned(),
        expires_in_seconds: ttl.as_secs(),
    }))
}

fn generate_serial_num() -> AgenticResult<NoteIdHex> {
    let mut rng = rand::rng();
    let mut buf = [0u8; 32];
    rng.fill_bytes(&mut buf);
    let hex = format!("0x{}", hex::encode(buf));
    hex.parse::<NoteIdHex>()
        .map_err(|e| AgenticError::Internal(format!("generated serial_num: {e}")))
}
