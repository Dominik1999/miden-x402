//! Syncs the local client against testnet and prints what the BUYER
//! account currently knows about: incoming unconsumed notes (the faucet's
//! P2ID drop is one of these) and on-vault balances.
//!
//! Run after funding the BUYER via the testnet faucet.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use miden_client::DebugMode;
use miden_client::asset::Asset;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::store::{NoteFilter, Store};
use miden_client_sqlite_store::SqliteStore;
use tracing::info;
use tracing_subscriber::EnvFilter;

const STATE_DIR: &str = "./testnet-accounts";

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("check_buyer: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = PathBuf::from(STATE_DIR);
    let store_path = state.join("store.sqlite3");
    let keys_dir = state.join("keys");

    if !store_path.exists() {
        return Err(format!(
            "missing {} — run miden-x402-create-accounts first",
            store_path.display()
        )
        .into());
    }

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

    let account_ids = store.get_account_ids().await?;
    println!();
    println!("--- accounts known to local store ---");
    for id in &account_ids {
        println!("  {}", id.to_hex());
    }

    let unspent = client.get_input_notes(NoteFilter::Unspent).await?;
    println!();
    println!("--- unspent input notes ---");
    if unspent.is_empty() {
        println!("  (none — faucet drop hasn't been seen yet, retry in a few seconds)");
    }
    for note in &unspent {
        let id = note.id();
        let metadata = note.metadata();
        let assets = note.assets();
        println!("  noteId      = 0x{}", id.to_hex().trim_start_matches("0x"));
        if let Some(m) = metadata {
            println!("    sender    = {}", m.sender().to_hex());
            println!("    tag       = {:?}", m.tag());
            println!("    note_type = {:?}", m.note_type());
        }
        for a in assets.iter() {
            match a {
                Asset::Fungible(fa) => {
                    println!(
                        "    asset     = fungible faucet={} amount={}",
                        fa.faucet_id().to_hex(),
                        fa.amount(),
                    );
                }
                Asset::NonFungible(_) => println!("    asset     = non-fungible"),
            }
        }
        println!("    state     = {:?}", note.state());
        println!("    is_auth   = {}", note.is_authenticated());
    }

    println!();
    println!("--- account vaults ---");
    for id in &account_ids {
        let vault = store.get_account_vault(*id).await?;
        let assets: Vec<_> = vault.assets().collect();
        if assets.is_empty() {
            println!("  {}: empty", id.to_hex());
        } else {
            println!("  {}:", id.to_hex());
            for a in assets {
                match a {
                    Asset::Fungible(fa) => println!(
                        "    fungible faucet={} amount={}",
                        fa.faucet_id().to_hex(),
                        fa.amount(),
                    ),
                    Asset::NonFungible(_) => println!("    non-fungible"),
                }
            }
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
