//! Read-only Miden node abstraction.
//!
//! The facilitator talks to the node to:
//!
//! 1. Fetch a *public* P2ID note by ID, including its body, sender, recipient,
//!    asset, commitment block, and on-chain consumption status — see
//!    [`MidenNode::fetch_public_p2id_note`].
//! 2. Fetch a *private* note's on-chain header by ID — only sender and block
//!    number are observable, the rest comes from the buyer's off-chain
//!    `NoteFile` blob carried in the x402 payload. See
//!    [`MidenNode::fetch_private_p2id_note`].
//! 3. Check whether a precomputed nullifier has been consumed — see
//!    [`MidenNode::is_nullifier_consumed`]. The verifier uses this for the
//!    private path where the nullifier is derived off-chain from the blob.
//! 4. Read the current chain tip via [`MidenNode::latest_block_num`].
//!
//! All operations are async, and the trait is mockable so the verifier can be
//! unit-tested without a live node.

use std::collections::BTreeSet;

use async_trait::async_trait;
use miden_client::account::AccountId;
use miden_client::asset::Asset;
use miden_client::block::BlockNumber;
use miden_client::note::{NoteId, Nullifier};
use miden_client::rpc::domain::note::FetchedNote;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::transaction::{ProvenTransaction, TransactionInputs};
use miden_standards::note::{P2idNote, P2idNoteStorage};
use miden_x402_types::{AccountIdHex, NoteIdHex};
use thiserror::Error;

/// Errors produced by the Miden node abstraction.
#[derive(Debug, Error)]
pub enum NodeError {
    /// The provided hex did not parse into a Miden identifier.
    #[error("invalid hex identifier: {0}")]
    InvalidIdentifier(String),
    /// `fetch_public_p2id_note` was called on a note whose on-chain commitment
    /// is private. The caller should route through `fetch_private_p2id_note`
    /// and the off-chain blob path instead.
    #[error("note is private; only the header is observable on chain")]
    NotePrivate,
    /// `fetch_private_p2id_note` was called on a note that is in fact public.
    /// This should not happen in practice — the buyer chose `noteType` —
    /// but the trait honours the asymmetry to keep the API tight.
    #[error("note is public; use fetch_public_p2id_note")]
    NotePublic,
    /// The note exists on chain but is not a P2ID note.
    #[error("note is not a P2ID note")]
    NotP2id,
    /// The note's storage does not encode a valid target account ID.
    #[error("note storage does not encode a valid P2ID target")]
    InvalidP2idStorage,
    /// The note resolves on chain but carries an unexpected asset shape.
    #[error("note has no fungible asset")]
    NoFungibleAsset,
    /// Any failure inside the underlying RPC transport.
    #[error("rpc transport error: {0}")]
    Rpc(String),
}

/// A snapshot of a public P2ID note as observed on chain.
///
/// All identifiers are in their canonical lowercase `0x`-prefixed hex form so
/// the verifier can compare them directly with values pulled from the x402
/// payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteSnapshot {
    /// Block number in which the note was committed.
    pub block_num: u32,
    /// Account ID of the note's sender (payer).
    pub sender: AccountIdHex,
    /// Account ID encoded in the P2ID storage (the recipient / `payTo`).
    pub recipient: AccountIdHex,
    /// Faucet account ID of the fungible asset carried by the note.
    ///
    /// For MVP we require exactly one fungible asset per P2ID payment.
    pub asset_faucet: AccountIdHex,
    /// Amount of the fungible asset in atomic units.
    pub asset_amount: u64,
    /// Whether the note's nullifier has already been recorded on chain.
    pub is_consumed: bool,
}

/// A snapshot of a private note as observed on chain.
///
/// Only the metadata header (sender, note type, tag) and the inclusion proof
/// are exposed on chain for private notes; the recipient, script, assets and
/// serial number live in the buyer's off-chain `NoteFile` blob. The verifier
/// fills in the remaining fields from the blob and binds them to the on-chain
/// commitment by recomputing the note id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateNoteSnapshot {
    /// Block number in which the note's commitment was committed.
    pub block_num: u32,
    /// Account ID of the note's sender (payer), from the on-chain header.
    pub sender: AccountIdHex,
}

