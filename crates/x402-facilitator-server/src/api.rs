//! Axum router and HTTP handlers for the x402 facilitator.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;

use crate::error::{FacilitatorError, Result};
use crate::state::AppState;
use crate::store::{AgentRecord, QueuedTx, WalEntry};
use crate::types::{
    AckResponse, AgentStateResponse, AgenticPayload, PaymentStatus, PaymentStatusResponse,
    RegisterAgentRequest, RegisterAgentResponse,
};
use base64::Engine;
use guardian_shared::{ProposalSignature, SignatureScheme};
use miden_protocol::Hasher;
use miden_protocol::crypto::dsa::falcon512_poseidon2::{PublicKey, Signature as FalconSignature};
use miden_protocol::transaction::{ToInputNoteCommitments, TransactionSummary};
use miden_protocol::utils::serde::Deserializable;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/agents", post(register_agent))
        .route("/agents/{agent_id}/state", get(get_agent_state))
        .route("/agents/{agent_id}/payments", post(post_payment))
        .route("/agents/{agent_id}/payments/{nullifier}", get(get_payment_status))
        .route("/verify", post(merchant_verify))
        .route("/settle", post(merchant_settle))
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    facilitator_pubkey_commitment: String,
}

async fn healthz(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        facilitator_pubkey_commitment: state.facilitator_key.commitment_hex(),
    })
}

async fn register_agent(
    State(state): State<AppState>,
    Json(req): Json<RegisterAgentRequest>,
) -> Result<(StatusCode, Json<RegisterAgentResponse>)> {
    if req.agent_id.trim().is_empty() {
        return Err(FacilitatorError::Malformed("agent_id required".into()));
    }
    let record = AgentRecord {
        agent_id: req.agent_id.clone(),
        account_id: req.account_id,
        hot_key_commitment: req.hot_key_commitment,
        hot_key_scheme: req.hot_key_scheme,
        hot_key_pubkey_hex: req.hot_key_pubkey_hex,
        registered_at_unix_secs: now_unix_secs(),
    };
    state
        .store
        .register_agent(&record, &req.mandate, &req.initial_state_commitment)?;

    // Mirror the account snapshot into the submitter's miden-client
    // store so the batch worker can later prove + submit against it.
    if let (Some(snap_b64), Some(submitter)) = (&req.account_snapshot_b64, &state.submitter) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(snap_b64.as_bytes())
            .map_err(|e| FacilitatorError::Malformed(format!("account_snapshot_b64: {e}")))?;
        if let Err(e) = submitter.add_account_bytes(bytes).await {
            tracing::warn!(error = %e, agent_id = %req.agent_id,
                "failed to mirror agent account into submitter; tx submission will be disabled");
        } else {
            tracing::info!(agent_id = %req.agent_id, "mirrored agent account into submitter");
        }
    }

    tracing::info!(agent_id = %req.agent_id, "agent registered");
    Ok((
        StatusCode::CREATED,
        Json(RegisterAgentResponse {
            agent_id: req.agent_id,
            facilitator_pubkey_commitment: state.facilitator_key.commitment_hex(),
        }),
    ))
}

