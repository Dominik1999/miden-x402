//! M4a positive-path live testnet test.
//!
//! Uses the BUYER + MERCHANT accounts created by `miden-x402-create-accounts`
//! to:
//!
//!   1. Sync local state.
//!   2. Consume the faucet's unspent P2ID note targeting BUYER and create
//!      a new public P2ID note paying MERCHANT 1000 atomic units, all in
//!      one transaction.
//!   3. Wait for the create-note tx to commit.
//!   4. Build the `MidenPaymentRequirements` + `MidenPaymentPayload` that
//!      the merchant would have received from the buyer in the real x402
//!      flow, then run the facilitator's `verify` + `settle` against live
//!      Miden testnet via `GrpcMidenNode::testnet`.
//!
//! Build/run in `--release` — `miden-client` 0.14.8 trips a debug-only
//! sync assertion that does not occur in release mode.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use miden_client::DebugMode;
use miden_client::account::AccountId;
use miden_client::asset::{Asset, FungibleAsset};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::note::NoteId;
use miden_client::store::{NoteFilter, OutputNoteRecord, Store};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client_sqlite_store::SqliteStore;
use miden_protocol::note::{NoteAttachment, NoteType};
use miden_standards::note::P2idNote;
use miden_x402_facilitator::config::FaucetAllowlist;
use miden_x402_facilitator::{FacilitatorConfig, GrpcMidenNode};
use miden_x402_facilitator::verifier;
use miden_x402_types::{
    AccountIdHex, AssetTransferMethodTag, ExactScheme, MidenExactExtra, MidenExactPayload,
    MidenPaymentPayload, MidenPaymentRequirements, NoteIdHex, NoteKind, PublicP2idPayload,
    TransactionIdHex, miden_testnet,
};
use serde::Deserialize;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use x402_types::proto::v1::{SettleResponse, VerifyResponse};
use x402_types::proto::v2::X402Version2;

const STATE_DIR: &str = "./testnet-accounts";
const DEFAULT_AMOUNT: u64 = 1_000;
const DEFAULT_FAUCET: &str = "0x0a7d175ed63ec5200fb2ced86f6aa5";
const FRESHNESS_BLOCKS: u32 = 1_000_000;
const RPC_TIMEOUT_MS: u64 = 30_000;
const COMMIT_POLL_INTERVAL: Duration = Duration::from_secs(3);
const COMMIT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Deserialize)]
struct Roles {
    buyer: String,
    merchant: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pay_and_verify: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = PathBuf::from(STATE_DIR);
    let store_path = state.join("store.sqlite3");
    let keys_dir = state.join("keys");
    let roles_path = state.join("roles.json");

    if !store_path.exists() {
        return Err(format!(
            "missing {} — run miden-x402-create-accounts first",
            store_path.display()
        )
        .into());
    }
    if !roles_path.exists() {
        return Err(format!(
            "missing {} — run miden-x402-create-accounts first",
            roles_path.display()
        )
        .into());
    }

    let roles: Roles = serde_json::from_slice(&std::fs::read(&roles_path)?)?;
    let buyer = AccountId::from_hex(&roles.buyer)?;
    let merchant = AccountId::from_hex(&roles.merchant)?;
    let faucet_id = AccountId::from_hex(DEFAULT_FAUCET)?;
    info!(
        buyer = %buyer.to_hex(),
        merchant = %merchant.to_hex(),
        faucet = %faucet_id.to_hex(),
        "loaded roles"
    );

    let store = Arc::new(SqliteStore::new(store_path).await?);
    let keystore = FilesystemKeyStore::new(keys_dir)?;

    let mut client = ClientBuilder::for_testnet()
        .store(store.clone() as Arc<dyn Store>)
        .authenticator(Arc::new(keystore))
        .in_debug_mode(DebugMode::Disabled)
        .build()
        .await?;

    info!("syncing state against testnet");
    let sync = client.sync_state().await?;
    info!(?sync, "sync complete");

    // Inspect buyer's vault and incoming notes.
    let buyer_vault = store.get_account_vault(buyer).await?;
    let buyer_balance: u64 = buyer_vault
        .assets()
        .filter_map(|a| match a {
            Asset::Fungible(fa) if fa.faucet_id() == faucet_id => Some(fa.amount()),
            _ => None,
        })
        .sum();
    info!(buyer_balance, "buyer faucet-asset balance in vault");

    let unspent = client.get_input_notes(NoteFilter::Unspent).await?;
    let faucet_note_record = unspent.into_iter().find(|r| {
        r.assets().iter().any(|a| match a {
            Asset::Fungible(fa) => fa.faucet_id() == faucet_id,
            Asset::NonFungible(_) => false,
        })
    });

    // Build the transaction.
    let mut builder = TransactionRequestBuilder::new();

