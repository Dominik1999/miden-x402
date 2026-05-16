//! Merchant-side note consumption — the second half of the settled-at-commit
//! model.
//!
//! The buyer's `pay_and_verify` run leaves a public P2ID note on chain that
//! is locked to the MERCHANT account. Under the x402 wire contract the
//! payment is "settled" the moment that note commits — the facilitator
//! confirms it, the merchant returns the resource, and the buyer is done.
//! Actually pulling the tokens into the merchant's vault is a separate,
//! merchant-owned step that can be deferred, batched, or run on a cron.
//!
//! This binary is the bare minimum form of that step: find every unspent
//! note targeting the merchant, consume them all in one transaction,
//! print the merchant's new vault balance.
//!
//! Run with `--release` for the same `miden-client` 0.14.8 reason as the
//! other testnet binaries.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use miden_client::DebugMode;
use miden_client::account::AccountId;
use miden_client::asset::Asset;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::store::{NoteFilter, Store};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client_sqlite_store::SqliteStore;
use miden_protocol::note::Note;
use serde::Deserialize;
use tracing::info;
use tracing_subscriber::EnvFilter;

const STATE_DIR: &str = "./testnet-accounts";
const DEFAULT_FAUCET: &str = "0x0a7d175ed63ec5200fb2ced86f6aa5";

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
            eprintln!("claim_payment: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = PathBuf::from(STATE_DIR);
    let store_path = state.join("store.sqlite3");
    let keys_dir = state.join("keys");
    let roles_path = state.join("roles.json");

    let roles: Roles = serde_json::from_slice(&std::fs::read(&roles_path)?)?;
    let merchant = AccountId::from_hex(&roles.merchant)?;
    let _ = roles.buyer; // unused here; merchant is the executor
    let faucet_id = AccountId::from_hex(DEFAULT_FAUCET)?;

    info!(merchant = %merchant.to_hex(), "claiming as merchant");

    let store = Arc::new(SqliteStore::new(store_path).await?);
    let keystore = FilesystemKeyStore::new(keys_dir)?;

    let mut client = ClientBuilder::for_testnet()
        .store(store.clone() as Arc<dyn Store>)
        .authenticator(Arc::new(keystore))
        .in_debug_mode(DebugMode::Disabled)
        .build()
        .await?;

    info!("syncing state");
    let sync = client.sync_state().await?;
    info!(?sync, "sync complete");

    let unspent = client.get_input_notes(NoteFilter::Unspent).await?;
    let mut to_consume: Vec<Note> = Vec::new();
    let mut total_amount: u64 = 0;
    for record in unspent {
        let assets = record.assets();
        let has_target_asset = assets.iter().any(|a| match a {
            Asset::Fungible(fa) => fa.faucet_id() == faucet_id,
            _ => false,
        });
        if !has_target_asset {
            continue;
        }
        match (&record).try_into() {
            Ok(note) => {
                let amount: u64 = assets
                    .iter()
                    .filter_map(|a| match a {
                        Asset::Fungible(fa) if fa.faucet_id() == faucet_id => Some(fa.amount()),
                        _ => None,
                    })
                    .sum();
                total_amount += amount;
                info!(note_id = %record.id().to_hex(), amount, "queued for consumption");
                to_consume.push(note);
            }
            Err(e) => {
                info!(note_id = %record.id().to_hex(), error = %e, "skipping (no metadata)");
            }
        }
    }

    if to_consume.is_empty() {
        println!("no unspent notes for merchant — nothing to claim");
        return Ok(());
    }

    let pre_balance = vault_balance(store.as_ref(), merchant, faucet_id).await?;
    println!("merchant pre-claim vault balance: {pre_balance}");

    let request =
        TransactionRequestBuilder::new().build_consume_notes(to_consume.clone())?;
    info!(
        n_notes = to_consume.len(),
        total_amount, "submitting consume-notes transaction"
    );

    let tx_id = client.submit_new_transaction(merchant, request).await?;
    info!(tx_id = %tx_id.to_hex(), "consume-notes tx submitted");

    // Poll until the tx commits — the simplest signal is the merchant's
    // vault balance going up.
    let mut post_balance = pre_balance;
    for _ in 0..40 {
        client.sync_state().await?;
        post_balance = vault_balance(store.as_ref(), merchant, faucet_id).await?;
        if post_balance > pre_balance {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    println!("merchant post-claim vault balance: {post_balance}");
    if post_balance > pre_balance {
        println!(
            "merchant claimed {} atomic units (tx {})",
            post_balance - pre_balance,
            tx_id.to_hex(),
        );
        Ok(())
    } else {
        Err("consume tx did not land within timeout".into())
    }
}

async fn vault_balance(
    store: &SqliteStore,
    account: AccountId,
    faucet: AccountId,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let vault = store.get_account_vault(account).await?;
    Ok(vault
        .assets()
        .filter_map(|a| match a {
            Asset::Fungible(fa) if fa.faucet_id() == faucet => Some(fa.amount()),
            _ => None,
        })
        .sum())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
