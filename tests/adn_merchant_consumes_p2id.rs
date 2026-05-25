//! Full ADN flow: Agent → Merchant → Facilitator → Merchant consumes P2ID
//!
//! 1. Agent creates ADN note on-chain (private)
//! 2. Agent sends serialized note to merchant
//! 3. Merchant validates and forwards to facilitator
//! 4. Facilitator consumes ADN → creates P2ID to merchant + remainder note
//! 5. Facilitator exports the P2ID output note
//! 6. Merchant (client C) imports and consumes the P2ID note
//!
//! Three clients: agent (A), facilitator (B), merchant (C).
//! All BasicWallet + AuthSingleSig. No Guardian, no HTTP.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test adn_merchant_consumes_p2id -- --ignored --nocapture

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
use miden_protocol::account::{Account, AccountId};
use miden_protocol::asset::Asset;
use miden_protocol::note::*;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Hasher, Word};
use miden_standards::code_builder::CodeBuilder;
use rand::RngCore;

const ADN_MASM: &str = include_str!("../masm/agent_debit_note.masm");

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
async fn adn_merchant_consumes_p2id() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // ════════════════════════════════════════════════════════════════════
    // SETUP: Create all accounts using agent's client
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ SETUP ══");

    let agent_tmp = tempfile::tempdir()?;
    let (mut agent_client, agent_keystore) = build_client(agent_tmp.path()).await?;
    agent_client.sync_state().await?;
    eprintln!("[setup] client synced");

    // Deploy faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let symbol = TokenSymbol::new("XFUL").map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let faucet = AccountBuilder::new(rand_seed(&mut agent_client))
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
    agent_client.add_account(&faucet, false).await?;
    agent_keystore.add_key(&faucet_key, faucet_id).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[setup] faucet: {}", faucet_id.to_hex());
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Deploy agent wallet
    let agent_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let agent_account = AccountBuilder::new(rand_seed(&mut agent_client))
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
    agent_client.add_account(&agent_account, false).await?;
    agent_keystore.add_key(&agent_key, agent_id).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[setup] agent: {}", agent_id.to_hex());
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create facilitator wallet (NOT in agent's client)
    let facilitator_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let facilitator_account = AccountBuilder::new(rand_seed(&mut agent_client))
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
    agent_keystore.add_key(&facilitator_key, facilitator_id).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[setup] facilitator: {}", facilitator_id.to_hex());

    // Create merchant wallet (NOT in agent's client)
    let merchant_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let merchant_account = AccountBuilder::new(rand_seed(&mut agent_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            merchant_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let merchant_id = merchant_account.id();
    agent_keystore.add_key(&merchant_key, merchant_id).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[setup] merchant: {}", merchant_id.to_hex());

    // Mint tokens to agent
    let mint_asset = FungibleAsset::new(faucet_id, 10_000).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, agent_client.rng())
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    agent_client.submit_new_transaction(faucet_id, mint_req).await?;
    for attempt in 0..60 {
        agent_client.sync_state().await?;
        let consumable = agent_client.get_consumable_notes(Some(agent_id)).await?;
        if !consumable.is_empty() {
            eprintln!("[setup] mint consumable (attempt {attempt})");
            let notes: Vec<_> = consumable.into_iter().map(|(n, _)| n.try_into()).collect::<Result<_, _>>()?;
            let req = TransactionRequestBuilder::new().build_consume_notes(notes).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            agent_client.submit_new_transaction(agent_id, req).await?;
            break;
        }
        if attempt == 59 { anyhow::bail!("mint timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[setup] agent funded");

    // Save keystore + account bytes for facilitator and merchant
    let fac_account_bytes = facilitator_account.to_bytes();
    let merchant_account_bytes = merchant_account.to_bytes();
    let src_ks = agent_tmp.path().join("keystore");
    let mut keystore_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        keystore_files.push((
            entry.file_name().to_string_lossy().to_string(),
            std::fs::read(entry.path())?,
        ));
    }

    // ════════════════════════════════════════════════════════════════════
    // PHASE 1: Agent creates ADN note
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 1: AGENT CREATES ADN ══");

    let agent_signing_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let agent_pk: Word = agent_signing_key.public_key().to_commitment().into();

    let note_script = CodeBuilder::default().compile_note_script(ADN_MASM)?;
    let adn_balance = 1_000u64;
    let asset = FungibleAsset::new(faucet_id, adn_balance).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let expiry_block = 10_000_000u64;
    let storage = NoteStorage::new(vec![
        agent_pk[0], agent_pk[1], agent_pk[2], agent_pk[3],
        agent_id.suffix(), agent_id.prefix().as_felt(),
        Felt::new(expiry_block),
    ])?;

    let mut serial_bytes = [0u8; 32];
    agent_client.rng().fill_bytes(&mut serial_bytes);
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
    eprintln!("[agent] ADN note: {note_id} (balance={adn_balance}, expiry={expiry_block})");

    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let tx_id = agent_client.submit_new_transaction(agent_id, create_req).await?;
    eprintln!("[agent] submitted: {tx_id}");

    // Wait for inclusion proof
    let mut note_file_bytes: Vec<u8> = Vec::new();
    for attempt in 0..60 {
        agent_client.sync_state().await?;
        let output_notes = agent_client.get_output_notes(NoteFilter::List(vec![note_id])).await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let nf = record.into_note_file(&NoteExportType::NoteWithProof)
                    .map_err(|e| anyhow::anyhow!("into_note_file: {e:?}"))?;
                note_file_bytes = nf.to_bytes();
                eprintln!("[agent] NoteWithProof: {} bytes (attempt {attempt})", note_file_bytes.len());
                break;
            }
        }
        if attempt == 59 { anyhow::bail!("inclusion proof timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let serial_felts: [Felt; 4] = serial_num.into();
    let agent_sk_bytes = agent_signing_key.to_bytes();
    drop(agent_client);
    drop(agent_keystore);

    // ════════════════════════════════════════════════════════════════════
    // PHASE 2: Merchant validates note
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 2: MERCHANT VALIDATES ══");

    let merchant_nf = NoteFile::read_from_bytes(&note_file_bytes)
        .map_err(|e| anyhow::anyhow!("merchant decode: {e}"))?;
    let merchant_note = match &merchant_nf {
        NoteFile::NoteWithProof(n, _) => n,
        _ => anyhow::bail!("expected NoteWithProof"),
    };
    eprintln!("[merchant] validated note: {}, assets: {}", merchant_note.id(), merchant_note.assets().num_assets());

    // Verify roundtrip
    let roundtrip = merchant_nf.to_bytes();
    assert_eq!(roundtrip, note_file_bytes, "roundtrip mismatch");
    eprintln!("[merchant] roundtrip OK, forwarding to facilitator");

    // ════════════════════════════════════════════════════════════════════
    // PHASE 3: Facilitator consumes ADN note
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 3: FACILITATOR CONSUMES ADN ══");

    let fac_tmp = tempfile::tempdir()?;
    let fac_ks_dir = fac_tmp.path().join("keystore");
    std::fs::create_dir_all(&fac_ks_dir)?;
    for (name, data) in &keystore_files {
        std::fs::write(fac_ks_dir.join(name), data)?;
    }
    let (mut fac_client, _fac_ks) = build_client(fac_tmp.path()).await?;

    let facilitator_account = Account::read_from_bytes(&fac_account_bytes)?;
    fac_client.add_account(&facilitator_account, false).await?;
    fac_client.sync_state().await?;

    let note_file = NoteFile::read_from_bytes(&note_file_bytes)?;
    let (note, note_file_for_import) = match &note_file {
        NoteFile::NoteWithProof(n, _) => (n.clone(), note_file),
        _ => anyhow::bail!("expected NoteWithProof"),
    };
    let note_id = note.id();
    fac_client.import_notes(&[note_file_for_import]).await?;
    eprintln!("[facilitator] note imported, syncing...");

    for attempt in 0..60 {
        fac_client.sync_state().await?;
        if let Ok(Some(record)) = fac_client.get_input_note(note_id).await {
            if record.is_authenticated() {
                eprintln!("[facilitator] note authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 { eprintln!("[facilitator] WARNING: not authenticated"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Build note_args with MERCHANT as payment target
    let debit_amount = 100u64;
    let note_args: Word = [
        merchant_id.suffix(),
        merchant_id.prefix().as_felt(),
        Felt::new(debit_amount),
        Felt::ZERO,
    ].into();

    let serial_num: Word = [
        Felt::new(serial_felts[0].as_canonical_u64()),
        Felt::new(serial_felts[1].as_canonical_u64()),
        Felt::new(serial_felts[2].as_canonical_u64()),
        Felt::new(serial_felts[3].as_canonical_u64()),
    ].into();

    let agent_sk = AuthSecretKey::read_from_bytes(&agent_sk_bytes)
        .map_err(|e| anyhow::anyhow!("agent_sk: {e}"))?;
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();

    let message: Word = Hasher::merge(&[serial_num.into(), note_args.into()]).into();
    let sig = agent_sk.sign(message);
    let prepared: Vec<Felt> = sig.to_prepared_signature(message);
    let sig_key: Word = Hasher::merge(&[agent_pk.into(), message.into()]).into();

    eprintln!("[facilitator] consuming ADN (debit={debit_amount} to merchant {})...", merchant_id.to_hex());

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, Some(note_args))])
        .extend_advice_map([(sig_key, prepared.as_slice())])
        .build()
        .map_err(|e| anyhow::anyhow!("consume build: {e:?}"))?;

    let fac_tx_id = fac_client
        .submit_new_transaction(facilitator_id, consume_req)
        .await
        .map_err(|e| anyhow::anyhow!("facilitator consume: {e}"))?;
    eprintln!("[facilitator] SUCCESS: ADN consumed, tx={fac_tx_id}");

    // ════════════════════════════════════════════════════════════════════
    // PHASE 4: Export the P2ID output note for the merchant
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 4: FACILITATOR EXPORTS P2ID FOR MERCHANT ══");

    // Wait for the output notes from the facilitator's consume tx to be committed
    let mut p2id_note_file_bytes: Vec<u8> = Vec::new();
    let mut p2id_note_id = None;
    for attempt in 0..60 {
        fac_client.sync_state().await?;
        let output_notes = fac_client.get_output_notes(NoteFilter::All).await?;
        eprintln!("[facilitator] attempt {attempt}: {} output notes", output_notes.len());

        for record in output_notes {
            if record.inclusion_proof().is_some() {
                // Try to export as NoteWithProof
                let rid = record.id();
                match record.into_note_file(&NoteExportType::NoteWithProof) {
                    Ok(nf) => {
                        p2id_note_file_bytes = nf.to_bytes();
                        p2id_note_id = Some(rid);
                        eprintln!(
                            "[facilitator] exported output note {} ({} bytes)",
                            rid, p2id_note_file_bytes.len()
                        );
                        break;
                    }
                    Err(_) => continue,
                }
            }
        }
        if p2id_note_id.is_some() { break; }
        if attempt == 59 { anyhow::bail!("timed out waiting for P2ID output note"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let p2id_note_id = p2id_note_id.unwrap();
    eprintln!("[facilitator] P2ID note for merchant: {p2id_note_id}");

    drop(fac_client);
    drop(_fac_ks);

    // ════════════════════════════════════════════════════════════════════
    // PHASE 5: Merchant consumes the P2ID note
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 5: MERCHANT CONSUMES P2ID ══");

    let merchant_tmp = tempfile::tempdir()?;
    let merchant_ks_dir = merchant_tmp.path().join("keystore");
    std::fs::create_dir_all(&merchant_ks_dir)?;
    for (name, data) in &keystore_files {
        std::fs::write(merchant_ks_dir.join(name), data)?;
    }
    let (mut merchant_client, _merchant_ks) = build_client(merchant_tmp.path()).await?;

    let merchant_account = Account::read_from_bytes(&merchant_account_bytes)?;
    merchant_client.add_account(&merchant_account, false).await?;
    merchant_client.sync_state().await?;
    eprintln!("[merchant] client ready, id={}", merchant_id.to_hex());

    // Import the P2ID note
    let p2id_note_file = NoteFile::read_from_bytes(&p2id_note_file_bytes)?;
    let (p2id_note, p2id_for_import) = match &p2id_note_file {
        NoteFile::NoteWithProof(n, _) => (n.clone(), p2id_note_file),
        _ => anyhow::bail!("expected NoteWithProof for P2ID"),
    };
    eprintln!("[merchant] P2ID note: {}, assets: {}", p2id_note.id(), p2id_note.assets().num_assets());

    merchant_client.import_notes(&[p2id_for_import]).await?;
    eprintln!("[merchant] P2ID note imported");

    // Wait for authentication
    for attempt in 0..60 {
        merchant_client.sync_state().await?;
        if let Ok(Some(record)) = merchant_client.get_input_note(p2id_note_id).await {
            if record.is_authenticated() {
                eprintln!("[merchant] P2ID authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 { eprintln!("[merchant] WARNING: P2ID not authenticated"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Consume P2ID (no note_args needed for standard P2ID)
    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(p2id_note, None)])
        .build()
        .map_err(|e| anyhow::anyhow!("p2id consume build: {e:?}"))?;

    let merchant_tx = merchant_client
        .submit_new_transaction(merchant_id, consume_req)
        .await
        .map_err(|e| anyhow::anyhow!("merchant P2ID consume: {e}"))?;

    eprintln!("[merchant] ════════════════════════════════════════════════");
    eprintln!("[merchant] SUCCESS: P2ID consumed! tx={merchant_tx}");
    eprintln!("[merchant] ════════════════════════════════════════════════");
    eprintln!("\n══ FULL FLOW COMPLETE ══");
    eprintln!("  Agent created ADN (balance={adn_balance})");
    eprintln!("  Merchant relayed to facilitator");
    eprintln!("  Facilitator consumed ADN (debit={debit_amount} to merchant)");
    eprintln!("  Merchant consumed P2ID ({debit_amount} received)");

    Ok(())
}
