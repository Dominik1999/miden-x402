//! One-shot Miden testnet setup for the x402 demo.
//!
//! Brings up the on-chain state the bench expects:
//!   1. Deploys a fungible faucet ("X402TEST" token).
//!   2. Deploys `--agents N` agent wallet accounts (each with a fresh Falcon key).
//!   3. Mints one note from the faucet to each agent.
//!   4. Waits for the notes to become consumable, then consumes them.
//!   5. Saves everything the bench needs into `--out-dir`:
//!        - `setup.toml`           — bench config + IDs
//!        - `faucet_id.txt`        — bech32 faucet id (testnet)
//!        - `agents/<i>/account_id.txt`
//!        - `agents/<i>/account_snapshot.b64`  — base64 `Account::write_to_bytes`
//!        - `agents/<i>/hot_key.bin`           — Falcon `SecretKey::write_to_bytes`
//!        - `keystore/`            — miden-client filesystem keystore (shared)
//!        - `store.sqlite3`        — miden-client store
//!
//! Setup is idempotent up to disk: a fresh run wipes `--out-dir`
//! first so it's safe to re-run.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use base64::Engine;
use clap::Parser;
use miden_client::account::component::{AuthControlled, BasicFungibleFaucet, BasicWallet};
use miden_client::account::{AccountBuilder, AccountStorageMode, AccountType};
use miden_client::address::NetworkId;
use miden_client::asset::{FungibleAsset, TokenSymbol};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::NoteType;
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::{Client, Felt};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use guardian_shared::SignatureScheme as GuardianSignatureScheme;
use miden_client::{ClientError, transaction::TransactionExecutorError};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::Serializable;
use rand::RngCore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(about = "One-shot Miden testnet setup for the x402 bench")]
struct Args {
    /// RPC endpoint for the Miden node.
    #[arg(long, default_value = "https://rpc.testnet.miden.io")]
    rpc_endpoint: String,

    /// Number of agent accounts to provision.
    #[arg(long, default_value_t = 1)]
    agents: usize,

    /// Amount of test tokens to mint per agent.
    #[arg(long, default_value_t = 1_000_000u64)]
    mint_amount: u64,

    /// Output directory; will be created if missing. NOTE: contents are
    /// *not* wiped — re-running against an existing dir reuses the
    /// existing miden-client store.
    #[arg(long, default_value = "./testnet-state")]
    out_dir: PathBuf,

