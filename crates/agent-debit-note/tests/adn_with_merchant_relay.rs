//! Three-role ADN test: Agent → Merchant → Facilitator
//!
//! Agent (client A) creates a private ADN note on-chain.
//! Merchant receives the serialized note bytes + agent signing key + serial_num.
//! Merchant validates the note (deserializes), then forwards to the facilitator.
//! Facilitator (client B) imports the note, builds the Falcon sig, and consumes.
//!
//! No HTTP — data passes as byte slices between functions.
//! No Guardian — both agent and facilitator are BasicWallet + AuthSingleSig.
//!
//! Run:
//!   RUST_LOG=info cargo test --release --test adn_with_merchant_relay -- --ignored --nocapture

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
use miden_protocol::account::Account;
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

// ════════════════════════════════════════════════════════════════════════
// Data structures passed between roles (simulates what HTTP would carry)
// ════════════════════════════════════════════════════════════════════════

/// What the agent sends to the merchant (the "payment request")
struct AgentToMerchant {
    note_file_bytes: Vec<u8>,        // NoteFile::NoteWithProof serialized
    agent_sk_bytes: Vec<u8>,         // agent signing key (AuthSecretKey)
    serial_num_felts: [u64; 4],      // serial number as u64s
    facilitator_account_bytes: Vec<u8>, // facilitator Account serialized
    facilitator_keystore_files: Vec<(String, Vec<u8>)>, // keystore file entries
}

/// What the merchant forwards to the facilitator
struct MerchantToFacilitator {
    note_file_bytes: Vec<u8>,        // NoteFile bytes (validated by merchant)
    agent_sk_bytes: Vec<u8>,         // agent signing key
    serial_num_felts: [u64; 4],      // serial number
    facilitator_account_bytes: Vec<u8>,
    facilitator_keystore_files: Vec<(String, Vec<u8>)>,
}

