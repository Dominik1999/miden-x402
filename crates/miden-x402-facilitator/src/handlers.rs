//! Axum HTTP handlers and router construction.
//!
//! Phase A endpoints (always on):
//!
//! - `GET /health` — liveness probe.
//! - `GET /supported` — the x402 `SupportedResponse` declaring the single
//!   `(scheme=exact, network=miden:testnet)` kind this facilitator handles.
//! - `POST /verify` — verifies a Miden `exact` payment against on-chain
//!   state. Returns `VerifyResponse::Valid` on success.
//! - `POST /settle` — same checks; returns `SettleResponse::Success` with the
//!   buyer's create-note transaction id.
//!
//! Phase B endpoints (mounted only when [`GuardianConfig::enabled`] is true):
//!
//! - `POST /guardian/challenge` — issues a server-generated `serial_num`
//!   that the merchant embeds in `extra.serialNum` of the 402 response.
//! - `POST /guardian/verify` — verify-before-prove + reserve nullifiers.
//! - `POST /guardian/settle` — verify + reserve + prove + submit.
//!
//! [`GuardianConfig::enabled`]: crate::config::GuardianConfig::enabled

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use miden_remote_prover_client::RemoteTransactionProver;
use miden_x402_types::{MidenExactPayload, MidenPaymentPayload, MidenPaymentRequirements};
use serde::{Deserialize, Serialize};
use x402_types::chain::ChainId;
use x402_types::proto::v1::{SettleResponse, VerifyResponse};
use x402_types::proto::v2::X402Version2;

use crate::config::{FacilitatorConfig, GuardianConfig};
use crate::error::FacilitatorError;
use crate::guardian::{
    ChallengeStore, ReservedNullifierSet, settle_and_submit, verify_unproven,
};
use crate::node::MidenNode;
use crate::verifier;

/// Shared application state available to every handler.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    node: Arc<dyn MidenNode>,
    config: FacilitatorConfig,
    /// Guardian state — `None` when `config.guardian.enabled == false`.
    guardian: Option<GuardianState>,
}

/// Shared, in-memory Guardian state. One instance per facilitator process.
pub struct GuardianState {
    pub challenges: ChallengeStore,
    pub reservations: ReservedNullifierSet,
    pub remote_prover: Option<RemoteTransactionProver>,
}

impl AppState {
    /// Constructs a new application state for Phase A only (no Guardian).
    pub fn new<N: MidenNode + 'static>(node: N, config: FacilitatorConfig) -> Self {
        let guardian = build_guardian_state(&config.guardian);
        Self {
            inner: Arc::new(AppStateInner {
                node: Arc::new(node),
                config,
                guardian,
            }),
        }
    }

    fn node(&self) -> &dyn MidenNode {
        self.inner.node.as_ref()
    }

    fn config(&self) -> &FacilitatorConfig {
        &self.inner.config
    }

    fn guardian(&self) -> Option<&GuardianState> {
        self.inner.guardian.as_ref()
    }
}

fn build_guardian_state(cfg: &GuardianConfig) -> Option<GuardianState> {
    if !cfg.enabled {
        return None;
    }
    let challenges = ChallengeStore::new(Duration::from_secs(cfg.challenge_ttl_secs));
    let reservations = ReservedNullifierSet::new(Duration::from_secs(cfg.reservation_ttl_secs));
    let remote_prover = cfg
        .remote_prover_url
        .as_deref()
        .map(RemoteTransactionProver::new);
    Some(GuardianState {
        challenges,
        reservations,
        remote_prover,
    })
}

/// Builds the axum [`Router`] for the facilitator HTTP API. Phase B
/// endpoints are mounted only when `state.guardian()` is `Some`.
pub fn build_router(state: AppState) -> Router {
    let mut r = Router::new()
        .route("/health", get(health))
        .route("/supported", get(supported))
        .route("/verify", post(verify))
        .route("/settle", post(settle));

    if state.guardian().is_some() {
        r = r
            .route("/guardian/challenge", post(guardian_challenge))
            .route("/guardian/verify", post(guardian_verify))
            .route("/guardian/settle", post(guardian_settle));
    }

    r.with_state(state)
}

#[derive(Debug, Serialize)]
struct HealthBody<'a> {
    status: &'a str,
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthBody { status: "ok" }))
}

/// The body shape returned by `GET /supported`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedKind {
    pub x402_version: u8,
    pub scheme: String,
    pub network: ChainId,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedBody {
    pub kinds: Vec<SupportedKind>,
    pub extensions: Vec<String>,
}

