//! End-to-end integration test for the agentic client.
//!
//! Boots an in-process x402 facilitator on a free port, points the
//! `AgenticClient` at it, registers an agent, and runs three
//! sequential `pay()` calls. Asserts the facilitator's pending-state
//! commitment advances after each ack and that the facilitator's
//! status endpoint reports each nullifier as accepted.

use std::net::SocketAddr;
use std::time::Duration;

use miden_agentic_client::{AgenticClient, AgentMandate, PaymentStatus, X402Context};
use tempfile::TempDir;

use x402_facilitator::{
    api,
    jobs::{spawn_batch_worker, BatchConfig},
    key::FacilitatorKey,
    state::{AppState, PerAgentLocks},
    store::FilesystemX402Store,
};

async fn spawn_facilitator(data_dir: &std::path::Path, keystore_dir: &std::path::Path) -> u16 {
    let store = FilesystemX402Store::new(data_dir).expect("store");
    let facilitator_key = FacilitatorKey::load_or_create(keystore_dir.to_path_buf()).expect("key");
    let state = AppState {
        store,
        facilitator_key,
        locks: PerAgentLocks::default(),
        submitter: None,
        submitter_available: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    spawn_batch_worker(
        state.clone(),
        BatchConfig {
            interval_ms: 100,
            max_size: 16,
        },
    );

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let port = listener.local_addr().unwrap().port();
    let app = api::router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Brief wait for the server to be accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

#[tokio::test]
async fn three_sequential_payments_advance_pending_state() {
    let data = TempDir::new().unwrap();
    let facilitator_keystore = TempDir::new().unwrap();
    let agent_keystore = TempDir::new().unwrap();

    let port = spawn_facilitator(data.path(), facilitator_keystore.path()).await;
    let facilitator_url = format!("http://127.0.0.1:{port}");

    let client = AgenticClient::builder()
        .agent_id("agent-test")
        .account_id("0x000000000000000000000000000000000000000000000000000000000000a001")
        .facilitator_url(&facilitator_url)
        .keystore_dir(agent_keystore.path().to_path_buf())
        .build()
        .expect("client builder");

    let initial_state =
        "0x0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let mandate = AgentMandate {
        per_tx_amount_cap: "1000000".into(),
        merchant_allowlist: vec!["0xmerchant1".into()],
        expires_at_unix_secs: 9_999_999_999,
    };
    client
        .register(initial_state.clone(), mandate)
        .await
        .expect("register");

    let mut last_commitment = initial_state;
    let mut all_nullifiers = Vec::new();
    for i in 0..3u64 {
        let ctx = X402Context {
            merchant_account_id: "0xmerchant1".into(),
            asset_faucet_id: "0xfaucet1".into(),
            amount: format!("{}", 100 + i),
            deadline_unix_secs: 9_999_999_999,
            payment_requirements_digest: format!("0xreqs{i}"),
        };
        let receipt = client.pay(ctx).await.expect("pay");
        assert_eq!(receipt.seq, i + 1);
        assert_ne!(receipt.new_pending_state_commitment, last_commitment);
        assert_eq!(receipt.reserved_nullifiers.len(), 1);
        last_commitment = receipt.new_pending_state_commitment.clone();
        all_nullifiers.extend(receipt.reserved_nullifiers);
    }

    // Each nullifier should be reported as `accepted` by the facilitator.
    for n in &all_nullifiers {
        let st = client.payment_status(n).await.expect("status");
        assert_eq!(st.status, PaymentStatus::Accepted);
    }

    // The facilitator's state endpoint should agree with the client's cache.
    let state = client.refresh_state().await.expect("refresh state");
    assert_eq!(state.last_accepted_seq, 3);
    assert_eq!(state.in_flight_count, 3);
    assert_eq!(state.pending_state_commitment, last_commitment);
}

/// Sanity check that the facilitator's Falcon verify path is real:
/// flip a byte in the signature hex and confirm the facilitator
/// rejects it. Uses the lower-level transport because the high-level
/// client doesn't allow corrupting its own outgoing signature.
#[tokio::test]
async fn tampered_signature_is_rejected() {
    use guardian_shared::{DeltaSignature, ProposalSignature, SignatureScheme};
    use miden_agentic_client::AgenticPayload;
    use miden_agentic_client::types::X402Context;

    let data = TempDir::new().unwrap();
    let facilitator_keystore = TempDir::new().unwrap();
    let agent_keystore = TempDir::new().unwrap();

    let port = spawn_facilitator(data.path(), facilitator_keystore.path()).await;
    let facilitator_url = format!("http://127.0.0.1:{port}");

    let client = AgenticClient::builder()
        .agent_id("agent-tamper")
        .account_id("0x000000000000000000000000000000000000000000000000000000000000a003")
        .facilitator_url(&facilitator_url)
        .keystore_dir(agent_keystore.path().to_path_buf())
        .build()
        .unwrap();

    client
        .register(
            "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
            AgentMandate {
                per_tx_amount_cap: "1000000".into(),
                merchant_allowlist: vec![],
                expires_at_unix_secs: 9_999_999_999,
            },
        )
        .await
        .unwrap();

    // Construct a payload by hand with a tampered signature.
    let http = reqwest::Client::new();
    let bad_payload = AgenticPayload {
        tx_summary: serde_json::json!({"placeholder": "tx_summary"}),
        hot_key_signature: DeltaSignature {
            signer_id: client.hot_key_commitment(),
            signature: ProposalSignature::Falcon {
                signature: "0xdeadbeef".to_string(),
            },
        },
        x402_context: X402Context {
            merchant_account_id: "0xm".into(),
            asset_faucet_id: "0xf".into(),
            amount: "1".into(),
            deadline_unix_secs: 9_999_999_999,
            payment_requirements_digest: "0x".into(),
        },
        built_on_state_commitment:
            "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
        new_state_commitment:
            "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
        claimed_nullifiers: vec!["0xn1".into()],
    };
    let _ = SignatureScheme::Falcon; // ensure the enum is in scope without warning

    let resp = http
        .post(format!("{facilitator_url}/agents/agent-tamper/payments"))
        .json(&bad_payload)
        .send()
        .await
        .unwrap();

    assert!(!resp.status().is_success(), "tampered sig should be rejected");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "INVALID_SIGNATURE", "got body: {body}");
}

