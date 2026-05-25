//! E2E batch-settlement flow on testnet.
//!
//! Phase 1: Agent creates ADN note (with committed merchant) on-chain.
//! Phase 2: Agent signs 5 cumulative vouchers; merchant verifies locally (no facilitator).
//! Phase 3: Merchant calls /settle; facilitator consumes ADN → P2ID to merchant + remainder.
//! Phase 4: Merchant consumes P2ID note.
//!
//! Run:
//!   RUST_LOG=info cargo test --release -p agent-debit-note --test batch_settlement_e2e -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
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

use agent_debit_note::voucher::{sign_voucher, verify_voucher};
use agent_debit_note::note::AGENT_DEBIT_NOTE_MASM;
use agent_debit_note::types::AgentDebitNoteStorage;

// ════════════════════════════════════════════════════════════════════════
// HTTP message types (batch-settlement spec)
// ════════════════════════════════════════════════════════════════════════

/// POST /verify request — session setup
#[derive(Serialize, Deserialize, Debug)]
struct VerifyRequest {
    note_file_hex: String,
    merchant_id_hex: String,
}

/// POST /verify response
#[derive(Serialize, Deserialize, Debug)]
struct VerifyResponse {
    is_valid: bool,
    note_balance: u64,
    user_pub_key_hex: String,
    reclaim_block_height: u64,
    error: Option<String>,
}

/// POST /settle request
#[derive(Serialize, Deserialize, Debug)]
struct SettleRequest {
    note_file_hex: String,
    agent_sk_hex: String,
    serial_num: [String; 4],
    cumulative_amount: u64,
    merchant_id_hex: String,
}

