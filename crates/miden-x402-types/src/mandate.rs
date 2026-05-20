//! AP2 mandate schema per [`ideas/NEW_DESIGN.md`].
//!
//! The user signs an `Ap2Mandate` at agent-setup time with their cold key.
//! The agentic-guardian stores `Ap2SignedMandate`, verifies the user's
//! signature, and on every incoming hot-key-signed tx enforces the
//! mandate's four constraints:
//!
//! 1. per-tx amount cap (`amount_cap_per_tx`)
//! 2. merchant allowlist (`merchant_allowlist` by `payTo`)
//! 3. rolling time window (`time_window_secs`)
//! 4. daily total cap (`daily_total_cap`)
//!
//! ## Why no crypto here
//!
//! This crate stays free of Miden crypto deps so it can be embedded into
//! merchant SDKs (Node, Python via PyO3 bindings, future Go ports). The
//! `Ap2Mandate::canonical_bytes()` method returns deterministic bytes that
//! downstream code (agentic-guardian, miden-agentic-client) hashes via
//! `Rpo256` to produce the commitment the user signs. Putting the hash
//! here would force every downstream consumer to pull in `miden-crypto`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AccountIdHex;

/// Errors from canonical-encoding / mandate validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MandateSchemaError {
    #[error("mandate expired at {expires_at} (now {now})")]
    Expired { expires_at: u64, now: u64 },
    #[error("amount {amount} exceeds per-tx cap {cap}")]
    AmountCapExceeded { amount: u64, cap: u64 },
    #[error("merchant {merchant} not in allowlist")]
    MerchantNotAllowed { merchant: String },
    #[error("rolling-window total {observed} would exceed window cap {cap} (window={window_secs}s)")]
    TimeWindowExceeded { observed: u64, cap: u64, window_secs: u64 },
    #[error("24h total {observed} would exceed daily cap {cap}")]
    DailyTotalExceeded { observed: u64, cap: u64 },
    #[error("malformed mandate: {0}")]
    Malformed(String),
}

/// AP2 mandate body — what the user signs.
///
/// JSON is camelCase to match the rest of the x402 wire. Field ordering
/// matters for [`Self::canonical_bytes`], which downstream code hashes
/// into the commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2Mandate {
    /// Unique mandate identifier (opaque string the agentic-guardian
    /// uses to look this mandate up; included in the 402's
    /// `extra.mandateId`).
    pub mandate_id: String,
    /// The agent's Miden account id this mandate authorises.
    pub agent_account_id: AccountIdHex,
    /// Bullet 1: maximum amount per individual transaction (atomic
    /// units of `requirements.asset`).
    pub amount_cap_per_tx: u64,
    /// Bullet 2: list of merchant account ids (`payTo`) the agent is
    /// allowed to send to. Empty list means "no merchants allowed."
    pub merchant_allowlist: Vec<AccountIdHex>,
    /// Bullet 3: rolling time window length in seconds. Combined with
    /// `daily_total_cap`, restricts spend rate.
    pub time_window_secs: u64,
    /// Bullet 4: daily total cap (atomic units). Sum of all txs
    /// authorised by this mandate in any rolling 24h window must not
    /// exceed this.
    pub daily_total_cap: u64,
    /// UNIX seconds when the user issued the mandate.
    pub issued_at_unix_secs: u64,
    /// UNIX seconds after which the mandate is invalid.
    pub expires_at_unix_secs: u64,
}

impl Ap2Mandate {
    /// Returns a deterministic byte encoding of the mandate for hashing
    /// into the commitment. **Stable across releases** — anything that
    /// changes the canonical bytes is a breaking wire change.
    ///
    /// Format: each field encoded in declaration order, length-prefixed
    /// where variable. The exact format:
    ///
    /// ```text
    /// u32-le len(mandate_id) || mandate_id (utf-8)
    /// u32-le len(agent_account_id_str) || agent_account_id_str (utf-8)
    /// u64-le amount_cap_per_tx
    /// u32-le len(merchant_allowlist)
    /// for each merchant:
    ///   u32-le len(merchant_id_str) || merchant_id_str (utf-8)
    /// u64-le time_window_secs
    /// u64-le daily_total_cap
    /// u64-le issued_at_unix_secs
    /// u64-le expires_at_unix_secs
    /// ```
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        push_lenprefixed_str(&mut out, &self.mandate_id);
        push_lenprefixed_str(&mut out, self.agent_account_id.as_str());
        out.extend_from_slice(&self.amount_cap_per_tx.to_le_bytes());
        out.extend_from_slice(&(self.merchant_allowlist.len() as u32).to_le_bytes());
        for m in &self.merchant_allowlist {
            push_lenprefixed_str(&mut out, m.as_str());
        }
        out.extend_from_slice(&self.time_window_secs.to_le_bytes());
        out.extend_from_slice(&self.daily_total_cap.to_le_bytes());
        out.extend_from_slice(&self.issued_at_unix_secs.to_le_bytes());
        out.extend_from_slice(&self.expires_at_unix_secs.to_le_bytes());
        out
    }

    /// `true` if this mandate authorises a payment of `amount` to
    /// `merchant` at wall-clock time `now_unix_secs`, ignoring rolling
    /// counters (those are checked by the agentic-guardian against its
    /// counter store). Use this for **static** validation only.
    pub fn pre_check(
        &self,
        amount: u64,
        merchant: &AccountIdHex,
        now_unix_secs: u64,
    ) -> Result<(), MandateSchemaError> {
        if now_unix_secs >= self.expires_at_unix_secs {
            return Err(MandateSchemaError::Expired {
                expires_at: self.expires_at_unix_secs,
                now: now_unix_secs,
            });
        }
        if amount > self.amount_cap_per_tx {
            return Err(MandateSchemaError::AmountCapExceeded {
                amount,
                cap: self.amount_cap_per_tx,
            });
        }
        if !self.merchant_allowlist.contains(merchant) {
            return Err(MandateSchemaError::MerchantNotAllowed {
                merchant: merchant.as_str().to_owned(),
            });
        }
        Ok(())
    }
}

