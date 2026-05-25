//! Approach 1: Guardian verify-before-prove e2e test.
//!
//! Agent has a MultisigGuardian account (threshold=1). On each payment:
//! 1. Agent builds P2ID tx WITHOUT signatures → execute → Unauthorized(TransactionSummary)
//! 2. Agent signs the summary commitment with Falcon
//! 3. Facilitator rebuilds P2ID tx WITH signature advice → submit (proves + submits)
//! 4. Merchant consumes resulting P2ID notes in a batch
//!
//! Run:
//!   RUST_LOG=info cargo test --release -p miden-x402-facilitator --test approach1_guardian_p2id -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use miden_client::account::component::{AuthControlled, BasicFungibleFaucet, BasicWallet};
use miden_client::account::{AccountBuilder, AccountInterfaceExt, AccountStorageMode, AccountType};
use miden_client::asset::{FungibleAsset, TokenSymbol};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::NoteType;
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::store::{NoteExportType, NoteFilter};
use miden_client::transaction::{
    TransactionExecutorError, TransactionRequestBuilder, TransactionSummary,
};
use miden_client::{Client, ClientError, Felt};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_protocol::account::{Account, AccountId};
use miden_protocol::note::*;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_standards::account::interface::AccountInterface;
use miden_standards::note::P2idNote;
use rand::RngCore;

// MultisigGuardian account creation
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};

// Guardian signature scheme for building advice entries
use guardian_shared::SignatureScheme;

type MidenSdkClient = Client<FilesystemKeyStore>;

// ════════════════════════════════════════════════════════════════════════
// Inlined from miden-multisig-client (private modules)
// ════════════════════════════════════════════════════════════════════════

/// Execute a transaction to get TransactionSummary (expects Unauthorized error).
async fn execute_for_summary(
    client: &mut MidenSdkClient,
    account_id: AccountId,
    request: miden_client::transaction::TransactionRequest,
) -> anyhow::Result<TransactionSummary> {
    match client.execute_transaction(account_id, request).await {
        Ok(_) => anyhow::bail!("expected Unauthorized, got success"),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(e) => anyhow::bail!("execute_for_summary: {e}"),
    }
}

/// Build a P2ID transaction request with optional signature advice.
fn build_p2id_request(
    sender: &Account,
    recipient: AccountId,
    assets: Vec<miden_protocol::asset::Asset>,
    salt: Word,
    signature_advice: Vec<(Word, Vec<Felt>)>,
) -> anyhow::Result<miden_client::transaction::TransactionRequest> {
    let mut rng = miden_protocol::crypto::rand::RandomCoin::new(salt);
    let note = P2idNote::create(
        sender.id(),
        recipient,
        assets,
        NoteType::Public,
        Default::default(),
        &mut rng,
    )
    .map_err(|e| anyhow::anyhow!("P2ID note: {e}"))?;

    let send_script = AccountInterface::from_account(sender)
        .build_send_notes_script(&[note.clone().into()], None)
        .map_err(|e| anyhow::anyhow!("send script: {e}"))?;

    let request = TransactionRequestBuilder::new()
        .custom_script(send_script)
        .expected_output_recipients(vec![note.recipient().clone()])
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}

fn generate_salt() -> Word {
    let mut bytes = [0u8; 32];
    rand::Rng::fill(&mut rand::rng(), &mut bytes);
    let mut felts = [Felt::ZERO; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(chunk);
        felts[i] = Felt::new(u64::from_le_bytes(arr));
    }
    felts.into()
}

// ════════════════════════════════════════════════════════════════════════

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