/// Read-only Miden node interface used by the verifier.
#[async_trait]
pub trait MidenNode: Send + Sync + 'static {
    /// Fetches a public P2ID note's snapshot. Returns `Ok(None)` if the node
    /// has not seen the note (not yet committed, or never created).
    async fn fetch_public_p2id_note(
        &self,
        note_id: &NoteIdHex,
    ) -> Result<Option<NoteSnapshot>, NodeError>;

    /// Fetches a private P2ID note's on-chain header. Returns `Ok(None)` if
    /// the node has not seen the note. The caller is responsible for
    /// reconstructing recipient, assets, and nullifier from the off-chain
    /// `NoteFile` blob.
    async fn fetch_private_p2id_note(
        &self,
        note_id: &NoteIdHex,
    ) -> Result<Option<PrivateNoteSnapshot>, NodeError>;

    /// Returns `true` if the given nullifier has been recorded as consumed in
    /// a committed block. The nullifier is precomputed by the caller; for the
    /// public path the verifier reads it from the on-chain note, for the
    /// private path the verifier reads it from the off-chain blob.
    async fn is_nullifier_consumed(
        &self,
        nullifier_hex: &str,
    ) -> Result<bool, NodeError>;

    /// Returns the chain's latest committed block number.
    async fn latest_block_num(&self) -> Result<u32, NodeError>;

    /// Submits a `ProvenTransaction` to the Miden node. Returns the block
    /// number it was accepted into the mempool against. Only used by the
    /// Guardian flow (Phase B) — the Phase A facilitator is read-only.
    async fn submit_proven_transaction(
        &self,
        proven: ProvenTransaction,
        tx_inputs: TransactionInputs,
    ) -> Result<u32, NodeError>;
}

/// `MidenNode` implementation backed by `miden_client::rpc::GrpcClient`.
pub struct GrpcMidenNode {
    client: GrpcClient,
}

impl GrpcMidenNode {
    /// Constructs a new node wrapper around a gRPC client.
    pub fn new(client: GrpcClient) -> Self {
        Self { client }
    }

    /// Convenience constructor that connects to the Miden testnet with the
    /// given RPC timeout. Use `from_url` for any custom endpoint.
    pub fn testnet(timeout_ms: u64) -> Self {
        let client = GrpcClient::new(&Endpoint::testnet(), timeout_ms);
        Self::new(client)
    }

    /// Builds a node wrapper from a custom RPC URL.
    pub fn from_url(url: &str, timeout_ms: u64) -> Result<Self, NodeError> {
        let endpoint = Endpoint::try_from(url)
            .map_err(|e| NodeError::Rpc(format!("invalid endpoint: {e}")))?;
        let client = GrpcClient::new(&endpoint, timeout_ms);
        Ok(Self::new(client))
    }
}

impl GrpcMidenNode {
    /// Shared helper: asks the node whether a single nullifier is in the
    /// committed nullifier tree.
    async fn nullifier_consumed_internal(&self, nullifier: Nullifier) -> Result<bool, NodeError> {
        let mut set = BTreeSet::new();
        set.insert(nullifier);
        let consumed = self
            .client
            .get_nullifier_commit_heights(set, BlockNumber::from(0u32))
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;
        Ok(consumed
            .get(&nullifier)
            .map(|height| height.is_some())
            .unwrap_or(false))
    }
}

