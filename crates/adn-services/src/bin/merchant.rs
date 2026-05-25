//! adn-merchant — HTTP server that gates resources behind x402 batch-settlement payments.
//!
//! GET /resource → 402 with payment details
//! POST /pay     → verify voucher locally, settle when batch is full
//!
//! After settlement, stores the remainder note for the next batch.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use clap::Parser;
use tokio::sync::Mutex;

use adn_services::{
    PaymentRequired, PaymentRequest, PaymentResponse, SettleRequest, SettleResponse,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 7001)]
    port: u16,
    #[arg(long)]
    facilitator_url: String,
    #[arg(long)]
    merchant_id: String,
    #[arg(long, default_value_t = 1000)]
    amount_per_request: u64,
    #[arg(long, default_value_t = 5)]
    settle_after: usize,
}

struct MerchantInner {
    facilitator_url: String,
    merchant_id: String,
    amount_per_request: u64,
    settle_after: usize,
    /// Requests since last settlement
    batch_count: usize,
    latest_payment: Option<PaymentRequest>,
    /// Current NoteFile hex (from agent on first request, from facilitator remainder after settle)
    current_note_file_hex: Option<String>,
}

type SharedMerchant = Arc<Mutex<MerchantInner>>;

async fn handle_resource(State(state): State<SharedMerchant>) -> impl IntoResponse {
    let s = state.lock().await;
    (
        StatusCode::PAYMENT_REQUIRED,
        Json(PaymentRequired {
            facilitator_url: s.facilitator_url.clone(),
            merchant_id_hex: s.merchant_id.clone(),
            amount_per_request: s.amount_per_request,
        }),
    )
        .into_response()
}

async fn handle_pay(
    State(state): State<SharedMerchant>,
    Json(payment): Json<PaymentRequest>,
) -> impl IntoResponse {
    let mut s = state.lock().await;
    s.batch_count += 1;
    let batch_count = s.batch_count;
    let settle_after = s.settle_after;

    // Store NoteFile if provided (first request or after agent learns new serial)
    if payment.note_file_hex.is_some() {
        s.current_note_file_hex = payment.note_file_hex.clone();
    }
    s.latest_payment = Some(payment);

    tracing::info!(batch_count, settle_after, "voucher received");

    if batch_count >= settle_after {
        let payment = s.latest_payment.take().unwrap();
        let facilitator_url = s.facilitator_url.clone();
        let merchant_id = s.merchant_id.clone();
        let note_file_hex = s.current_note_file_hex.clone().unwrap_or_default();
        drop(s);

        let settle_req = SettleRequest {
            note_file_hex,
            agent_sk_hex: payment.agent_sk_hex,
            serial_num: payment.serial_num,
            cumulative_amount: payment.cumulative_amount,
            merchant_id_hex: merchant_id,
        };

        let http = reqwest::Client::new();
        let resp = match http
            .post(format!("{facilitator_url}/settle"))
            .json(&settle_req)
            .timeout(Duration::from_secs(300))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, Json(PaymentResponse {
                    success: false, resource: None,
                    error: Some(format!("facilitator: {e}")),
                    new_serial_num: None, settlement_occurred: None,
                })).into_response();
            }
        };

        let settle_resp: SettleResponse = match resp.json().await {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, Json(PaymentResponse {
                    success: false, resource: None,
                    error: Some(format!("parse: {e}")),
                    new_serial_num: None, settlement_occurred: None,
                })).into_response();
            }
        };

        if settle_resp.success {
            tracing::info!(
                settled = settle_resp.settled_amount,
                remainder = settle_resp.remainder_balance,
                "settlement complete"
            );

            // Store remainder note for next batch
            let mut s = state.lock().await;
            if let Some(ref remainder) = settle_resp.remainder_note_file_hex {
                s.current_note_file_hex = Some(remainder.clone());
                tracing::info!("stored remainder note for next batch");
            }
            s.batch_count = 0; // Reset batch counter

            (StatusCode::OK, Json(PaymentResponse {
                success: true,
                resource: Some("premium content (after settlement)".into()),
                error: None,
                new_serial_num: settle_resp.new_serial_num,
                settlement_occurred: Some(true),
            })).into_response()
        } else {
            (StatusCode::PAYMENT_REQUIRED, Json(PaymentResponse {
                success: false, resource: None,
                error: settle_resp.error,
                new_serial_num: None, settlement_occurred: None,
            })).into_response()
        }
    } else {
        tracing::info!(batch_count, settle_after, "voucher accepted");
        (StatusCode::OK, Json(PaymentResponse {
            success: true,
            resource: Some(format!("premium content (request #{batch_count})")),
            error: None,
            new_serial_num: None, settlement_occurred: None,
        })).into_response()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    tracing::info!(port = args.port, facilitator = %args.facilitator_url, "starting merchant");

    let state: SharedMerchant = Arc::new(Mutex::new(MerchantInner {
        facilitator_url: args.facilitator_url,
        merchant_id: args.merchant_id,
        amount_per_request: args.amount_per_request,
        settle_after: args.settle_after,
        batch_count: 0,
        latest_payment: None,
        current_note_file_hex: None,
    }));

    let app = axum::Router::new()
        .route("/resource", get(handle_resource))
        .route("/pay", post(handle_pay))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", args.port);
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
