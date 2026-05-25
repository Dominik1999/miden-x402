//! adn-agent — CLI that creates an ADN note, signs vouchers, and sends payments.
//!
//! Modes:
//!   setup     — create faucet, accounts, mint, create ADN note, save config
//!   benchmark — sign N vouchers and send to merchant
//!
//! Usage:
//!   adn-agent setup --data-dir /tmp/agent --out-config /tmp/bench-config.json
//!   adn-agent benchmark --config /tmp/bench-config.json --merchant-url http://merchant:7001 --payments 50

use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

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
use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::note::*;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::Word;
use miden_standards::code_builder::CodeBuilder;
use rand::RngCore;

use agent_debit_note::note::AGENT_DEBIT_NOTE_MASM;
use agent_debit_note::types::AgentDebitNoteStorage;
use agent_debit_note::voucher::sign_voucher;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create accounts, mint, create ADN note, save config for benchmark
    Setup {
        #[arg(long, default_value = "/tmp/adn-agent")]
        data_dir: String,
        #[arg(long, default_value = "/tmp/bench-config.json")]
        out_config: String,
        #[arg(long, default_value = "https://rpc.testnet.miden.io")]
        rpc_url: String,
        /// Total ADN balance
        #[arg(long, default_value_t = 100_000)]
        adn_balance: u64,
    },
    /// Run benchmark: sign vouchers and send to merchant
    Benchmark {
        #[arg(long)]
        config: String,
        #[arg(long)]
        merchant_url: String,
        #[arg(long, default_value_t = 50)]
        payments: usize,
        #[arg(long, default_value_t = 1000)]
        amount_per_payment: u64,
    },
}

/// Config saved by setup, consumed by benchmark
#[derive(Serialize, Deserialize)]
struct BenchConfig {
    agent_id_hex: String,
    merchant_id_hex: String,
    facilitator_id_hex: String,
    faucet_id_hex: String,
    agent_sk_hex: String,
    note_file_hex: String,
    serial_num: [String; 4],
    adn_balance: u64,
    // For facilitator + merchant setup
    facilitator_account_b64: String,
    merchant_account_b64: String,
    keystore_dir: String,
}

fn rand_seed(client: &mut Client<FilesystemKeyStore>) -> [u8; 32] {
    let mut s = [0u8; 32];
    client.rng().fill_bytes(&mut s);
    s
}