#[async_trait]
impl MidenNode for GrpcMidenNode {
    async fn fetch_public_p2id_note(
        &self,
        note_id: &NoteIdHex,
    ) -> Result<Option<NoteSnapshot>, NodeError> {
        let parsed = NoteId::try_from_hex(note_id.as_str())
            .map_err(|e| NodeError::InvalidIdentifier(e.to_string()))?;

        let fetched = self
            .client
            .get_notes_by_id(&[parsed])
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;

        let Some(fetched_note) = fetched.into_iter().next() else {
            return Ok(None);
        };

        let (note, proof) = match fetched_note {
            FetchedNote::Public(note, proof) => (note, proof),
            FetchedNote::Private(_, _) => return Err(NodeError::NotePrivate),
        };

        let block_num = proof.location().block_num().as_u32();

        // P2ID enforcement: script root must match the canonical script.
        if note.recipient().script().root() != P2idNote::script_root() {
            return Err(NodeError::NotP2id);
        }

        // Recipient extraction: P2ID storage stores [target.suffix(), target.prefix()].
        let storage = P2idNoteStorage::try_from(note.recipient().storage().items())
            .map_err(|_| NodeError::InvalidP2idStorage)?;
        let recipient = account_id_to_hex(&storage.target())?;

        // Sender lives in the note metadata.
        let sender = account_id_to_hex(&note.metadata().sender())?;

        // Asset extraction: MVP only handles a single fungible asset per note.
        let (asset_faucet, asset_amount) = note
            .assets()
            .iter()
            .find_map(|asset| match asset {
                Asset::Fungible(fa) => Some((fa.faucet_id(), fa.amount())),
                Asset::NonFungible(_) => None,
            })
            .ok_or(NodeError::NoFungibleAsset)?;

        let asset_faucet = account_id_to_hex(&asset_faucet)?;

        let is_consumed = self.nullifier_consumed_internal(note.nullifier()).await?;

        Ok(Some(NoteSnapshot {
            block_num,
            sender,
            recipient,
            asset_faucet,
            asset_amount,
            is_consumed,
        }))
    }

    async fn fetch_private_p2id_note(
        &self,
        note_id: &NoteIdHex,
    ) -> Result<Option<PrivateNoteSnapshot>, NodeError> {
        let parsed = NoteId::try_from_hex(note_id.as_str())
            .map_err(|e| NodeError::InvalidIdentifier(e.to_string()))?;

        let fetched = self
            .client
            .get_notes_by_id(&[parsed])
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;

        let Some(fetched_note) = fetched.into_iter().next() else {
            return Ok(None);
        };

        let (header, proof) = match fetched_note {
            FetchedNote::Private(header, proof) => (header, proof),
            FetchedNote::Public(_, _) => return Err(NodeError::NotePublic),
        };

        let block_num = proof.location().block_num().as_u32();
        let sender = account_id_to_hex(&header.metadata().sender())?;

        Ok(Some(PrivateNoteSnapshot { block_num, sender }))
    }

    async fn is_nullifier_consumed(&self, nullifier_hex: &str) -> Result<bool, NodeError> {
        let nullifier = Nullifier::from_hex(nullifier_hex)
            .map_err(|e| NodeError::InvalidIdentifier(e.to_string()))?;
        self.nullifier_consumed_internal(nullifier).await
    }

    async fn latest_block_num(&self) -> Result<u32, NodeError> {
        let (header, _) = self
            .client
            .get_block_header_by_number(None, false)
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;
        Ok(header.block_num().as_u32())
    }

    async fn submit_proven_transaction(
        &self,
        proven: ProvenTransaction,
        tx_inputs: TransactionInputs,
    ) -> Result<u32, NodeError> {
        let block_num = self
            .client
            .submit_proven_transaction(proven, tx_inputs)
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;
        Ok(block_num.as_u32())
    }
}

