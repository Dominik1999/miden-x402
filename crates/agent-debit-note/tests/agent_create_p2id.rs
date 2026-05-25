//! Agent (client A): Creates a private P2ID note targeting the facilitator
//! and saves artifacts to /tmp/agent_p2id/ for the facilitator process.
//!
//! Both agent and facilitator are BasicWallet + AuthSingleSig. No Guardian.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test agent_create_p2id -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

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
use miden_protocol::asset::Asset;
use miden_protocol::note::*;
use miden_protocol::utils::serde::Serializable;
use miden_protocol::Word;
use miden_standards::code_builder::CodeBuilder;
use rand::RngCore;

const OUTPUT_DIR: &str = "/tmp/agent_p2id";

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

#[tokio::test]
#[ignore = "requires testnet access"]
async fn agent_create_p2id() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let output_dir = std::path::Path::new(OUTPUT_DIR);
    if output_dir.exists() {
        std::fs::remove_dir_all(output_dir)?;
    }
    std::fs::create_dir_all(output_dir.join("keystore"))?;

    // ── Build client ──
    let tmp = tempfile::tempdir()?;
    let (mut client, keystore) = build_client(tmp.path()).await?;
    client.sync_state().await?;
    eprintln!("[agent] client synced");

    // 1. Deploy faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let symbol = TokenSymbol::new("XPID").map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let faucet = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            faucet_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(
            BasicFungibleFaucet::new(symbol, 6, Felt::new(1_000_000_000))
                .map_err(|e| anyhow::anyhow!("{e:?}"))?,
        )
        .with_component(AuthControlled::allow_all())
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let faucet_id = faucet.id();
    client.add_account(&faucet, false).await?;
    keystore
        .add_key(&faucet_key, faucet_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[agent] step 1: faucet deployed: {}", faucet_id.to_hex());
    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 2. Deploy agent wallet (BasicWallet + AuthSingleSig)
    let agent_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let agent_account = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            agent_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let agent_id = agent_account.id();
    client.add_account(&agent_account, false).await?;
    keystore
        .add_key(&agent_key, agent_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[agent] step 2: agent deployed: {}", agent_id.to_hex());
    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 3. Create facilitator wallet (BasicWallet + AuthSingleSig) — NOT in agent's client
    let facilitator_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let facilitator_account = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            facilitator_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let facilitator_id = facilitator_account.id();
    // Save facilitator key so the consume process can authenticate
    keystore
        .add_key(&facilitator_key, facilitator_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!(
        "[agent] step 3: facilitator account created (NOT in this client): {}",
        facilitator_id.to_hex()
    );

    // 4. Mint tokens to agent, consume the mint note
    eprintln!("[agent] step 4: minting tokens to agent...");
    let mint_asset =
        FungibleAsset::new(faucet_id, 10_000).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, client.rng())
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mint_tx = client.submit_new_transaction(faucet_id, mint_req).await?;
    eprintln!("[agent] step 4: mint tx: {mint_tx}");

    for attempt in 0..60 {
        client.sync_state().await?;
        let consumable = client.get_consumable_notes(Some(agent_id)).await?;
        if !consumable.is_empty() {
            eprintln!(
                "[agent] step 4: mint note consumable (attempt {attempt}), consuming..."
            );
            let notes: Vec<_> = consumable
                .into_iter()
                .map(|(note, _)| note.try_into())
                .collect::<Result<_, _>>()?;
            let consume_req = TransactionRequestBuilder::new()
                .build_consume_notes(notes)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            client
                .submit_new_transaction(agent_id, consume_req)
                .await?;
            break;
        }
        if attempt == 59 {
            anyhow::bail!("timed out waiting for consumable mint note");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[agent] step 4: agent funded");

    // 5. Build P2ID note targeting facilitator (PRIVATE)
    let p2id_script_src = "use miden::standards::notes::p2id\n@note_script\npub proc main\n    exec.p2id::main\nend";
    let p2id_script = CodeBuilder::default().compile_note_script(p2id_script_src)?;

    let storage =
        NoteStorage::new(vec![facilitator_id.suffix(), facilitator_id.prefix().as_felt()])?;

    let mut serial_bytes = [0u8; 32];
    client.rng().fill_bytes(&mut serial_bytes);
    let serial: Word = [
        Felt::new(u64::from_le_bytes(serial_bytes[0..8].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[8..16].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[16..24].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[24..32].try_into().unwrap())),
    ]
    .into();

    let send_asset =
        FungibleAsset::new(faucet_id, 1_000).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let tag = NoteTag::with_account_target(facilitator_id);
    let metadata = NoteMetadata::new(agent_id, NoteType::Private).with_tag(tag);
    let vault = NoteAssets::new(vec![Asset::Fungible(send_asset)])?;
    let recipient = NoteRecipient::new(serial, p2id_script, storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();
    eprintln!("[agent] step 5: P2ID note built, id={note_id}");

    // 6. Submit via own_output_notes
    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let p2id_tx = client
        .submit_new_transaction(agent_id, create_req)
        .await?;
    eprintln!("[agent] step 6: P2ID note submitted: {p2id_tx}");

    // 7. Wait for on-chain inclusion + sync to get inclusion proof
    eprintln!("[agent] step 7: waiting for on-chain commit + inclusion proof...");
    let mut got_proof = false;
    for attempt in 0..60 {
        client.sync_state().await?;
        let output_notes = client
            .get_output_notes(NoteFilter::List(vec![note_id]))
            .await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let note_file = record
                    .into_note_file(&NoteExportType::NoteWithProof)
                    .map_err(|e| anyhow::anyhow!("into_note_file: {e:?}"))?;
                let note_file_bytes = note_file.to_bytes();
                let note_file_b64 = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &note_file_bytes,
                );
                std::fs::write(output_dir.join("note.mno"), &note_file_b64)?;
                eprintln!(
                    "[agent] step 7: note.mno saved ({} bytes b64), note_id={}",
                    note_file_b64.len(),
                    note_id
                );
                got_proof = true;
                break;
            }
        }
        if attempt == 59 {
            anyhow::bail!("timed out waiting for inclusion proof");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    eprintln!("[agent] step 7: inclusion proof received");

    // 8. Save facilitator account bytes
    let facilitator_bytes = facilitator_account.to_bytes();
    let facilitator_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &facilitator_bytes,
    );
    std::fs::write(output_dir.join("facilitator.b64"), &facilitator_b64)?;
    eprintln!(
        "[agent] step 8: facilitator.b64 saved ({} bytes), id={}",
        facilitator_b64.len(),
        facilitator_id.to_hex()
    );

    // 9. Copy keystore files
    let src_ks = tmp.path().join("keystore");
    let dst_ks = output_dir.join("keystore");
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        std::fs::copy(entry.path(), dst_ks.join(entry.file_name()))?;
        eprintln!("[agent]   keystore: copied {:?}", entry.file_name());
    }

    // 10. Save setup.toml
    let setup_toml = format!(
        "facilitator_id_hex = \"{}\"\n",
        facilitator_id.to_hex(),
    );
    std::fs::write(output_dir.join("setup.toml"), &setup_toml)?;

    eprintln!("[agent] ════════════════════════════════════════════════");
    eprintln!("[agent] SUCCESS — artifacts saved to {OUTPUT_DIR}");
    eprintln!("[agent] Facilitator ID: {}", facilitator_id.to_hex());
    eprintln!("[agent] Now run: cargo test --release --test facilitator_consume_p2id -- --ignored --nocapture");
    eprintln!("[agent] ════════════════════════════════════════════════");

    Ok(())
}