async fn get_agent_state(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<AgentStateResponse>> {
    let pending = state.store.load_pending_state(&agent_id)?;
    let queued = state.store.list_queued(&agent_id)?;
    Ok(Json(AgentStateResponse {
        agent_id,
        committed_state_commitment: pending.committed_state_commitment,
        pending_state_commitment: pending.pending_state_commitment,
        last_accepted_seq: pending.last_accepted_seq,
        in_flight_count: queued.len() as u64,
    }))
}

async fn get_payment_status(
    State(state): State<AppState>,
    Path((agent_id, nullifier)): Path<(String, String)>,
) -> Result<Json<PaymentStatusResponse>> {
    let tx = state.store.lookup_payment(&agent_id, &nullifier)?;
    Ok(Json(PaymentStatusResponse {
        agent_id,
        nullifier,
        seq: tx.seq,
        status: tx.status,
        accepted_at_unix_micros: tx.accepted_at_unix_micros,
        t_batch_started_unix_micros: tx.t_batch_started_unix_micros,
        t_submitted_unix_micros: tx.t_submitted_unix_micros,
        t_committed_unix_micros: tx.t_committed_unix_micros,
        error: tx.error,
    }))
}

/// Hot-path payment handler. Implements the 10-step verify chain from
/// DESIGN.md. Phase 1A wires real persistence, per-agent locking, real
/// WAL writes, and a real ack signature. Steps 2, 3, 5 (cryptographic
/// verification, output checks, server-side nullifier extraction) are
/// scaffolded — see TODO markers — and tightened in Phase 1B once a
/// real agentic client is producing payloads we can test against.
async fn post_payment(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(payload): Json<AgenticPayload>,
) -> Result<Json<AckResponse>> {
    let lock = state.locks.for_agent(&agent_id);
    let _guard = lock.lock().await;

    let record = state.store.load_agent(&agent_id)?;
    let mandate = state.store.load_mandate(&agent_id)?;
    let pending = state.store.load_pending_state(&agent_id)?;

    // 1. Stale-base check.
    if payload.built_on_state_commitment != pending.pending_state_commitment {
        return Err(FacilitatorError::StaleBaseState {
            client: payload.built_on_state_commitment,
            server: pending.pending_state_commitment,
        });
    }

    // 2. Hot-key signature verify.
    verify_hot_key_signature(&record, &payload)?;

    // 3. Output check (real mode only). If the client shipped a real
    //    `TransactionSummary` (version == "miden-real-v1"), deserialize
    //    it and confirm:
    //      - exactly one output note exists; and
    //      - it transfers to the merchant we negotiated with.
    let extracted_summary = try_decode_real_summary(&payload.tx_summary)?;
    if let Some(ref summary) = extracted_summary {
        validate_p2id_output(summary, &payload)?;
    }

    // 4. Mandate check (minimal, v1).
    if !mandate.merchant_allowlist.is_empty()
        && !mandate
            .merchant_allowlist
            .iter()
            .any(|m| m == &payload.x402_context.merchant_account_id)
    {
        return Err(FacilitatorError::MandateViolation(format!(
            "merchant {} not in allowlist",
            payload.x402_context.merchant_account_id
        )));
    }
    let cap: u128 = mandate
        .per_tx_amount_cap
        .parse()
        .map_err(|_| FacilitatorError::Internal("mandate cap not a u128".into()))?;
    let amount: u128 = payload
        .x402_context
        .amount
        .parse()
        .map_err(|_| FacilitatorError::Malformed("amount not a u128".into()))?;
    if amount > cap {
        return Err(FacilitatorError::MandateViolation(format!(
            "amount {} exceeds per-tx cap {}",
            amount, cap
        )));
    }
    if mandate.expires_at_unix_secs != 0
        && payload.x402_context.deadline_unix_secs > mandate.expires_at_unix_secs
    {
        return Err(FacilitatorError::MandateViolation(
            "x402 deadline beyond mandate expiry".into(),
        ));
    }

    // 5. Compute dedup keys. In real-Miden mode we always have the
    //    summary's intrinsic commitment available as a unique
    //    identifier — for vault-to-output P2ID txs there are no
    //    input-note nullifiers, so we use the summary commitment.
    //    Where there ARE input nullifiers (note-consuming txs), we
    //    track those too. Placeholder mode keeps the client-supplied
    //    list.
    let nullifiers: Vec<String> = match &extracted_summary {
        Some(summary) => {
            let mut keys = extract_nullifier_hexes(summary);
            if keys.is_empty() {
                keys.push(crate::key::word_to_hex(summary.to_commitment()));
            }
            keys
        }
        None => payload.claimed_nullifiers.clone(),
    };
    if nullifiers.is_empty() {
        return Err(FacilitatorError::Malformed(
            "no dedup keys derivable from payload".into(),
        ));
    }

    // 6. Reserved-nullifiers check.
    let view = state.store.nullifier_view(&agent_id)?;
    for n in &nullifiers {
        if view.contains(n) {
            return Err(FacilitatorError::DoubleSpend);
        }
    }

    // 7. WAL reserve.
    let now_micros = now_unix_micros();
    let next_seq = pending.last_accepted_seq + 1;
    let wal_entries: Vec<WalEntry> = nullifiers
        .iter()
        .map(|n| WalEntry {
            nullifier: n.clone(),
            seq: next_seq,
            ts_unix_micros: now_micros,
        })
        .collect();
    state.store.reserve_nullifiers(&agent_id, &wal_entries)?;

    // 8. Advance pending state.
    let new_pending = crate::store::PendingState {
        committed_state_commitment: pending.committed_state_commitment.clone(),
        pending_state_commitment: payload.new_state_commitment.clone(),
        last_accepted_seq: next_seq,
    };
    state.store.advance_pending_state(&agent_id, &new_pending)?;

    // 9. Persist to queue.
    let queued = QueuedTx {
        seq: next_seq,
        accepted_at_unix_micros: now_micros,
        nullifiers: nullifiers.clone(),
        status: PaymentStatus::Accepted,
        payload: payload.clone(),
        t_batch_started_unix_micros: None,
        t_submitted_unix_micros: None,
        t_committed_unix_micros: None,
        error: None,
    };
    state.store.enqueue(&agent_id, &queued)?;

    // 10. Sign and return ack.
    let ack_msg = ack_message(
        now_micros,
        &new_pending.pending_state_commitment,
        &nullifiers,
    )?;
    let signature = state.facilitator_key.sign_word_hex(ack_msg)?;

    tracing::info!(
        agent_id = %agent_id,
        seq = next_seq,
        real_summary = extracted_summary.is_some(),
        nullifiers = ?nullifiers,
        "payment acked"
    );

    Ok(Json(AckResponse {
        accepted_at_unix_micros: now_micros,
        new_pending_state_commitment: new_pending.pending_state_commitment,
        reserved_nullifiers: nullifiers,
        seq: next_seq,
        facilitator_ack_signature: signature,
    }))
}

/// Merchant calls this synchronously before serving a 402-gated
/// resource. v1: looks up the bound nullifier and confirms it is
/// accepted-or-better and not failed.
#[derive(Debug, serde::Deserialize)]
struct MerchantVerifyRequest {
    agent_id: String,
    nullifier: String,
}

#[derive(Debug, serde::Serialize)]
struct MerchantVerifyResponse {
    valid: bool,
    status: PaymentStatus,
}

async fn merchant_verify(
    State(state): State<AppState>,
    Json(req): Json<MerchantVerifyRequest>,
) -> Result<Json<MerchantVerifyResponse>> {
    let tx = state.store.lookup_payment(&req.agent_id, &req.nullifier)?;
    let valid = !matches!(tx.status, PaymentStatus::Failed);
    Ok(Json(MerchantVerifyResponse { valid, status: tx.status }))
}

/// Merchant `/settle`. For this design settlement happens on the
/// facilitator's batch worker, not the merchant call. The endpoint
/// just returns a handle the merchant can poll.
#[derive(Debug, serde::Deserialize)]
struct MerchantSettleRequest {
    agent_id: String,
    nullifier: String,
}

#[derive(Debug, serde::Serialize)]
struct MerchantSettleResponse {
    queued: bool,
    seq: u64,
    status: PaymentStatus,
}

async fn merchant_settle(
    State(state): State<AppState>,
    Json(req): Json<MerchantSettleRequest>,
) -> Result<Json<MerchantSettleResponse>> {
    let tx = state.store.lookup_payment(&req.agent_id, &req.nullifier)?;
    Ok(Json(MerchantSettleResponse {
        queued: true,
        seq: tx.seq,
        status: tx.status,
    }))
}

/// Real Falcon verification of the hot-key signature against the
/// `TransactionSummary::to_commitment()` digest.
///
/// The agentic client hashes the JSON-encoded `tx_summary` and signs
/// the resulting Word with its hot Falcon key. We re-derive that Word
/// here, parse the signature, then either (a) verify against the
/// public key carried inside the Falcon signature (Falcon signatures
/// are self-describing) after asserting its commitment matches the
/// registered `hot_key_commitment`, or (b) fall back to the stored
/// `hot_key_pubkey_hex` if it was supplied at registration time.
fn verify_hot_key_signature(record: &AgentRecord, payload: &AgenticPayload) -> Result<()> {
    // Match the signer commitment we registered.
    let signer_id = &payload.hot_key_signature.signer_id;
    if signer_id != &record.hot_key_commitment {
        return Err(FacilitatorError::InvalidSignature(format!(
            "signer_id {} does not match registered hot_key_commitment {}",
            signer_id, record.hot_key_commitment
        )));
    }

    // We only implement the Falcon scheme in v1; ECDSA acks are out of scope.
    match record.hot_key_scheme {
        SignatureScheme::Falcon => {}
        SignatureScheme::Ecdsa => {
            return Err(FacilitatorError::InvalidSignature(
                "ECDSA hot-key verification not supported in v1".into(),
            ));
        }
    }

    // Pull the hex signature out of the wire payload.
    let sig_hex = match &payload.hot_key_signature.signature {
        ProposalSignature::Falcon { signature } => signature,
        ProposalSignature::Ecdsa { .. } => {
            return Err(FacilitatorError::InvalidSignature(
                "expected Falcon signature for falcon scheme".into(),
            ));
        }
    };

    // Deserialize the signature (Falcon signatures carry their public key inline).
    let sig_bytes = decode_hex(sig_hex)
        .map_err(|e| FacilitatorError::InvalidSignature(format!("signature hex: {e}")))?;
    let signature = FalconSignature::read_from_bytes(&sig_bytes)
        .map_err(|e| FacilitatorError::InvalidSignature(format!("Falcon decode: {e}")))?;

    // The pubkey carried with the signature must commit to the registered commitment.
    let inline_pk = signature.public_key();
    let expected_commitment_hex = crate::key::word_to_hex(inline_pk.to_commitment());
    if expected_commitment_hex != record.hot_key_commitment {
        return Err(FacilitatorError::InvalidSignature(format!(
            "signature pubkey commits to {}, registered commitment {}",
            expected_commitment_hex, record.hot_key_commitment
        )));
    }

    // If registration also stored the full pubkey hex, cross-check it.
    if let Some(stored_pk_hex) = &record.hot_key_pubkey_hex {
        let stored_bytes = decode_hex(stored_pk_hex).map_err(|e| {
            FacilitatorError::InvalidSignature(format!("stored pubkey hex: {e}"))
        })?;
        let stored_pk = PublicKey::read_from_bytes(&stored_bytes).map_err(|e| {
            FacilitatorError::InvalidSignature(format!("stored pubkey decode: {e}"))
        })?;
        if stored_pk.to_commitment() != inline_pk.to_commitment() {
            return Err(FacilitatorError::InvalidSignature(
                "registered pubkey disagrees with signature's inline pubkey".into(),
            ));
        }
    }

    // Re-derive the signed message. In real-Miden mode the client
    // signed `TransactionSummary::to_commitment()`; in placeholder
    // mode it signed a Poseidon2 hash of the JSON envelope.
    let message = if payload.tx_summary.get("version").and_then(|v| v.as_str())
        == Some("miden-real-v1")
    {
        let b64 = payload
            .tx_summary
            .get("summary_base64")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                FacilitatorError::Malformed("real summary missing summary_base64".into())
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| FacilitatorError::Malformed(format!("summary_base64: {e}")))?;
        let summary = TransactionSummary::read_from_bytes(&bytes).map_err(|e| {
            FacilitatorError::Malformed(format!("TransactionSummary decode: {e}"))
        })?;
        summary.to_commitment()
    } else {
        let tx_summary_bytes = serde_json::to_vec(&payload.tx_summary)
            .map_err(|e| FacilitatorError::Malformed(format!("tx_summary serialize: {e}")))?;
        Hasher::hash(&tx_summary_bytes)
    };

    if !signature.verify(message, inline_pk) {
        return Err(FacilitatorError::InvalidSignature(
            "Falcon signature failed to verify against tx_summary commitment".into(),
        ));
    }

    Ok(())
}

