//! adn-facilitator — HTTP server that handles /verify and /settle for ADN batch-settlement.
//!
//! Usage:
//!   adn-facilitator --port 7002 --data-dir /tmp/facilitator

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::post;
use axum::Json;
use clap::Parser;
use tokio::sync::mpsc;

use miden_client::auth::AuthSecretKey;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::store::{NoteExportType, NoteFilter};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::{Client, Felt};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::note::NoteFile;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Hasher, Word};

use adn_services::{SettleRequest, SettleResponse, VerifyRequest, VerifyResponse};

#[derive(Parser)]
struct Args {
    /// Port to listen on
    #[arg(long, default_value_t = 7002)]
    port: u16,

    /// Data directory (store + keystore)
    #[arg(long, default_value = "/tmp/adn-facilitator")]
    data_dir: String,

    /// Hex-encoded facilitator Account bytes (base64)
    #[arg(long)]
    account_b64: Option<String>,

    /// Path to facilitator account file (base64-encoded Account bytes)
    #[arg(long)]
    account_file: Option<String>,

    /// Path to keystore directory to import
    #[arg(long)]
    import_keystore: Option<String>,

    /// Miden testnet RPC URL
    #[arg(long, default_value = "https://rpc.testnet.miden.io")]
    rpc_url: String,
}

type FacilitatorMsg = (SettleRequest, tokio::sync::oneshot::Sender<SettleResponse>);

#[derive(Clone)]
struct FacilitatorHandle {
    tx: mpsc::Sender<FacilitatorMsg>,
}

fn parse_hex_felt(s: &str) -> u64 {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(s, 16).unwrap()
}

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

async fn handle_verify(Json(req): Json<VerifyRequest>) -> Json<VerifyResponse> {
    // For now, verify just checks the NoteFile deserializes correctly
    let note_file_bytes = match hex::decode(&req.note_file_hex) {
        Ok(b) => b,
        Err(e) => {
            return Json(VerifyResponse {
                is_valid: false, note_balance: 0, user_pub_key_hex: String::new(),
                reclaim_block_height: 0, error: Some(format!("hex: {e}")),
            });
        }
    };
    let note_file = match NoteFile::read_from_bytes(&note_file_bytes) {
        Ok(nf) => nf,
        Err(e) => {
            return Json(VerifyResponse {
                is_valid: false, note_balance: 0, user_pub_key_hex: String::new(),
                reclaim_block_height: 0, error: Some(format!("note: {e}")),
            });
        }
    };
    let note = match &note_file {
        NoteFile::NoteWithProof(n, _) => n,
        _ => {
            return Json(VerifyResponse {
                is_valid: false, note_balance: 0, user_pub_key_hex: String::new(),
                reclaim_block_height: 0, error: Some("expected NoteWithProof".into()),
            });
        }
    };

    let balance = note.assets().iter().next()
        .map(|a| match a {
            miden_protocol::asset::Asset::Fungible(f) => f.amount(),
            _ => 0,
        })
        .unwrap_or(0);

    let storage_elems = note.recipient().storage().to_elements();
    let user_pk_hex = if storage_elems.len() >= 4 {
        format!("0x{}", hex::encode(
            storage_elems[..4].iter()
                .flat_map(|f| f.as_canonical_u64().to_le_bytes())
                .collect::<Vec<_>>()
        ))
    } else {
        String::new()
    };

    let reclaim = if storage_elems.len() >= 9 {
        storage_elems[8].as_canonical_u64()
    } else {
        0
    };

    Json(VerifyResponse {
        is_valid: true,
        note_balance: balance,
        user_pub_key_hex: user_pk_hex,
        reclaim_block_height: reclaim,
        error: None,
    })
}