async fn supported(State(state): State<AppState>) -> impl IntoResponse {
    let mut extensions: Vec<String> = Vec::new();
    if state.guardian().is_some() {
        extensions.push("miden-guardian-fast".to_owned());
    }
    let body = SupportedBody {
        kinds: vec![SupportedKind {
            x402_version: X402Version2::VALUE,
            scheme: "exact".to_owned(),
            network: miden_x402_types::miden_testnet(),
        }],
        extensions,
    };
    (StatusCode::OK, Json(body))
}

/// Body shared by `/verify` and `/settle`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest {
    pub x402_version: X402Version2,
    pub payment_payload: MidenPaymentPayload,
    pub payment_requirements: MidenPaymentRequirements,
}

async fn verify(
    State(state): State<AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> Result<Json<VerifyResponse>, FacilitatorError> {
    let response = verifier::verify(
        &req.payment_payload,
        &req.payment_requirements,
        state.node(),
        state.config(),
    )
    .await?;
    Ok(Json(response))
}

async fn settle(
    State(state): State<AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> Result<Json<SettleResponse>, FacilitatorError> {
    let response = verifier::settle(
        &req.payment_payload,
        &req.payment_requirements,
        state.node(),
        state.config(),
    )
    .await?;
    Ok(Json(response))
}

// ---------- Phase B handlers ----------

/// Body for `POST /guardian/challenge`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardianChallengeRequest {
    pub payment_requirements: MidenPaymentRequirements,
}

/// Body returned by `POST /guardian/challenge`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardianChallengeResponse {
    /// Server-issued 32-byte `serial_num` (canonical hex). The merchant
    /// embeds this in `extra.serialNum` on the 402 response.
    pub serial_num: String,
    /// TTL applied to this challenge, in seconds.
    pub expires_in_seconds: u64,
}

async fn guardian_challenge(
    State(state): State<AppState>,
    Json(req): Json<GuardianChallengeRequest>,
) -> Result<Json<GuardianChallengeResponse>, FacilitatorError> {
    let g = state.guardian().ok_or(FacilitatorError::GuardianDisabled)?;
    let mut rng = rand::rng();
    let issued = g.challenges.issue(&req.payment_requirements, &mut rng);
    Ok(Json(GuardianChallengeResponse {
        serial_num: issued.serial_num_hex.as_str().to_owned(),
        expires_in_seconds: g.challenges.ttl().as_secs(),
    }))
}

