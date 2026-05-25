//! Facilitator (client B): Consumes a private P2ID note created by the agent.
//!
//! Reads artifacts from /tmp/agent_p2id/, creates a FRESH client, imports
//! the facilitator account + NoteFile, syncs, and consumes.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test facilitator_consume_p2id -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::Client;
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_protocol::account::{Account, AccountId};
use miden_protocol::note::NoteFile;
use miden_protocol::utils::serde::Deserializable;

const INPUT_DIR: &str = "/tmp/agent_p2id";

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

#[tokio::test]
#[ignore = "requires agent_create_p2id to have run first + testnet access"]
async fn facilitator_consume_p2id() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let input_dir = std::path::Path::new(INPUT_DIR);
    if !input_dir.exists() {
        anyhow::bail!(
            "Input directory {INPUT_DIR} does not exist. Run agent_create_p2id first."
        );
    }

    // ── Read setup.toml ──
    let setup_toml: toml::Value =
        toml::from_str(&std::fs::read_to_string(input_dir.join("setup.toml"))?)?;
    let facilitator_id_hex = setup_toml["facilitator_id_hex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing facilitator_id_hex"))?;
    let facilitator_id = AccountId::from_hex(facilitator_id_hex)?;
    eprintln!("[facilitator] id: {}", facilitator_id.to_hex());

    // ── Read facilitator account ──
    let facilitator_b64 = std::fs::read_to_string(input_dir.join("facilitator.b64"))?;
    let facilitator_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        facilitator_b64.trim(),
    )?;
    let facilitator_account = Account::read_from_bytes(&facilitator_bytes)?;
    eprintln!(
        "[facilitator] account loaded: id={}",
        facilitator_account.id().to_hex()
    );

    // ── Read note from .mno (NoteFile) ──
    let mno_b64 = std::fs::read_to_string(input_dir.join("note.mno"))?;
    let mno_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        mno_b64.trim(),
    )?;
    let note_file = NoteFile::read_from_bytes(&mno_bytes)
        .map_err(|e| anyhow::anyhow!("NoteFile decode: {e}"))?;

    let (note, note_file_for_import) = match &note_file {
        NoteFile::NoteWithProof(n, _proof) => {
            eprintln!("[facilitator] loaded NoteFile::NoteWithProof");
            (n.clone(), note_file)
        }
        other => {
            anyhow::bail!("Expected NoteWithProof, got {:?}", std::mem::discriminant(other));
        }
    };
    let note_id = note.id();
    eprintln!("[facilitator] note id: {note_id}");
    eprintln!("[facilitator]   assets: {}", note.assets().num_assets());

    // ── Create fresh client ──
    let tmp = tempfile::tempdir()?;
    let data_dir = tmp.path();

    // Copy keystore files
    let dst_ks = data_dir.join("keystore");
    std::fs::create_dir_all(&dst_ks)?;
    let src_ks = input_dir.join("keystore");
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        std::fs::copy(entry.path(), dst_ks.join(entry.file_name()))?;
        eprintln!("[facilitator] keystore: copied {:?}", entry.file_name());
    }

    let (mut client, _ks) = build_client(data_dir).await?;

    // Import facilitator account
    client.add_account(&facilitator_account, false).await?;
    eprintln!("[facilitator] account imported");

    // Sync first
    client.sync_state().await?;
    eprintln!("[facilitator] initial sync done");

    // Import note
    client.import_notes(&[note_file_for_import]).await?;
    eprintln!("[facilitator] note imported");

    // Wait for note to be authenticated
    eprintln!("[facilitator] waiting for note to authenticate...");
    for attempt in 0..60 {
        client.sync_state().await?;
        if let Ok(Some(record)) = client.get_input_note(note_id).await {
            if record.is_authenticated() {
                eprintln!("[facilitator] note authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 {
            eprintln!("[facilitator] WARNING: note never authenticated after 60 attempts");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // ── Consume ──
    eprintln!("[facilitator] consuming P2ID note...");
    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, None)])
        .build()
        .map_err(|e| anyhow::anyhow!("consume build: {e:?}"))?;

    match client
        .submit_new_transaction(facilitator_id, consume_req)
        .await
    {
        Ok(tx_id) => {
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            eprintln!("[facilitator] SUCCESS: P2ID consumed! tx={tx_id}");
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            Ok(())
        }
        Err(e) => {
            let err_str = format!("{e:?}");
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            eprintln!(
                "[facilitator] FAILED: {}",
                &err_str[..err_str.len().min(500)]
            );
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            anyhow::bail!("P2ID consume failed: {e}");
        }
    }
}
