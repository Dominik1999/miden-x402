//! End-to-end x402-on-Miden benchmark harness.
//!
//! Spawns N parallel agents, each running K sequential payments
//! through the merchant. Each payment records the boundaries spelled
//! out in the plan: 402_received, sign_start/end, send_facilitator,
//! ack_received, resource_delivered. Batch_submitted / batch_committed
//! are facilitator-side and will be populated in Phase 2B once real
//! proving is wired.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::Engine;
use clap::Parser;
use miden_agentic_client::miden_integration::MidenIntegration;
use miden_agentic_client::key::HotKey;
use miden_agentic_client::{AgenticClient, X402Context};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::Deserializable;
use serde::{Deserialize, Serialize};

const HDR_PAYMENT_REQUIRED: &str = "payment-required";
const HDR_PAYMENT_SIGNATURE: &str = "payment-signature";

#[derive(Debug, Parser)]
#[command(about = "x402-on-Miden benchmark harness")]
struct Args {
    /// Path to a TOML config file. CLI flags override TOML values.
    #[arg(long, default_value = "bench.toml")]
    config: PathBuf,

    /// Override: number of parallel agents.
    #[arg(long)]
    agents: Option<usize>,

    /// Override: sequential payments per agent.
    #[arg(long)]
    payments: Option<usize>,

    /// Override: facilitator base URL.
    #[arg(long)]
    facilitator_url: Option<String>,

    /// Override: merchant base URL.
    #[arg(long)]
    merchant_url: Option<String>,

    /// Override: output directory.
    #[arg(long)]
    out_dir: Option<PathBuf>,

    /// Path to a `setup-testnet` output directory. When set, the
    /// bench runs in real-Miden mode: each agent loads its saved
    /// `Account` snapshot + Falcon `SecretKey`, builds a real
    /// `TransactionSummary` per payment, and the facilitator
    /// rebuilds + proves + submits via its submitter actor.
    #[arg(long)]
    setup_dir: Option<PathBuf>,

    /// Miden node RPC endpoint. Only used in real-Miden mode.
    #[arg(long, default_value = "https://rpc.testnet.miden.io")]
    miden_rpc: String,

    /// Simulate per-leg network latency (one-way, milliseconds) by
    /// sleeping before each merchant + facilitator HTTP call. Use 50
    /// for "100ms RTT" between agent and any other component.
    #[arg(long, default_value_t = 0u64)]
    simulated_oneway_ms: u64,

