//! `AdnClient` — agent-side client for AgentDebitNote payments.
//!
//! The agent's hot path is just Falcon signing (~2ms). No kernel execution,
//! no miden-client dependency, no proving. The facilitator handles chain interaction.

use std::time::{SystemTime, UNIX_EPOCH};

use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::utils::serde::Serializable;

use agent_debit_note::message::debit_message;

use crate::transport::FacilitatorTransport;
use crate::types::*;

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn word_to_hex_array(w: Word) -> [String; 4] {
    [
        format!("0x{:016x}", w[0].as_canonical_u64()),
        format!("0x{:016x}", w[1].as_canonical_u64()),
        format!("0x{:016x}", w[2].as_canonical_u64()),
        format!("0x{:016x}", w[3].as_canonical_u64()),
    ]
}

/// Agent-side client for AgentDebitNote payments.
pub struct AdnClient {
    agent_sk: AuthSecretKey,
    facilitator: FacilitatorTransport,
    note_id: String,
    note_serial: Word,
    balance: u64,
    expiry_block: u32,
    agent_pubkey_commitment: Word,
}

impl AdnClient {
    pub fn new(
        agent_sk: AuthSecretKey,
        facilitator_url: impl Into<String>,
        note_id: String,
        note_serial: Word,
        balance: u64,
        expiry_block: u32,
    ) -> Self {
        let agent_pubkey_commitment: Word = agent_sk.public_key().to_commitment().into();
        Self {
            agent_sk,
            facilitator: FacilitatorTransport::new(facilitator_url),
            note_id,
            note_serial,
            balance,
            expiry_block,
            agent_pubkey_commitment,
        }
    }

    pub fn note_id(&self) -> &str {
        &self.note_id
    }

    pub fn balance(&self) -> u64 {
        self.balance
    }

    pub fn expiry_block(&self) -> u32 {
        self.expiry_block
    }

    /// Hot path: sign a debit authorization and send to the facilitator.
    /// Returns the facilitator's ack + timing data.
    pub async fn pay(
        &self,
        merchant: AccountId,
        amount: u64,
    ) -> Result<(PayAck, AdnPayTimings), AdnPayError> {
        let mut timings = AdnPayTimings {
            t_pay_start: now_micros(),
            ..Default::default()
        };

        if amount > self.balance {
            return Err(AdnPayError::InsufficientBalance {
                requested: amount,
                available: self.balance,
            });
        }

        // Sign the debit message (~2ms)
        timings.t_sign_start = now_micros();
        let message = debit_message(self.note_serial, merchant, amount);
        let signature = self.agent_sk.sign(message);
        let sig_bytes = signature.to_bytes();
        let prepared_sig = signature.to_prepared_signature(message);
        timings.t_sign_end = now_micros();

        // Build the signed debit payload
        let debit = SignedDebit {
            note_id: self.note_id.clone(),
            serial_num_hex: word_to_hex_array(self.note_serial),
            merchant_account_id: merchant.to_hex(),
            amount,
            signature_hex: format!("0x{}", hex::encode(&sig_bytes)),
            prepared_signature_hex: format!(
                "0x{}",
                hex::encode(
                    prepared_sig
                        .iter()
                        .flat_map(|f| f.as_canonical_u64().to_le_bytes())
                        .collect::<Vec<u8>>()
                )
            ),
            expiry_block_height: self.expiry_block,
            agent_pubkey_commitment_hex: format!(
                "0x{}",
                hex::encode(self.agent_pubkey_commitment.to_bytes())
            ),
        };

        // Send to facilitator
        timings.t_send_facilitator = now_micros();
        let ack = self
            .facilitator
            .pay(&debit)
            .await
            .map_err(|e| AdnPayError::Transport(format!("{e}")))?;
        timings.t_ack_received = now_micros();

        Ok((ack, timings))
    }

    /// Update local state after the facilitator reports successful settlement.
    pub fn update_remainder(&mut self, remainder: &RemainderInfo) {
        self.note_id = remainder.note_id.clone();
        self.balance = remainder.balance;
        // Parse serial from hex
        // For now, trust the facilitator's reported serial.
        // In production, derive it deterministically.
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdnPayError {
    #[error("insufficient balance: requested {requested}, available {available}")]
    InsufficientBalance { requested: u64, available: u64 },
    #[error("transport error: {0}")]
    Transport(String),
}