fn account_id_to_hex(id: &AccountId) -> Result<AccountIdHex, NodeError> {
    // `AccountId::to_hex()` already returns the canonical `0x`-prefixed form;
    // do not double-prefix.
    id.to_hex()
        .parse()
        .map_err(|e: miden_x402_types::IdError| NodeError::InvalidIdentifier(e.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// A no-RPC mock node used by the verifier's unit tests.
    ///
    /// The mock stores both public and private snapshots in independent maps
    /// and the set of nullifiers that have been "consumed" on chain. The
    /// verifier branches on payload `noteType` and the mock answers the
    /// corresponding method.
    pub(crate) struct MockNode {
        public_snapshots: Mutex<Vec<(NoteIdHex, Option<NoteSnapshot>)>>,
        private_snapshots: Mutex<Vec<(NoteIdHex, Option<PrivateNoteSnapshot>)>>,
        consumed_nullifiers: Mutex<HashSet<String>>,
        pub(crate) tip: u32,
    }

    impl MockNode {
        pub(crate) fn new(tip: u32) -> Self {
            Self {
                public_snapshots: Mutex::new(Vec::new()),
                private_snapshots: Mutex::new(Vec::new()),
                consumed_nullifiers: Mutex::new(HashSet::new()),
                tip,
            }
        }

        pub(crate) fn insert(&self, note_id: NoteIdHex, snapshot: Option<NoteSnapshot>) {
            self.public_snapshots
                .lock()
                .unwrap()
                .push((note_id, snapshot));
        }

        pub(crate) fn insert_private(
            &self,
            note_id: NoteIdHex,
            snapshot: Option<PrivateNoteSnapshot>,
        ) {
            self.private_snapshots
                .lock()
                .unwrap()
                .push((note_id, snapshot));
        }

        pub(crate) fn mark_nullifier_consumed(&self, nullifier_hex: &str) {
            self.consumed_nullifiers
                .lock()
                .unwrap()
                .insert(nullifier_hex.to_owned());
        }
    }

    #[async_trait]
    impl MidenNode for MockNode {
        async fn fetch_public_p2id_note(
            &self,
            note_id: &NoteIdHex,
        ) -> Result<Option<NoteSnapshot>, NodeError> {
            let guard = self.public_snapshots.lock().unwrap();
            for (id, snap) in guard.iter() {
                if id == note_id {
                    return Ok(snap.clone());
                }
            }
            Ok(None)
        }

        async fn fetch_private_p2id_note(
            &self,
            note_id: &NoteIdHex,
        ) -> Result<Option<PrivateNoteSnapshot>, NodeError> {
            let guard = self.private_snapshots.lock().unwrap();
            for (id, snap) in guard.iter() {
                if id == note_id {
                    return Ok(snap.clone());
                }
            }
            Ok(None)
        }

        async fn is_nullifier_consumed(&self, nullifier_hex: &str) -> Result<bool, NodeError> {
            Ok(self
                .consumed_nullifiers
                .lock()
                .unwrap()
                .contains(nullifier_hex))
        }

        async fn latest_block_num(&self) -> Result<u32, NodeError> {
            Ok(self.tip)
        }

        async fn submit_proven_transaction(
            &self,
            _proven: ProvenTransaction,
            _tx_inputs: TransactionInputs,
        ) -> Result<u32, NodeError> {
            // Mock node never actually submits — return the current tip.
            // Phase B settle-path tests stub at a higher level rather than
            // exercising this method.
            Ok(self.tip)
        }
    }

    #[tokio::test]
    async fn mock_node_returns_inserted_snapshots() {
        let node = MockNode::new(100);
        let note: NoteIdHex = format!("0x{}", "a".repeat(64)).parse().unwrap();
        let snap = NoteSnapshot {
            block_num: 90,
            sender: "0x111111111111111111111111111111".parse().unwrap(),
            recipient: "0x222222222222222222222222222222".parse().unwrap(),
            asset_faucet: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            asset_amount: 1000,
            is_consumed: false,
        };
        node.insert(note.clone(), Some(snap.clone()));

        assert_eq!(
            node.fetch_public_p2id_note(&note).await.unwrap(),
            Some(snap)
        );
        assert_eq!(node.latest_block_num().await.unwrap(), 100);
    }

    #[tokio::test]
    async fn mock_node_handles_private_snapshots_and_nullifiers() {
        let node = MockNode::new(150);
        let note: NoteIdHex = format!("0x{}", "b".repeat(64)).parse().unwrap();
        let snap = PrivateNoteSnapshot {
            block_num: 120,
            sender: "0x111111111111111111111111111111".parse().unwrap(),
        };
        node.insert_private(note.clone(), Some(snap.clone()));

        assert_eq!(
            node.fetch_private_p2id_note(&note).await.unwrap(),
            Some(snap)
        );

        let nullifier_hex = format!("0x{}", "c".repeat(64));
        assert!(!node.is_nullifier_consumed(&nullifier_hex).await.unwrap());
        node.mark_nullifier_consumed(&nullifier_hex);
        assert!(node.is_nullifier_consumed(&nullifier_hex).await.unwrap());
    }
}