    if let Some(record) = faucet_note_record {
        info!(note_id = %record.id().to_hex(), "consuming faucet P2ID note in same tx");
        let note: miden_protocol::note::Note = (&record).try_into()?;
        builder = builder.input_notes(vec![(note, None)]);
    } else if buyer_balance < DEFAULT_AMOUNT {
        return Err(format!(
            "no unspent faucet note and buyer's vault balance ({} atomic) < {} required",
            buyer_balance, DEFAULT_AMOUNT
        )
        .into());
    }

    let asset = Asset::Fungible(FungibleAsset::new(faucet_id, DEFAULT_AMOUNT)?);
    let p2id = P2idNote::create(
        buyer,
        merchant,
        vec![asset],
        NoteType::Public,
        NoteAttachment::default(),
        client.rng(),
    )?;
    let merchant_note_id = p2id.id();
    info!(merchant_note_id = %merchant_note_id.to_hex(), "constructed merchant-targeted P2ID");

    let request = builder.own_output_notes(vec![p2id]).build()?;

    info!("submitting transaction (execute → prove → submit)");
    let tx_id = client.submit_new_transaction(buyer, request).await?;
    info!(tx_id = %tx_id.to_hex(), "transaction submitted");

    info!(
        merchant_note_id = %merchant_note_id.to_hex(),
        "waiting for merchant note to commit"
    );
    let commit_block = wait_for_commit(&mut client, merchant_note_id).await?;
    info!(block_num = commit_block, "merchant note committed");

    // Build the x402 wire types.
    let merchant_hex: AccountIdHex = merchant.to_hex().parse()?;
    let buyer_hex: AccountIdHex = buyer.to_hex().parse()?;
    let faucet_hex: AccountIdHex = faucet_id.to_hex().parse()?;
    let note_id_hex: NoteIdHex = merchant_note_id.to_hex().parse()?;
    let tx_id_hex: TransactionIdHex = tx_id.to_hex().parse()?;
    let amount_str = DEFAULT_AMOUNT.to_string();

    let requirements = MidenPaymentRequirements {
        scheme: ExactScheme,
        network: miden_testnet(),
        amount: amount_str.clone(),
        pay_to: merchant_hex,
        max_timeout_seconds: 120,
        asset: faucet_hex.clone(),
        extra: MidenExactExtra {
            asset_transfer_method: AssetTransferMethodTag,
            token_symbol: "USDC".to_owned(),
            decimals: 6,
            note_type: NoteKind::Public,
        },
    };

    let payload = MidenPaymentPayload {
        accepted: requirements.clone(),
        payload: MidenExactPayload::Public(PublicP2idPayload {
            note_id: note_id_hex,
            transaction_id: tx_id_hex,
            sender: buyer_hex,
            block_num: commit_block,
            asset: faucet_hex,
            amount: amount_str,
        }),
        resource: None,
        x402_version: X402Version2,
        extensions: None,
    };

    // Run facilitator verify + settle directly (no HTTP) against live testnet.
    let node = GrpcMidenNode::testnet(RPC_TIMEOUT_MS);
    let config = FacilitatorConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        rpc_url: "https://rpc.testnet.miden.io".to_owned(),
        rpc_timeout_ms: RPC_TIMEOUT_MS,
        allowed_faucets: FaucetAllowlist::Any,
        freshness_blocks: FRESHNESS_BLOCKS,
    };

    info!("running facilitator verify against live testnet");
    let verify = verifier::verify(&payload, &requirements, &node, &config).await?;
    match &verify {
        VerifyResponse::Valid { payer } => println!("verify: VALID payer={payer}"),
        VerifyResponse::Invalid { reason, payer } => {
            return Err(format!("verify Invalid: {reason} payer={payer:?}").into());
        }
    }

    info!("running facilitator settle against live testnet");
    let settle = verifier::settle(&payload, &requirements, &node, &config).await?;
    match settle {
        SettleResponse::Success {
            payer,
            transaction,
            network,
        } => println!("settle: SUCCESS payer={payer} tx={transaction} network={network}"),
        SettleResponse::Error { reason, network } => {
            return Err(format!("settle Error: {reason} network={network}").into());
        }
    }

    println!();
    println!("M4a positive path: GREEN");
    Ok(())
}

async fn wait_for_commit(
    client: &mut miden_client::Client<FilesystemKeyStore>,
    note_id: NoteId,
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    let deadline = std::time::Instant::now() + COMMIT_TIMEOUT;
    loop {
        client.sync_state().await?;
        let records = client.get_output_notes(NoteFilter::Unique(note_id)).await?;
        if let Some(record) = records.into_iter().next() {
            if let Some(block_num) = output_note_commit_block(&record) {
                return Ok(block_num);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "merchant note {} did not commit within {:?}",
                note_id.to_hex(),
                COMMIT_TIMEOUT
            )
            .into());
        }
        warn!("note not yet committed; polling");
        tokio::time::sleep(COMMIT_POLL_INTERVAL).await;
    }
}

fn output_note_commit_block(record: &OutputNoteRecord) -> Option<u32> {
    record.inclusion_proof().map(|p| p.location().block_num().as_u32())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
