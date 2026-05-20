//! `AgenticClient` — the agent-side hot path.

use std::sync::Arc;

use guardian_shared::{DeltaSignature, ProposalSignature, SignatureScheme};
use miden_protocol::Hasher;
use miden_protocol::account::AccountId;
use tokio::sync::RwLock;

use crate::miden_integration::{
    MidenIntegration, extract_input_nullifiers_hex, request_to_base64, summary_to_base64,
};

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AgenticError, Result};
use crate::key::HotKey;
use crate::transport::FacilitatorClient;
use crate::types::*;

fn now_unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Agent-side client. Build with [`AgenticClientBuilder`].
pub struct AgenticClient {
    agent_id: String,
    account_id: String,
    hot_key: HotKey,
    facilitator: FacilitatorClient,
    pending_cache: Arc<RwLock<PendingCache>>,
    miden: Option<Arc<MidenIntegration>>,
}

#[derive(Debug, Clone)]
struct PendingCache {
    pending_state_commitment: String,
    last_accepted_seq: u64,
    nullifier_salt: u64,
}

impl AgenticClient {
    pub fn builder() -> AgenticClientBuilder {
        AgenticClientBuilder::default()
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn hot_key_commitment(&self) -> String {
        self.hot_key.commitment_hex()
    }

    /// One-shot registration. The caller supplies an initial Miden account
    /// state commitment — typically the agent's on-chain commitment at
    /// registration time — and the mandate to enforce.
    pub async fn register(
        &self,
        initial_state_commitment: String,
        mandate: AgentMandate,
    ) -> Result<RegisterAgentResponse> {
        let req = RegisterAgentRequest {
            agent_id: self.agent_id.clone(),
            account_id: self.account_id.clone(),
            hot_key_commitment: self.hot_key.commitment_hex(),
            hot_key_scheme: SignatureScheme::Falcon,
            hot_key_pubkey_hex: None,
            initial_state_commitment: initial_state_commitment.clone(),
            mandate,
        };
        let res = self.facilitator.register_agent(&req).await?;
        let mut cache = self.pending_cache.write().await;
        cache.pending_state_commitment = initial_state_commitment;
        cache.last_accepted_seq = 0;
        Ok(res)
    }

    /// Force-refresh the pending-state cache against the facilitator.
    pub async fn refresh_state(&self) -> Result<AgentStateResponse> {
        let state = self.facilitator.get_state(&self.agent_id).await?;
        let mut cache = self.pending_cache.write().await;
        cache.pending_state_commitment = state.pending_state_commitment.clone();
        cache.last_accepted_seq = state.last_accepted_seq;
        Ok(state)
    }

    /// Hot path. Build a payment payload for the given x402 context,
    /// sign with the hot key, submit to the facilitator. Retries once
    /// if the facilitator returns STALE_BASE_STATE.
    pub async fn pay(&self, ctx: X402Context) -> Result<PaymentReceipt> {
        Ok(self.pay_with_metrics(ctx).await?.0)
    }

    /// Same as [`pay`] but also returns a per-call timing breakdown
    /// (`PayTimings`). Used by the bench harness.
    pub async fn pay_with_metrics(
        &self,
        ctx: X402Context,
    ) -> Result<(PaymentReceipt, PayTimings)> {
        let mut timings = PayTimings::default();
        timings.t_pay_start = now_unix_micros();
        match self.pay_once(&ctx, &mut timings).await {
            Ok(receipt) => Ok((receipt, timings)),
            Err(e) if e.is_stale_base() => {
                tracing::warn!("stale base after first attempt; refreshing and retrying once");
                timings.retries = timings.retries.saturating_add(1);
                self.refresh_state().await?;
                let receipt = self.pay_once(&ctx, &mut timings).await?;
                Ok((receipt, timings))
            }
            Err(e) => Err(e),
        }
    }

    async fn pay_once(
        &self,
        ctx: &X402Context,
        timings: &mut PayTimings,
    ) -> Result<PaymentReceipt> {
        let (built_on, next_seq) = {
            let cache = self.pending_cache.read().await;
            (cache.pending_state_commitment.clone(), cache.last_accepted_seq + 1)
        };

        if let Some(miden) = &self.miden {
            // ─── Real Miden path: build a P2ID tx, execute for summary ───
            let recipient = parse_account_id(&ctx.merchant_account_id)?;
            let faucet = parse_account_id(&ctx.asset_faucet_id)?;
            let amount: u64 = ctx
                .amount
                .parse()
                .map_err(|_| AgenticError::Config("amount must be u64 for real path".into()))?;
            let request = miden.build_p2id_request(recipient, faucet, amount).await?;
            let request_b64 = request_to_base64(&request);
            let summary = miden.execute_for_summary(request).await?;

            // The Word we sign is the summary's own commitment.
            let summary_commitment = summary.to_commitment();
            timings.t_sign_start = now_unix_micros();
            let signature_hex = self.hot_key.sign_word_hex(summary_commitment)?;
            timings.t_sign_end = now_unix_micros();

            // Wire-encode the summary, the consumed-note nullifiers,
            // and the original TransactionRequest the facilitator
            // needs to rebuild + prove + submit.
            let tx_summary_json = serde_json::json!({
                "version": "miden-real-v1",
                "summary_base64": summary_to_base64(&summary),
                "tx_request_base64": request_b64,
                "commitment_hex": word_hex(summary_commitment),
            });
            let nullifiers = extract_input_nullifiers_hex(&summary);
            // Use the summary's own commitment as the new pending-state
            // identifier — it's a deterministic, agent-and-facilitator
            // agreed digest of the entire transition.
            let next_state_hex = word_hex(summary_commitment);

            self
                .ship_payload(
                    ctx,
                    timings,
                    built_on,
                    next_seq,
                    tx_summary_json,
                    signature_hex,
                    next_state_hex,
                    nullifiers,
                )
                .await
        } else {
            // ─── Placeholder path (kept for tests w/o a Miden node) ───
            let tx_summary = serde_json::json!({
                "version": "agentic-v1-placeholder",
                "agent_id": self.agent_id,
                "account_id": self.account_id,
                "built_on": built_on,
                "ctx": ctx,
                "seq": next_seq,
            });

            timings.t_sign_start = now_unix_micros();
            let summary_bytes =
                serde_json::to_vec(&tx_summary).map_err(AgenticError::Serialize)?;
            let summary_commitment = Hasher::hash(&summary_bytes);
            let signature_hex = self.hot_key.sign_word_hex(summary_commitment)?;
            timings.t_sign_end = now_unix_micros();

            let next_state_word = derive_next_state(&built_on, summary_commitment);
            let next_state_hex = word_hex(next_state_word);
            let nullifiers = self.derive_nullifiers(next_seq).await;

            self
                .ship_payload(
                    ctx,
                    timings,
                    built_on,
                    next_seq,
                    tx_summary,
                    signature_hex,
                    next_state_hex,
                    nullifiers,
                )
                .await
        }
    }

    async fn ship_payload(
        &self,
        ctx: &X402Context,
        timings: &mut PayTimings,
        built_on: String,
        _next_seq: u64,
        tx_summary: serde_json::Value,
        signature_hex: String,
        next_state_hex: String,
        nullifiers: Vec<String>,
    ) -> Result<PaymentReceipt> {

        let payload = AgenticPayload {
            tx_summary,
            hot_key_signature: DeltaSignature {
                signer_id: self.hot_key.commitment_hex(),
                signature: ProposalSignature::Falcon { signature: signature_hex },
            },
            x402_context: ctx.clone(),
            built_on_state_commitment: built_on.clone(),
            new_state_commitment: next_state_hex.clone(),
            claimed_nullifiers: nullifiers,
        };

        timings.t_send_facilitator = now_unix_micros();
        let ack = self.facilitator.post_payment(&self.agent_id, &payload).await?;
        timings.t_ack_received = now_unix_micros();

        {
            let mut cache = self.pending_cache.write().await;
            cache.pending_state_commitment = ack.new_pending_state_commitment.clone();
            cache.last_accepted_seq = ack.seq;
        }

        Ok(PaymentReceipt {
            agent_id: self.agent_id.clone(),
            seq: ack.seq,
            reserved_nullifiers: ack.reserved_nullifiers,
            new_pending_state_commitment: ack.new_pending_state_commitment,
            facilitator_ack_signature: ack.facilitator_ack_signature,
            accepted_at_unix_micros: ack.accepted_at_unix_micros,
        })
    }

    pub fn miden_integration(&self) -> Option<&Arc<MidenIntegration>> {
        self.miden.as_ref()
    }

    pub async fn payment_status(&self, nullifier: &str) -> Result<PaymentStatusResponse> {
        self.facilitator
            .get_payment_status(&self.agent_id, nullifier)
            .await
    }

    async fn derive_nullifiers(&self, seq: u64) -> Vec<String> {
        // Phase 2B will pull these from the real TransactionSummary
        // output deltas. For now: derive a deterministic placeholder
        // per (agent_id, seq, salt) so the facilitator's dedup works.
        let mut cache = self.pending_cache.write().await;
        cache.nullifier_salt = cache.nullifier_salt.wrapping_add(1);
        let salt = cache.nullifier_salt;
        drop(cache);

        let mut buf = Vec::new();
        buf.extend_from_slice(self.agent_id.as_bytes());
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(&salt.to_be_bytes());
        let w = Hasher::hash(&buf);
        vec![word_hex(w)]
    }
}

fn derive_next_state(
    built_on_hex: &str,
    summary_commitment: miden_protocol::Word,
) -> miden_protocol::Word {
    use miden_protocol::utils::serde::Serializable;
    let mut buf = Vec::new();
    buf.extend_from_slice(built_on_hex.as_bytes());
    buf.extend_from_slice(&summary_commitment.to_bytes());
    Hasher::hash(&buf)
}

fn word_hex(w: miden_protocol::Word) -> String {
    use miden_protocol::utils::serde::Serializable;
    format!("0x{}", hex::encode(w.to_bytes()))
}

#[derive(Default)]
pub struct AgenticClientBuilder {
    agent_id: Option<String>,
    account_id: Option<String>,
    facilitator_url: Option<String>,
    keystore_dir: Option<std::path::PathBuf>,
    miden: Option<Arc<MidenIntegration>>,
}

impl AgenticClientBuilder {
    pub fn agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }
    pub fn account_id(mut self, id: impl Into<String>) -> Self {
        self.account_id = Some(id.into());
        self
    }
    pub fn facilitator_url(mut self, url: impl Into<String>) -> Self {
        self.facilitator_url = Some(url.into());
        self
    }
    pub fn keystore_dir(mut self, p: impl Into<std::path::PathBuf>) -> Self {
        self.keystore_dir = Some(p.into());
        self
    }
    /// Attach a real Miden integration so `pay()` builds real
    /// `TransactionSummary` payloads.
    pub fn miden(mut self, integration: Arc<MidenIntegration>) -> Self {
        self.miden = Some(integration);
        self
    }
    pub fn build(self) -> Result<AgenticClient> {
        let agent_id = self
            .agent_id
            .ok_or_else(|| AgenticError::Config("agent_id required".into()))?;
        let account_id = self
            .account_id
            .ok_or_else(|| AgenticError::Config("account_id required".into()))?;
        let facilitator_url = self
            .facilitator_url
            .ok_or_else(|| AgenticError::Config("facilitator_url required".into()))?;
        let keystore_dir = self
            .keystore_dir
            .ok_or_else(|| AgenticError::Config("keystore_dir required".into()))?;
        let hot_key = HotKey::load_or_create(keystore_dir)?;
        Ok(AgenticClient {
            agent_id,
            account_id,
            hot_key,
            facilitator: FacilitatorClient::new(facilitator_url),
            pending_cache: Arc::new(RwLock::new(PendingCache {
                pending_state_commitment: String::new(),
                last_accepted_seq: 0,
                nullifier_salt: 0,
            })),
            miden: self.miden,
        })
    }
}

fn parse_account_id(s: &str) -> Result<AccountId> {
    AccountId::from_hex(s).map_err(|e| AgenticError::Config(format!("AccountId from hex: {e}")))
}
