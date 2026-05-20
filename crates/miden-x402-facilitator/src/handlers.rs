//! Axum handlers for the `/x402/*` routes mounted on top of OZ Guardian.
//!
//! Endpoints:
//!
//! - `GET  /x402/health` — liveness probe.
//! - `GET  /x402/supported` — declares the `(scheme=miden-p2id-private,
//!   network=miden:testnet|miden:mainnet)` kinds.
//! - `GET  /x402/pubkey` — returns the facilitator's settle-receipt pubkey
//!   so merchants can cache it and verify receipts locally.
//! - `POST /x402/challenge` — issues a server-generated `serial_num`.
//! - `POST /x402/verify` — verify-before-prove + reserve nullifiers.
//! - `POST /x402/settle` — verify + enqueue + sign receipt. Prove + submit
//!   happen asynchronously in [`crate::batch::BatchSettleWorker`].
//!
//! These handlers are pure on `AppState`; the binary applies Guardian's
//! auth middleware (`server::middleware::*`) on top.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use miden_protocol::{Felt, Word};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use x402_types::proto::v1::VerifyResponse;
use x402_types::proto::v2::X402Version2;

use crate::balance::BalanceLookup;
use crate::batch::BatchSettleQueue;
use crate::buyer_auth::BuyerAuthLookup;
use crate::config::FacilitatorConfig;
use crate::error::FacilitatorError;
use crate::mandate::ArcMandatePolicy;
use crate::receipt::ReceiptSigner;
use crate::settle::{X402SettleSuccess, settle};
use crate::storage::{
    ChallengeRepo, IssuedChallenge, ReservationRepo, SerializableWord, unix_now,
};
use crate::verify::{NullifierBackstop, VerifyDeps, verify_unproven};
use miden_x402_types::{
    MidenPaymentPayload, MidenPaymentRequirements, MidenWirePayload, NoteIdHex,
};

/// Shared application state for the x402 routes. Cloneable handles around
/// `Arc`s — every field is shared with the background batch worker.
#[derive(Clone)]
pub struct X402AppState {
    pub config: Arc<FacilitatorConfig>,
    pub challenges: Arc<dyn ChallengeRepo>,
    pub reservations: Arc<dyn ReservationRepo>,
    pub queue: BatchSettleQueue,
    pub mandate: ArcMandatePolicy,
    pub signer: Arc<ReceiptSigner>,
    pub buyer_auth: Arc<dyn BuyerAuthLookup>,
    pub balance: Arc<dyn BalanceLookup>,
    pub nullifier_backstop: Arc<dyn NullifierBackstop>,
}

/// Builds the `/x402/*` router. The binary `.merge(...)` this on top of
/// the OZ Guardian router built from `server::api::http::*` pub handlers.
pub fn x402_router(state: X402AppState) -> Router {
    Router::new()
        .route("/x402/health", get(health))
        .route("/x402/supported", get(supported))
        .route("/x402/pubkey", get(pubkey))
        .route("/x402/challenge", post(issue_challenge))
        .route("/x402/verify", post(verify))
        .route("/x402/settle", post(settle_handler))
        .with_state(state)
}

// ---------- Health + supported + pubkey ----------

#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportedKind {
    x402_version: u8,
    scheme: String,
    network: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SupportedBody {
    kinds: Vec<SupportedKind>,
    extensions: Vec<String>,
}

