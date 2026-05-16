//! Live testnet smoke test for the Miden x402 facilitator.
//!
//! Takes a public P2ID note that already exists on Miden testnet (the
//! operator submitted it ahead of time with `miden-client-cli` or the M4b
//! Node agent) and runs the facilitator's verification pipeline against it.
//!
//! On success the binary prints the `VerifyResponse::Valid` payer and the
//! `SettleResponse::Success` transaction id, then exits with code 0. Any
//! failure exits non-zero with the underlying error.
//!
//! See [`docs/smoke-testnet.md`](../../../../docs/smoke-testnet.md) for how
//! to create the note and configure the env vars.

use std::process::ExitCode;

use miden_x402_facilitator::{FacilitatorConfig, GrpcMidenNode};
use miden_x402_facilitator::config::FaucetAllowlist;
use miden_x402_facilitator::verifier;
use miden_x402_types::{
    AccountIdHex, AssetTransferMethodTag, ExactScheme, MidenExactExtra, MidenExactPayload,
    MidenPaymentPayload, MidenPaymentRequirements, NoteIdHex, NoteKind, PublicP2idPayload,
    TransactionIdHex, miden_testnet,
};
use tracing::info;
use tracing_subscriber::EnvFilter;
use x402_types::proto::v1::{SettleResponse, VerifyResponse};
use x402_types::proto::v2::X402Version2;

