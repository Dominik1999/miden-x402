//! Buyer-balance lookup against Guardian's stored account state.
//!
//! Guardian persists each account's state as opaque JSON in
//! `StateObject.state_json` — there is no dedicated balance API. We parse
//! the JSON shape produced by Miden's standard wallet (`Account` →
//! `AssetVault` → `fungible: [ { faucet_id, amount } ]`) and confirm the
//! requested asset has ≥ the requested amount.
//!
//! Behaviour when the shape is unrecognized: **allow** the payment.
//! Rationale — this Guardian instance may host non-Miden state shapes (or
//! a buyer whose state hasn't been canonicalised yet). The on-chain check
//! at prove+submit time is the real enforcement; rejecting verifies because
//! we couldn't parse Guardian's state JSON would break the flow for any
//! deployment that uses a non-standard state shape.

use async_trait::async_trait;
use thiserror::Error;

use miden_x402_types::AccountIdHex;

/// Errors returned by [`BalanceLookup`]. Backend errors should surface as
/// transient (the verify path returns 503); "insufficient" is logical and
/// non-transient (412).
#[derive(Debug, Error)]
pub enum BalanceError {
    #[error("insufficient balance: have {have}, need {need}")]
    Insufficient { have: u128, need: u128 },
    #[error("balance backend error: {0}")]
    Backend(String),
}

/// Returns `Ok(())` if the buyer holds ≥ `required_amount` of `faucet`.
/// `Err(Insufficient { .. })` for hard-negative answers; `Ok(())` for
/// "shape unknown — degrade gracefully" so non-Miden state blobs don't
/// break the flow.
#[async_trait]
pub trait BalanceLookup: Send + Sync + 'static {
    async fn check_sufficient(
        &self,
        buyer: &AccountIdHex,
        faucet: &AccountIdHex,
        required_amount: u128,
    ) -> Result<(), BalanceError>;
}

/// Parse-only logic factored out so it's unit-testable without depending on
/// Guardian's storage trait.
///
/// Accepts the canonical Miden wallet vault shape:
///
/// ```json
/// { "vault": { "fungible": [{ "faucet_id": "0x...", "amount": 12000 }, ...] } }
/// ```
///
/// Returns:
/// - `Some(true)`  — recognised shape, balance sufficient
/// - `Some(false)` — recognised shape, balance insufficient
/// - `None`        — unrecognised shape, fall through to "allow"
pub fn check_balance_against_state_json(
    state_json: &serde_json::Value,
    faucet_hex: &str,
    required_amount: u128,
) -> Option<bool> {
    let vault = state_json.get("vault")?;
    let fungible = vault.get("fungible")?.as_array()?;
    let total: u128 = fungible
        .iter()
        .filter_map(|entry| {
            let id = entry.get("faucet_id")?.as_str()?;
            if !eq_account_hex(id, faucet_hex) {
                return None;
            }
            entry
                .get("amount")
                .and_then(|a| {
                    a.as_u64()
                        .map(u128::from)
                        .or_else(|| a.as_str().and_then(|s| s.parse::<u128>().ok()))
                })
        })
        .sum();
    Some(total >= required_amount)
}

fn eq_account_hex(a: &str, b: &str) -> bool {
    a.trim_start_matches("0x").eq_ignore_ascii_case(b.trim_start_matches("0x"))
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// In-memory `BalanceLookup` for tests. Map of `(buyer, faucet) -> balance`.
    #[derive(Default, Clone)]
    pub struct MockBalanceLookup {
        inner: Arc<Mutex<HashMap<(String, String), u128>>>,
    }

    impl MockBalanceLookup {
        pub fn new() -> Self { Self::default() }
        pub async fn set(&self, buyer: &AccountIdHex, faucet: &AccountIdHex, amount: u128) {
            self.inner
                .lock()
                .await
                .insert((buyer.as_str().to_owned(), faucet.as_str().to_owned()), amount);
        }
    }

    #[async_trait]
    impl BalanceLookup for MockBalanceLookup {
        async fn check_sufficient(
            &self,
            buyer: &AccountIdHex,
            faucet: &AccountIdHex,
            required_amount: u128,
        ) -> Result<(), BalanceError> {
            let g = self.inner.lock().await;
            let have = g
                .get(&(buyer.as_str().to_owned(), faucet.as_str().to_owned()))
                .copied()
                .unwrap_or(0);
            if have >= required_amount {
                Ok(())
            } else {
                Err(BalanceError::Insufficient { have, need: required_amount })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_sufficient_balance() {
        let json = serde_json::json!({
            "vault": {
                "fungible": [{ "faucet_id": "0x0a", "amount": 5000 }]
            }
        });
        assert_eq!(check_balance_against_state_json(&json, "0x0a", 1000), Some(true));
    }

    #[test]
    fn recognises_insufficient_balance() {
        let json = serde_json::json!({
            "vault": {
                "fungible": [{ "faucet_id": "0x0a", "amount": 500 }]
            }
        });
        assert_eq!(check_balance_against_state_json(&json, "0x0a", 1000), Some(false));
    }

    #[test]
    fn unrecognised_shape_returns_none() {
        let json = serde_json::json!({ "some_unknown_blob": 42 });
        assert!(check_balance_against_state_json(&json, "0x0a", 1000).is_none());
    }

    #[test]
    fn ignores_other_faucets() {
        let json = serde_json::json!({
            "vault": {
                "fungible": [
                    { "faucet_id": "0xff", "amount": 9999 },
                    { "faucet_id": "0x0a", "amount": 500 }
                ]
            }
        });
        assert_eq!(check_balance_against_state_json(&json, "0x0a", 1000), Some(false));
        assert_eq!(check_balance_against_state_json(&json, "0xff", 1000), Some(true));
    }
}