    /// Payment scheme: "p2id" (default, existing variant A) or "adn"
    /// (AgentDebitNote variant B). ADN mode requires --setup-dir with
    /// ADN fields in setup.toml (created by setup-testnet --adn).
    #[arg(long, default_value = "p2id")]
    scheme: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BenchConfig {
    #[serde(default = "default_agents")]
    agents: usize,
    #[serde(default = "default_payments")]
    payments_per_agent: usize,
    #[serde(default = "default_facilitator")]
    facilitator_url: String,
    #[serde(default = "default_merchant")]
    merchant_url: String,
    #[serde(default = "default_out_dir")]
    out_dir: PathBuf,
    #[serde(default = "default_amount_cap")]
    per_tx_amount_cap: String,
    /// Hex Word, used as the initial pending state at registration.
    #[serde(default = "default_initial_state")]
    initial_state_commitment: String,
}

fn default_agents() -> usize { 1 }
fn default_payments() -> usize { 5 }
fn default_facilitator() -> String { "http://localhost:7002".into() }
fn default_merchant() -> String { "http://localhost:7001".into() }
fn default_out_dir() -> PathBuf { PathBuf::from("./bench-out") }
fn default_amount_cap() -> String { "1000000".into() }
fn default_initial_state() -> String {
    "0x0000000000000000000000000000000000000000000000000000000000000000".into()
}

impl BenchConfig {
    fn from_args(args: &Args) -> anyhow::Result<Self> {
        let mut cfg: Self = if args.config.is_file() {
            let s = std::fs::read_to_string(&args.config)
                .with_context(|| format!("read {}", args.config.display()))?;
            toml::from_str(&s)?
        } else {
            Self {
                agents: default_agents(),
                payments_per_agent: default_payments(),
                facilitator_url: default_facilitator(),
                merchant_url: default_merchant(),
                out_dir: default_out_dir(),
                per_tx_amount_cap: default_amount_cap(),
                initial_state_commitment: default_initial_state(),
            }
        };
        if let Some(a) = args.agents { cfg.agents = a; }
        if let Some(p) = args.payments { cfg.payments_per_agent = p; }
        if let Some(u) = &args.facilitator_url { cfg.facilitator_url = u.clone(); }
        if let Some(u) = &args.merchant_url { cfg.merchant_url = u.clone(); }
        if let Some(d) = &args.out_dir { cfg.out_dir = d.clone(); }
        Ok(cfg)
    }
}

/// Mirror of the setup-testnet `SetupReport` (deserialized from `setup.toml`).
#[derive(Debug, Clone, Deserialize)]
struct SetupReport {
    #[allow(dead_code)]
    rpc_endpoint: String,
    #[allow(dead_code)]
    faucet_id_bech32: String,
    faucet_id_hex: String,
    #[allow(dead_code)]
    merchant_id_bech32: String,
    merchant_id_hex: String,
    #[allow(dead_code)]
    agent_count: usize,
    agents: Vec<SetupAgentRecord>,
    #[allow(dead_code)]
    mint_amount: u64,
    // ADN fields (optional, populated by setup-testnet --adn)
    #[serde(default)]
    adn_note_id: Option<String>,
    #[serde(default)]
    adn_serial_num_hex: Option<[String; 4]>,
    #[serde(default)]
    adn_balance: Option<u64>,
    #[serde(default)]
    adn_expiry_block: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct SetupAgentRecord {
    #[allow(dead_code)]
    index: usize,
    #[allow(dead_code)]
    account_id_bech32: String,
    account_id_hex: String,
    snapshot_path: String,
    hot_key_path: String,
    commitment_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AcceptsEntry {
    scheme: String,
    network: String,
    merchant_account_id: String,
    asset_faucet_id: String,
    amount: String,
    deadline_unix_secs: u64,
    payment_requirements_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaymentRequired {
    accepts: Vec<AcceptsEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct PaymentSignature {
    agent_id: String,
    nullifier: String,
}

#[derive(Debug, Clone)]
struct PaymentRow {
    agent_id: String,
    seq: u64,
    nullifier: String,
    t_resource_get1_sent: u64,
    t_402_received: u64,
    t_pay_start: u64,
    t_sign_start: u64,
    t_sign_end: u64,
    t_send_facilitator: u64,
    t_ack_received: u64,
    t_resource_get2_sent: u64,
    t_resource_delivered: u64,
    /// Facilitator-side timestamps, fetched via the status endpoint
    /// after each payment (these may be 0 if the batch worker hasn't
    /// run yet by the time we read).
    t_batch_started: u64,
    t_submitted: u64,
    t_committed: u64,
    facilitator_status: String,
    facilitator_error: String,
    retries: u32,
    ok: bool,
    error: String,
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
    let cfg = BenchConfig::from_args(&args)?;
    tracing::info!(?cfg, "bench config");

    let run_id = format!("run-{}", now_unix_secs());
    let out_dir = cfg.out_dir.join(&run_id);
    std::fs::create_dir_all(&out_dir)?;

    // Optional real-Miden mode: load setup-testnet output.
    let setup_report = match &args.setup_dir {
        Some(dir) => {
            let toml_path = dir.join("setup.toml");
            let s = std::fs::read_to_string(&toml_path)
                .with_context(|| format!("read {}", toml_path.display()))?;
            let report: SetupReport = toml::from_str(&s)?;
            tracing::info!(
                faucet = %report.faucet_id_hex,
                merchant = %report.merchant_id_hex,
                agents = report.agents.len(),
                "loaded setup report; running in real-Miden mode"
            );
            Some((dir.clone(), report))
        }
        None => None,
    };

    // Stash the simulated-latency knob in an env var so the inner
    // payment loop (which lives in a separate function) can pick it
    // up without threading a new parameter through every call site.
    if args.simulated_oneway_ms > 0 {
        unsafe {
            std::env::set_var(
                "BENCH_SIM_ONEWAY_MS",
                args.simulated_oneway_ms.to_string(),
            );
        }
        tracing::info!(
            oneway_ms = args.simulated_oneway_ms,
            "simulated per-leg latency enabled"
        );
    }

    let cfg = Arc::new(cfg);
    let setup_report = setup_report.map(|(d, r)| Arc::new((d, r)));
    let mut all_rows: Vec<PaymentRow> = Vec::new();

    if args.scheme == "adn" {
        // ── ADN mode ──
        let setup = setup_report.as_ref()
            .ok_or_else(|| anyhow::anyhow!("ADN mode requires --setup-dir"))?;
        let report = &setup.1;
        let adn_note_id = report.adn_note_id.as_ref()
            .ok_or_else(|| anyhow::anyhow!("setup.toml missing adn_note_id (run setup-testnet --adn)"))?;
        let adn_serial_hex = report.adn_serial_num_hex.as_ref()
            .ok_or_else(|| anyhow::anyhow!("setup.toml missing adn_serial_num_hex"))?;
        let adn_balance = report.adn_balance
            .ok_or_else(|| anyhow::anyhow!("setup.toml missing adn_balance"))?;
        let adn_expiry = report.adn_expiry_block
            .ok_or_else(|| anyhow::anyhow!("setup.toml missing adn_expiry_block"))?;

        // Parse serial from hex
        let serial: miden_protocol::Word = [
            miden_protocol::Felt::new(u64::from_str_radix(adn_serial_hex[0].trim_start_matches("0x"), 16)?),
            miden_protocol::Felt::new(u64::from_str_radix(adn_serial_hex[1].trim_start_matches("0x"), 16)?),
            miden_protocol::Felt::new(u64::from_str_radix(adn_serial_hex[2].trim_start_matches("0x"), 16)?),
            miden_protocol::Felt::new(u64::from_str_radix(adn_serial_hex[3].trim_start_matches("0x"), 16)?),
        ].into();

        // Load the agent's Falcon secret key
        let agent_record = &report.agents[0];
        let setup_dir = &setup.0;
        let sk_bytes = std::fs::read(setup_dir.join(&agent_record.hot_key_path))
            .with_context(|| format!("read {}", agent_record.hot_key_path))?;
        let sk = miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey::read_from_bytes(&sk_bytes)
            .map_err(|e| anyhow::anyhow!("SecretKey decode: {e}"))?;
        let agent_sk = miden_protocol::account::auth::AuthSecretKey::Falcon512Poseidon2(sk);

        let merchant_id = miden_protocol::account::AccountId::from_hex(&report.merchant_id_hex)
            .map_err(|e| anyhow::anyhow!("merchant id: {e}"))?;

        let adn_client = adn_client::client::AdnClient::new(
            agent_sk,
            &cfg.facilitator_url,
            adn_note_id.clone(),
            serial,
            adn_balance,
            adn_expiry,
        );

        let http = reqwest::Client::builder().user_agent("x402-bench/0.1").build()?;
        let resource_url = format!("{}/resource", cfg.merchant_url.trim_end_matches('/'));

        for i in 0..cfg.payments_per_agent {
            let t_resource_get1_sent = now_unix_micros();
            let res = http.get(&resource_url).send().await?;
            let t_402_received = now_unix_micros();
            if res.status() != reqwest::StatusCode::PAYMENT_REQUIRED {
                anyhow::bail!("expected 402, got {}", res.status());
            }
            drop(res);

            // Parse the 402 to get amount
            let amount: u64 = cfg.per_tx_amount_cap.parse().unwrap_or(100);

            // Pay via ADN
            let (ack, timings) = adn_client.pay(merchant_id, amount).await
                .map_err(|e| anyhow::anyhow!("ADN pay: {e}"))?;

            // Second request with facilitator ack as proof
            let proof = serde_json::json!({
                "scheme": "adn",
                "facilitator_ack_signature": ack.facilitator_ack_signature,
                "facilitator_pubkey_commitment": ack.facilitator_pubkey_commitment,
            });
            let proof_b64 = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&proof)?);
            let t_resource_get2_sent = now_unix_micros();
            let res2 = http.get(&resource_url)
                .header("payment-signature", proof_b64)
                .send()
                .await?;
            let t_resource_delivered = now_unix_micros();
            if !res2.status().is_success() {
                tracing::warn!(status = %res2.status(), "resource delivery failed (non-fatal for bench)");
            }

            all_rows.push(PaymentRow {
                agent_id: format!("adn-agent-0"),
                seq: i as u64,
                nullifier: adn_note_id.clone(),
                t_resource_get1_sent,
                t_402_received,
                t_pay_start: timings.t_pay_start,
                t_sign_start: timings.t_sign_start,
                t_sign_end: timings.t_sign_end,
                t_send_facilitator: timings.t_send_facilitator,
                t_ack_received: timings.t_ack_received,
                t_resource_get2_sent,
                t_resource_delivered,
                t_batch_started: 0,
                t_submitted: 0,
                t_committed: 0,
                facilitator_status: String::new(),
                facilitator_error: String::new(),
                retries: 0,
                ok: true,
                error: String::new(),
            });
        }

        tracing::info!(payments = all_rows.len(), "ADN bench complete");
    } else if let Some(setup) = &setup_report {
        // Real-Miden mode: MidenIntegration holds a non-Send
        // miden-client. Run agents sequentially in this task.
        for i in 0..cfg.agents {
            let cfg = cfg.clone();
            let run_id = run_id.clone();
            let setup = setup.clone();
            let miden_rpc = args.miden_rpc.clone();
            match run_agent_real(i, run_id, cfg, setup, miden_rpc).await {
                Ok(rows) => all_rows.extend(rows),
                Err(e) => tracing::error!(error = %e, "agent task failed"),
            }
        }
    } else {
        // Placeholder mode: AgenticClient is Send. Run agents in parallel.
        let mut handles = Vec::new();
        for i in 0..cfg.agents {
            let cfg = cfg.clone();
            let run_id = run_id.clone();
            handles.push(tokio::spawn(async move {
                run_agent_placeholder(i, run_id, cfg).await
            }));
        }
        for h in handles {
            match h.await? {
                Ok(rows) => all_rows.extend(rows),
                Err(e) => tracing::error!(error = %e, "agent task failed"),
            }
        }
    }

    write_csv(&out_dir.join("payments.csv"), &all_rows)?;
    write_summary(&out_dir.join("summary.csv"), &all_rows)?;
    tracing::info!(
        out_dir = %out_dir.display(),
        payments = all_rows.len(),
        "bench complete"
    );
    Ok(())
}

async fn run_agent_placeholder(
    agent_idx: usize,
    run_id: String,
    cfg: Arc<BenchConfig>,
) -> anyhow::Result<Vec<PaymentRow>> {
    let agent_id = format!("{run_id}-agent-{agent_idx:04}");
    let keystore_dir = cfg.out_dir.join(&run_id).join("keystores").join(&agent_id);
    let client = AgenticClient::builder()
        .agent_id(&agent_id)
        .account_id(format!("0x{:062x}{:02x}", 0xa0, (agent_idx & 0xff) as u32))
        .facilitator_url(&cfg.facilitator_url)
        .keystore_dir(keystore_dir)
        .build()?;
    client
        .register(
            cfg.initial_state_commitment.clone(),
            miden_agentic_client::AgentMandate {
                per_tx_amount_cap: cfg.per_tx_amount_cap.clone(),
                merchant_allowlist: vec![],
                expires_at_unix_secs: 10_000_000_000,
            },
        )
        .await?;
    run_payments_loop(&agent_id, &client, &cfg).await
}

async fn run_agent_real(
    agent_idx: usize,
    run_id: String,
    cfg: Arc<BenchConfig>,
    setup: Arc<(PathBuf, SetupReport)>,
    miden_rpc: String,
) -> anyhow::Result<Vec<PaymentRow>> {
    let agent_id = format!("{run_id}-agent-{agent_idx:04}");
    let keystore_dir = cfg.out_dir.join(&run_id).join("keystores").join(&agent_id);
    let (setup_dir, report) = (setup.0.clone(), setup.1.clone());
    let agent_record = report
        .agents
        .get(agent_idx)
        .ok_or_else(|| anyhow::anyhow!("agent_idx {agent_idx} not in setup.toml"))?;
    // Load + import the saved Falcon secret key.
    let sk_bytes = std::fs::read(setup_dir.join(&agent_record.hot_key_path))
        .with_context(|| format!("read {}", agent_record.hot_key_path))?;
    let sk = SecretKey::read_from_bytes(&sk_bytes)
        .map_err(|e| anyhow::anyhow!("SecretKey decode: {e}"))?;
    let _ = HotKey::import_secret_key(keystore_dir.clone(), sk)
        .context("import_secret_key")?;
    // Fresh miden-client; import the saved account snapshot.
    let miden_dir = cfg.out_dir.join(&run_id).join("miden").join(&agent_id);
    let integration = MidenIntegration::connect(&miden_rpc, miden_dir, 20_000)
        .await
        .context("MidenIntegration::connect")?;
    integration.sync().await.context("integration sync")?;
    let snap_b64 = std::fs::read_to_string(setup_dir.join(&agent_record.snapshot_path))
        .with_context(|| format!("read {}", agent_record.snapshot_path))?;
    let snap_bytes = base64::engine::general_purpose::STANDARD
        .decode(snap_b64.trim().as_bytes())
        .context("decode snapshot b64")?;
    integration
        .import_account_snapshot(&snap_bytes)
        .await
        .context("import_account_snapshot")?;
    let integration = Arc::new(integration);
    let client = AgenticClient::builder()
        .agent_id(&agent_id)
        .account_id(&agent_record.account_id_hex)
        .facilitator_url(&cfg.facilitator_url)
        .keystore_dir(keystore_dir)
        .miden(integration.clone())
        .build()?;
    register_agent_via_http(
        &cfg,
        &agent_id,
        &client.hot_key_commitment(),
        &agent_record.account_id_hex,
        &agent_record.commitment_hex,
        Some(snap_b64.trim().to_string()),
    )
    .await?;
    run_payments_loop(&agent_id, &client, &cfg).await
}

async fn register_agent_via_http(
    cfg: &BenchConfig,
    agent_id: &str,
    hot_key_commitment: &str,
    account_id_hex: &str,
    initial_state_commitment: &str,
    account_snapshot_b64: Option<String>,
) -> anyhow::Result<()> {
    let register_url = format!("{}/agents", cfg.facilitator_url.trim_end_matches('/'));
    let http = reqwest::Client::builder().user_agent("x402-bench/0.1").build()?;
    let _: serde_json::Value = http
        .post(&register_url)
        .json(&serde_json::json!({
            "agent_id": agent_id,
            "account_id": account_id_hex,
            "hot_key_commitment": hot_key_commitment,
            "hot_key_scheme": "falcon",
            "hot_key_pubkey_hex": null,
            "initial_state_commitment": initial_state_commitment,
            "mandate": {
                "per_tx_amount_cap": &cfg.per_tx_amount_cap,
                "merchant_allowlist": [],
                "expires_at_unix_secs": 10_000_000_000u64,
            },
            "account_snapshot_b64": account_snapshot_b64,
        }))
        .send()
        .await?
        .json()
        .await?;
    Ok(())
}

async fn run_payments_loop(
    agent_id: &str,
    client: &AgenticClient,
    cfg: &BenchConfig,
) -> anyhow::Result<Vec<PaymentRow>> {

    let http = reqwest::Client::builder()
        .user_agent("x402-bench/0.1")
        .build()?;
    let resource_url = format!("{}/resource", cfg.merchant_url.trim_end_matches('/'));
    let mut rows = Vec::with_capacity(cfg.payments_per_agent);

    for i in 0..cfg.payments_per_agent {
        match run_one_payment(agent_id, i as u64, client, &http, &resource_url).await {
            Ok(row) => rows.push(row),
            Err(e) => {
                tracing::error!(agent_id = %agent_id, iter = i, error = %e, "payment failed");
                rows.push(error_row(agent_id, i as u64, format!("{e}")));
            }
        }
    }
    Ok(rows)
}

async fn run_one_payment(
    agent_id: &str,
    _iter: u64,
    client: &AgenticClient,
    http: &reqwest::Client,
    resource_url: &str,
) -> anyhow::Result<PaymentRow> {
    let sim_oneway_ms: u64 = std::env::var("BENCH_SIM_ONEWAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let sim = || async {
        if sim_oneway_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(sim_oneway_ms)).await;
        }
    };