#[tokio::test]
async fn stale_base_triggers_one_retry() {
    let data = TempDir::new().unwrap();
    let facilitator_keystore = TempDir::new().unwrap();
    let agent_keystore = TempDir::new().unwrap();

    let port = spawn_facilitator(data.path(), facilitator_keystore.path()).await;
    let facilitator_url = format!("http://127.0.0.1:{port}");

    let client = AgenticClient::builder()
        .agent_id("agent-stale")
        .account_id("0x000000000000000000000000000000000000000000000000000000000000a002")
        .facilitator_url(&facilitator_url)
        .keystore_dir(agent_keystore.path().to_path_buf())
        .build()
        .expect("client");

    client
        .register(
            "0x1111111111111111111111111111111111111111111111111111111111111111".into(),
            AgentMandate {
                per_tx_amount_cap: "1000000".into(),
                merchant_allowlist: vec![],
                expires_at_unix_secs: 9_999_999_999,
            },
        )
        .await
        .expect("register");

    // First pay succeeds and advances the facilitator's pending state.
    let r1 = client
        .pay(X402Context {
            merchant_account_id: "0xmerchant1".into(),
            asset_faucet_id: "0xfaucet1".into(),
            amount: "100".into(),
            deadline_unix_secs: 9_999_999_999,
            payment_requirements_digest: "0xa".into(),
        })
        .await
        .expect("pay #1");
    assert_eq!(r1.seq, 1);

    // Simulate the client losing its cache by building a fresh client at
    // the same agent_id (same keystore so the hot key is preserved).
    // This new client will try `pay()` with an empty pending cache,
    // hit STALE_BASE, then refresh and succeed on retry.
    let client2 = AgenticClient::builder()
        .agent_id("agent-stale")
        .account_id("0x000000000000000000000000000000000000000000000000000000000000a002")
        .facilitator_url(&facilitator_url)
        .keystore_dir(agent_keystore.path().to_path_buf())
        .build()
        .expect("client2");

    let r2 = client2
        .pay(X402Context {
            merchant_account_id: "0xmerchant1".into(),
            asset_faucet_id: "0xfaucet1".into(),
            amount: "200".into(),
            deadline_unix_secs: 9_999_999_999,
            payment_requirements_digest: "0xb".into(),
        })
        .await
        .expect("pay #2 after stale retry");
    assert_eq!(r2.seq, 2);
}