async fn supported(State(state): State<X402AppState>) -> impl IntoResponse {
    let body = SupportedBody {
        kinds: vec![SupportedKind {
            x402_version: X402Version2::VALUE,
            scheme: "miden-p2id-private".to_owned(),
            network: state.config.network.clone(),
        }],
        extensions: vec!["miden-guardian-facilitator".to_owned()],
    };
    (StatusCode::OK, Json(body))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PubkeyBody {
    /// Falcon-512 Poseidon2 pubkey commitment (canonical hex).
    commitment: String,
    /// Base64 of the raw Falcon public-key bytes — clients can verify
    /// signatures locally without rederiving the commitment.
    pubkey_b64: String,
}

async fn pubkey(State(state): State<X402AppState>) -> impl IntoResponse {
    let body = PubkeyBody {
        commitment: state.signer.pubkey_commitment_hex(),
        pubkey_b64: BASE64.encode(state.signer.pubkey_bytes()),
    };
    (StatusCode::OK, Json(body))
}

// ---------- Challenge ----------

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

async fn issue_challenge(
    State(state): State<X402AppState>,
    Json(req): Json<ChallengeRequest>,
) -> Result<Json<ChallengeResponse>, FacilitatorError> {
    use miden_x402_types::network::is_miden;
    if !is_miden(&req.payment_requirements.network) {
        return Err(FacilitatorError::UnsupportedNetwork);
    }
    // Generate the serial_num synchronously (the thread-local RNG is not
    // Send across .await points). Scope the borrow to keep the future Send.
    let serial = generate_serial_num();
    let serial_hex: NoteIdHex = serial.to_hex().parse().map_err(|_| {
        FacilitatorError::Internal("generated serial_num is not a valid NoteIdHex".into())
    })?;
    let now = unix_now();
    let ttl = state.config.challenge_ttl;
    let issued = IssuedChallenge {
        serial_num: SerializableWord(serial),
        serial_num_hex: serial_hex.clone(),
        requirements: req.payment_requirements,
        issued_at_unix_secs: now,
        expires_at_unix_secs: now + ttl.as_secs(),
    };
    state.challenges.put(issued).await?;
    Ok(Json(ChallengeResponse {
        serial_num: serial_hex.as_str().to_owned(),
        expires_in_seconds: ttl.as_secs(),
    }))
}

fn generate_serial_num() -> Word {
    let mut rng = rand::rng();
    random_word(&mut rng)
}

fn random_word<R: RngCore>(rng: &mut R) -> Word {
    let f = |rng: &mut R| {
        let mut buf = [0u8; 8];
        rng.fill_bytes(&mut buf);
        Felt::new(u64::from_le_bytes(buf))
    };
    Word::new([f(rng), f(rng), f(rng), f(rng)])
}

// ---------- Verify + settle ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest {
    #[serde(default)]
    pub x402_version: X402Version2,
    pub payment_payload: MidenPaymentPayload,
    pub payment_requirements: MidenPaymentRequirements,
}

async fn verify(
    State(state): State<X402AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> Result<Json<VerifyResponse>, FacilitatorError> {
    let payload = inner_payload(&req.payment_payload);
    let deps = build_verify_deps(&state);
    let verified = verify_unproven(payload, &req.payment_requirements, deps).await?;
    // We don't promote on bare /verify — the nullifiers stay reserved until
    // /x402/settle enqueues and the batch worker promotes them on submit.
    Ok(Json(VerifyResponse::valid(verified.payer.into_inner())))
}

async fn settle_handler(
    State(state): State<X402AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> Result<Json<X402SettleSuccess>, FacilitatorError> {
    let payload = inner_payload(&req.payment_payload);
    let deps = build_verify_deps(&state);
    let verified = verify_unproven(payload, &req.payment_requirements, deps).await?;
    let response = settle(verified, &state.queue, &state.signer, &state.config.network).await?;
    Ok(Json(response))
}

fn inner_payload(payload: &MidenPaymentPayload) -> &miden_x402_types::MidenP2idPrivatePayload {
    match &payload.payload {
        MidenWirePayload::MidenP2idPrivate(p) => p,
    }
}

fn build_verify_deps<'a>(state: &'a X402AppState) -> VerifyDeps<'a> {
    let reservation_ttl: Duration = state.config.reservation_ttl;
    VerifyDeps {
        network: &state.config.network,
        challenges: state.challenges.as_ref(),
        reservations: state.reservations.as_ref(),
        reservation_ttl,
        mandate: state.mandate.clone(),
        buyer_auth: state.buyer_auth.as_ref(),
        balance: state.balance.as_ref(),
        nullifier_backstop: state.nullifier_backstop.as_ref(),
    }
}
