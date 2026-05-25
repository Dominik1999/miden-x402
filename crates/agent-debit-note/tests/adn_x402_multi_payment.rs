//! x402 multi-payment test: 5 sequential payments from one ADN note.
//!
//! Payment 1: agent sends full NoteFile + sig
//! Payments 2-5: agent sends only new sig (facilitator already has the remainder note)
//!
//! After each payment the facilitator:
//!   - finds the remainder ADN output note
//!   - waits for inclusion proof
//!   - re-imports it as input for the next payment
//!
//! The agent tracks the serial_num (incremented by 1 each time on element[0]).
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test adn_x402_multi_payment -- --ignored --nocapture

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
// HTTP message types
// ════════════════════════════════════════════════════════════════════════

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PaymentRequired {
    facilitator_id_hex: String,
    amount: u64,
    merchant_id_hex: String,
}

/// First payment: note_file_hex is Some. Subsequent: None (facilitator has remainder).
#[derive(Serialize, Deserialize, Debug)]
struct PaymentRequest {
    note_file_hex: Option<String>,
    agent_sk_hex: String,
    serial_num: [String; 4],
    amount: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct ConsumeRequest {
    note_file_hex: Option<String>,
    agent_sk_hex: String,
    serial_num: [String; 4],
    merchant_id_hex: String,
    amount: u64,
}

#[derive(Serialize, Deserialize, Debug)]
struct ConsumeResponse {
    success: bool,
    tx_id: String,
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct PaymentResponse {
    success: bool,
    resource_content: Option<String>,
    error: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════
// Facilitator worker (owns the miden client, not Send)
// ════════════════════════════════════════════════════════════════════════

type FacilitatorMsg = (ConsumeRequest, tokio::sync::oneshot::Sender<ConsumeResponse>);

#[derive(Clone)]
struct FacilitatorHandle {
    tx: tokio::sync::mpsc::Sender<FacilitatorMsg>,
}

async fn facilitator_worker(
    mut client: Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    mut rx: tokio::sync::mpsc::Receiver<FacilitatorMsg>,
) {
    let mut current_note: Option<Note> = None;
    let mut used_note_ids: Vec<NoteId> = Vec::new();

    while let Some((req, resp_tx)) = rx.recv().await {
        let resp =
            process_consume(&mut client, facilitator_id, &req, &mut current_note, &mut used_note_ids).await;
        let _ = resp_tx.send(resp);
    }
}

async fn process_consume(
    client: &mut Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    req: &ConsumeRequest,
    current_note: &mut Option<Note>,
    used_note_ids: &mut Vec<NoteId>,
) -> ConsumeResponse {
    let merchant_id = AccountId::from_hex(&req.merchant_id_hex).unwrap();
    let serial_num: Word = [
        Felt::new(parse_hex_felt(&req.serial_num[0])),
        Felt::new(parse_hex_felt(&req.serial_num[1])),
        Felt::new(parse_hex_felt(&req.serial_num[2])),
        Felt::new(parse_hex_felt(&req.serial_num[3])),
    ]
    .into();

    // Get the note to consume
    let note = if let Some(hex) = &req.note_file_hex {
        // First payment: import the NoteFile from agent
        let note_file_bytes = hex::decode(hex).unwrap();
        let note_file = NoteFile::read_from_bytes(&note_file_bytes).unwrap();
        let (note, note_file_for_import) = match &note_file {
            NoteFile::NoteWithProof(n, _) => (n.clone(), note_file),
            _ => {
                return ConsumeResponse {
                    success: false,
                    tx_id: "expected NoteWithProof".into(),
                    error: Some("bad note file".into()),
                };
            }
        };
        client.sync_state().await.unwrap();
        client.import_notes(&[note_file_for_import]).await.unwrap();
        eprintln!("[facilitator] imported NoteFile, note_id={}", note.id());
        note
    } else if let Some(note) = current_note.take() {
        // Subsequent payment: use the remainder note from previous consume
        eprintln!("[facilitator] using remainder note: {}", note.id());
        note
    } else {
        return ConsumeResponse {
            success: false,
            tx_id: String::new(),
            error: Some("no note available (no NoteFile and no remainder)".into()),
        };
    };

    let note_id = note.id();
    used_note_ids.push(note_id);

    // Wait for note to be authenticated
    for attempt in 0..60 {
        client.sync_state().await.unwrap();
        if let Ok(Some(record)) = client.get_input_note(note_id).await {
            if record.is_authenticated() {
                eprintln!("[facilitator] note authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 {
            return ConsumeResponse {
                success: false,
                tx_id: "not authenticated".into(),
                error: Some("timeout".into()),
            };
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Build agent signature
    let agent_sk_bytes = hex::decode(&req.agent_sk_hex).unwrap();
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
        "[facilitator] consuming (debit={}, merchant={})",
        req.amount,
        merchant_id.to_hex()
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

            // Find the remainder ADN note in output notes
            // Wait for inclusion proof, then export and re-import as input
            eprintln!("[facilitator] looking for remainder note...");
            let mut found_remainder = false;
            for attempt in 0..60 {
                client.sync_state().await.unwrap();
                let output_notes = client.get_output_notes(NoteFilter::All).await.unwrap();

                // Find new output notes with inclusion proofs (skip already-used ones)
                let candidates: Vec<_> = output_notes
                    .into_iter()
                    .filter(|r| r.inclusion_proof().is_some() && !used_note_ids.contains(&r.id()))
                    .collect();

                for record in candidates {
                    let rid = record.id();
                    if let Ok(nf) = record.into_note_file(&NoteExportType::NoteWithProof) {
                        let nf_bytes = nf.to_bytes();
                        // Re-deserialize to check if it's an ADN (7 storage items)
                        if let Ok(nf2) = NoteFile::read_from_bytes(&nf_bytes) {
                            if let NoteFile::NoteWithProof(n, _) = &nf2 {
                                if n.recipient().storage().num_items() == 7 {
                                    let remainder_note = n.clone();
                                    eprintln!(
                                        "[facilitator] found remainder: {} ({} assets)",
                                        rid,
                                        remainder_note.assets().num_assets()
                                    );
                                    client.import_notes(&[nf2]).await.unwrap();
                                    *current_note = Some(remainder_note);
                                    found_remainder = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if found_remainder {
                    break;
                }
                if attempt == 59 {
                    eprintln!("[facilitator] WARNING: remainder note not found");
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }

            ConsumeResponse {
                success: true,
                tx_id: format!("{tx_id}"),
                error: None,
            }
        }
        Err(e) => {
            eprintln!("[facilitator] FAILED: {e}");
            ConsumeResponse {
                success: false,
                tx_id: format!("{e:?}"),
                error: Some(format!("{e}")),
            }
        }
    }
}

fn parse_hex_felt(s: &str) -> u64 {
    let s = s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(s, 16).unwrap()
}

// ════════════════════════════════════════════════════════════════════════
// Merchant HTTP handlers
// ════════════════════════════════════════════════════════════════════════

#[derive(Clone)]
struct MerchantState {
    facilitator_url: String,
    payment_details: PaymentRequired,
}

async fn merchant_get_resource(
    State(state): State<MerchantState>,
) -> impl IntoResponse {
    eprintln!("[merchant] GET /resource → 402");
    (StatusCode::PAYMENT_REQUIRED, Json(&state.payment_details)).into_response()
}

async fn merchant_post_pay(
    State(state): State<MerchantState>,
    Json(payment): Json<PaymentRequest>,
) -> impl IntoResponse {
    eprintln!("[merchant] POST /pay");

    let consume_req = ConsumeRequest {
        note_file_hex: payment.note_file_hex,
        agent_sk_hex: payment.agent_sk_hex,
        serial_num: payment.serial_num,
        merchant_id_hex: state.payment_details.merchant_id_hex.clone(),
        amount: payment.amount,
    };

    let http = reqwest::Client::new();
    let resp = match http
        .post(format!("{}/consume", state.facilitator_url))
        .json(&consume_req)
        .timeout(Duration::from_secs(300))
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
                    error: Some(format!("facilitator error: {e}")),
                }),
            )
                .into_response();
        }
    };

    let consume_resp: ConsumeResponse = resp.json().await.unwrap();
    if consume_resp.success {
        eprintln!("[merchant] payment OK, releasing resource");
        (
            StatusCode::OK,
            Json(PaymentResponse {
                success: true,
                resource_content: Some("premium content".into()),
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
                error: consume_resp.error,
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
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if handle.tx.send((req, resp_tx)).await.is_err() {
        return Json(ConsumeResponse {
            success: false,
            tx_id: "channel closed".into(),
            error: Some("facilitator down".into()),
        });
    }
    Json(resp_rx.await.unwrap_or(ConsumeResponse {
        success: false,
        tx_id: String::new(),
        error: Some("facilitator dropped".into()),
    }))
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

// ════════════════════════════════════════════════════════════════════════
// Main test
// ════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires testnet access"]
async fn adn_x402_multi_payment() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    const NUM_PAYMENTS: usize = 5;
    const DEBIT_PER_PAYMENT: u64 = 100;

    // ══ SETUP ══
    eprintln!("\n══ SETUP ══");

    let setup_tmp = tempfile::tempdir()?;
    let (mut setup_client, setup_ks) = build_client(setup_tmp.path()).await?;
    setup_client.sync_state().await?;

    // Faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let faucet = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(faucet_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicFungibleFaucet::new(TokenSymbol::new("XMUL").unwrap(), 6, Felt::new(1_000_000_000)).unwrap())
        .with_component(AuthControlled::allow_all())
        .build().unwrap();
    let faucet_id = faucet.id();
    setup_client.add_account(&faucet, false).await?;
    setup_ks.add_key(&faucet_key, faucet_id).await.unwrap();
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Agent
    let agent_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let agent_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(agent_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let agent_id = agent_account.id();
    setup_client.add_account(&agent_account, false).await?;
    setup_ks.add_key(&agent_key, agent_id).await.unwrap();
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Facilitator
    let facilitator_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let facilitator_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(facilitator_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let facilitator_id = facilitator_account.id();
    setup_ks.add_key(&facilitator_key, facilitator_id).await.unwrap();
    let fac_account_bytes = facilitator_account.to_bytes();

    // Merchant
    let merchant_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let merchant_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(merchant_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let merchant_id = merchant_account.id();
    setup_ks.add_key(&merchant_key, merchant_id).await.unwrap();

    eprintln!("[setup] agent:       {}", agent_id.to_hex());
    eprintln!("[setup] facilitator: {}", facilitator_id.to_hex());
    eprintln!("[setup] merchant:    {}", merchant_id.to_hex());

    // Mint
    let adn_balance = (NUM_PAYMENTS as u64) * DEBIT_PER_PAYMENT + 500; // extra buffer
    let mint_asset = FungibleAsset::new(faucet_id, adn_balance + 1000).unwrap();
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, setup_client.rng())
        .unwrap();
    setup_client.submit_new_transaction(faucet_id, mint_req).await?;
    for attempt in 0..60 {
        setup_client.sync_state().await?;
        let consumable = setup_client.get_consumable_notes(Some(agent_id)).await?;
        if !consumable.is_empty() {
            eprintln!("[setup] mint consumable (attempt {attempt})");
            let notes: Vec<_> = consumable.into_iter().map(|(n, _)| n.try_into()).collect::<Result<_, _>>()?;
            let req = TransactionRequestBuilder::new().build_consume_notes(notes).unwrap();
            setup_client.submit_new_transaction(agent_id, req).await?;
            break;
        }
        if attempt == 59 { anyhow::bail!("mint timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[setup] agent funded with {}", adn_balance + 1000);

    // Create ADN note
    let agent_signing_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let agent_pk: Word = agent_signing_key.public_key().to_commitment().into();
    let note_script = CodeBuilder::default().compile_note_script(ADN_MASM)?;

    let asset = FungibleAsset::new(faucet_id, adn_balance).unwrap();
    let storage = NoteStorage::new(vec![
        agent_pk[0], agent_pk[1], agent_pk[2], agent_pk[3],
        agent_id.suffix(), agent_id.prefix().as_felt(),
        Felt::new(10_000_000),
    ])?;

    let mut serial_bytes = [0u8; 32];
    setup_client.rng().fill_bytes(&mut serial_bytes);
    let initial_serial: Word = [
        Felt::new(u64::from_le_bytes(serial_bytes[0..8].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[8..16].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[16..24].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[24..32].try_into().unwrap())),
    ].into();

    let tag = NoteTag::with_account_target(facilitator_id);
    let metadata = NoteMetadata::new(agent_id, NoteType::Private).with_tag(tag);
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(initial_serial, note_script, storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();

    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build().unwrap();
    setup_client.submit_new_transaction(agent_id, create_req).await?;
    eprintln!("[setup] ADN note: {note_id} (balance={adn_balance})");

    // Wait for inclusion proof
    let mut note_file_bytes: Vec<u8> = Vec::new();
    for attempt in 0..60 {
        setup_client.sync_state().await?;
        let output_notes = setup_client.get_output_notes(NoteFilter::List(vec![note_id])).await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let nf = record.into_note_file(&NoteExportType::NoteWithProof).unwrap();
                note_file_bytes = nf.to_bytes();
                eprintln!("[setup] NoteWithProof: {} bytes (attempt {attempt})", note_file_bytes.len());
                break;
            }
        }
        if attempt == 59 { anyhow::bail!("inclusion proof timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let agent_sk_hex = hex::encode(agent_signing_key.to_bytes());

    // Collect keystore
    let src_ks = setup_tmp.path().join("keystore");
    let mut keystore_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        keystore_files.push((entry.file_name().to_string_lossy().to_string(), std::fs::read(entry.path())?));
    }

    drop(setup_client);
    drop(setup_ks);

    // ══ START SERVERS ══
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

    let (fac_tx, fac_rx) = tokio::sync::mpsc::channel::<FacilitatorMsg>(1);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(facilitator_worker(fac_client, facilitator_id, fac_rx));
    });

    let fac_app = axum::Router::new()
        .route("/consume", post(facilitator_post_consume))
        .with_state(FacilitatorHandle { tx: fac_tx });
    let fac_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fac_addr = fac_listener.local_addr()?;
    let facilitator_url = format!("http://{fac_addr}");
    eprintln!("[facilitator] listening on {facilitator_url}");
    tokio::spawn(async move { axum::serve(fac_listener, fac_app).await.unwrap(); });

    let merchant_app = axum::Router::new()
        .route("/resource", get(merchant_get_resource))
        .route("/pay", post(merchant_post_pay))
        .with_state(MerchantState {
            facilitator_url,
            payment_details: PaymentRequired {
                facilitator_id_hex: facilitator_id.to_hex(),
                amount: DEBIT_PER_PAYMENT,
                merchant_id_hex: merchant_id.to_hex(),
            },
        });
    let merchant_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let merchant_addr = merchant_listener.local_addr()?;
    let merchant_url = format!("http://{merchant_addr}");
    eprintln!("[merchant] listening on {merchant_url}");
    tokio::spawn(async move { axum::serve(merchant_listener, merchant_app).await.unwrap(); });

    // ══ AGENT: 5 PAYMENTS ══
    let http = reqwest::Client::new();
    let initial_serial_felts: [Felt; 4] = initial_serial.into();

    // Agent tracks the serial — element[0] increments by 1 each payment
    let mut serial_counter = initial_serial_felts[0].as_canonical_u64();
    let s1 = initial_serial_felts[1].as_canonical_u64();
    let s2 = initial_serial_felts[2].as_canonical_u64();
    let s3 = initial_serial_felts[3].as_canonical_u64();

    for payment_num in 1..=NUM_PAYMENTS {
        eprintln!("\n══ PAYMENT {payment_num}/{NUM_PAYMENTS} ══");

        // Step 1: GET /resource → 402
        let resp = http.get(format!("{merchant_url}/resource")).send().await?;
        assert_eq!(resp.status().as_u16(), 402);
        let _pr: PaymentRequired = resp.json().await?;

        // Step 2: POST /pay
        let current_serial = [
            format!("0x{:x}", serial_counter),
            format!("0x{:x}", s1),
            format!("0x{:x}", s2),
            format!("0x{:x}", s3),
        ];

        let payment = PaymentRequest {
            // First payment: send NoteFile. Subsequent: None.
            note_file_hex: if payment_num == 1 {
                Some(hex::encode(&note_file_bytes))
            } else {
                None
            },
            agent_sk_hex: agent_sk_hex.clone(),
            serial_num: current_serial,
            amount: DEBIT_PER_PAYMENT,
        };

        eprintln!(
            "[agent] POST /pay #{payment_num} (note_file={}, serial[0]=0x{:x})",
            if payment_num == 1 { "YES" } else { "no" },
            serial_counter
        );

        let resp = http
            .post(format!("{merchant_url}/pay"))
            .json(&payment)
            .timeout(Duration::from_secs(300))
            .send()
            .await?;

        let payment_resp: PaymentResponse = resp.json().await?;

        if payment_resp.success {
            eprintln!(
                "[agent] payment {payment_num} SUCCESS: {:?}",
                payment_resp.resource_content
            );
        } else {
            eprintln!(
                "[agent] payment {payment_num} FAILED: {:?}",
                payment_resp.error
            );
            anyhow::bail!(
                "payment {payment_num} failed: {:?}",
                payment_resp.error
            );
        }

        // Agent increments serial[0] for next payment
        serial_counter += 1;
    }

    eprintln!("\n══════════════════════════════════════════════════════");
    eprintln!("  ALL {NUM_PAYMENTS} PAYMENTS SUCCESSFUL!");
    eprintln!("  ADN balance started at {adn_balance}");
    eprintln!("  Debited {DEBIT_PER_PAYMENT} × {NUM_PAYMENTS} = {} to merchant", DEBIT_PER_PAYMENT * NUM_PAYMENTS as u64);
    eprintln!("  Remaining: {}", adn_balance - DEBIT_PER_PAYMENT * NUM_PAYMENTS as u64);
    eprintln!("══════════════════════════════════════════════════════");

    Ok(())
}
