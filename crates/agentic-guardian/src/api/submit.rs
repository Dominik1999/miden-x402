//! `POST /agentic/submit` — the hot-path 8-step verify (NEW_DESIGN §37-50).

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use miden_x402_types::{AgenticPayload, MidenPaymentRequirements, NoteIdHex};

use super::AppState;
use crate::auth::hot_key::verify_hot_signature;
use crate::error::{AgenticError, AgenticResult};
use crate::mandate::ap2::MandateContext;
use crate::storage::{BatchQueueEntry, memory::unix_now};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    pub payment_requirements: MidenPaymentRequirements,
    pub payload: AgenticPayload,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitAck {
    pub queued_id: String,
    pub new_pending_state_commitment: String,
}

pub async fn submit(
    State(state): State<AppState>,
    Json(req): Json<SubmitRequest>,
) -> AgenticResult<Json<SubmitAck>> {
    let payload = req.payload;
    let requirements = req.payment_requirements;

    // 0. Look up agent.
    let agent = state
        .agents
        .get(payload.sender.as_str())
        .await
        .map_err(|e| AgenticError::Storage(e.to_string()))?
        .ok_or(AgenticError::AgentNotRegistered)?;

    // 1. Pending-state alignment.
    let current = state.pending_state.current(payload.sender.as_str()).await?;
    if current.current_commitment_hex != payload.pending_state_commitment.as_str() {
        return Err(AgenticError::PendingStateMismatch {
            claimed: payload.pending_state_commitment.as_str().to_owned(),
            actual: current.current_commitment_hex.clone(),
        });
    }

    // 2. Hot-key signature.
    verify_hot_signature(
        &payload.hot_signature,
        &payload.signed_summary,
        &agent.hot_pubkey_commitment_hex,
    )?;

    // 3. 402 binding — asset + amount.
    if payload.asset != requirements.asset {
        return Err(AgenticError::AssetMismatch);
    }
    if payload.amount != requirements.amount {
        return Err(AgenticError::AssetMismatch);
    }

    // 4. AP2 mandate evaluation.
    let mandate = state
        .mandates
        .get(&payload.mandate_id)
        .await
        .map_err(|e| AgenticError::Storage(e.to_string()))?
        .ok_or_else(|| AgenticError::MandateNotFound(payload.mandate_id.clone()))?;
    let amount_u64: u64 = payload
        .amount
        .parse()
        .map_err(|e| AgenticError::BadRequest(format!("amount not u64: {e}")))?;
    let now = unix_now();
    state
        .policy
        .evaluate(
            &mandate.signed.mandate,
            &MandateContext {
                agent_account_id: &payload.sender,
                amount: amount_u64,
                merchant: &requirements.pay_to,
                now_unix_secs: now,
            },
        )
        .await?;

    // 5. Compute queued_id deterministically.
    let queued_id = compute_queued_id(&payload.serial_num, &payload.signed_summary);

    // 6. Reserve nullifiers + 7. WAL-persist + 8. Advance pending state
    //    — done atomically in the Postgres transaction backing this
    //    request. The skeleton uses memory backends; production
    //    aggregates these into a single `conn.transaction(...)`.
    //
    //    For now we just reserve a sentinel nullifier from the
    //    queued_id so the trait surface is exercised. Real impl
    //    derives nullifiers from `tx_inputs.input_notes` + the future
    //    output P2ID's nullifier.
    let nullifiers = vec![format!("nullifier:{queued_id}")];
    state
        .reservations
        .try_reserve_all(
            &nullifiers,
            state.config.mandate.reservation_ttl,
            &queued_id,
        )
        .await
        .map_err(|e| match e {
            crate::storage::StorageError::Conflict(_) => AgenticError::AlreadyReserved,
            other => AgenticError::Storage(other.to_string()),
        })?;

    // Enqueue.
    state
        .queue
        .enqueue(BatchQueueEntry {
            queued_id: queued_id.clone(),
            agent_account_id: payload.sender.clone(),
            mandate_id: payload.mandate_id.clone(),
            serial_num: payload.serial_num.clone(),
            payer: payload.sender.clone(),
            tx_inputs_b64: payload.tx_inputs.clone(),
            hot_signature_b64: payload.hot_signature.clone(),
            signed_summary_b64: payload.signed_summary.clone(),
            network: requirements.network.to_string(),
            enqueued_at_unix_secs: now,
            submitted: false,
            on_chain_tx_id: None,
        })
        .await?;

    // Advance pending state. The new commitment would normally be
    // derived from the proven `Account.commitment()`; skeleton uses
    // the queued_id as a placeholder.
    let new_commitment_hex = format!("0x{}", "0".repeat(63) + "1");
    state
        .pending_state
        .try_advance(
            payload.sender.as_str(),
            &current.current_commitment_hex,
            &new_commitment_hex,
            current.nonce + 1,
        )
        .await?;

    Ok(Json(SubmitAck {
        queued_id,
        new_pending_state_commitment: new_commitment_hex,
    }))
}

fn compute_queued_id(serial_num: &NoteIdHex, signed_summary_b64: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(serial_num.as_str().as_bytes());
    h.update(b"|");
    h.update(signed_summary_b64.as_bytes());
    format!("0x{}", hex::encode(h.finalize().as_bytes()))
}