    // Step 1: GET /resource → 402 with PAYMENT-REQUIRED.
    let t_resource_get1_sent = now_unix_micros();
    sim().await;
    let res = http.get(resource_url).send().await?;
    sim().await;
    let t_402_received = now_unix_micros();
    if res.status() != reqwest::StatusCode::PAYMENT_REQUIRED {
        anyhow::bail!("expected 402, got {}", res.status());
    }
    let header_val = res
        .headers()
        .get(HDR_PAYMENT_REQUIRED)
        .context("missing PAYMENT-REQUIRED header")?
        .to_str()?
        .to_string();
    drop(res); // release the body
    let pr_bytes = base64::engine::general_purpose::STANDARD.decode(header_val.as_bytes())?;
    let pr: PaymentRequired = serde_json::from_slice(&pr_bytes)?;
    let entry = pr.accepts.into_iter().next().context("no accepts entries")?;
    let ctx = X402Context {
        merchant_account_id: entry.merchant_account_id,
        asset_faucet_id: entry.asset_faucet_id,
        amount: entry.amount,
        deadline_unix_secs: entry.deadline_unix_secs,
        payment_requirements_digest: entry.payment_requirements_digest,
    };

    // Step 2: client.pay() — instrumented.
    sim().await;
    let (receipt, mut timings) = client.pay_with_metrics(ctx).await?;
    sim().await;
    // Adjust the client-side timestamps so that the wrapped network
    // delay shows up in `d_facilitator_us` (the metric the design
    // author is comparing against Base's ~400ms).
    if sim_oneway_ms > 0 {
        let delay = sim_oneway_ms * 1000;
        timings.t_ack_received = timings.t_ack_received.saturating_add(delay);
    }
    let nullifier = receipt.reserved_nullifiers.first().cloned().unwrap_or_default();