    /// How many seconds to wait for consumable notes per agent before giving up.
    #[arg(long, default_value_t = 120u64)]
    consumable_wait_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetupReport {
    rpc_endpoint: String,
    faucet_id_bech32: String,
    faucet_id_hex: String,
    merchant_id_bech32: String,
    merchant_id_hex: String,
    agent_count: usize,
    agents: Vec<AgentRecord>,
    mint_amount: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentRecord {
    index: usize,
    account_id_bech32: String,
    account_id_hex: String,
    /// Path to the base64-encoded `Account` snapshot.
    snapshot_path: String,
    /// Path to the Falcon `SecretKey::write_to_bytes` blob.
    hot_key_path: String,
    commitment_hex: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.out_dir)?;
    tracing::info!(?args, "setup-testnet starting");

    let (mut client, keystore) = build_client(&args).await?;
    client
        .sync_state()
        .await
        .context("initial sync_state failed")?;

    // ─── 1. Deploy faucet ───
    let faucet_init_seed = rand_seed_32(&mut client);
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let symbol = TokenSymbol::new("XTEST")
        .map_err(|e| anyhow::anyhow!("TokenSymbol: {e:?}"))?;
    let max_supply = Felt::new(1_000_000_000);
    let faucet = AccountBuilder::new(faucet_init_seed)
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            faucet_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(
            BasicFungibleFaucet::new(symbol, 6, max_supply)
                .map_err(|e| anyhow::anyhow!("BasicFungibleFaucet: {e:?}"))?,
        )
        .with_component(AuthControlled::allow_all())
        .build()
        .map_err(|e| anyhow::anyhow!("faucet build: {e:?}"))?;
    client
        .add_account(&faucet, false)
        .await
        .context("add faucet account")?;
    keystore
        .add_key(&faucet_key, faucet.id())
        .await
        .map_err(|e| anyhow::anyhow!("faucet keystore add: {e:?}"))?;
    let faucet_id = faucet.id();
    let faucet_bech32 = faucet_id.to_bech32(NetworkId::Testnet);
    tracing::info!(faucet_id = %faucet_bech32, "faucet deployed locally");

    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ─── 1b. Deploy a placeholder merchant account ───
    // The bench's reference-merchant verifies payments by nullifier
    // lookup on the facilitator, but the on-chain tx still needs a
    // real-looking `recipient` `AccountId`. Deploying a public
    // BasicWallet here gives us a stable merchant id for the run.
    let merchant_init_seed = rand_seed_32(&mut client);
    let merchant_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let merchant = AccountBuilder::new(merchant_init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(
            merchant_key.public_key().to_commitment().into(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .map_err(|e| anyhow::anyhow!("merchant build: {e:?}"))?;
    client
        .add_account(&merchant, false)
        .await
        .context("add merchant account")?;
    keystore
        .add_key(&merchant_key, merchant.id())
        .await
        .map_err(|e| anyhow::anyhow!("merchant keystore add: {e:?}"))?;
    let merchant_id_bech32 = merchant.id().to_bech32(NetworkId::Testnet);
    let merchant_id_hex = merchant.id().to_hex();
    tracing::info!(%merchant_id_bech32, "merchant deployed locally");

    // ─── 2. Deploy agents (MultisigGuardian, threshold=1, guardian disabled) ───
    //
    // The auth flow on `MultisigGuardian` surfaces the
    // `TransactionSummary` via the executor's `Unauthorized` error
    // variant when no signature is configured — which is exactly what
    // the design's sign-without-prove pattern needs. The simpler
    // `BasicWallet + AuthSingleSig` path actively signs during
    // execution and fails with `UnknownPublicKey` if the key isn't
    // in the keystore.
    //
    // We disable the guardian co-sig for the agent payment flow: a
    // single-signer agent only needs its own hot-key signature per
    // payment, matching DESIGN.md's per-payment surface.
    let mut agents = Vec::new();
    for i in 0..args.agents {
        tracing::info!(i, "deploying agent account (multisig-guardian, threshold=1)");
        let init_seed = rand_seed_32(&mut client);
        let key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
        let signer_commitment: Word = key.public_key().to_commitment().into();
        let cfg = MultisigGuardianConfig::new(
            1,
            vec![signer_commitment],
            // guardian_commitment unused when guardian_enabled = false.
            Word::default(),
        )
        .with_guardian_enabled(false)
        .with_storage_mode(AccountStorageMode::Public);
        let account = MultisigGuardianBuilder::new(cfg)
            .with_seed(init_seed)
            .with_storage_mode(AccountStorageMode::Public)
            .build()
            .map_err(|e| anyhow::anyhow!("agent {i} build: {e:?}"))?;
        client
            .add_account(&account, false)
            .await
            .context("add agent account")?;
        keystore
            .add_key(&key, account.id())
            .await
            .map_err(|e| anyhow::anyhow!("agent {i} keystore add: {e:?}"))?;
        agents.push((i, account, key));
    }
    client.sync_state().await?;

    // ─── 3. Mint one note from faucet to each agent ───
    for (i, account, _key) in &agents {
        let asset = FungibleAsset::new(faucet_id, args.mint_amount)
            .map_err(|e| anyhow::anyhow!("FungibleAsset: {e:?}"))?;
        let req = TransactionRequestBuilder::new()
            .build_mint_fungible_asset(asset, account.id(), NoteType::Public, client.rng())
            .map_err(|e| anyhow::anyhow!("mint tx build: {e:?}"))?;
        let tx_id = client
            .submit_new_transaction(faucet_id, req)
            .await
            .with_context(|| format!("mint to agent {i}"))?;
        tracing::info!(i, %tx_id, "mint submitted");
    }

    // ─── 4. Wait for consumable, then consume per agent ───
    //
    // Agents use the MultisigGuardian auth component, which DOES NOT
    // auto-sign during execution. We have to drive the sign-then-inject
    // dance manually here too (the same dance the bench's facilitator
    // does for payments).
    for (i, account, key) in &agents {
        let agent_id = account.id();
        let sk = match key {
            AuthSecretKey::Falcon512Poseidon2(sk) => sk.clone(),
            _ => anyhow::bail!("agent {i} expects a Falcon SecretKey"),
        };
        let signer_commitment: Word = sk.public_key().to_commitment().into();

        let deadline = std::time::Instant::now()
            + Duration::from_secs(args.consumable_wait_secs);
        loop {
            client.sync_state().await?;
            let consumable = client
                .get_consumable_notes(Some(agent_id))
                .await
                .with_context(|| format!("get_consumable_notes agent {i}"))?;
            if consumable.is_empty() {
                if std::time::Instant::now() > deadline {
                    anyhow::bail!("agent {i} never saw consumable notes within deadline");
                }
                tracing::info!(i, "no consumable notes yet; waiting…");
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            tracing::info!(i, count = consumable.len(), "consuming notes");
            let notes = consumable
                .into_iter()
                .map(|(note, _)| note.try_into())
                .collect::<Result<Vec<_>, _>>()
                .context("note conversion")?;
            let req = TransactionRequestBuilder::new()
                .build_consume_notes(notes)
                .map_err(|e| anyhow::anyhow!("consume tx build: {e:?}"))?;

            // First execution to surface the TransactionSummary via Unauthorized.
            let summary = execute_for_summary(&mut client, agent_id, req.clone())
                .await
                .with_context(|| format!("agent {i} execute_for_summary"))?;
            let message = summary.to_commitment();
            // Sign the summary commitment.
            let sig = sk.sign(message);
            let sig_hex = format!("0x{}", hex::encode(sig.to_bytes()));
            // Build the advice entry and inject it into the request.
            let parsed = GuardianSignatureScheme::Falcon
                .parse_signature_hex(&sig_hex)
                .map_err(|e| anyhow::anyhow!("parse_signature_hex: {e}"))?;
            let (advice_key, advice_vals) = GuardianSignatureScheme::Falcon
                .build_signature_advice_entry(signer_commitment, message, &parsed, None)
                .map_err(|e| anyhow::anyhow!("build_signature_advice_entry: {e}"))?;
            let mut signed_req = req;
            signed_req.advice_map_mut().insert(advice_key, advice_vals);
            let tx_id = client
                .submit_new_transaction(agent_id, signed_req)
                .await
                .with_context(|| format!("agent {i} consume submit"))?;
            tracing::info!(i, %tx_id, "consume submitted (multisig sign+inject path)");
            // Wait for the consume tx to land before we serialize the account snapshot.
            tokio::time::sleep(Duration::from_secs(6)).await;
            client.sync_state().await?;
            break;
        }
    }

    // ─── 5. Snapshot + save ───
    let agents_dir = args.out_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;
    let mut report_agents = Vec::new();
    for (i, _account_at_deploy, key) in &agents {
        let agent_dir = agents_dir.join(format!("{i:04}"));
        std::fs::create_dir_all(&agent_dir)?;
        // Re-fetch the latest account from the client store (post-consume).
        let agent_at_deploy = &agents[*i].1;
        let id = agent_at_deploy.id();
        let updated = client
            .get_account(id)
            .await
            .context("re-fetch agent account after consume")?
            .ok_or_else(|| anyhow::anyhow!("agent {i} missing from store"))?;
        let snap_bytes = updated.to_bytes();
        let snap_b64 = base64::engine::general_purpose::STANDARD.encode(&snap_bytes);
        let snap_path = agent_dir.join("account_snapshot.b64");
        std::fs::write(&snap_path, &snap_b64)?;
        let key_bytes = match key {
            AuthSecretKey::Falcon512Poseidon2(sk) => sk.to_bytes(),
            _ => anyhow::bail!("agent {i} used non-Falcon key (unexpected)"),
        };
        let hot_key_path = agent_dir.join("hot_key.bin");
        std::fs::write(&hot_key_path, &key_bytes)?;
        std::fs::write(
            agent_dir.join("account_id.txt"),
            id.to_bech32(NetworkId::Testnet),
        )?;
        let commitment_hex = format!(
            "0x{}",
            hex::encode(updated.to_commitment().to_bytes())
        );
        report_agents.push(AgentRecord {
            index: *i,
            account_id_bech32: id.to_bech32(NetworkId::Testnet),
            account_id_hex: id.to_hex(),
            snapshot_path: relative(&args.out_dir, &snap_path),
            hot_key_path: relative(&args.out_dir, &hot_key_path),
            commitment_hex,
        });
        tracing::info!(i, "snapshot saved");
    }

    let report = SetupReport {
        rpc_endpoint: args.rpc_endpoint.clone(),
        faucet_id_bech32: faucet_bech32,
        faucet_id_hex: faucet_id.to_hex(),
        merchant_id_bech32,
        merchant_id_hex,
        agent_count: agents.len(),
        agents: report_agents,
        mint_amount: args.mint_amount,
    };
    let toml_path = args.out_dir.join("setup.toml");
    std::fs::write(&toml_path, toml::to_string_pretty(&report)?)?;
    std::fs::write(args.out_dir.join("faucet_id.txt"), &report.faucet_id_bech32)?;

    tracing::info!(report_path = %toml_path.display(), "setup complete");
    Ok(())
}

async fn build_client(
    args: &Args,
) -> anyhow::Result<(Client<FilesystemKeyStore>, Arc<FilesystemKeyStore>)> {
    let endpoint = Endpoint::try_from(args.rpc_endpoint.as_str())
        .map_err(|e| anyhow::anyhow!("endpoint: {e:?}"))?;
    let rpc = Arc::new(GrpcClient::new(&endpoint, 20_000));
    let keystore = Arc::new(
        FilesystemKeyStore::new(args.out_dir.join("keystore"))
            .map_err(|e| anyhow::anyhow!("keystore: {e:?}"))?,
    );
    let store = args.out_dir.join("store.sqlite3");
    let client = ClientBuilder::new()
        .rpc(rpc)
        .sqlite_store(store)
        .authenticator(keystore.clone())
        .in_debug_mode(false.into())
        .build()
        .await
        .context("client build")?;
    Ok((client, keystore))
}

async fn execute_for_summary(
    client: &mut Client<FilesystemKeyStore>,
    account_id: miden_protocol::account::AccountId,
    request: miden_client::transaction::TransactionRequest,
) -> anyhow::Result<TransactionSummary> {
    match client.execute_transaction(account_id, request).await {
        Ok(_) => anyhow::bail!("expected Unauthorized error carrying summary"),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(other) => Err(anyhow::anyhow!("execute: {other} | debug: {other:?}")),
    }
}

fn rand_seed_32<KS>(client: &mut Client<KS>) -> [u8; 32]
where
    KS: Keystore + 'static,
{
    let mut seed = [0u8; 32];
    client.rng().fill_bytes(&mut seed);
    seed
}

fn relative(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .map(|q| q.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned())
}
