//! Creates buyer + merchant wallet accounts on Miden testnet and persists
//! their keys + state under `./testnet-accounts/`.
//!
//! Run once. The output prints the two `0x...` account ids; hand those to
//! the testnet faucet to fund the buyer, then point the M4a smoke binary
//! at the same env vars.
//!
//! The local state directory is gitignored — the keys never leave the
//! machine that ran this binary.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use miden_client::DebugMode;
use miden_client::account::component::BasicWallet;
use miden_client::auth::{AuthSecretKey, AuthSingleSig, RPO_FALCON_SCHEME_ID};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::store::Store;
use miden_client_sqlite_store::SqliteStore;
use miden_protocol::account::{Account, AccountBuilder, AccountStorageMode, AccountType};
use miden_protocol::account::component::AccountComponent;
use rand::RngCore;
use tracing::info;
use tracing_subscriber::EnvFilter;

const STATE_DIR: &str = "./testnet-accounts";

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("create_accounts: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = PathBuf::from(STATE_DIR);
    std::fs::create_dir_all(&state)?;
    let store_path = state.join("store.sqlite3");
    let keys_dir = state.join("keys");
    std::fs::create_dir_all(&keys_dir)?;

    info!(?store_path, ?keys_dir, "initialising local state");

    let store = Arc::new(SqliteStore::new(store_path.clone()).await?);
    let keystore = FilesystemKeyStore::new(keys_dir.clone())?;

    let mut client = ClientBuilder::for_testnet()
        .store(store.clone() as Arc<dyn Store>)
        .authenticator(Arc::new(keystore.clone()))
        .in_debug_mode(DebugMode::Disabled)
        .build()
        .await?;

    info!("syncing client state from testnet (this may take a moment on first run)");
    let sync = client.sync_state().await?;
    info!(?sync, "sync complete");

    let buyer = create_wallet("buyer", &mut client, &keystore).await?;
    let merchant = create_wallet("merchant", &mut client, &keystore).await?;

    let buyer_hex = buyer.id().to_hex();
    let merchant_hex = merchant.id().to_hex();

    let roles_path = state.join("roles.json");
    let roles_json = serde_json::json!({
        "buyer": buyer_hex,
        "merchant": merchant_hex,
    });
    std::fs::write(&roles_path, serde_json::to_string_pretty(&roles_json)?)?;
    info!(path = %roles_path.display(), "wrote role mapping");

    println!();
    println!("=================================================================");
    println!("BUYER    {}", buyer_hex);
    println!("MERCHANT {}", merchant_hex);
    println!("=================================================================");
    println!();
    println!("State dir: {}", state.display());
    println!("Store:     {}", store_path.display());
    println!("Keys:      {}", keys_dir.display());
    println!();
    println!("Next steps:");
    println!("  1. Visit https://faucet.testnet.miden.io and fund BUYER with the");
    println!("     default faucet asset 0x0a7d175ed63ec5200fb2ced86f6aa5 (paste BUYER above).");
    println!("  2. Wait for the faucet tx to commit (~1-2 minutes).");
    println!("  3. Run `cargo run -p miden-x402-facilitator --bin miden-x402-pay-and-verify`");
    println!("     to build a P2ID note, submit it, and verify via the facilitator.");

    Ok(())
}

async fn create_wallet(
    role: &str,
    client: &mut miden_client::Client<FilesystemKeyStore>,
    keystore: &FilesystemKeyStore,
) -> Result<Account, Box<dyn std::error::Error + Send + Sync>> {
    let mut init_seed = [0u8; 32];
    rand::rng().fill_bytes(&mut init_seed);

    let key = AuthSecretKey::new_falcon512_poseidon2();
    let auth_component: AccountComponent =
        AuthSingleSig::new(key.public_key().to_commitment(), RPO_FALCON_SCHEME_ID).into();

    let account = AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(auth_component)
        .with_component(BasicWallet)
        .build()?;

    keystore.add_key(&key, account.id()).await?;
    client.add_account(&account, false).await?;

    info!(role, id = %account.id(), "created wallet account");
    Ok(account)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