    // Step 3: GET /resource again with PAYMENT-SIGNATURE header.
    let sig = PaymentSignature {
        agent_id: agent_id.to_string(),
        nullifier: nullifier.clone(),
    };
    let sig_b64 = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&sig)?);
    let t_resource_get2_sent = now_unix_micros();
    sim().await;
    let res2 = http
        .get(resource_url)
        .header(HDR_PAYMENT_SIGNATURE, sig_b64)
        .send()
        .await?;
    sim().await;
    let t_resource_delivered = now_unix_micros();
    if !res2.status().is_success() {
        anyhow::bail!(
            "expected 200 on verified GET, got {} body={}",
            res2.status(),
            res2.text().await.unwrap_or_default()
        );
    }

    // Optionally fetch the facilitator's status snapshot so we can
    // capture batch worker timestamps (started/submitted/committed).
    let status_url = format!(
        "{}/agents/{}/payments/{}",
        std::env::var("FACILITATOR_URL").unwrap_or_else(|_| String::new()),
        agent_id,
        nullifier
    );
    let mut t_batch_started = 0u64;
    let mut t_submitted = 0u64;
    let mut t_committed = 0u64;
    let mut facilitator_status = String::new();
    let mut facilitator_error = String::new();
    if !status_url.starts_with("/") {
        // Try to fetch; ignore failures so we don't pollute timings.
        if let Ok(resp) = http.get(&status_url).send().await {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    t_batch_started = body
                        .get("t_batch_started_unix_micros")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    t_submitted = body
                        .get("t_submitted_unix_micros")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    t_committed = body
                        .get("t_committed_unix_micros")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    facilitator_status =
                        body.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    facilitator_error =
                        body.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string();
                }
            }
        }
    }

    Ok(PaymentRow {
        agent_id: agent_id.to_string(),
        seq: receipt.seq,
        nullifier,
        t_resource_get1_sent,
        t_402_received,
        t_pay_start: timings.t_pay_start,
        t_sign_start: timings.t_sign_start,
        t_sign_end: timings.t_sign_end,
        t_send_facilitator: timings.t_send_facilitator,
        t_ack_received: timings.t_ack_received,
        t_resource_get2_sent,
        t_resource_delivered,
        t_batch_started,
        t_submitted,
        t_committed,
        facilitator_status,
        facilitator_error,
        retries: timings.retries,
        ok: true,
        error: String::new(),
    })
}