#[tokio::test]
#[ignore = "requires testnet access"]
async fn adn_with_merchant_relay() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // ════════════════════════════════════════════════════════════════════
    // PHASE 1: Agent creates ADN note
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 1: AGENT ══");

    let agent_tmp = tempfile::tempdir()?;
    let (mut agent_client, agent_keystore) = build_client(agent_tmp.path()).await?;
    agent_client.sync_state().await?;
    eprintln!("[agent] client synced");

    // Deploy faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let symbol = TokenSymbol::new("XRLY").map_err(|e| anyhow::anyhow!("{e:?}"))?;
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
    agent_keystore
        .add_key(&faucet_key, faucet_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[agent] faucet deployed: {}", faucet_id.to_hex());
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
    agent_keystore
        .add_key(&agent_key, agent_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!("[agent] wallet deployed: {}", agent_id.to_hex());
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create facilitator wallet (NOT in agent's client)
    let facilitator_key =
        AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
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
    agent_keystore
        .add_key(&facilitator_key, facilitator_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    eprintln!(
        "[agent] facilitator account created: {}",
        facilitator_id.to_hex()
    );

    // Mint tokens to agent
    let mint_asset =
        FungibleAsset::new(faucet_id, 10_000).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, agent_client.rng())
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mint_tx = agent_client
        .submit_new_transaction(faucet_id, mint_req)
        .await?;
    eprintln!("[agent] mint tx: {mint_tx}");

    for attempt in 0..60 {
        agent_client.sync_state().await?;
        let consumable = agent_client
            .get_consumable_notes(Some(agent_id))
            .await?;
        if !consumable.is_empty() {
            eprintln!("[agent] mint consumable (attempt {attempt}), consuming...");
            let notes: Vec<_> = consumable
                .into_iter()
                .map(|(note, _)| note.try_into())
                .collect::<Result<_, _>>()?;
            let consume_req = TransactionRequestBuilder::new()
                .build_consume_notes(notes)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?;
            agent_client
                .submit_new_transaction(agent_id, consume_req)
                .await?;
            break;
        }
        if attempt == 59 {
            anyhow::bail!("timed out waiting for consumable mint note");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[agent] funded");

    // Generate agent signing key
    let agent_signing_key =
        AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let agent_pk: Word = agent_signing_key.public_key().to_commitment().into();

    // Compile ADN script + build note
    let note_script = CodeBuilder::default().compile_note_script(ADN_MASM)?;

    let adn_balance = 1_000u64;
    let asset =
        FungibleAsset::new(faucet_id, adn_balance).map_err(|e| anyhow::anyhow!("{e:?}"))?;
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
    agent_client.rng().fill_bytes(&mut serial_bytes);
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
    eprintln!("[agent] ADN note built: {note_id}");

    // Submit
    let create_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![note.clone()])
        .build()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let tx_id = agent_client
        .submit_new_transaction(agent_id, create_req)
        .await?;
    eprintln!("[agent] ADN submitted: {tx_id}");

    // Wait for inclusion proof and export
    let mut note_file_bytes: Vec<u8> = Vec::new();
    for attempt in 0..60 {
        agent_client.sync_state().await?;
        let output_notes = agent_client
            .get_output_notes(NoteFilter::List(vec![note_id]))
            .await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let note_file = record
                    .into_note_file(&NoteExportType::NoteWithProof)
                    .map_err(|e| anyhow::anyhow!("into_note_file: {e:?}"))?;
                note_file_bytes = note_file.to_bytes();
                eprintln!(
                    "[agent] NoteWithProof exported ({} bytes), attempt {attempt}",
                    note_file_bytes.len()
                );
                break;
            }
        }
        if attempt == 59 {
            anyhow::bail!("timed out waiting for inclusion proof");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Collect keystore files
    let src_ks = agent_tmp.path().join("keystore");
    let mut keystore_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let data = std::fs::read(entry.path())?;
        keystore_files.push((name, data));
    }

    // Package what the agent sends to the merchant
    let serial_felts: [Felt; 4] = serial_num.into();
    let agent_to_merchant = AgentToMerchant {
        note_file_bytes,
        agent_sk_bytes: agent_signing_key.to_bytes(),
        serial_num_felts: [
            serial_felts[0].as_canonical_u64(),
            serial_felts[1].as_canonical_u64(),
            serial_felts[2].as_canonical_u64(),
            serial_felts[3].as_canonical_u64(),
        ],
        facilitator_account_bytes: facilitator_account.to_bytes(),
        facilitator_keystore_files: keystore_files,
    };
    eprintln!("[agent] payload packaged for merchant");

    // Drop agent client — we're done with it
    drop(agent_client);
    drop(agent_keystore);

    // ════════════════════════════════════════════════════════════════════
    // PHASE 2: Merchant receives and validates, then forwards
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 2: MERCHANT ══");

    // Merchant validates the note bytes by deserializing
    let merchant_note_file = NoteFile::read_from_bytes(&agent_to_merchant.note_file_bytes)
        .map_err(|e| anyhow::anyhow!("merchant: NoteFile decode failed: {e}"))?;
    let merchant_note = match &merchant_note_file {
        NoteFile::NoteWithProof(n, _) => n,
        _ => anyhow::bail!("merchant: expected NoteWithProof"),
    };
    eprintln!(
        "[merchant] validated NoteFile ({} bytes), note_id={}",
        agent_to_merchant.note_file_bytes.len(),
        merchant_note.id()
    );
    eprintln!(
        "[merchant]   assets: {}, storage: {}",
        merchant_note.assets().num_assets(),
        merchant_note.recipient().storage().num_items()
    );

    // Merchant could inspect the note here (check amounts, verify signatures, etc.)
    // For now it just validates and forwards.

    // Forward to facilitator (re-serialize to prove roundtrip works)
    let forwarded_bytes = merchant_note_file.to_bytes();
    eprintln!(
        "[merchant] re-serialized: {} bytes (original: {})",
        forwarded_bytes.len(),
        agent_to_merchant.note_file_bytes.len()
    );
    assert_eq!(
        forwarded_bytes, agent_to_merchant.note_file_bytes,
        "merchant roundtrip must be lossless"
    );
    eprintln!("[merchant] roundtrip OK — forwarding to facilitator");

    let merchant_to_facilitator = MerchantToFacilitator {
        note_file_bytes: forwarded_bytes,
        agent_sk_bytes: agent_to_merchant.agent_sk_bytes,
        serial_num_felts: agent_to_merchant.serial_num_felts,
        facilitator_account_bytes: agent_to_merchant.facilitator_account_bytes,
        facilitator_keystore_files: agent_to_merchant.facilitator_keystore_files,
    };

    // ════════════════════════════════════════════════════════════════════
    // PHASE 3: Facilitator consumes the ADN note
    // ════════════════════════════════════════════════════════════════════
    eprintln!("\n══ PHASE 3: FACILITATOR ══");

    // Deserialize everything from the bytes received from merchant
    let facilitator_account =
        Account::read_from_bytes(&merchant_to_facilitator.facilitator_account_bytes)?;
    let facilitator_id = facilitator_account.id();
    eprintln!("[facilitator] id: {}", facilitator_id.to_hex());

    let serial_num: Word = [
        Felt::new(merchant_to_facilitator.serial_num_felts[0]),
        Felt::new(merchant_to_facilitator.serial_num_felts[1]),
        Felt::new(merchant_to_facilitator.serial_num_felts[2]),
        Felt::new(merchant_to_facilitator.serial_num_felts[3]),
    ]
    .into();

    let agent_sk = match AuthSecretKey::read_from_bytes(&merchant_to_facilitator.agent_sk_bytes)
    {
        Ok(sk) => sk,
        Err(_) => {
            let falcon_sk =
                miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey::read_from_bytes(
                    &merchant_to_facilitator.agent_sk_bytes,
                )
                .map_err(|e| anyhow::anyhow!("agent_sk decode: {e}"))?;
            AuthSecretKey::Falcon512Poseidon2(falcon_sk)
        }
    };
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();

    let note_file =
        NoteFile::read_from_bytes(&merchant_to_facilitator.note_file_bytes)
            .map_err(|e| anyhow::anyhow!("facilitator: NoteFile decode: {e}"))?;
    let (note, note_file_for_import) = match &note_file {
        NoteFile::NoteWithProof(n, _) => {
            eprintln!("[facilitator] loaded NoteWithProof");
            (n.clone(), note_file)
        }
        _ => anyhow::bail!("facilitator: expected NoteWithProof"),
    };
    let note_id = note.id();
    eprintln!("[facilitator] note id: {note_id}");

    // Create fresh client
    let fac_tmp = tempfile::tempdir()?;
    let fac_dir = fac_tmp.path();
    let fac_ks_dir = fac_dir.join("keystore");
    std::fs::create_dir_all(&fac_ks_dir)?;
    for (name, data) in &merchant_to_facilitator.facilitator_keystore_files {
        std::fs::write(fac_ks_dir.join(name), data)?;
    }

    let (mut fac_client, _fac_ks) = build_client(fac_dir).await?;

    // Import facilitator account
    fac_client.add_account(&facilitator_account, false).await?;
    eprintln!("[facilitator] account imported");

    // Sync
    fac_client.sync_state().await?;
    eprintln!("[facilitator] synced");

    // Import note
    fac_client.import_notes(&[note_file_for_import]).await?;
    eprintln!("[facilitator] note imported");

    // Wait for authentication
    for attempt in 0..60 {
        fac_client.sync_state().await?;
        if let Ok(Some(record)) = fac_client.get_input_note(note_id).await {
            if record.is_authenticated() {
                eprintln!("[facilitator] note authenticated (attempt {attempt})");
                break;
            }
        }
        if attempt == 59 {
            eprintln!("[facilitator] WARNING: note never authenticated");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Build note_args + Falcon signature
    let debit_amount = 100u64;
    let note_args: Word = [
        facilitator_id.suffix(),
        facilitator_id.prefix().as_felt(),
        Felt::new(debit_amount),
        Felt::ZERO,
    ]
    .into();

    let message: Word = Hasher::merge(&[serial_num.into(), note_args.into()]).into();
    let sig = agent_sk.sign(message);
    let prepared: Vec<Felt> = sig.to_prepared_signature(message);
    let sig_key: Word = Hasher::merge(&[agent_pk.into(), message.into()]).into();

    eprintln!("[facilitator] consuming ADN...");
    eprintln!("[facilitator]   debit_amount: {debit_amount}");
    eprintln!("[facilitator]   prepared_sig len: {}", prepared.len());

    let consume_req = TransactionRequestBuilder::new()
        .input_notes([(note, Some(note_args))])
        .extend_advice_map([(sig_key, prepared.as_slice())])
        .build()
        .map_err(|e| anyhow::anyhow!("consume build: {e:?}"))?;

    match fac_client
        .submit_new_transaction(facilitator_id, consume_req)
        .await
    {
        Ok(tx_id) => {
            eprintln!("[facilitator] ════════════════════════════════════════════════");
            eprintln!("[facilitator] SUCCESS: ADN consumed via merchant relay! tx={tx_id}");
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