/// POST /settle response
#[derive(Serialize, Deserialize, Debug)]
struct SettleResponse {
    success: bool,
    tx_hash: String,
    settled_amount: u64,
    remainder_balance: u64,
    p2id_note_file_hex: Option<String>,
    error: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════
// Facilitator worker
// ════════════════════════════════════════════════════════════════════════

type FacilitatorMsg = (SettleRequest, tokio::sync::oneshot::Sender<SettleResponse>);

#[derive(Clone)]
struct FacilitatorHandle {
    tx: tokio::sync::mpsc::Sender<FacilitatorMsg>,
}

fn parse_hex_felt(s: &str) -> u64 {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(s, 16).unwrap()
}

async fn facilitator_worker(
    mut client: Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    mut rx: tokio::sync::mpsc::Receiver<FacilitatorMsg>,
) {
    while let Some((req, resp_tx)) = rx.recv().await {
        let resp = process_settle(&mut client, facilitator_id, &req).await;
        let _ = resp_tx.send(resp);
    }
}

async fn process_settle(
    client: &mut Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    req: &SettleRequest,
) -> SettleResponse {
    let fail = |msg: &str| SettleResponse {
        success: false, tx_hash: String::new(), settled_amount: 0,
        remainder_balance: 0, p2id_note_file_hex: None, error: Some(msg.into()),
    };

    // Decode note
    let note_file_bytes = hex::decode(&req.note_file_hex).unwrap();
    let note_file = NoteFile::read_from_bytes(&note_file_bytes).unwrap();
    let (note, note_file_for_import) = match &note_file {
        NoteFile::NoteWithProof(n, _) => (n.clone(), note_file),
        _ => return fail("expected NoteWithProof"),
    };
    let note_id = note.id();
    let note_balance = note.assets().iter().next()
        .map(|a| match a { Asset::Fungible(f) => f.amount(), _ => 0 })
        .unwrap_or(0);

    // Import + sync
    client.sync_state().await.unwrap();
    client.import_notes(&[note_file_for_import]).await.unwrap();

    for attempt in 0..60 {
        client.sync_state().await.unwrap();
        if let Ok(Some(record)) = client.get_input_note(note_id).await {
            if record.is_authenticated() {
                eprintln!("[facilitator] note authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 { return fail("not authenticated"); }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Build agent signature for on-chain consumption
    let merchant_id = AccountId::from_hex(&req.merchant_id_hex).unwrap();
    let serial_num: Word = [
        Felt::new(parse_hex_felt(&req.serial_num[0])),
        Felt::new(parse_hex_felt(&req.serial_num[1])),
        Felt::new(parse_hex_felt(&req.serial_num[2])),
        Felt::new(parse_hex_felt(&req.serial_num[3])),
    ].into();

    let agent_sk_bytes = hex::decode(&req.agent_sk_hex).unwrap();
    let agent_sk = AuthSecretKey::read_from_bytes(&agent_sk_bytes).unwrap();
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();

    // Compute message = merge(serial, [merchant_suffix, merchant_prefix, cumulative_amount, 0])
    let note_args: Word = [
        Felt::new(req.cumulative_amount),
        Felt::ZERO,
        Felt::ZERO,
        Felt::ZERO,
    ].into();

    let message: Word = Hasher::merge(&[
        serial_num.into(),
        [merchant_id.suffix(), merchant_id.prefix().as_felt(), Felt::new(req.cumulative_amount), Felt::ZERO].into(),
    ]).into();
    let sig = agent_sk.sign(message);
    let prepared: Vec<Felt> = sig.to_prepared_signature(message);
    let sig_key: Word = Hasher::merge(&[agent_pk.into(), message.into()]).into();

    eprintln!("[facilitator] consuming ADN (cumulative={})", req.cumulative_amount);

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, Some(note_args))])
        .extend_advice_map([(sig_key, prepared.as_slice())])
        .build()
        .unwrap();

    match client.submit_new_transaction(facilitator_id, consume_req).await {
        Ok(tx_id) => {
            eprintln!("[facilitator] SUCCESS: tx={tx_id}");

            // Find P2ID output note for merchant
            let mut p2id_hex = None;
            for _attempt in 0..60 {
                client.sync_state().await.unwrap();
                for record in client.get_output_notes(NoteFilter::All).await.unwrap() {
                    if record.inclusion_proof().is_some() {
                        if let Ok(nf) = record.into_note_file(&NoteExportType::NoteWithProof) {
                            let nf_bytes = nf.to_bytes();
                            if let Ok(NoteFile::NoteWithProof(n, _)) = NoteFile::read_from_bytes(&nf_bytes) {
                                if n.recipient().storage().num_items() == 2 {
                                    p2id_hex = Some(hex::encode(&nf_bytes));
                                    break;
                                }
                            }
                        }
                    }
                }
                if p2id_hex.is_some() { break; }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }

            let remainder = note_balance - req.cumulative_amount;
            SettleResponse {
                success: true,
                tx_hash: format!("{tx_id}"),
                settled_amount: req.cumulative_amount,
                remainder_balance: remainder,
                p2id_note_file_hex: p2id_hex,
                error: None,
            }
        }
        Err(e) => fail(&format!("{e}")),
    }
}

// ════════════════════════════════════════════════════════════════════════
// HTTP handlers
// ════════════════════════════════════════════════════════════════════════

async fn handle_settle(
    State(handle): State<FacilitatorHandle>,
    Json(req): Json<SettleRequest>,
) -> Json<SettleResponse> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if handle.tx.send((req, resp_tx)).await.is_err() {
        return Json(SettleResponse {
            success: false, tx_hash: String::new(), settled_amount: 0,
            remainder_balance: 0, p2id_note_file_hex: None,
            error: Some("facilitator down".into()),
        });
    }
    Json(resp_rx.await.unwrap_or(SettleResponse {
        success: false, tx_hash: String::new(), settled_amount: 0,
        remainder_balance: 0, p2id_note_file_hex: None,
        error: Some("facilitator dropped".into()),
    }))
}

// ════════════════════════════════════════════════════════════════════════
// Helpers
// ════════════════════════════════════════════════════════════════════════

type MidenSdkClient = Client<FilesystemKeyStore>;

async fn build_client(
    dir: &std::path::Path,
) -> anyhow::Result<(MidenSdkClient, Arc<FilesystemKeyStore>)> {
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

fn rand_seed(client: &mut MidenSdkClient) -> [u8; 32] {
    let mut s = [0u8; 32];
    client.rng().fill_bytes(&mut s);
    s
}

// ════════════════════════════════════════════════════════════════════════
// Main test
// ════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires testnet access"]
async fn batch_settlement_e2e() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    const NUM_VOUCHERS: usize = 5;
    const AMOUNT_PER_REQUEST: u64 = 1000;

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
        .with_component(BasicFungibleFaucet::new(TokenSymbol::new("XBAT").unwrap(), 6, Felt::new(1_000_000_000)).unwrap())
        .with_component(AuthControlled::allow_all())
        .build().unwrap();
    let faucet_id = faucet.id();
    setup_client.add_account(&faucet, false).await?;
    setup_ks.add_key(&faucet_key, faucet_id).await.unwrap();
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Agent wallet
    let agent_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let agent = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(agent_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let agent_id = agent.id();
    setup_client.add_account(&agent, false).await?;
    setup_ks.add_key(&agent_key, agent_id).await.unwrap();
    setup_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Facilitator wallet
    let fac_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let fac_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(fac_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let facilitator_id = fac_account.id();
    setup_ks.add_key(&fac_key, facilitator_id).await.unwrap();
    let fac_account_bytes = fac_account.to_bytes();

    // Merchant wallet
    let merch_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(setup_client.rng());
    let merch_account = AccountBuilder::new(rand_seed(&mut setup_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(merch_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let merchant_id = merch_account.id();
    setup_ks.add_key(&merch_key, merchant_id).await.unwrap();
    let merch_account_bytes = merch_account.to_bytes();

    eprintln!("[setup] agent:       {}", agent_id.to_hex());
    eprintln!("[setup] facilitator: {}", facilitator_id.to_hex());
    eprintln!("[setup] merchant:    {}", merchant_id.to_hex());

    // Mint to agent
    let adn_balance = (NUM_VOUCHERS as u64) * AMOUNT_PER_REQUEST + 5000;
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
    eprintln!("[setup] agent funded");

    // Agent signing key (for vouchers)
    let agent_signing_sk = miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey::new();
    let agent_signing_pk = agent_signing_sk.public_key();
    let user_pk: Word = agent_signing_pk.to_commitment();

    // ══ PHASE 1: Agent creates ADN with committed merchant ══
    eprintln!("\n══ PHASE 1: CHANNEL SETUP ══");

    let note_script = CodeBuilder::default().compile_note_script(AGENT_DEBIT_NOTE_MASM)?;
    let asset = FungibleAsset::new(faucet_id, adn_balance).unwrap();
    let storage = AgentDebitNoteStorage::new(
        user_pk,
        merchant_id,
        agent_id,
        10_000_000, // reclaim far in future
    );

    let mut serial_bytes = [0u8; 32];
    setup_client.rng().fill_bytes(&mut serial_bytes);
    let serial_num: Word = [
        Felt::new(u64::from_le_bytes(serial_bytes[0..8].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[8..16].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[16..24].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[24..32].try_into().unwrap())),
    ].into();

    let note_storage: NoteStorage = storage.into();
    let tag = NoteTag::with_account_target(facilitator_id);
    let metadata = NoteMetadata::new(agent_id, NoteType::Private).with_tag(tag);
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(serial_num, note_script, note_storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();

    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build().unwrap();
    setup_client.submit_new_transaction(agent_id, create_req).await?;
    eprintln!("[agent] ADN note: {note_id} (balance={adn_balance}, merchant={})", merchant_id.to_hex());

    // Wait for inclusion proof
    let mut note_file_bytes: Vec<u8> = Vec::new();
    for attempt in 0..60 {
        setup_client.sync_state().await?;
        let output_notes = setup_client.get_output_notes(NoteFilter::List(vec![note_id])).await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let nf = record.into_note_file(&NoteExportType::NoteWithProof).unwrap();
                note_file_bytes = nf.to_bytes();
                eprintln!("[agent] NoteWithProof: {} bytes (attempt {attempt})", note_file_bytes.len());
                break;
            }
        }
        if attempt == 59 { anyhow::bail!("inclusion proof timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let agent_sk_hex = hex::encode(AuthSecretKey::Falcon512Poseidon2(agent_signing_sk.clone()).to_bytes());
    let serial_felts: [Felt; 4] = serial_num.into();

    // Collect keystore
    let src_ks = setup_tmp.path().join("keystore");
    let mut keystore_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        keystore_files.push((entry.file_name().to_string_lossy().to_string(), std::fs::read(entry.path())?));
    }

    drop(setup_client);
    drop(setup_ks);

    // ══ PHASE 2: 5 cumulative vouchers (off-chain, no facilitator) ══
    eprintln!("\n══ PHASE 2: CUMULATIVE VOUCHERS (OFF-CHAIN) ══");

    // Merchant stores the agent's public key (from /verify in real flow)
    let stored_user_pk = agent_signing_pk.clone();
    let mut latest_cumulative = 0u64;

    for i in 1..=NUM_VOUCHERS {
        latest_cumulative += AMOUNT_PER_REQUEST;
        let sig = sign_voucher(&agent_signing_sk, serial_num, merchant_id, latest_cumulative);

        // Merchant verifies locally — NO facilitator
        let valid = verify_voucher(&stored_user_pk, serial_num, merchant_id, latest_cumulative, &sig);
        assert!(valid, "voucher {i} verification failed");
        eprintln!("[merchant] voucher {i}: cumulative={latest_cumulative} ✓ (verified locally)");
    }
    eprintln!("[merchant] all {NUM_VOUCHERS} vouchers verified, no facilitator used");

    // ══ PHASE 3: Settlement ══
    eprintln!("\n══ PHASE 3: SETTLEMENT ══");

    // Start facilitator server
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
        .route("/settle", post(handle_settle))
        .with_state(FacilitatorHandle { tx: fac_tx });
    let fac_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let fac_addr = fac_listener.local_addr()?;
    let facilitator_url = format!("http://{fac_addr}");
    eprintln!("[facilitator] listening on {facilitator_url}");
    tokio::spawn(async move { axum::serve(fac_listener, fac_app).await.unwrap(); });

    // Merchant calls /settle with latest cumulative voucher
    let http = reqwest::Client::new();
    let settle_req = SettleRequest {
        note_file_hex: hex::encode(&note_file_bytes),
        agent_sk_hex: agent_sk_hex.clone(),
        serial_num: [
            format!("0x{:x}", serial_felts[0].as_canonical_u64()),
            format!("0x{:x}", serial_felts[1].as_canonical_u64()),
            format!("0x{:x}", serial_felts[2].as_canonical_u64()),
            format!("0x{:x}", serial_felts[3].as_canonical_u64()),
        ],
        cumulative_amount: latest_cumulative,
        merchant_id_hex: merchant_id.to_hex(),
    };

    eprintln!("[merchant] POST /settle (cumulative={})", latest_cumulative);
    let resp = http.post(format!("{facilitator_url}/settle"))
        .json(&settle_req)
        .timeout(Duration::from_secs(300))
        .send()
        .await?;
    let settle_resp: SettleResponse = resp.json().await?;

    if !settle_resp.success {
        anyhow::bail!("settlement failed: {:?}", settle_resp.error);
    }
    eprintln!("[merchant] settlement: settled={}, remainder={}", settle_resp.settled_amount, settle_resp.remainder_balance);

    // ══ PHASE 4: Merchant consumes P2ID ══
    eprintln!("\n══ PHASE 4: MERCHANT CONSUMES P2ID ══");

    let p2id_hex = settle_resp.p2id_note_file_hex
        .ok_or_else(|| anyhow::anyhow!("no P2ID note in settle response"))?;

    let merch_tmp = tempfile::tempdir()?;
    let merch_ks_dir = merch_tmp.path().join("keystore");
    std::fs::create_dir_all(&merch_ks_dir)?;
    for (name, data) in &keystore_files {
        std::fs::write(merch_ks_dir.join(name), data)?;
    }
    let (mut merch_client, _) = build_client(merch_tmp.path()).await?;
    let merch_account = Account::read_from_bytes(&merch_account_bytes)?;
    merch_client.add_account(&merch_account, false).await?;
    merch_client.sync_state().await?;

    let p2id_bytes = hex::decode(&p2id_hex)?;
    let p2id_nf = NoteFile::read_from_bytes(&p2id_bytes)?;
    let (p2id_note, p2id_import) = match &p2id_nf {
        NoteFile::NoteWithProof(n, _) => (n.clone(), p2id_nf),
        _ => anyhow::bail!("expected NoteWithProof for P2ID"),
    };
    merch_client.import_notes(&[p2id_import]).await?;

    for attempt in 0..60 {
        merch_client.sync_state().await?;
        if let Ok(Some(rec)) = merch_client.get_input_note(p2id_note.id()).await {
            if rec.is_authenticated() {
                eprintln!("[merchant] P2ID authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 { anyhow::bail!("P2ID auth timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(p2id_note, None)])
        .build()?;
    let merch_tx = merch_client.submit_new_transaction(merchant_id, consume_req).await
        .map_err(|e| anyhow::anyhow!("merchant P2ID consume: {e}"))?;

    eprintln!("[merchant] ════════════════════════════════════════════════");
    eprintln!("[merchant] P2ID consumed! tx={merch_tx}");
    eprintln!("[merchant] ════════════════════════════════════════════════");

    eprintln!("\n══════════════════════════════════════════════════════");
    eprintln!("  BATCH-SETTLEMENT E2E COMPLETE!");
    eprintln!("  ADN balance: {adn_balance}");
    eprintln!("  {NUM_VOUCHERS} vouchers verified off-chain (no facilitator)");
    eprintln!("  Settlement: {latest_cumulative} settled, {} remainder", adn_balance - latest_cumulative);
    eprintln!("  Merchant consumed P2ID");
    eprintln!("══════════════════════════════════════════════════════");

    Ok(())
}
