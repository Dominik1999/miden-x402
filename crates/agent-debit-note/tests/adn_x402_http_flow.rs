//! Full x402 HTTP flow test:
//!
//! 1. Agent   → Merchant:     GET /resource
//! 2. Merchant → Agent:        402 Payment Required (facilitator_id, amount, etc.)
//! 3. Agent   → Merchant:     POST /pay (ADN NoteFile + sig + serial_num)
//! 4. Merchant → Facilitator:  POST /consume (forwards note + sig + merchant_id + amount)
//! 5. Facilitator consumes ADN → creates P2ID to merchant
//! 6. Facilitator → Merchant:  200 OK (P2ID note bytes)
//! 7. Merchant → Agent:        200 OK (resource content)
//!
//! Three HTTP servers: merchant (:3401), facilitator (:3402).
//! Agent is an HTTP client.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test adn_x402_http_flow -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};

use miden_client::account::component::{AuthControlled, BasicFungibleFaucet, BasicWallet};
use miden_client::account::{AccountBuilder, AccountStorageMode, AccountType};
use miden_client::asset::{FungibleAsset, TokenSymbol};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::NoteType;
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::store::{NoteExportType, NoteFilter};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::{Client, Felt};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::asset::Asset;
use miden_protocol::note::*;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Hasher, Word};
use miden_standards::code_builder::CodeBuilder;
use rand::RngCore;

const ADN_MASM: &str = include_str!("../masm/agent_debit_note.masm");

// ════════════════════════════════════════════════════════════════════════
// HTTP message types (what goes over the wire as JSON)
// ════════════════════════════════════════════════════════════════════════

/// 402 response: merchant tells agent how to pay
#[derive(Serialize, Deserialize, Debug, Clone)]
struct PaymentRequired {
    facilitator_id_hex: String,
    amount: u64,
    merchant_id_hex: String,
    resource: String,
}

/// Agent's payment to merchant
#[derive(Serialize, Deserialize, Debug)]
struct PaymentRequest {
    /// hex-encoded NoteFile::NoteWithProof bytes (first request only)
    note_file_hex: Option<String>,
    /// hex-encoded note_id (second+ request, note already on-chain)
    note_id_hex: Option<String>,
    /// hex-encoded agent signing key bytes
    agent_sk_hex: String,
    /// serial number as 4 hex u64s
    serial_num: [String; 4],
    /// hex-encoded facilitator Account bytes
    facilitator_account_hex: String,
    /// keystore files as (name, hex-encoded data) pairs
    keystore_files: Vec<(String, String)>,
}

/// Merchant forwards to facilitator
#[derive(Serialize, Deserialize, Debug)]
struct ConsumeRequest {
    /// hex-encoded NoteFile bytes
    note_file_hex: String,
    /// hex-encoded agent signing key
    agent_sk_hex: String,
    /// serial number
    serial_num: [String; 4],
    /// merchant account id
    merchant_id_hex: String,
    /// debit amount
    amount: u64,
}

/// Facilitator response to merchant
#[derive(Serialize, Deserialize, Debug)]
struct ConsumeResponse {
    success: bool,
    tx_id: String,
    /// hex-encoded P2ID NoteFile bytes for merchant
    p2id_note_file_hex: Option<String>,
}

