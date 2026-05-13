//! Read-only Miden node abstraction.
//!
//! The facilitator only talks to the node to (1) fetch a public P2ID note
//! by ID, including its commitment block and consumption status, and
//! (2) read the current chain tip. Both operations are async, and both have
//! a mockable trait so the verifier can be unit-tested without a live node.

use std::collections::BTreeSet;

use async_trait::async_trait;
use miden_client::account::AccountId;
use miden_client::asset::Asset;
use miden_client::block::BlockNumber;
use miden_client::note::NoteId;
use miden_client::rpc::domain::note::FetchedNote;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_standards::note::{P2idNote, P2idNoteStorage};
use miden_x402_types::{AccountIdHex, NoteIdHex};
use thiserror::Error;

/// Errors produced by the Miden node abstraction.
#[derive(Debug, Error)]
pub enum NodeError {
    /// The provided hex did not parse into a Miden identifier.
    #[error("invalid hex identifier: {0}")]
    InvalidIdentifier(String),
    /// The note exists on chain but is private (no body is returned).
    #[error("note is private; only the header is observable on chain")]
    NotePrivate,
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

/// Read-only Miden node interface used by the verifier.
#[async_trait]
pub trait MidenNode: Send + Sync + 'static {
    /// Fetches a public P2ID note's snapshot. Returns `Ok(None)` if the node
    /// has not seen the note (not yet committed, or never created).
    async fn fetch_public_p2id_note(
        &self,
        note_id: &NoteIdHex,
    ) -> Result<Option<NoteSnapshot>, NodeError>;

    /// Returns the chain's latest committed block number.
    async fn latest_block_num(&self) -> Result<u32, NodeError>;
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

        // Nullifier check via prefix-based sync (the trait method's default
        // implementation in miden-client takes care of the filtering).
        let nullifier = note.nullifier();
        let mut set = BTreeSet::new();
        set.insert(nullifier);
        let consumed = self
            .client
            .get_nullifier_commit_heights(set, BlockNumber::from(0u32))
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;
        let is_consumed = consumed
            .get(&nullifier)
            .map(|height| height.is_some())
            .unwrap_or(false);

        Ok(Some(NoteSnapshot {
            block_num,
            sender,
            recipient,
            asset_faucet,
            asset_amount,
            is_consumed,
        }))
    }

    async fn latest_block_num(&self) -> Result<u32, NodeError> {
        let (header, _) = self
            .client
            .get_block_header_by_number(None, false)
            .await
            .map_err(|e| NodeError::Rpc(e.to_string()))?;
        Ok(header.block_num().as_u32())
    }
}

fn account_id_to_hex(id: &AccountId) -> Result<AccountIdHex, NodeError> {
    let formatted = format!("0x{}", id.to_hex());
    formatted
        .parse()
        .map_err(|e: miden_x402_types::IdError| NodeError::InvalidIdentifier(e.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A no-RPC mock node used by the verifier's unit tests.
    pub(crate) struct MockNode {
        pub(crate) snapshots: Mutex<Vec<(NoteIdHex, Option<NoteSnapshot>)>>,
        pub(crate) tip: u32,
    }

    impl MockNode {
        pub(crate) fn new(tip: u32) -> Self {
            Self {
                snapshots: Mutex::new(Vec::new()),
                tip,
            }
        }

        pub(crate) fn insert(&self, note_id: NoteIdHex, snapshot: Option<NoteSnapshot>) {
            self.snapshots.lock().unwrap().push((note_id, snapshot));
        }
    }

    #[async_trait]
    impl MidenNode for MockNode {
        async fn fetch_public_p2id_note(
            &self,
            note_id: &NoteIdHex,
        ) -> Result<Option<NoteSnapshot>, NodeError> {
            let guard = self.snapshots.lock().unwrap();
            for (id, snap) in guard.iter() {
                if id == note_id {
                    return Ok(snap.clone());
                }
            }
            Ok(None)
        }

        async fn latest_block_num(&self) -> Result<u32, NodeError> {
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
}