async fn run_setup(data_dir: &str, out_config: &str, rpc_url: &str, adn_balance: u64) -> anyhow::Result<()> {
    let data_path = std::path::PathBuf::from(data_dir);
    if data_path.exists() { std::fs::remove_dir_all(&data_path)?; }
    std::fs::create_dir_all(data_path.join("keystore"))?;

    let endpoint = Endpoint::try_from(rpc_url).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let rpc = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let ks = Arc::new(FilesystemKeyStore::new(data_path.join("keystore")).unwrap());
    let mut client = ClientBuilder::new()
        .rpc(rpc)
        .sqlite_store(data_path.join("store.sqlite3"))
        .authenticator(ks.clone())
        .in_debug_mode(false.into())
        .build()
        .await?;
    client.sync_state().await?;

    // Faucet
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let faucet = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(faucet_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicFungibleFaucet::new(TokenSymbol::new("XBEN").unwrap(), 6, Felt::new(1_000_000_000)).unwrap())
        .with_component(AuthControlled::allow_all())
        .build().unwrap();
    let faucet_id = faucet.id();
    client.add_account(&faucet, false).await?;
    ks.add_key(&faucet_key, faucet_id).await.unwrap();
    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    eprintln!("[setup] faucet: {}", faucet_id.to_hex());

    // Agent
    let agent_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let agent = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(agent_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let agent_id = agent.id();
    client.add_account(&agent, false).await?;
    ks.add_key(&agent_key, agent_id).await.unwrap();
    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Facilitator
    let fac_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let fac = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(fac_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let facilitator_id = fac.id();
    ks.add_key(&fac_key, facilitator_id).await.unwrap();
    let fac_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &fac.to_bytes());

    // Merchant
    let merch_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let merch = AccountBuilder::new(rand_seed(&mut client))
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthSingleSig::new(merch_key.public_key().to_commitment().into(), AuthSchemeId::Falcon512Poseidon2))
        .with_component(BasicWallet)
        .build().unwrap();
    let merchant_id = merch.id();
    ks.add_key(&merch_key, merchant_id).await.unwrap();
    let merch_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &merch.to_bytes());

    eprintln!("[setup] agent: {}", agent_id.to_hex());
    eprintln!("[setup] facilitator: {}", facilitator_id.to_hex());
    eprintln!("[setup] merchant: {}", merchant_id.to_hex());

    // Mint
    let mint_asset = FungibleAsset::new(faucet_id, adn_balance + 1000).unwrap();
    let mint_req = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(mint_asset, agent_id, NoteType::Public, client.rng()).unwrap();
    client.submit_new_transaction(faucet_id, mint_req).await?;
    for attempt in 0..60 {
        client.sync_state().await?;
        let consumable = client.get_consumable_notes(Some(agent_id)).await?;
        if !consumable.is_empty() {
            eprintln!("[setup] mint consumable (attempt {attempt})");
            let notes: Vec<_> = consumable.into_iter().map(|(n, _)| n.try_into()).collect::<Result<_, _>>()?;
            let req = TransactionRequestBuilder::new().build_consume_notes(notes).unwrap();
            client.submit_new_transaction(agent_id, req).await?;
            break;
        }
        if attempt == 59 { anyhow::bail!("mint timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    client.sync_state().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    eprintln!("[setup] agent funded");

    // Agent signing key
    let agent_signing_sk = miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey::new();
    let user_pk: Word = agent_signing_sk.public_key().to_commitment();

    // Create ADN
    let note_script = CodeBuilder::default().compile_note_script(AGENT_DEBIT_NOTE_MASM)?;
    let asset = FungibleAsset::new(faucet_id, adn_balance).unwrap();
    let storage = AgentDebitNoteStorage::new(user_pk, merchant_id, agent_id, 10_000_000);
    let note_storage: NoteStorage = storage.into();

    let mut serial_bytes = [0u8; 32];
    client.rng().fill_bytes(&mut serial_bytes);
    let serial_num: Word = [
        Felt::new(u64::from_le_bytes(serial_bytes[0..8].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[8..16].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[16..24].try_into().unwrap())),
        Felt::new(u64::from_le_bytes(serial_bytes[24..32].try_into().unwrap())),
    ].into();

    let tag = NoteTag::with_account_target(facilitator_id);
    let metadata = NoteMetadata::new(agent_id, NoteType::Private).with_tag(tag);
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(serial_num, note_script, note_storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();

    let create_req = TransactionRequestBuilder::new().own_output_notes(vec![note]).build().unwrap();
    client.submit_new_transaction(agent_id, create_req).await?;
    eprintln!("[setup] ADN note: {note_id}");

    // Wait for inclusion proof
    let mut note_file_hex = String::new();
    for attempt in 0..60 {
        client.sync_state().await?;
        let output_notes = client.get_output_notes(NoteFilter::List(vec![note_id])).await?;
        if let Some(record) = output_notes.into_iter().next() {
            if record.inclusion_proof().is_some() {
                let nf = record.into_note_file(&NoteExportType::NoteWithProof).unwrap();
                note_file_hex = hex::encode(nf.to_bytes());
                eprintln!("[setup] NoteWithProof: {} bytes (attempt {attempt})", note_file_hex.len() / 2);
                break;
            }
        }
        if attempt == 59 { anyhow::bail!("inclusion proof timeout"); }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let serial_felts: [Felt; 4] = serial_num.into();
    let agent_sk_hex = hex::encode(AuthSecretKey::Falcon512Poseidon2(agent_signing_sk).to_bytes());

    let config = BenchConfig {
        agent_id_hex: agent_id.to_hex(),
        merchant_id_hex: merchant_id.to_hex(),
        facilitator_id_hex: facilitator_id.to_hex(),
        faucet_id_hex: faucet_id.to_hex(),
        agent_sk_hex,
        note_file_hex,
        serial_num: [
            format!("0x{:x}", serial_felts[0].as_canonical_u64()),
            format!("0x{:x}", serial_felts[1].as_canonical_u64()),
            format!("0x{:x}", serial_felts[2].as_canonical_u64()),
            format!("0x{:x}", serial_felts[3].as_canonical_u64()),
        ],
        adn_balance,
        facilitator_account_b64: fac_b64,
        merchant_account_b64: merch_b64,
        keystore_dir: data_path.join("keystore").to_string_lossy().to_string(),
    };

    std::fs::write(out_config, serde_json::to_string_pretty(&config)?)?;
    eprintln!("[setup] config saved to {out_config}");
    eprintln!("[setup] DONE");
    Ok(())
}

async fn run_benchmark(config_path: &str, merchant_url: &str, payments: usize, amount_per: u64) -> anyhow::Result<()> {
    let config: BenchConfig = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
    let merchant_id = AccountId::from_hex(&config.merchant_id_hex)?;

    // Parse agent signing key
    let agent_sk_bytes = hex::decode(&config.agent_sk_hex)?;
    let agent_sk = match AuthSecretKey::read_from_bytes(&agent_sk_bytes) {
        Ok(AuthSecretKey::Falcon512Poseidon2(sk)) => sk,
        _ => anyhow::bail!("expected Falcon512Poseidon2 key"),
    };

    // Mutable state — updated after each settlement
    let mut current_serial = [
        config.serial_num[0].clone(),
        config.serial_num[1].clone(),
        config.serial_num[2].clone(),
        config.serial_num[3].clone(),
    ];
    let mut current_serial_word: Word = [
        Felt::new(parse_hex(&current_serial[0])),
        Felt::new(parse_hex(&current_serial[1])),
        Felt::new(parse_hex(&current_serial[2])),
        Felt::new(parse_hex(&current_serial[3])),
    ].into();
    let mut cumulative = 0u64;
    let mut send_note_file = true; // Send NoteFile on first request and after settlement
    let mut settlements = 0u32;

    let http = reqwest::Client::new();

    eprintln!("\n=== BENCHMARK: {payments} payments of {amount_per} each ===\n");

    let bench_start = std::time::Instant::now();
    let mut voucher_times = Vec::new();

    for i in 1..=payments {
        cumulative += amount_per;

        let sign_start = std::time::Instant::now();
        let sig = sign_voucher(&agent_sk, current_serial_word, merchant_id, cumulative);
        let sign_dur = sign_start.elapsed();
        let sig_hex = hex::encode(sig.to_bytes());

        let send_start = std::time::Instant::now();
        let payment = adn_services::PaymentRequest {
            note_file_hex: if send_note_file { Some(config.note_file_hex.clone()) } else { None },
            agent_sk_hex: config.agent_sk_hex.clone(),
            serial_num: current_serial.clone(),
            cumulative_amount: cumulative,
            signature_hex: sig_hex,
        };
        send_note_file = false; // Only send on first request per batch

        let resp = http
            .post(format!("{merchant_url}/pay"))
            .json(&payment)
            .timeout(Duration::from_secs(300))
            .send()
            .await?;
        let send_dur = send_start.elapsed();

        let payment_resp: adn_services::PaymentResponse = resp.json().await?;
        let total_dur = sign_start.elapsed();
        voucher_times.push(total_dur);

        if payment_resp.success {
            let settled = payment_resp.settlement_occurred.unwrap_or(false);
            eprintln!(
                "[{i:>3}/{payments}] cumulative={cumulative:>8}  sign={:>6.2}ms  rtt={:>8.2}ms  total={:>8.2}ms{}",
                sign_dur.as_secs_f64() * 1000.0,
                send_dur.as_secs_f64() * 1000.0,
                total_dur.as_secs_f64() * 1000.0,
                if settled { "  ← SETTLED" } else { "" },
            );

            // If settlement occurred, update serial and reset cumulative
            if settled {
                settlements += 1;
                if let Some(new_serial) = payment_resp.new_serial_num {
                    current_serial = new_serial;
                    current_serial_word = [
                        Felt::new(parse_hex(&current_serial[0])),
                        Felt::new(parse_hex(&current_serial[1])),
                        Felt::new(parse_hex(&current_serial[2])),
                        Felt::new(parse_hex(&current_serial[3])),
                    ].into();
                    eprintln!("        → new serial[0]={}  cumulative reset to 0", &current_serial[0]);
                }
                cumulative = 0;
                send_note_file = true; // Merchant has new note, but agent doesn't have it
                // Actually the merchant already stored the remainder note from facilitator
                // Agent doesn't need to resend it — merchant will use its stored copy
                send_note_file = false;
            }
        } else {
            eprintln!("[{i:>3}/{payments}] FAILED: {:?}", payment_resp.error);
            if i > 1 { continue; } else { anyhow::bail!("first payment failed"); }
        }
    }

    let bench_dur = bench_start.elapsed();

    // Stats
    let mut times_ms: Vec<f64> = voucher_times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    times_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = times_ms[times_ms.len() / 2];
    let p95 = times_ms[(times_ms.len() as f64 * 0.95) as usize];
    let p99 = times_ms[(times_ms.len() as f64 * 0.99) as usize];
    let mean: f64 = times_ms.iter().sum::<f64>() / times_ms.len() as f64;
    let throughput = payments as f64 / bench_dur.as_secs_f64();

    // Separate settlement latencies from voucher latencies
    let settle_threshold_ms = 1000.0; // anything > 1s is a settlement
    let voucher_only: Vec<f64> = times_ms.iter().filter(|&&t| t < settle_threshold_ms).copied().collect();
    let v_p50 = if !voucher_only.is_empty() { voucher_only[voucher_only.len() / 2] } else { 0.0 };
    let v_mean: f64 = if !voucher_only.is_empty() { voucher_only.iter().sum::<f64>() / voucher_only.len() as f64 } else { 0.0 };

    eprintln!("\n=== RESULTS ===");
    eprintln!("  Payments:      {payments}");
    eprintln!("  Settlements:   {settlements}");
    eprintln!("  Total time:    {:.2}s", bench_dur.as_secs_f64());
    eprintln!("  Throughput:    {throughput:.1} vouchers/sec");
    eprintln!("  --- All (incl. settlement) ---");
    eprintln!("  Latency p50:   {p50:.2}ms");
    eprintln!("  Latency p95:   {p95:.2}ms");
    eprintln!("  Latency p99:   {p99:.2}ms");
    eprintln!("  Latency avg:   {mean:.2}ms");
    eprintln!("  --- Voucher only (off-chain) ---");
    eprintln!("  Voucher p50:   {v_p50:.2}ms");
    eprintln!("  Voucher avg:   {v_mean:.2}ms");
    eprintln!("  Voucher count: {}", voucher_only.len());

    Ok(())
}

fn parse_hex(s: &str) -> u64 {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(s, 16).unwrap()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    match args.command {
        Command::Setup { data_dir, out_config, rpc_url, adn_balance } => {
            run_setup(&data_dir, &out_config, &rpc_url, adn_balance).await
        }
        Command::Benchmark { config, merchant_url, payments, amount_per_payment } => {
            run_benchmark(&config, &merchant_url, payments, amount_per_payment).await
        }
    }
}