/// Merchant response to agent
#[derive(Serialize, Deserialize, Debug)]
struct PaymentResponse {
    success: bool,
    resource_content: Option<String>,
    error: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════
// Shared state
// ════════════════════════════════════════════════════════════════════════

/// Facilitator uses a channel to serialize access to the miden client
/// (which isn't Send, so can't be held across await in axum handlers)
type FacilitatorRequest = (
    ConsumeRequest,
    tokio::sync::oneshot::Sender<ConsumeResponse>,
);

#[derive(Clone)]
struct FacilitatorHandle {
    tx: tokio::sync::mpsc::Sender<FacilitatorRequest>,
}

#[derive(Clone)]
struct MerchantState {
    merchant_id: AccountId,
    facilitator_url: String,
    payment_details: PaymentRequired,
}


// ════════════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════════════

async fn build_client(
    dir: &std::path::Path,
) -> anyhow::Result<(Client<FilesystemKeyStore>, Arc<FilesystemKeyStore>)> {
    let endpoint = Endpoint::try_from("https://rpc.testnet.miden.io").unwrap();
    let rpc = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let ks_dir = dir.join("keystore");
    std::fs::create_dir_all(&ks_dir)?;
    let ks = Arc::new(FilesystemKeyStore::new(ks_dir).unwrap());
    let c = ClientBuilder::new()
        .rpc(rpc)
        .sqlite_store(dir.join("store.sqlite3"))
        .authenticator(ks.clone())
        .in_debug_mode(false.into())
        .build()
        .await?;
    Ok((c, ks))
}

fn rand_seed(client: &mut Client<FilesystemKeyStore>) -> [u8; 32] {
    let mut s = [0u8; 32];
    client.rng().fill_bytes(&mut s);
    s
}

fn parse_hex_felt(s: &str) -> u64 {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(s, 16).unwrap()
}

// ════════════════════════════════════════════════════════════════════════
// Merchant HTTP handlers
// ════════════════════════════════════════════════════════════════════════

async fn merchant_get_resource(
    State(state): State<MerchantState>,
) -> impl IntoResponse {
    eprintln!("[merchant-http] GET /resource → 402 Payment Required");
    (StatusCode::PAYMENT_REQUIRED, Json(&state.payment_details)).into_response()
}

async fn merchant_post_pay(
    State(state): State<MerchantState>,
    Json(payment): Json<PaymentRequest>,
) -> impl IntoResponse {
    let s = &state;
    eprintln!("[merchant-http] POST /pay received");

    // Forward to facilitator
    let note_file_hex = match &payment.note_file_hex {
        Some(h) => h.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(PaymentResponse {
                    success: false,
                    resource_content: None,
                    error: Some("note_file_hex required".into()),
                }),
            )
                .into_response();
        }
    };

    let consume_req = ConsumeRequest {
        note_file_hex,
        agent_sk_hex: payment.agent_sk_hex.clone(),
        serial_num: payment.serial_num.clone(),
        merchant_id_hex: s.merchant_id.to_hex(),
        amount: s.payment_details.amount,
    };

    eprintln!(
        "[merchant-http] forwarding to facilitator at {}",
        s.facilitator_url
    );

    let http_client = reqwest::Client::new();
    let resp = match http_client
        .post(format!("{}/consume", s.facilitator_url))
        .json(&consume_req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(PaymentResponse {
                    success: false,
                    resource_content: None,
                    error: Some(format!("facilitator unreachable: {e}")),
                }),
            )
                .into_response();
        }
    };

    let consume_resp: ConsumeResponse = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(PaymentResponse {
                    success: false,
                    resource_content: None,
                    error: Some(format!("facilitator bad response: {e}")),
                }),
            )
                .into_response();
        }
    };

    if consume_resp.success {
        eprintln!("[merchant-http] facilitator success, releasing resource");
        (
            StatusCode::OK,
            Json(PaymentResponse {
                success: true,
                resource_content: Some("🎉 Here is your premium content!".into()),
                error: None,
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::PAYMENT_REQUIRED,
            Json(PaymentResponse {
                success: false,
                resource_content: None,
                error: Some("facilitator failed".into()),
            }),
        )
            .into_response()
    }
}

// ════════════════════════════════════════════════════════════════════════
// Facilitator HTTP handler
// ════════════════════════════════════════════════════════════════════════

async fn facilitator_post_consume(
    State(handle): State<FacilitatorHandle>,
    Json(req): Json<ConsumeRequest>,
) -> Json<ConsumeResponse> {
    eprintln!("[facilitator-http] POST /consume received");
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if handle.tx.send((req, resp_tx)).await.is_err() {
        return Json(ConsumeResponse {
            success: false,
            tx_id: "facilitator channel closed".into(),
            p2id_note_file_hex: None,
        });
    }
    match resp_rx.await {
        Ok(resp) => Json(resp),
        Err(_) => Json(ConsumeResponse {
            success: false,
            tx_id: "facilitator dropped response".into(),
            p2id_note_file_hex: None,
        }),
    }
}

/// Background task that owns the miden client (not Send-safe, so runs on its own task)
async fn facilitator_worker(
    mut client: Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    mut rx: tokio::sync::mpsc::Receiver<FacilitatorRequest>,
) {
    while let Some((req, resp_tx)) = rx.recv().await {
        let resp = process_consume_request(&mut client, facilitator_id, req).await;
        let _ = resp_tx.send(resp);
    }
}

async fn process_consume_request(
    client: &mut Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    req: ConsumeRequest,
) -> ConsumeResponse {
    let note_file_bytes = match hex::decode(&req.note_file_hex) {
        Ok(b) => b,
        Err(_) => {
            return ConsumeResponse {
                success: false,
                tx_id: "bad hex".into(),
                p2id_note_file_hex: None,
            };
        }
    };
    let agent_sk_bytes = hex::decode(&req.agent_sk_hex).unwrap();
    let merchant_id = AccountId::from_hex(&req.merchant_id_hex).unwrap();
    let serial_num: Word = [
        Felt::new(parse_hex_felt(&req.serial_num[0])),
        Felt::new(parse_hex_felt(&req.serial_num[1])),
        Felt::new(parse_hex_felt(&req.serial_num[2])),
        Felt::new(parse_hex_felt(&req.serial_num[3])),
    ]
    .into();

    let note_file = NoteFile::read_from_bytes(&note_file_bytes).unwrap();
    let (note, note_file_for_import) = match &note_file {
        NoteFile::NoteWithProof(n, _) => (n.clone(), note_file),
        _ => {
            return ConsumeResponse {
                success: false,
                tx_id: "expected NoteWithProof".into(),
                p2id_note_file_hex: None,
            };
        }
    };
    let note_id = note.id();
    eprintln!("[facilitator] note: {note_id}");

    client.sync_state().await.unwrap();
    client.import_notes(&[note_file_for_import]).await.unwrap();

    for attempt in 0..60 {
        client.sync_state().await.unwrap();
        if let Ok(Some(record)) = client.get_input_note(note_id).await {
            if record.is_authenticated() {
                eprintln!("[facilitator] authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 {
            return ConsumeResponse {
                success: false,
                tx_id: "not authenticated".into(),
                p2id_note_file_hex: None,
            };
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let agent_sk = AuthSecretKey::read_from_bytes(&agent_sk_bytes).unwrap();
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();

    let note_args: Word = [
        merchant_id.suffix(),
        merchant_id.prefix().as_felt(),
        Felt::new(req.amount),
        Felt::ZERO,
    ]
    .into();

    let message: Word = Hasher::merge(&[serial_num.into(), note_args.into()]).into();
    let sig = agent_sk.sign(message);
    let prepared: Vec<Felt> = sig.to_prepared_signature(message);
    let sig_key: Word = Hasher::merge(&[agent_pk.into(), message.into()]).into();

    eprintln!(
        "[facilitator] consuming ADN (debit={} to merchant {})",
        req.amount, merchant_id.to_hex()
    );

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, Some(note_args))])
        .extend_advice_map([(sig_key, prepared.as_slice())])
        .build()
        .unwrap();

    match client
        .submit_new_transaction(facilitator_id, consume_req)
        .await
    {
        Ok(tx_id) => {
            eprintln!("[facilitator] SUCCESS: tx={tx_id}");

            // Export P2ID output note
            let mut p2id_hex = None;
            for _attempt in 0..60 {
                client.sync_state().await.unwrap();
                let output_notes = client.get_output_notes(NoteFilter::All).await.unwrap();
                for record in output_notes {
                    if record.inclusion_proof().is_some() {
                        if let Ok(nf) =
                            record.into_note_file(&NoteExportType::NoteWithProof)
                        {
                            p2id_hex = Some(hex::encode(nf.to_bytes()));
                            break;
                        }
                    }
                }
                if p2id_hex.is_some() { break; }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }

            ConsumeResponse {
                success: true,
                tx_id: format!("{tx_id}"),
                p2id_note_file_hex: p2id_hex,
            }
        }
        Err(e) => {
            eprintln!("[facilitator] FAILED: {e}");
            ConsumeResponse {
                success: false,
                tx_id: format!("{e:?}"),
                p2id_note_file_hex: None,
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════
// Main test
// ════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires testnet access"]
async fn adn_x402_http_flow() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // ══ SETUP: Create all accounts ══
    eprintln!("\n══ SETUP ══");

    let setup_tmp = tempfile::tempdir()?;
    let (mut setup_client, setup_ks) = build_client(setup_tmp.path()).await?;
    setup_client.sync_state().await?;

    // Faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let faucet = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            faucet_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(
            BasicFungibleFaucet::new(
                TokenSymbol::new("XHTTP").unwrap(),
                6,
                Felt::new(1_000_000_000),
            )
            .unwrap(),
        )
        .with_component(AuthControlled::allow_all())
        .build()
        .unwrap();
    let faucet_id = faucet.id();
    setup_client.add_account(&faucet, false).await?;
    setup_ks.add_key(&faucet_key, faucet_id).await.unwrap();
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Agent wallet
    let agent_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let agent_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            agent_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    let agent_id = agent_account.id();
    setup_client.add_account(&agent_account, false).await?;
    setup_ks.add_key(&agent_key, agent_id).await.unwrap();
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Facilitator wallet
    let facilitator_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let facilitator_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            facilitator_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    let facilitator_id = facilitator_account.id();
    setup_ks
        .add_key(&facilitator_key, facilitator_id)
        .await
        .unwrap();
    let fac_account_bytes = facilitator_account.to_bytes();

    // Merchant wallet
    let merchant_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let merchant_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            merchant_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    let merchant_id = merchant_account.id();
    setup_ks.add_key(&merchant_key, merchant_id).await.unwrap();

    eprintln!("[setup] agent:       {}", agent_id.to_hex());
    eprintln!("[setup] facilitator: {}", facilitator_id.to_hex());
    eprintln!("[setup] merchant:    {}", merchant_id.to_hex());

    // Mint to agent
    let mint_asset = FungibleAsset::new(faucet_id, 10_000).unwrap();
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, setup_client.rng())
        .unwrap();
    setup_client
        .submit_new_transaction(faucet_id, mint_req)
        .await?;
    for attempt in 0..60 {
        setup_client.sync_state().await?;
        let consumable = setup_client.get_consumable_notes(Some(agent_id)).await?;
        if !consumable.is_empty() {
            eprintln!("[setup] mint consumable (attempt {attempt})");
            let notes: Vec<_> = consumable
                .into_iter()
                .map(|(n, _)| n.try_into())
                .collect::<Result<_, _>>()?;
            let req = TransactionRequestBuilder::new()
                .build_consume_notes(notes)
                .unwrap();
            setup_client.submit_new_transaction(agent_id, req).await?;
            break;
        }
        if attempt == 59 {
            anyhow::bail!("mint timeout");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[setup] agent funded");

    // Create ADN note
    let agent_signing_key =
        AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let agent_pk: Word = agent_signing_key.public_key().to_commitment().into();
    let note_script = CodeBuilder::default().compile_note_script(ADN_MASM)?;

    let adn_balance = 1_000u64;
    let asset = FungibleAsset::new(faucet_id, adn_balance).unwrap();
    let storage = NoteStorage::new(vec![
        agent_pk[0], agent_pk[1], agent_pk[2], agent_pk[3],
        agent_id.suffix(), agent_id.prefix().as_felt(),
        Felt::new(10_000_000),
    ])?;

    let mut serial_bytes = [0u8; 32];
    setup_client.rng().fill_bytes(&mut serial_bytes);
    let serial_num: Word = [
        Felt::new(u64::from_le_bytes(serial_bytes[0..8].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[8..16].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[16..24].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[24..32].try_into().unwrap())),
    ].into();

    let tag = NoteTag::with_account_target(facilitator_id);
    let metadata = NoteMetadata::new(agent_id, NoteType::Private).with_tag(tag);
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(serial_num, note_script, storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();

    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build()
        .unwrap();
    let tx_id = setup_client
        .submit_new_transaction(agent_id, create_req)
        .await?;
    eprintln!("[setup] ADN note: {note_id}, tx={tx_id}");

    // Wait for inclusion proof
    let mut note_file_bytes: Vec<u8> = Vec::new();
    for attempt in 0..60 {
        setup_client.sync_state().await?;
        let output_notes = setup_client
            .get_output_notes(NoteFilter::List(vec![note_id]))
            .await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let nf = record
                    .into_note_file(&NoteExportType::NoteWithProof)
                    .unwrap();
                note_file_bytes = nf.to_bytes();
                eprintln!("[setup] NoteWithProof: {} bytes (attempt {attempt})", note_file_bytes.len());
                break;
            }
        }
        if attempt == 59 { anyhow::bail!("inclusion proof timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Collect keystore files
    let src_ks = setup_tmp.path().join("keystore");
    let mut keystore_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        keystore_files.push((
            entry.file_name().to_string_lossy().to_string(),
            std::fs::read(entry.path())?,
        ));
    }

    let agent_sk_bytes = agent_signing_key.to_bytes();
    let serial_felts: [Felt; 4] = serial_num.into();

    drop(setup_client);
    drop(setup_ks);

    // ══ START FACILITATOR SERVER ══
    eprintln!("\n══ STARTING SERVERS ══");

    let fac_tmp = tempfile::tempdir()?;
    let fac_ks_dir = fac_tmp.path().join("keystore");
    std::fs::create_dir_all(&fac_ks_dir)?;
    for (name, data) in &keystore_files {
        std::fs::write(fac_ks_dir.join(name), data)?;
    }
    let (mut fac_client, _) = build_client(fac_tmp.path()).await?;
    let fac_account = Account::read_from_bytes(&fac_account_bytes)?;
    fac_client.add_account(&fac_account, false).await?;
    fac_client.sync_state().await?;

    let (fac_tx, fac_rx) = tokio::sync::mpsc::channel::<FacilitatorRequest>(1);

    // Spawn the facilitator worker on a LocalSet so it can use !Send types
    let fac_handle_for_worker = facilitator_id;
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(facilitator_worker(fac_client, fac_handle_for_worker, fac_rx));
    });

    let fac_app = axum::Router::new()
        .route("/consume", post(facilitator_post_consume))
        .with_state(FacilitatorHandle { tx: fac_tx });

    let fac_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fac_addr = fac_listener.local_addr()?;
    let facilitator_url = format!("http://{fac_addr}");
    eprintln!("[facilitator] listening on {facilitator_url}");

    tokio::spawn(async move {
        axum::serve(fac_listener, fac_app).await.unwrap();
    });

    // ══ START MERCHANT SERVER ══
    let merchant_state = MerchantState {
        merchant_id,
        facilitator_url: facilitator_url.clone(),
        payment_details: PaymentRequired {
            facilitator_id_hex: facilitator_id.to_hex(),
            amount: 100,
            merchant_id_hex: merchant_id.to_hex(),
            resource: "premium-content".into(),
        },
    };

    let merchant_app = axum::Router::new()
        .route("/resource", get(merchant_get_resource))
        .route("/pay", post(merchant_post_pay))
        .with_state(merchant_state);

    let merchant_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let merchant_addr = merchant_listener.local_addr()?;
    let merchant_url = format!("http://{merchant_addr}");
    eprintln!("[merchant] listening on {merchant_url}");

    tokio::spawn(async move {
        axum::serve(merchant_listener, merchant_app).await.unwrap();
    });

    // ══ AGENT: x402 FLOW ══
    eprintln!("\n══ AGENT: x402 FLOW ══");

    let http = reqwest::Client::new();

    // Step 1: GET /resource → 402
    eprintln!("[agent] step 1: GET /resource");
    let resp = http.get(format!("{merchant_url}/resource")).send().await?;
    assert_eq!(resp.status().as_u16(), 402, "expected 402");
    let payment_required: PaymentRequired = resp.json().await?;
    eprintln!(
        "[agent] step 2: got 402 — amount={}, merchant={}, facilitator={}",
        payment_required.amount,
        payment_required.merchant_id_hex,
        payment_required.facilitator_id_hex
    );

    // Step 3: POST /pay with ADN note + sig
    eprintln!("[agent] step 3: POST /pay");
    let payment = PaymentRequest {
        note_file_hex: Some(hex::encode(&note_file_bytes)),
        note_id_hex: None,
        agent_sk_hex: hex::encode(&agent_sk_bytes),
        serial_num: [
            format!("0x{:x}", serial_felts[0].as_canonical_u64()),
            format!("0x{:x}", serial_felts[1].as_canonical_u64()),
            format!("0x{:x}", serial_felts[2].as_canonical_u64()),
            format!("0x{:x}", serial_felts[3].as_canonical_u64()),
        ],
        facilitator_account_hex: hex::encode(&fac_account_bytes),
        keystore_files: keystore_files
            .iter()
            .map(|(n, d)| (n.clone(), hex::encode(d)))
            .collect(),
    };

    let resp = http
        .post(format!("{merchant_url}/pay"))
        .json(&payment)
        .timeout(Duration::from_secs(120))
        .send()
        .await?;

    let status = resp.status();
    let payment_resp: PaymentResponse = resp.json().await?;

    if payment_resp.success {
        eprintln!("[agent] ════════════════════════════════════════════════");
        eprintln!(
            "[agent] SUCCESS: resource received: {:?}",
            payment_resp.resource_content
        );
        eprintln!("[agent] ════════════════════════════════════════════════");
    } else {
        eprintln!("[agent] FAILED: {:?}", payment_resp.error);
        anyhow::bail!(
            "x402 flow failed: status={}, error={:?}",
            status,
            payment_resp.error
        );
    }

    eprintln!("\n══ x402 HTTP FLOW COMPLETE ══");
    Ok(())
}
