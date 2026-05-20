//! Buyer-account auth lookup.
//!
//! The Guardian-facilitator must verify the buyer's Falcon signature over
//! the tx summary against a public key bound to the buyer's on-chain
//! multisig account. We do NOT read the buyer's pubkey from the on-chain
//! `AuthSingleSig` storage slot (the previous standalone-facilitator
//! approach) because:
//!
//! - OZ Guardian's account model is multisig-with-cosigners, not single-sig.
//!   The buyer's `Auth` policy is a list of `cosigner_commitments`, any of
//!   which is a valid signer.
//! - Reading from chain would force a node RPC on every verify; pulling
//!   from Guardian's own metadata store keeps the hot path local.
//!
//! This module defines a narrow trait, [`BuyerAuthLookup`], with one
//! method that returns the buyer's accepted cosigner commitments. The
//! binary wires this up by calling `MetadataStore::get(account_id)` on
//! the Guardian server side; the trait keeps the verify path decoupled
//! from Guardian's exact API so it stays unit-testable.

use async_trait::async_trait;
use miden_protocol::Word;
use thiserror::Error;

use miden_x402_types::AccountIdHex;

/// Errors returned by [`BuyerAuthLookup`].
#[derive(Debug, Error)]
pub enum BuyerAuthError {
    #[error("buyer account not configured on this Guardian")]
    NotConfigured,
    #[error("buyer account uses unsupported auth scheme (only Falcon-512 Poseidon2 cosigners supported)")]
    UnsupportedScheme,
    #[error("metadata backend error: {0}")]
    Backend(String),
    #[error("commitment hex parse failed: {0}")]
    InvalidCommitment(String),
}

/// Returns the list of Falcon-512 Poseidon2 pubkey commitments accepted as
/// cosigners on the buyer's account.
#[async_trait]
pub trait BuyerAuthLookup: Send + Sync + 'static {
    async fn cosigner_commitments(
        &self,
        buyer: &AccountIdHex,
    ) -> Result<Vec<Word>, BuyerAuthError>;
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// In-memory `BuyerAuthLookup` for tests. Pre-populate with the buyer
    /// accounts you want to recognise.
    #[derive(Default, Clone)]
    pub struct MockBuyerAuthLookup {
        inner: Arc<Mutex<HashMap<String, Vec<Word>>>>,
    }

    impl MockBuyerAuthLookup {
        pub fn new() -> Self { Self::default() }
        pub async fn set(&self, buyer: &AccountIdHex, commitments: Vec<Word>) {
            self.inner
                .lock()
                .await
                .insert(buyer.as_str().to_owned(), commitments);
        }
    }

    #[async_trait]
    impl BuyerAuthLookup for MockBuyerAuthLookup {
        async fn cosigner_commitments(
            &self,
            buyer: &AccountIdHex,
        ) -> Result<Vec<Word>, BuyerAuthError> {
            let g = self.inner.lock().await;
            g.get(buyer.as_str())
                .cloned()
                .ok_or(BuyerAuthError::NotConfigured)
        }
    }
}
