//! Facilitator-owned receipt key + signer.
//!
//! On a successful `POST /x402/settle`, the facilitator returns a
//! [`SettleResponse::Success { receipt_sig, receipt_pubkey_commitment, .. }`].
//! The merchant verifies the receipt against the facilitator's pubkey fetched
//! from `GET /x402/pubkey`, then delivers the resource — this is the
//! Guardian-signed-proof-at-delivery property DESIGN.md calls for.
//!
//! Why a facilitator-owned key (not Guardian's existing `ack` key)? OZ
//! Guardian's `AckRegistry::sign_with_server_key` is `pub(crate)` and the
//! high-level `ack_delta` requires a `DeltaObject` — we can't sign arbitrary
//! digests with the ack key from outside `guardian-server`. The trade-off
//! and the upstream PR that would collapse this back to one key are
//! described in [`docs/UPSTREAM_WISHLIST.md`].
//!
//! Trust model is unchanged from the merchant's standpoint: they cache one
//! pubkey from the facilitator operator and verify every receipt against it.
//! Key rotation is straightforward — delete the persisted key, restart, the
//! binary generates a new one and exposes it via `GET /x402/pubkey`.

use std::sync::Arc;

use miden_crypto::Word;
use miden_crypto::dsa::falcon512_poseidon2::{PublicKey, SecretKey, Signature};
use miden_crypto::hash::rpo::Rpo256;
use miden_crypto::utils::{Deserializable, Serializable};
use miden_protocol::Felt;
use thiserror::Error;

use crate::storage::{FacilitatorKeyStore, StorageError};

/// Errors returned by the receipt signer.
#[derive(Debug, Error)]
pub enum ReceiptError {
    #[error("keystore error: {0}")]
    Storage(#[from] StorageError),
    #[error("falcon key error: {0}")]
    Crypto(String),
}

/// Signs settle receipts with a persistent Falcon-512 key.
pub struct ReceiptSigner {
    secret: SecretKey,
    public: PublicKey,
}

impl ReceiptSigner {
    /// Loads the persisted key from `store`, or generates a fresh keypair on
    /// first boot (and saves it back so subsequent boots are stable).
    pub async fn load_or_generate<S: FacilitatorKeyStore + ?Sized>(
        store: &S,
    ) -> Result<Arc<Self>, ReceiptError> {
        if let Some(bytes) = store.load().await? {
            let secret = SecretKey::read_from_bytes(&bytes)
                .map_err(|e| ReceiptError::Crypto(format!("read secret: {e}")))?;
            let public = secret.public_key();
            return Ok(Arc::new(Self { secret, public }));
        }
        // First boot — generate a new keypair and persist.
        let secret = SecretKey::new();
        let bytes = (&secret).to_bytes();
        store.save(&bytes).await?;
        let public = secret.public_key();
        Ok(Arc::new(Self { secret, public }))
    }

    /// Returns the commitment of the receipt-signing public key (canonical
    /// `Word`). Merchants cache this from `GET /x402/pubkey`.
    pub fn pubkey_commitment(&self) -> Word {
        self.public.to_commitment()
    }

    /// Same as [`Self::pubkey_commitment`] but encoded as canonical hex for
    /// the wire — used by `GET /x402/pubkey` and `SettleResponse::Success`.
    pub fn pubkey_commitment_hex(&self) -> String {
        self.pubkey_commitment().to_hex()
    }

    /// Returns the raw public-key bytes for clients that want to verify
    /// signatures locally without rederiving from the commitment.
    pub fn pubkey_bytes(&self) -> Vec<u8> {
        (&self.public).to_bytes()
    }

    /// Signs the digest of `(payer, queued_id, network)` so the merchant can
    /// retain a Guardian-signed proof of the settlement.
    pub fn sign_receipt(
        &self,
        payer_hex: &str,
        queued_id_hex: &str,
        network: &str,
    ) -> Result<Signature, ReceiptError> {
        let digest = receipt_digest(payer_hex, queued_id_hex, network);
        Ok(self.secret.sign(digest))
    }

    /// Verifies a previously-issued receipt. Exposed for tests; production
    /// merchants implement this in their SDKs using `pubkey_b64` from
    /// `GET /x402/pubkey`.
    #[cfg(test)]
    pub fn verify_receipt(&self, message: Word, signature: &Signature) -> bool {
        self.public.verify(message, signature)
    }
}

/// Canonical signing message for a settle receipt:
/// `RPO256([payer_hash_words, queued_id_hash_words, network_hash])`.
///
/// All three inputs go through RPO256 first to produce fixed-length 4-felt
/// words, then we hash the concatenation. Stable across schema changes
/// because the inputs are length-prefixed via RPO256.
pub fn receipt_digest(payer_hex: &str, queued_id_hex: &str, network: &str) -> Word {
    let payer_word = Rpo256::hash(payer_hex.as_bytes());
    let queued_word = Rpo256::hash(queued_id_hex.as_bytes());
    let network_word = Rpo256::hash(network.as_bytes());
    let mut elems: Vec<Felt> = Vec::with_capacity(12);
    elems.extend(payer_word.as_elements().iter().copied());
    elems.extend(queued_word.as_elements().iter().copied());
    elems.extend(network_word.as_elements().iter().copied());
    Rpo256::hash_elements(&elems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory::MemoryFacilitatorKeyStore;

    #[tokio::test]
    async fn load_or_generate_creates_and_persists() {
        let store = MemoryFacilitatorKeyStore::new();
        let signer = ReceiptSigner::load_or_generate(&store).await.unwrap();
        let first = signer.pubkey_commitment_hex();
        // Re-load — should produce the same key.
        let signer2 = ReceiptSigner::load_or_generate(&store).await.unwrap();
        let second = signer2.pubkey_commitment_hex();
        assert_eq!(first, second, "key must be stable across reloads");
    }

    #[tokio::test]
    async fn sign_receipt_is_verifiable() {
        let store = MemoryFacilitatorKeyStore::new();
        let signer = ReceiptSigner::load_or_generate(&store).await.unwrap();
        let sig = signer
            .sign_receipt("0xabcd", "0xfeed", "miden:testnet")
            .unwrap();
        let digest = receipt_digest("0xabcd", "0xfeed", "miden:testnet");
        assert!(signer.verify_receipt(digest, &sig), "self-issued receipt must verify");
    }

    #[tokio::test]
    async fn receipt_digest_changes_when_any_field_changes() {
        let a = receipt_digest("0xa", "0xb", "miden:testnet");
        let b = receipt_digest("0xa", "0xb", "miden:mainnet");
        assert_ne!(a, b);
        let c = receipt_digest("0xa", "0xc", "miden:testnet");
        assert_ne!(a, c);
    }
}
