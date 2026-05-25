//! Agent (client A): Creates a private ADN note and saves artifacts for the
//! facilitator to consume.
//!
//! Both agent and facilitator are BasicWallet + AuthSingleSig. No Guardian, no HTTP.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test agent_create_adn -- --ignored --nocapture

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

const ADN_MASM: &str = include_str!("../masm/agent_debit_note.masm");
const OUTPUT_DIR: &str = "/tmp/agent_adn";

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
async fn agent_create_adn() -> anyhow::Result<()> {
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
    let symbol = TokenSymbol::new("XADN").map_err(|e| anyhow::anyhow!("{e:?}"))?;
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
    keystore
        .add_key(&facilitator_key, facilitator_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!(
        "[agent] step 3: facilitator created (NOT in this client): {}",
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

    // 5. Generate agent signing key (for ADN Falcon sig verification)
    let agent_signing_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let agent_pk: Word = agent_signing_key.public_key().to_commitment().into();
    eprintln!("[agent] step 5: agent signing key generated");

    // 6. Compile ADN note script
    let note_script = CodeBuilder::default().compile_note_script(ADN_MASM)?;
    eprintln!("[agent] step 6: ADN note script compiled");

    // 7. Build ADN note (PRIVATE)
    let adn_balance = 1_000u64;
    let asset =
        FungibleAsset::new(faucet_id, adn_balance).map_err(|e| anyhow::anyhow!("{e:?}"))?;

    // Storage: 7 items [agent_pk(4), agent_suffix, agent_prefix, expiry_block]
    // expiry set well above current testnet block (~1M) to ensure consume path
    let expiry_block = 10_000_000u64;
    let storage = NoteStorage::new(vec![
        agent_pk[0],
        agent_pk[1],
        agent_pk[2],
        agent_pk[3],
        agent_id.suffix(),
        agent_id.prefix().as_felt(),
        Felt::new(expiry_block),
    ])?;

    let mut serial_bytes = [0u8; 32];
    client.rng().fill_bytes(&mut serial_bytes);
    let serial_num: Word = [
        Felt::new(u64::from_le_bytes(serial_bytes[0..8].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[8..16].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[16..24].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[24..32].try_into().unwrap())),
    ]
    .into();

    let tag = NoteTag::with_account_target(facilitator_id);
    let metadata = NoteMetadata::new(agent_id, NoteType::Private).with_tag(tag);
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(serial_num, note_script, storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();
    eprintln!("[agent] step 7: ADN note built, id={note_id}");
    eprintln!("[agent]   expiry_block: {expiry_block}");
    eprintln!("[agent]   balance: {adn_balance}");

    // 8. Submit via own_output_notes
    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let tx_id = client
        .submit_new_transaction(agent_id, create_req)
        .await?;
    eprintln!("[agent] step 8: ADN note submitted: {tx_id}");

    // 9. Wait for inclusion proof
    eprintln!("[agent] step 9: waiting for inclusion proof...");
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
                    "[agent] step 9: note.mno saved ({} bytes b64)",
                    note_file_b64.len()
                );
                break;
            }
        }
        if attempt == 59 {
            anyhow::bail!("timed out waiting for inclusion proof");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // 10. Save facilitator account
    let facilitator_bytes = facilitator_account.to_bytes();
    let facilitator_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &facilitator_bytes,
    );
    std::fs::write(output_dir.join("facilitator.b64"), &facilitator_b64)?;

    // 11. Save agent signing key
    let agent_sk_bytes = agent_signing_key.to_bytes();
    std::fs::write(output_dir.join("agent_sk.bin"), &agent_sk_bytes)?;
    eprintln!(
        "[agent] step 11: agent_sk.bin saved ({} bytes)",
        agent_sk_bytes.len()
    );

    // 12. Copy keystore files
    let src_ks = tmp.path().join("keystore");
    let dst_ks = output_dir.join("keystore");
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        std::fs::copy(entry.path(), dst_ks.join(entry.file_name()))?;
        eprintln!("[agent]   keystore: copied {:?}", entry.file_name());
    }

    // 13. Save setup.toml
    let serial_felts: [Felt; 4] = serial_num.into();
    let setup_toml = format!(
        "facilitator_id_hex = \"{}\"\n\
         serial_num_hex = [\"0x{:x}\", \"0x{:x}\", \"0x{:x}\", \"0x{:x}\"]\n",
        facilitator_id.to_hex(),
        serial_felts[0].as_canonical_u64(),
        serial_felts[1].as_canonical_u64(),
        serial_felts[2].as_canonical_u64(),
        serial_felts[3].as_canonical_u64(),
    );
    std::fs::write(output_dir.join("setup.toml"), &setup_toml)?;

    eprintln!("[agent] ════════════════════════════════════════════════");
    eprintln!("[agent] SUCCESS — artifacts saved to {OUTPUT_DIR}");
    eprintln!("[agent] Facilitator ID: {}", facilitator_id.to_hex());
    eprintln!("[agent] Now run: cargo test --release --test facilitator_consume_adn -- --ignored --nocapture");
    eprintln!("[agent] ════════════════════════════════════════════════");

    Ok(())
}