async fn worker(
    mut client: Client<FilesystemKeyStore>,
    facilitator_id: AccountId,
    mut rx: mpsc::Receiver<FacilitatorMsg>,
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

    let note_file_bytes = match hex::decode(&req.note_file_hex) {
        Ok(b) => b,
        Err(e) => return fail(&format!("hex: {e}")),
    };
    let note_file = match NoteFile::read_from_bytes(&note_file_bytes) {
        Ok(nf) => nf,
        Err(e) => return fail(&format!("note: {e}")),
    };
    let (note, note_file_for_import) = match &note_file {
        NoteFile::NoteWithProof(n, _) => (n.clone(), note_file),
        _ => return fail("expected NoteWithProof"),
    };
    let note_id = note.id();
    let note_balance = note.assets().iter().next()
        .map(|a| match a { miden_protocol::asset::Asset::Fungible(f) => f.amount(), _ => 0 })
        .unwrap_or(0);

    client.sync_state().await.unwrap();
    if let Err(e) = client.import_notes(&[note_file_for_import]).await {
        tracing::warn!("import_notes: {e} (may already exist)");
    }

    for attempt in 0..60 {
        client.sync_state().await.unwrap();
        if let Ok(Some(record)) = client.get_input_note(note_id).await {
            if record.is_authenticated() {
                tracing::info!("note authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 { return fail("not authenticated timeout"); }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let merchant_id = match AccountId::from_hex(&req.merchant_id_hex) {
        Ok(id) => id,
        Err(e) => return fail(&format!("merchant_id: {e}")),
    };
    let serial_num: Word = [
        Felt::new(parse_hex_felt(&req.serial_num[0])),
        Felt::new(parse_hex_felt(&req.serial_num[1])),
        Felt::new(parse_hex_felt(&req.serial_num[2])),
        Felt::new(parse_hex_felt(&req.serial_num[3])),
    ].into();

    let agent_sk_bytes = match hex::decode(&req.agent_sk_hex) {
        Ok(b) => b,
        Err(e) => return fail(&format!("agent_sk hex: {e}")),
    };
    let agent_sk = match AuthSecretKey::read_from_bytes(&agent_sk_bytes) {
        Ok(sk) => sk,
        Err(e) => return fail(&format!("agent_sk: {e}")),
    };
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();

    let note_args: Word = [
        Felt::new(req.cumulative_amount), Felt::ZERO, Felt::ZERO, Felt::ZERO,
    ].into();

    let message: Word = Hasher::merge(&[
        serial_num.into(),
        [merchant_id.suffix(), merchant_id.prefix().as_felt(), Felt::new(req.cumulative_amount), Felt::ZERO].into(),
    ]).into();
    let sig = agent_sk.sign(message);
    let prepared: Vec<Felt> = sig.to_prepared_signature(message);
    let sig_key: Word = Hasher::merge(&[agent_pk.into(), message.into()]).into();

    tracing::info!(cumulative = req.cumulative_amount, "consuming ADN");

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, Some(note_args))])
        .extend_advice_map([(sig_key, prepared.as_slice())])
        .build()
        .unwrap();

    match client.submit_new_transaction(facilitator_id, consume_req).await {
        Ok(tx_id) => {
            tracing::info!(%tx_id, "settlement success");

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

            SettleResponse {
                success: true,
                tx_hash: format!("{tx_id}"),
                settled_amount: req.cumulative_amount,
                remainder_balance: note_balance.saturating_sub(req.cumulative_amount),
                p2id_note_file_hex: p2id_hex,
                error: None,
            }
        }
        Err(e) => fail(&format!("{e}")),
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
    let data_dir = std::path::PathBuf::from(&args.data_dir);
    std::fs::create_dir_all(&data_dir)?;

    // Import keystore if provided
    let ks_dir = data_dir.join("keystore");
    std::fs::create_dir_all(&ks_dir)?;
    if let Some(src) = &args.import_keystore {
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            std::fs::copy(entry.path(), ks_dir.join(entry.file_name()))?;
        }
        tracing::info!("imported keystore from {src}");
    }

    // Build miden client
    let endpoint = Endpoint::try_from(args.rpc_url.as_str())
        .map_err(|e| anyhow::anyhow!("endpoint: {e:?}"))?;
    let rpc = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let keystore = Arc::new(FilesystemKeyStore::new(ks_dir).unwrap());
    let mut client = ClientBuilder::new()
        .rpc(rpc)
        .sqlite_store(data_dir.join("store.sqlite3"))
        .authenticator(keystore)
        .in_debug_mode(false.into())
        .build()
        .await?;
    client.sync_state().await?;

    // Import facilitator account
    let account_bytes = if let Some(b64) = &args.account_b64 {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)?
    } else if let Some(path) = &args.account_file {
        let b64 = std::fs::read_to_string(path)?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64.trim())?
    } else {
        anyhow::bail!("provide --account-b64 or --account-file");
    };
    let account = Account::read_from_bytes(&account_bytes)?;
    let facilitator_id = account.id();
    client.add_account(&account, false).await?;
    client.sync_state().await?;
    tracing::info!(%facilitator_id, "facilitator ready");

    // Channel + worker
    let (tx, rx) = mpsc::channel::<FacilitatorMsg>(4);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(worker(client, facilitator_id, rx));
    });

    let app = axum::Router::new()
        .route("/verify", post(handle_verify))
        .route("/settle", post(handle_settle))
        .with_state(FacilitatorHandle { tx });

    let addr = format!("0.0.0.0:{}", args.port);
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