#[tokio::test]
#[ignore = "requires testnet access"]
async fn approach1_guardian_p2id() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    const NUM_PAYMENTS: usize = 5;
    const PAYMENT_AMOUNT: u64 = 100;

    // ══ SETUP ══
    eprintln!("\n══ SETUP ══");

    let agent_tmp = tempfile::tempdir()?;
    let (mut agent_client, agent_ks) = build_client(agent_tmp.path()).await?;
    agent_client.sync_state().await?;

    // 1. Faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let faucet = AccountBuilder::new(rand_seed(&mut agent_client))
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            faucet_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicFungibleFaucet::new(
            TokenSymbol::new("XGRD").unwrap(), 6, Felt::new(1_000_000_000),
        ).unwrap())
        .with_component(AuthControlled::allow_all())
        .build().unwrap();
    let faucet_id = faucet.id();
    agent_client.add_account(&faucet, false).await?;
    agent_ks.add_key(&faucet_key, faucet_id).await.unwrap();
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 2. MultisigGuardian agent (threshold=1, one Falcon signer)
    let agent_falcon_sk = FalconSecretKey::new();
    let signer_commitment: Word = agent_falcon_sk.public_key().to_commitment();
    let guardian_commitment: Word = [Felt::new(9), Felt::new(8), Felt::new(7), Felt::new(6)].into();

    let agent_account = MultisigGuardianBuilder::new(
        MultisigGuardianConfig::new(1, vec![signer_commitment], guardian_commitment),
    )
    .with_seed(rand_seed(&mut agent_client))
    .with_storage_mode(AccountStorageMode::Public)
    .build()?;
    let agent_id = agent_account.id();
    let agent_auth_sk = AuthSecretKey::Falcon512Poseidon2(agent_falcon_sk.clone());
    agent_ks.add_key(&agent_auth_sk, agent_id).await.unwrap();
    agent_client.add_account(&agent_account, false).await?;
    eprintln!("[setup] agent (MultisigGuardian): {}", agent_id.to_hex());
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 3. Merchant (BasicWallet)
    let merchant_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(agent_client.rng());
    let merchant_account = AccountBuilder::new(rand_seed(&mut agent_client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            merchant_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build().unwrap();
    let merchant_id = merchant_account.id();
    agent_ks.add_key(&merchant_key, merchant_id).await.unwrap();
    let merchant_account_bytes = merchant_account.to_bytes();
    eprintln!("[setup] merchant: {}", merchant_id.to_hex());

    // 4. Mint to agent and consume
    let total_needed = (NUM_PAYMENTS as u64) * PAYMENT_AMOUNT + 500;
    let mint_asset = FungibleAsset::new(faucet_id, total_needed).unwrap();
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, agent_client.rng())
        .unwrap();
    agent_client.submit_new_transaction(faucet_id, mint_req).await?;

    for attempt in 0..60 {
        agent_client.sync_state().await?;
        let consumable = agent_client.get_consumable_notes(Some(agent_id)).await?;
        if !consumable.is_empty() {
            eprintln!("[setup] mint consumable (attempt {attempt}), consuming...");

            // Consume the mint note using sign-without-prove pattern
            let notes: Vec<miden_protocol::note::Note> = consumable
                .into_iter().map(|(n, _)| n.try_into()).collect::<Result<_, _>>()?;

            let consume_req = TransactionRequestBuilder::new()
                .build_consume_notes(notes.clone())
                .unwrap();

            // Get summary
            let summary = execute_for_summary(&mut agent_client, agent_id, consume_req).await?;
            let tx_commitment: Word = summary.to_commitment();

            // Sign
            let sig = agent_falcon_sk.sign(tx_commitment);
            let sig_hex = format!("0x{}", hex::encode(sig.to_bytes()));
            let signer_hex = format!("0x{}", hex::encode(signer_commitment.to_bytes()));

            // Build signature advice
            let parsed_sig = SignatureScheme::Falcon
                .parse_signature_hex(&sig_hex)
                .map_err(|e| anyhow::anyhow!("parse sig: {e}"))?;
            let (advice_key, advice_vals) = SignatureScheme::Falcon
                .build_signature_advice_entry(signer_commitment, tx_commitment, &parsed_sig, None)
                .map_err(|e| anyhow::anyhow!("build advice: {e}"))?;

            // Rebuild consume with signature
            let salt = generate_salt();
            let signed_req = TransactionRequestBuilder::new()
                .build_consume_notes(notes)
                .unwrap();
            // We need to inject signature advice — rebuild with extend_advice_map
            let notes2: Vec<miden_protocol::note::Note> = agent_client.get_consumable_notes(Some(agent_id)).await?
                .into_iter().map(|(n, _)| n.try_into()).collect::<Result<_, _>>()?;
            let signed_req = TransactionRequestBuilder::new()
                .input_notes(notes2.into_iter().map(|n| (n, None)))
                .extend_advice_map([(advice_key, advice_vals.as_slice())])
                .auth_arg(salt)
                .build()?;

            agent_client.submit_new_transaction(agent_id, signed_req).await
                .map_err(|e| anyhow::anyhow!("consume mint: {e}"))?;
            eprintln!("[setup] mint consumed");
            break;
        }
        if attempt == 59 { anyhow::bail!("mint timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    agent_client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[setup] agent funded with {total_needed}");

    // Collect keystore files
    let src_ks = agent_tmp.path().join("keystore");
    let mut keystore_files: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(&src_ks)? {
        let entry = entry?;
        keystore_files.push((
            entry.file_name().to_string_lossy().to_string(),
            std::fs::read(entry.path())?,
        ));
    }

    // ══ 5 PAYMENTS ══
    let mut p2id_note_file_bytes_list: Vec<Vec<u8>> = Vec::new();
    let mut used_output_ids: Vec<NoteId> = Vec::new();

    for payment_num in 1..=NUM_PAYMENTS {
        eprintln!("\n══ PAYMENT {payment_num}/{NUM_PAYMENTS} ══");

        // Get fresh account state
        let account_snapshot = agent_client.get_account(agent_id).await?
            .ok_or_else(|| anyhow::anyhow!("agent account not found"))?;

        // 1. Build unsigned P2ID request
        let salt = generate_salt();
        let asset = FungibleAsset::new(faucet_id, PAYMENT_AMOUNT).unwrap();
        let unsigned_req = build_p2id_request(
            &account_snapshot,
            merchant_id,
            vec![asset.into()],
            salt,
            vec![],
        )?;

        // 2. Execute → Unauthorized(TransactionSummary)
        let summary = execute_for_summary(&mut agent_client, agent_id, unsigned_req).await?;
        let tx_commitment: Word = summary.to_commitment();
        eprintln!("[agent] TransactionSummary obtained (sign-without-prove)");

        // 3. Agent signs
        let sig = agent_falcon_sk.sign(tx_commitment);
        let sig_hex = format!("0x{}", hex::encode(sig.to_bytes()));
        let parsed_sig = SignatureScheme::Falcon
            .parse_signature_hex(&sig_hex)
            .map_err(|e| anyhow::anyhow!("parse sig: {e}"))?;
        let (advice_key, advice_vals) = SignatureScheme::Falcon
            .build_signature_advice_entry(signer_commitment, tx_commitment, &parsed_sig, None)
            .map_err(|e| anyhow::anyhow!("build advice: {e}"))?;
        eprintln!("[agent] signed commitment");

        // 4. Facilitator rebuilds with signature → proves + submits
        let asset2 = FungibleAsset::new(faucet_id, PAYMENT_AMOUNT).unwrap();
        let signed_req = build_p2id_request(
            &account_snapshot,
            merchant_id,
            vec![asset2.into()],
            salt,
            vec![(advice_key, advice_vals)],
        )?;

        let tx_id = agent_client.submit_new_transaction(agent_id, signed_req).await
            .map_err(|e| anyhow::anyhow!("submit P2ID: {e}"))?;
        eprintln!("[facilitator] P2ID proved + submitted: {tx_id}");

        // 5. Wait for output note with inclusion proof
        for attempt in 0..60 {
            agent_client.sync_state().await?;
            let output_notes = agent_client.get_output_notes(NoteFilter::All).await?;
            let mut found = false;
            for record in output_notes {
                if record.inclusion_proof().is_some() && !used_output_ids.contains(&record.id()) {
                    if let Ok(nf) = record.clone().into_note_file(&NoteExportType::NoteWithProof) {
                        let nf_bytes = nf.to_bytes();
                        // Check it's a P2ID (2 storage items) not something else
                        if let Ok(NoteFile::NoteWithProof(n, _)) = NoteFile::read_from_bytes(&nf_bytes) {
                            if n.recipient().storage().num_items() == 2 {
                                used_output_ids.push(record.id());
                                p2id_note_file_bytes_list.push(nf_bytes);
                                eprintln!("[facilitator] P2ID note exported: {} (attempt {attempt})", record.id());
                                found = true;
                                break;
                            }
                        }
                    }
                }
            }
            if found { break; }
            if attempt == 59 { anyhow::bail!("P2ID export timeout"); }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    eprintln!("\n══ ALL {NUM_PAYMENTS} PAYMENTS DONE ══");

    // ══ MERCHANT SETTLEMENT ══
    eprintln!("\n══ MERCHANT SETTLEMENT ══");

    let merchant_tmp = tempfile::tempdir()?;
    let merchant_ks_dir = merchant_tmp.path().join("keystore");
    std::fs::create_dir_all(&merchant_ks_dir)?;
    for (name, data) in &keystore_files {
        std::fs::write(merchant_ks_dir.join(name), data)?;
    }
    let (mut merchant_client, _) = build_client(merchant_tmp.path()).await?;
    let merchant_account = Account::read_from_bytes(&merchant_account_bytes)?;
    merchant_client.add_account(&merchant_account, false).await?;
    merchant_client.sync_state().await?;

    let mut p2id_notes: Vec<miden_protocol::note::Note> = Vec::new();
    for (i, nf_bytes) in p2id_note_file_bytes_list.iter().enumerate() {
        let nf = NoteFile::read_from_bytes(nf_bytes)?;
        let (note, nf_import) = match &nf {
            NoteFile::NoteWithProof(n, _) => (n.clone(), nf),
            _ => anyhow::bail!("P2ID #{}: expected NoteWithProof", i + 1),
        };
        eprintln!("[merchant] importing P2ID #{}: {}", i + 1, note.id());
        merchant_client.import_notes(&[nf_import]).await?;
        p2id_notes.push(note);
    }

    // Wait for all to authenticate
    for attempt in 0..60 {
        merchant_client.sync_state().await?;
        let mut all_auth = true;
        for note in &p2id_notes {
            match merchant_client.get_input_note(note.id()).await {
                Ok(Some(rec)) if rec.is_authenticated() => {}
                _ => { all_auth = false; break; }
            }
        }
        if all_auth {
            eprintln!("[merchant] all authenticated (attempt {attempt})");
            break;
        }
        if attempt == 59 { anyhow::bail!("P2ID auth timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    // Batch consume
    let batch_notes: Vec<(miden_protocol::note::Note, Option<Word>)> = p2id_notes
        .into_iter().map(|n| (n, None)).collect();
    let batch_req = TransactionRequestBuilder::new()
        .input_notes(batch_notes)
        .build()?;
    let settlement_tx = merchant_client.submit_new_transaction(merchant_id, batch_req).await
        .map_err(|e| anyhow::anyhow!("batch consume: {e}"))?;

    let total = PAYMENT_AMOUNT * NUM_PAYMENTS as u64;
    eprintln!("[merchant] ════════════════════════════════════════════════");
    eprintln!("[merchant] SETTLEMENT SUCCESS: tx={settlement_tx}");
    eprintln!("[merchant]   {NUM_PAYMENTS} P2ID notes, total={total}");
    eprintln!("[merchant] ════════════════════════════════════════════════");

    eprintln!("\n══ APPROACH 1 (Guardian verify-before-prove) COMPLETE ══");

    Ok(())
}
