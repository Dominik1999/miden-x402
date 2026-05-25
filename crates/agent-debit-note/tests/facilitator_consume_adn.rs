//! Facilitator (client B): Consumes a private ADN note created by the agent.
//!
//! Reads artifacts from /tmp/agent_adn/, creates a FRESH client, imports
//! the facilitator account + NoteFile, builds the Falcon signature over
//! the debit message, and consumes the note.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test facilitator_consume_adn -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use miden_client::auth::AuthSecretKey;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::Felt;
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_protocol::account::{Account, AccountId};
use miden_protocol::note::NoteFile;
use miden_protocol::utils::serde::Deserializable;
use miden_protocol::{Hasher, Word};

const INPUT_DIR: &str = "/tmp/agent_adn";

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

use miden_client::Client;

fn parse_hex_felt(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    let s = s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("parse hex felt '{s}': {e}"))
}

#[tokio::test]
#[ignore = "requires agent_create_adn to have run first + testnet access"]
async fn facilitator_consume_adn() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let input_dir = std::path::Path::new(INPUT_DIR);
    if !input_dir.exists() {
        anyhow::bail!(
            "Input directory {INPUT_DIR} does not exist. Run agent_create_adn first."
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

    // Parse serial_num
    let serial_arr = setup_toml["serial_num_hex"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing serial_num_hex"))?;
    if serial_arr.len() != 4 {
        anyhow::bail!("serial_num_hex must have 4 elements");
    }
    let serial_num: Word = [
        Felt::new(parse_hex_felt(serial_arr[0].as_str().unwrap())?),
        Felt::new(parse_hex_felt(serial_arr[1].as_str().unwrap())?),
        Felt::new(parse_hex_felt(serial_arr[2].as_str().unwrap())?),
        Felt::new(parse_hex_felt(serial_arr[3].as_str().unwrap())?),
    ]
    .into();
    eprintln!("[facilitator] serial_num loaded");

    // ── Read facilitator account ──
    let facilitator_b64 = std::fs::read_to_string(input_dir.join("facilitator.b64"))?;
    let facilitator_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        facilitator_b64.trim(),
    )?;
    let facilitator_account = Account::read_from_bytes(&facilitator_bytes)?;
    eprintln!(
        "[facilitator] account loaded: {}",
        facilitator_account.id().to_hex()
    );

    // ── Read agent signing key ──
    let agent_sk_bytes = std::fs::read(input_dir.join("agent_sk.bin"))?;
    let agent_sk = match AuthSecretKey::read_from_bytes(&agent_sk_bytes) {
        Ok(sk) => sk,
        Err(_) => {
            let falcon_sk =
                miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey::read_from_bytes(
                    &agent_sk_bytes,
                )
                .map_err(|e| anyhow::anyhow!("agent_sk decode: {e}"))?;
            AuthSecretKey::Falcon512Poseidon2(falcon_sk)
        }
    };
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();
    eprintln!("[facilitator] agent signing key loaded");

    // ── Read note from .mno ──
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
            anyhow::bail!(
                "Expected NoteWithProof, got {:?}",
                std::mem::discriminant(other)
            );
        }
    };
    let note_id = note.id();
    eprintln!("[facilitator] note id: {note_id}");
    eprintln!("[facilitator]   storage items: {}", note.recipient().storage().num_items());
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

    // Wait for note to authenticate
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

    // ── Build note_args + Falcon signature ──
    // note_args for consume path: [merchant_suffix, merchant_prefix, amount, 0]
    // In this test the facilitator is also the payment target
    let debit_amount = 100u64;
    let note_args: Word = [
        facilitator_id.suffix(),
        facilitator_id.prefix().as_felt(),
        Felt::new(debit_amount),
        Felt::ZERO,
    ]
    .into();

    // message = merge(serial_num, note_args)
    let message: Word = Hasher::merge(&[serial_num.into(), note_args.into()]).into();
    let sig = agent_sk.sign(message);
    let prepared: Vec<Felt> = sig.to_prepared_signature(message);

    // advice map key = merge(agent_pk, message)
    let sig_key: Word = Hasher::merge(&[agent_pk.into(), message.into()]).into();

    eprintln!("[facilitator] consuming ADN note...");
    eprintln!("[facilitator]   debit_amount: {debit_amount}");
    eprintln!("[facilitator]   prepared_sig len: {}", prepared.len());

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, Some(note_args))])
        .extend_advice_map([(sig_key, prepared.as_slice())])
        .build()
        .map_err(|e| anyhow::anyhow!("consume build: {e:?}"))?;

    match client
        .submit_new_transaction(facilitator_id, consume_req)
        .await
    {
        Ok(tx_id) => {
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            eprintln!("[facilitator] SUCCESS: ADN consumed! tx={tx_id}");
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            Ok(())
        }
        Err(e) => {
            let err_str = format!("{e:?}");
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            eprintln!(
                "[facilitator] FAILED: {}",
                &err_str[..err_str.len().min(800)]
            );
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            anyhow::bail!("ADN consume failed: {e}");
        }
    }
}