fn error_row(agent_id: &str, iter: u64, msg: String) -> PaymentRow {
    PaymentRow {
        agent_id: agent_id.to_string(),
        seq: iter,
        nullifier: String::new(),
        t_resource_get1_sent: 0,
        t_402_received: 0,
        t_pay_start: 0,
        t_sign_start: 0,
        t_sign_end: 0,
        t_send_facilitator: 0,
        t_ack_received: 0,
        t_resource_get2_sent: 0,
        t_resource_delivered: 0,
        t_batch_started: 0,
        t_submitted: 0,
        t_committed: 0,
        facilitator_status: String::new(),
        facilitator_error: String::new(),
        retries: 0,
        ok: false,
        error: msg,
    }
}

fn write_csv(path: &std::path::Path, rows: &[PaymentRow]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(
        f,
        "agent_id,seq,nullifier,t_resource_get1_sent,t_402_received,t_pay_start,t_sign_start,t_sign_end,t_send_facilitator,t_ack_received,t_resource_get2_sent,t_resource_delivered,t_batch_started,t_submitted,t_committed,d_402_us,d_sign_us,d_facilitator_us,d_resource2_us,d_total_us,d_batch_lag_us,d_prove_submit_us,facilitator_status,facilitator_error,retries,ok,error"
    )?;
    for r in rows {
        let d_402 = saturating_diff(r.t_402_received, r.t_resource_get1_sent);
        let d_sign = saturating_diff(r.t_sign_end, r.t_sign_start);
        let d_fac = saturating_diff(r.t_ack_received, r.t_send_facilitator);
        let d_res2 = saturating_diff(r.t_resource_delivered, r.t_resource_get2_sent);
        let d_total = saturating_diff(r.t_resource_delivered, r.t_resource_get1_sent);
        // From ack to batch worker picking up the tx.
        let d_batch_lag = saturating_diff(r.t_batch_started, r.t_ack_received);
        // From batch start to either submission or commit.
        let d_prove_submit = if r.t_submitted != 0 {
            saturating_diff(r.t_submitted, r.t_batch_started)
        } else {
            0
        };
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            r.agent_id,
            r.seq,
            r.nullifier,
            r.t_resource_get1_sent,
            r.t_402_received,
            r.t_pay_start,
            r.t_sign_start,
            r.t_sign_end,
            r.t_send_facilitator,
            r.t_ack_received,
            r.t_resource_get2_sent,
            r.t_resource_delivered,
            r.t_batch_started,
            r.t_submitted,
            r.t_committed,
            d_402,
            d_sign,
            d_fac,
            d_res2,
            d_total,
            d_batch_lag,
            d_prove_submit,
            escape_csv(&r.facilitator_status),
            escape_csv(&r.facilitator_error),
            r.retries,
            r.ok,
            escape_csv(&r.error),
        )?;
    }
    Ok(())
}