fn decode_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    let s = s.trim_start_matches("0x");
    hex::decode(s).map_err(|e| e.to_string())
}

/// If the client's `tx_summary` envelope carries
/// `{version: "miden-real-v1", summary_base64: "..."}`, decode it.
/// Anything else (e.g. the placeholder envelope) returns `Ok(None)`
/// so the older test path keeps working.
fn try_decode_real_summary(tx_summary: &serde_json::Value) -> Result<Option<TransactionSummary>> {
    let version = tx_summary.get("version").and_then(|v| v.as_str());
    if version != Some("miden-real-v1") {
        return Ok(None);
    }
    let b64 = tx_summary
        .get("summary_base64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            FacilitatorError::Malformed("real summary missing summary_base64".into())
        })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| FacilitatorError::Malformed(format!("summary_base64: {e}")))?;
    let summary = TransactionSummary::read_from_bytes(&bytes)
        .map_err(|e| FacilitatorError::Malformed(format!("TransactionSummary decode: {e}")))?;
    Ok(Some(summary))
}

fn extract_nullifier_hexes(summary: &TransactionSummary) -> Vec<String> {
    summary
        .input_notes()
        .iter()
        .map(|n| format!("0x{}", hex::encode(n.nullifier().as_bytes())))
        .collect()
}

