//! Background workers for the x402 facilitator.
//!
//! The batch worker picks up accepted payments per agent, records
//! `t_batch_started_unix_micros`, and attempts to prove + submit to
//! the configured Miden network. State-sync issues between the
//! agent's client and the facilitator's mirror may cause submission
//! to fail; failures are written into the queued record's `error`
//! field and the tx moves to `failed/`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use guardian_shared::{ProposalSignature, SignatureScheme};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::Deserializable;

use crate::state::AppState;
use crate::store::QueuedTx;
use crate::types::PaymentStatus;

#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    pub interval_ms: u64,
    pub max_size: usize,
}

impl BatchConfig {
    pub fn from_env() -> Self {
        let interval_ms = std::env::var("BATCH_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        let max_size = std::env::var("BATCH_MAX_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        Self {
            interval_ms,
            max_size,
        }
    }
}

pub fn spawn_batch_worker(state: AppState, config: BatchConfig) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(config.interval_ms));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracing::info!(?config, "batch worker started");
        loop {
            ticker.tick().await;
            if let Err(e) = run_once(&state, config).await {
                tracing::error!(error = %e, "batch worker tick failed");
            }
        }
    });
}

async fn run_once(state: &AppState, config: BatchConfig) -> anyhow::Result<()> {
    let agents = state.store.list_agents()?;
    for agent_id in agents {
        let queued = state.store.list_queued(&agent_id)?;
        if queued.is_empty() {
            continue;
        }
        let batch: Vec<_> = queued
            .into_iter()
            .filter(|t| matches!(t.status, PaymentStatus::Accepted))
            .take(config.max_size)
            .collect();
        if batch.is_empty() {
            continue;
        }
        let batch_size = batch.len();
        tracing::info!(
            agent_id = %agent_id,
            batch_size,
            "batch worker: starting batch"
        );

        for mut tx in batch {
            tx.t_batch_started_unix_micros = Some(now_unix_micros());
            tx.status = PaymentStatus::Proving;
            // Persist the "proving started" snapshot before we try anything.
            if let Err(e) = state.store.upsert_queued(&agent_id, &tx) {
                tracing::warn!(error = %e, "upsert(proving) failed");
            }

            match attempt_submit(state, &agent_id, &tx).await {
                Ok(()) => {
                    tx.status = PaymentStatus::Submitted;
                    tx.t_submitted_unix_micros = Some(now_unix_micros());
                    let _ = state.store.upsert_queued(&agent_id, &tx);
                    // Note: a real chain confirmation step would set
                    // PaymentStatus::Committed and
                    // `t_committed_unix_micros` after observing block
                    // inclusion. The current submitter only proves +
                    // submits — the inclusion watcher is the next
                    // iteration.
                }
                Err(e) => {
                    tracing::warn!(
                        agent_id = %agent_id,
                        seq = tx.seq,
                        error = %e,
                        "batch worker: submission attempt failed"
                    );
                    tx.status = PaymentStatus::Failed;
                    tx.error = Some(e.to_string());
                    let _ = state.store.upsert_queued(&agent_id, &tx);
                    let _ = state.store.move_to_failed(&agent_id, tx.seq);
                }
            }
        }
    }
    Ok(())
}

/// Prove + submit the signed-unproven `tx` to the network.
///
/// Wire path: the agentic client's "real-Miden" mode ships a base64
/// `TransactionRequest` blob inside the payload's `tx_summary`. We
/// deserialize it, inject the hot-key signature into the request's
/// advice map at the executor-expected key, and call
/// `client.submit_new_transaction(account_id, request)` via the
/// submitter actor — which proves locally and pushes the proven tx
/// to the configured Miden RPC endpoint.
async fn attempt_submit(state: &AppState, agent_id: &str, tx: &QueuedTx) -> anyhow::Result<()> {
    let Some(submitter) = state.submitter.as_ref() else {
        return Err(anyhow::anyhow!(
            "no MidenSubmitter configured; tx accepted but not submittable"
        ));
    };

    // ── 1. Decode the embedded TransactionRequest + TransactionSummary ──
    let summary_blob = tx.payload.tx_summary.clone();
    if summary_blob.get("version").and_then(|v| v.as_str()) != Some("miden-real-v1") {
        return Err(anyhow::anyhow!(
            "tx not in real-Miden mode; nothing to prove (placeholder bench mode)"
        ));
    }
    let request_b64 = summary_blob
        .get("tx_request_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("real summary missing tx_request_base64"))?;
    let request_bytes = base64::engine::general_purpose::STANDARD
        .decode(request_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("tx_request_base64 decode: {e}"))?;
    let summary_b64 = summary_blob
        .get("summary_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("real summary missing summary_base64"))?;
    let summary_bytes = base64::engine::general_purpose::STANDARD
        .decode(summary_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("summary_base64 decode: {e}"))?;
    let summary = TransactionSummary::read_from_bytes(&summary_bytes)
        .map_err(|e| anyhow::anyhow!("summary decode: {e}"))?;
    let message = summary.to_commitment();

    // ── 2. Pull signing material out of the payload + agent record ──
    let signer_id_hex = &tx.payload.hot_key_signature.signer_id;
    let pubkey_commitment = parse_word_hex(signer_id_hex)
        .map_err(|e| anyhow::anyhow!("pubkey commitment hex: {e}"))?;
    let (scheme, signature_hex, pubkey_hex) = match &tx.payload.hot_key_signature.signature {
        ProposalSignature::Falcon { signature } => (
            SignatureScheme::Falcon,
            signature.clone(),
            None::<String>,
        ),
        ProposalSignature::Ecdsa { signature, public_key } => (
            SignatureScheme::Ecdsa,
            signature.clone(),
            public_key.clone(),
        ),
    };

    // Agent account id from the on-disk agent record.
    let record = state.store.load_agent(agent_id)?;
    let account_id = AccountId::from_hex(&record.account_id)
        .map_err(|e| anyhow::anyhow!("AccountId from hex: {e}"))?;

    // ── 3. Hand off to the submitter actor ──
    let tx_id = submitter
        .rebuild_and_submit(
            account_id,
            request_bytes,
            scheme,
            pubkey_commitment,
            message,
            signature_hex,
            pubkey_hex,
        )
        .await
        .map_err(|e| anyhow::anyhow!("submitter: {e}"))?;
    tracing::info!(agent_id = %agent_id, seq = tx.seq, %tx_id, "submitted to chain");
    Ok(())
}

fn parse_word_hex(s: &str) -> std::result::Result<Word, String> {
    use miden_protocol::utils::serde::Deserializable as _;
    let s = s.trim_start_matches("0x");
    let bytes = hex::decode(s).map_err(|e| format!("hex Word: {e}"))?;
    Word::read_from_bytes(&bytes).map_err(|e| format!("Word read: {e}"))
}

fn now_unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