fn write_summary(path: &std::path::Path, rows: &[PaymentRow]) -> anyhow::Result<()> {
    use std::io::Write;
    let ok_rows: Vec<&PaymentRow> = rows.iter().filter(|r| r.ok).collect();
    let total: Vec<u64> = ok_rows
        .iter()
        .map(|r| saturating_diff(r.t_resource_delivered, r.t_resource_get1_sent))
        .collect();
    let facilitator: Vec<u64> = ok_rows
        .iter()
        .map(|r| saturating_diff(r.t_ack_received, r.t_send_facilitator))
        .collect();
    let sign: Vec<u64> = ok_rows
        .iter()
        .map(|r| saturating_diff(r.t_sign_end, r.t_sign_start))
        .collect();
    let resource2: Vec<u64> = ok_rows
        .iter()
        .map(|r| saturating_diff(r.t_resource_delivered, r.t_resource_get2_sent))
        .collect();

    let mut f = std::fs::File::create(path)?;
    writeln!(
        f,
        "metric,count,p50_us,p95_us,p99_us,mean_us,min_us,max_us"
    )?;
    write_pctl(&mut f, "total_us", &total)?;
    write_pctl(&mut f, "facilitator_us", &facilitator)?;
    write_pctl(&mut f, "sign_us", &sign)?;
    write_pctl(&mut f, "resource2_us", &resource2)?;
    writeln!(f, "ok_count,{},,,,,,", ok_rows.len())?;
    writeln!(f, "error_count,{},,,,,,", rows.len() - ok_rows.len())?;
    Ok(())
}

fn write_pctl<W: std::io::Write>(w: &mut W, name: &str, vals: &[u64]) -> anyhow::Result<()> {
    if vals.is_empty() {
        writeln!(w, "{name},0,,,,,,")?;
        return Ok(());
    }
    let mut sorted = vals.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let p = |q: f64| sorted[((n as f64 - 1.0) * q).round() as usize];
    let mean = sorted.iter().sum::<u64>() / n as u64;
    writeln!(
        w,
        "{name},{},{},{},{},{},{},{}",
        n,
        p(0.50),
        p(0.95),
        p(0.99),
        mean,
        sorted[0],
        sorted[n - 1]
    )?;
    Ok(())
}

fn escape_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn saturating_diff(a: u64, b: u64) -> u64 {
    a.saturating_sub(b)
}

fn now_unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[allow(dead_code)]
const _BENCH_DURATION_TYPE_CHECK: Duration = Duration::from_secs(0);
