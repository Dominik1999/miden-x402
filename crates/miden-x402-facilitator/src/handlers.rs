//! Axum HTTP handlers and router construction.
//!
//! The router exposes:
//!
//! - `GET /health` — liveness probe.
//! - `GET /supported` — the x402 `SupportedResponse` declaring the single
//!   `(scheme=exact, network=miden:testnet)` kind this facilitator handles.
//! - `POST /verify` — verifies a Miden `exact` payment against on-chain
//!   state. Returns `VerifyResponse::Valid` on success.
//! - `POST /settle` — same checks; returns `SettleResponse::Success` with the
//!   buyer's create-note transaction id.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use miden_x402_types::{MidenPaymentPayload, MidenPaymentRequirements};
use serde::{Deserialize, Serialize};
use x402_types::chain::ChainId;
use x402_types::proto::v1::{SettleResponse, VerifyResponse};
use x402_types::proto::v2::X402Version2;

use crate::config::FacilitatorConfig;
use crate::error::FacilitatorError;
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
}

impl AppState {
    /// Constructs a new application state.
    pub fn new<N: MidenNode + 'static>(node: N, config: FacilitatorConfig) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                node: Arc::new(node),
                config,
            }),
        }
    }

    fn node(&self) -> &dyn MidenNode {
        self.inner.node.as_ref()
    }

    fn config(&self) -> &FacilitatorConfig {
        &self.inner.config
    }
}

/// Builds the axum [`Router`] for the facilitator HTTP API.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/supported", get(supported))
        .route("/verify", post(verify))
        .route("/settle", post(settle))
        .with_state(state)
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

async fn supported() -> impl IntoResponse {
    let body = SupportedBody {
        kinds: vec![SupportedKind {
            x402_version: X402Version2::VALUE,
            scheme: "exact".to_owned(),
            network: miden_x402_types::miden_testnet(),
        }],
        extensions: Vec::new(),
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
        NoteIdHex, NoteKind, PublicP2idPayload, TransactionIdHex, miden_testnet,
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