const DEFAULT_RPC_URL: &str = "https://rpc.testnet.miden.io";
const DEFAULT_FAUCET: &str = "0x0a7d175ed63ec5200fb2ced86f6aa5";
const DEFAULT_AMOUNT: &str = "1000";
const DEFAULT_FRESHNESS_BLOCKS: u32 = 1_000_000;
const DEFAULT_RPC_TIMEOUT_MS: u64 = 30_000;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("smoke_testnet: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let env = Env::load()?;
    info!(
        rpc_url = %env.rpc_url,
        buyer = %env.buyer.as_str(),
        merchant = %env.merchant.as_str(),
        faucet = %env.faucet.as_str(),
        note_id = %env.note_id.as_str(),
        tx_id = %env.transaction_id.as_str(),
        block_num = env.block_num,
        amount = %env.amount,
        "loaded smoke env",
    );

    let requirements = MidenPaymentRequirements {
        scheme: ExactScheme,
        network: miden_testnet(),
        amount: env.amount.clone(),
        pay_to: env.merchant.clone(),
        max_timeout_seconds: 120,
        asset: env.faucet.clone(),
        extra: MidenExactExtra {
            asset_transfer_method: AssetTransferMethodTag,
            token_symbol: env.token_symbol.clone(),
            decimals: env.decimals,
            note_type: NoteKind::Public,
        },
    };

    let payload = MidenPaymentPayload {
        accepted: requirements.clone(),
        payload: MidenExactPayload::Public(PublicP2idPayload {
            note_id: env.note_id.clone(),
            transaction_id: env.transaction_id.clone(),
            sender: env.buyer.clone(),
            block_num: env.block_num,
            asset: env.faucet.clone(),
            amount: env.amount.clone(),
        }),
        resource: None,
        x402_version: X402Version2,
        extensions: None,
    };

    let node = GrpcMidenNode::from_url(&env.rpc_url, DEFAULT_RPC_TIMEOUT_MS)?;
    let config = FacilitatorConfig {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        rpc_url: env.rpc_url.clone(),
        rpc_timeout_ms: DEFAULT_RPC_TIMEOUT_MS,
        allowed_faucets: FaucetAllowlist::Any,
        freshness_blocks: env.freshness_blocks,
    };

    info!("calling verifier::verify against live testnet");
    let verify = verifier::verify(&payload, &requirements, &node, &config).await?;
    match &verify {
        VerifyResponse::Valid { payer } => println!("verify: VALID payer={payer}"),
        VerifyResponse::Invalid { reason, payer } => {
            return Err(
                format!("verify returned Invalid: reason={reason} payer={payer:?}").into(),
            );
        }
    }

    info!("calling verifier::settle against live testnet");
    let settle = verifier::settle(&payload, &requirements, &node, &config).await?;
    match settle {
        SettleResponse::Success {
            payer,
            transaction,
            network,
        } => println!("settle: SUCCESS payer={payer} tx={transaction} network={network}"),
        SettleResponse::Error { reason, network } => {
            return Err(
                format!("settle returned Error: reason={reason} network={network}").into(),
            );
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

struct Env {
    rpc_url: String,
    buyer: AccountIdHex,
    merchant: AccountIdHex,
    faucet: AccountIdHex,
    note_id: NoteIdHex,
    transaction_id: TransactionIdHex,
    block_num: u32,
    amount: String,
    token_symbol: String,
    decimals: u8,
    freshness_blocks: u32,
}

impl Env {
    fn load() -> Result<Self, EnvError> {
        Ok(Self {
            rpc_url: env_or("MIDEN_X402_SMOKE_RPC_URL", DEFAULT_RPC_URL),
            buyer: required_id("MIDEN_X402_SMOKE_BUYER")?,
            merchant: required_id("MIDEN_X402_SMOKE_MERCHANT")?,
            faucet: parse_id("MIDEN_X402_SMOKE_FAUCET", DEFAULT_FAUCET)?,
            note_id: required_note_id("MIDEN_X402_SMOKE_NOTE_ID")?,
            transaction_id: required_tx_id("MIDEN_X402_SMOKE_TX_ID")?,
            block_num: required_u32("MIDEN_X402_SMOKE_BLOCK_NUM")?,
            amount: env_or("MIDEN_X402_SMOKE_AMOUNT", DEFAULT_AMOUNT),
            token_symbol: env_or("MIDEN_X402_SMOKE_TOKEN_SYMBOL", "USDC"),
            decimals: optional_u8("MIDEN_X402_SMOKE_DECIMALS", 6)?,
            freshness_blocks: optional_u32("MIDEN_X402_SMOKE_FRESHNESS", DEFAULT_FRESHNESS_BLOCKS)?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum EnvError {
    #[error("missing required env var {0}")]
    Missing(&'static str),
    #[error("invalid {0}: {1}")]
    Invalid(&'static str, String),
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn required_id(key: &'static str) -> Result<AccountIdHex, EnvError> {
    let raw = std::env::var(key).map_err(|_| EnvError::Missing(key))?;
    raw.parse()
        .map_err(|e: miden_x402_types::IdError| EnvError::Invalid(key, e.to_string()))
}

fn parse_id(key: &'static str, default: &str) -> Result<AccountIdHex, EnvError> {
    let raw = std::env::var(key).unwrap_or_else(|_| default.to_owned());
    raw.parse()
        .map_err(|e: miden_x402_types::IdError| EnvError::Invalid(key, e.to_string()))
}

fn required_note_id(key: &'static str) -> Result<NoteIdHex, EnvError> {
    let raw = std::env::var(key).map_err(|_| EnvError::Missing(key))?;
    raw.parse()
        .map_err(|e: miden_x402_types::IdError| EnvError::Invalid(key, e.to_string()))
}

fn required_tx_id(key: &'static str) -> Result<TransactionIdHex, EnvError> {
    let raw = std::env::var(key).map_err(|_| EnvError::Missing(key))?;
    raw.parse()
        .map_err(|e: miden_x402_types::IdError| EnvError::Invalid(key, e.to_string()))
}

fn required_u32(key: &'static str) -> Result<u32, EnvError> {
    let raw = std::env::var(key).map_err(|_| EnvError::Missing(key))?;
    raw.parse()
        .map_err(|e: std::num::ParseIntError| EnvError::Invalid(key, e.to_string()))
}

fn optional_u32(key: &'static str, default: u32) -> Result<u32, EnvError> {
    match std::env::var(key) {
        Ok(raw) => raw
            .parse()
            .map_err(|e: std::num::ParseIntError| EnvError::Invalid(key, e.to_string())),
        Err(_) => Ok(default),
    }
}

fn optional_u8(key: &'static str, default: u8) -> Result<u8, EnvError> {
    match std::env::var(key) {
        Ok(raw) => raw
            .parse()
            .map_err(|e: std::num::ParseIntError| EnvError::Invalid(key, e.to_string())),
        Err(_) => Ok(default),
    }
}