fn push_lenprefixed_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// User-signed mandate: body + cold-key signature + pubkey.
///
/// The agent registers this with the agentic-guardian at setup time.
/// Guardian:
///
/// 1. Recomputes `commitment = Rpo256(mandate.canonical_bytes())`.
/// 2. Verifies `user_signature_b64` is a valid Falcon-512 signature
///    over that commitment using `user_pubkey_b64`.
/// 3. Verifies `user_pubkey_b64`'s commitment matches the on-chain
///    cold-key cosigner of `mandate.agent_account_id`.
/// 4. Stores `Ap2SignedMandate` keyed by `mandate.mandate_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ap2SignedMandate {
    pub mandate: Ap2Mandate,
    /// Base64-encoded `miden_protocol::account::auth::Signature` from
    /// the user's cold key over the mandate commitment.
    pub user_signature_b64: String,
    /// Base64-encoded raw Falcon-512 public key (the cold key). The
    /// agentic-guardian verifies this commits to the same value as the
    /// on-chain cold cosigner.
    pub user_pubkey_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_account(c: char) -> AccountIdHex {
        // AccountIdHex expects valid hex; restrict the sample alphabet.
        let digit = match c {
            'a'..='f' | '0'..='9' => c,
            _ => 'e',
        };
        format!("0x{}", digit.to_string().repeat(30)).parse().unwrap()
    }

    fn sample_mandate() -> Ap2Mandate {
        Ap2Mandate {
            mandate_id: "m-1".to_owned(),
            agent_account_id: sample_account('a'),
            amount_cap_per_tx: 10_000,
            merchant_allowlist: vec![sample_account('b'), sample_account('c')],
            time_window_secs: 3600,
            daily_total_cap: 1_000_000,
            issued_at_unix_secs: 1_700_000_000,
            expires_at_unix_secs: 1_700_086_400,
        }
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let m = sample_mandate();
        assert_eq!(m.canonical_bytes(), m.canonical_bytes());
    }

    #[test]
    fn canonical_bytes_change_on_any_field_change() {
        let a = sample_mandate();
        let mut b = sample_mandate();
        b.amount_cap_per_tx += 1;
        assert_ne!(a.canonical_bytes(), b.canonical_bytes());
    }

    #[test]
    fn pre_check_passes_in_mandate_payment() {
        let m = sample_mandate();
        assert!(
            m.pre_check(1000, &sample_account('b'), 1_700_000_500).is_ok()
        );
    }

    #[test]
    fn pre_check_rejects_amount_cap_exceeded() {
        let m = sample_mandate();
        let err = m.pre_check(20_000, &sample_account('b'), 1_700_000_500).unwrap_err();
        assert!(matches!(err, MandateSchemaError::AmountCapExceeded { .. }));
    }

    #[test]
    fn pre_check_rejects_merchant_not_allowed() {
        let m = sample_mandate();
        let err = m.pre_check(1000, &sample_account('z'), 1_700_000_500).unwrap_err();
        assert!(matches!(err, MandateSchemaError::MerchantNotAllowed { .. }));
    }

    #[test]
    fn pre_check_rejects_expired_mandate() {
        let m = sample_mandate();
        let err = m
            .pre_check(1000, &sample_account('b'), m.expires_at_unix_secs + 1)
            .unwrap_err();
        assert!(matches!(err, MandateSchemaError::Expired { .. }));
    }

    #[test]
    fn signed_mandate_round_trips_json() {
        let signed = Ap2SignedMandate {
            mandate: sample_mandate(),
            user_signature_b64: "c2ln".to_owned(),
            user_pubkey_b64: "cGs=".to_owned(),
        };
        let json = serde_json::to_string(&signed).unwrap();
        let decoded: Ap2SignedMandate = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, signed);
        assert!(json.contains("\"mandateId\":\"m-1\""));
        assert!(json.contains("\"amountCapPerTx\":10000"));
    }
}