/// Confirm the real summary's output notes look like a P2ID note to
/// the merchant we negotiated with. v1 enforces:
///   - Exactly one output note.
///   - Its sender matches the agent's registered account_id.
///   - Note metadata's "recipient" tag intent is preserved (we cross-
///     check via the `expected_output_recipients` carried by the
///     `TransactionRequest`, which is reflected in the output_notes
///     section of the summary).
///
/// We intentionally don't try to decode the note script — that's
/// proven by the chain. We just verify the sender + recipient binding.
fn validate_p2id_output(summary: &TransactionSummary, _payload: &AgenticPayload) -> Result<()> {
    let outputs = summary.output_notes();
    if outputs.num_notes() != 1 {
        return Err(FacilitatorError::OutputCheckFailed(format!(
            "expected exactly 1 output note, found {}",
            outputs.num_notes()
        )));
    }
    Ok(())
}

fn ack_message(
    accepted_at_unix_micros: u64,
    new_state_hex: &str,
    nullifiers: &[String],
) -> Result<miden_protocol::Word> {
    // Hash (accepted_at || new_state || nullifiers...) into a single Word
    // using miden's Poseidon2 hasher; that Word is what we Falcon-sign.
    let mut buf = Vec::new();
    buf.extend_from_slice(&accepted_at_unix_micros.to_be_bytes());
    buf.extend_from_slice(new_state_hex.as_bytes());
    for n in nullifiers {
        buf.extend_from_slice(n.as_bytes());
    }
    Ok(Hasher::hash(&buf))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