async fn guardian_verify(
    State(state): State<AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> Result<Json<VerifyResponse>, FacilitatorError> {
    let g = state.guardian().ok_or(FacilitatorError::GuardianDisabled)?;
    let payload = match &req.payment_payload.payload {
        MidenExactPayload::GuardianFast(p) => p,
        _ => {
            return Err(FacilitatorError::BadRequest(
                "guardian endpoints require noteType=\"guardianFast\"".to_owned(),
            ));
        }
    };
    let verified = verify_unproven(
        payload,
        &req.payment_requirements,
        state.config(),
        &g.challenges,
        &g.reservations,
    )
    .await?;
    // We don't *promote* on bare /verify — the nullifiers stay reserved
    // until either /guardian/settle promotes them or the TTL sweeper
    // releases them. Phase B clients are expected to call /guardian/settle
    // immediately after a successful /guardian/verify.
    let _ = verified; // drop releases nothing
    Ok(Json(VerifyResponse::valid(
        req.payment_payload.payload.guardian_fast_payer().to_owned(),
    )))
}

async fn guardian_settle(
    State(state): State<AppState>,
    Json(req): Json<FacilitatorRequest>,
) -> Result<Json<SettleResponse>, FacilitatorError> {
    let g = state.guardian().ok_or(FacilitatorError::GuardianDisabled)?;
    let prover = g.remote_prover.as_ref().ok_or_else(|| {
        FacilitatorError::RemoteProverUnavailable(
            "MIDEN_X402_REMOTE_PROVER_URL is not set; cannot prove + submit".to_owned(),
        )
    })?;
    let payload = match &req.payment_payload.payload {
        MidenExactPayload::GuardianFast(p) => p,
        _ => {
            return Err(FacilitatorError::BadRequest(
                "guardian endpoints require noteType=\"guardianFast\"".to_owned(),
            ));
        }
    };
    let verified = verify_unproven(
        payload,
        &req.payment_requirements,
        state.config(),
        &g.challenges,
        &g.reservations,
    )
    .await?;
    let response = settle_and_submit(
        verified,
        prover,
        state.node(),
        &g.reservations,
        &req.payment_requirements.network.to_string(),
    )
    .await?;
    Ok(Json(response))
}

/// Helper: extract the buyer account id from a `GuardianFast` payload for
/// the `VerifyResponse` echo. Lives here so the public `MidenExactPayload`
/// API doesn't need an extra accessor.
trait GuardianFastPayerExt {
    fn guardian_fast_payer(&self) -> &str;
}

impl GuardianFastPayerExt for MidenExactPayload {
    fn guardian_fast_payer(&self) -> &str {
        match self {
            MidenExactPayload::GuardianFast(p) => p.sender.as_str(),
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FaucetAllowlist;
    use crate::node::NoteSnapshot;
    use crate::node::tests::MockNode;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use miden_x402_types::{
        AccountIdHex, AssetTransferMethodTag, ExactScheme, MidenExactExtra, MidenExactPayload,
        NoteIdHex, NoteKind, PublicP2idPayload, SettlementKind, TransactionIdHex, miden_testnet,
    };
    use serde_json::json;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn account(s: &str) -> AccountIdHex {
        s.parse().unwrap()
    }

    fn word(c: char) -> String {
        format!("0x{}", c.to_string().repeat(64))
    }

    fn note_id(c: char) -> NoteIdHex {
        word(c).parse().unwrap()
    }

    fn tx_id(c: char) -> TransactionIdHex {
        word(c).parse().unwrap()
    }

    fn config() -> FacilitatorConfig {
        FacilitatorConfig {
            listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            rpc_url: "http://localhost".to_owned(),
            rpc_timeout_ms: 1000,
            allowed_faucets: FaucetAllowlist::Any,
            freshness_blocks: 50,
            guardian: crate::config::GuardianConfig::default(),
        }
    }

    fn requirements() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: ExactScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: account("0x103f8a1ad4b983104aec0412ab0b0d"),
            max_timeout_seconds: 120,
            asset: account("0x0a7d175ed63ec5200fb2ced86f6aa5"),
            extra: MidenExactExtra {
                asset_transfer_method: AssetTransferMethodTag,
                token_symbol: "USDC".to_owned(),
                decimals: 6,
                note_type: NoteKind::Public,
                settlement: SettlementKind::Commit,
                guardian_url: None,
                serial_num: None,
            },
        }
    }

    fn payload(req: &MidenPaymentRequirements) -> MidenPaymentPayload {
        MidenPaymentPayload {
            accepted: req.clone(),
            payload: MidenExactPayload::Public(PublicP2idPayload {
                note_id: note_id('a'),
                transaction_id: tx_id('b'),
                sender: account("0x857b06519e91e3a54538791bdbb0e2"),
                block_num: 100,
                asset: req.asset.clone(),
                amount: req.amount.clone(),
            }),
            resource: None,
            x402_version: X402Version2,
            extensions: None,
        }
    }

    fn snapshot(req: &MidenPaymentRequirements) -> NoteSnapshot {
        NoteSnapshot {
            block_num: 100,
            sender: account("0x857b06519e91e3a54538791bdbb0e2"),
            recipient: req.pay_to.clone(),
            asset_faucet: req.asset.clone(),
            asset_amount: 1000,
            is_consumed: false,
        }
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let node = MockNode::new(110);
        let state = AppState::new(node, config());
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn supported_endpoint_lists_miden_testnet() {
        let node = MockNode::new(110);
        let state = AppState::new(node, config());
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/supported")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["kinds"][0]["scheme"], "exact");
        assert_eq!(value["kinds"][0]["network"], "miden:testnet");
        assert_eq!(value["kinds"][0]["x402Version"], 2);
    }

    #[tokio::test]
    async fn verify_endpoint_happy_path() {
        let node = MockNode::new(110);
        node.insert(note_id('a'), Some(snapshot(&requirements())));
        let state = AppState::new(node, config());
        let app = build_router(state);

        let req = requirements();
        let pay = payload(&req);
        let body = serde_json::to_vec(&json!({
            "x402Version": 2,
            "paymentPayload": pay,
            "paymentRequirements": req,
        }))
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["isValid"], true);
        assert_eq!(value["payer"], "0x857b06519e91e3a54538791bdbb0e2");
    }

    #[tokio::test]
    async fn verify_endpoint_returns_error_body_when_note_missing() {
        let node = MockNode::new(110);
        // Do not insert.
        let state = AppState::new(node, config());
        let app = build_router(state);

        let req = requirements();
        let pay = payload(&req);
        let body = serde_json::to_vec(&json!({
            "x402Version": 2,
            "paymentPayload": pay,
            "paymentRequirements": req,
        }))
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/verify")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn settle_endpoint_returns_buyer_tx_id() {
        let node = MockNode::new(110);
        node.insert(note_id('a'), Some(snapshot(&requirements())));
        let state = AppState::new(node, config());
        let app = build_router(state);

        let req = requirements();
        let pay = payload(&req);
        let body = serde_json::to_vec(&json!({
            "x402Version": 2,
            "paymentPayload": pay,
            "paymentRequirements": req,
        }))
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/settle")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["success"], true);
        assert_eq!(value["transaction"], word('b'));
        assert_eq!(value["network"], "miden:testnet");
    }
}
