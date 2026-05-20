//! Pure verification logic for the Miden `exact` x402 scheme.
//!
//! The verifier is split from the HTTP handlers so it can be exercised in
//! tests without spinning up an axum router or a real Miden node — see the
//! `MockNode` impl in [`crate::node`]'s tests for the test pattern.
//!
//! Both public and private P2ID payments flow through the same pipeline:
//!
//! ```text
//! check_payment
//!   ├── step_1_3_agreement   // network, payTo, asset, amount, allowlist,
//!   │                        // and noteType (`extra` ↔ payload) agreement
//!   ├── resolve_note         // only step that branches on noteType:
//!   │                        //   public  → fetch the on-chain Note
//!   │                        //   private → decode noteBlob, recompute
//!   │                        //             note_id, fetch on-chain header
//!   │                        //             (rebind), compute nullifier
//!   └── step_4_11_verify     // recipient / asset / sender / nullifier /
//!                            // freshness — note-type agnostic
//! ```
//!
//! The cryptographic bind for the private path is **note_id recomputation**:
//! the note id is a hash over the full note (recipient, assets, serial num),
//! so a buyer who tampers with any of those fields in the blob produces a
//! different commitment than what the node has. The verifier fetches the
//! on-chain header by the recomputed id, which forces the off-chain blob to
//! match the on-chain reality. Recipient and asset come from the blob;
//! sender and block number come from the chain.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use miden_client::Deserializable;
use miden_client::account::AccountId;
use miden_client::asset::Asset;
use miden_client::note::{NoteDetails, NoteFile, NoteId, Nullifier};
use miden_standards::note::{P2idNote, P2idNoteStorage};
use miden_x402_types::{
    AccountIdHex, MidenExactPayload, MidenPaymentPayload, MidenPaymentRequirements, NoteIdHex,
    PrivateP2idPayload, PublicP2idPayload, TransactionIdHex, network::is_miden,
};
use x402_types::proto::v1::{SettleResponse, VerifyResponse};

use crate::config::FacilitatorConfig;
use crate::error::FacilitatorError;
use crate::node::MidenNode;

/// Verifies a Miden x402 `exact` payment against the requirements and the
/// node state. Returns a `VerifyResponse::Valid { payer }` on success.
pub async fn verify<N: MidenNode + ?Sized>(
    payload: &MidenPaymentPayload,
    requirements: &MidenPaymentRequirements,
    node: &N,
    config: &FacilitatorConfig,
) -> Result<VerifyResponse, FacilitatorError> {
    let resolved = check_payment(payload, requirements, node, config).await?;
    Ok(VerifyResponse::valid(resolved.onchain_sender.into_inner()))
}

/// Settles a Miden x402 `exact` payment. Identical checks to [`verify`] —
/// in this implementation, "settled" is synonymous with "the note is
/// committed on chain", so we just re-run verification and return the
/// buyer's create-note transaction id.
pub async fn settle<N: MidenNode + ?Sized>(
    payload: &MidenPaymentPayload,
    requirements: &MidenPaymentRequirements,
    node: &N,
    config: &FacilitatorConfig,
) -> Result<SettleResponse, FacilitatorError> {
    let resolved = check_payment(payload, requirements, node, config).await?;
    Ok(SettleResponse::Success {
        payer: resolved.onchain_sender.into_inner(),
        transaction: resolved.transaction_id.into_inner(),
        network: requirements.network.to_string(),
    })
}

/// Internal, note-type-agnostic view used by [`step_4_11_verify`].
///
/// For the public path every field comes from the on-chain note. For the
/// private path, `recipient` / `asset_faucet` / `asset_amount` come from the
/// off-chain blob and `onchain_sender` / `block_num` come from the on-chain
/// header — they're bound together by recomputing `note_id` from the blob
/// and looking it up on chain.
struct ResolvedNote {
    transaction_id: TransactionIdHex,
    claimed_sender: AccountIdHex,
    onchain_sender: AccountIdHex,
    recipient: AccountIdHex,
    asset_faucet: AccountIdHex,
    asset_amount: u64,
    block_num: u32,
    is_consumed: bool,
}

/// Shared verification path used by both `verify` and `settle`.
async fn check_payment<N: MidenNode + ?Sized>(
    payload: &MidenPaymentPayload,
    requirements: &MidenPaymentRequirements,
    node: &N,
    config: &FacilitatorConfig,
) -> Result<ResolvedNote, FacilitatorError> {
    // Guardian-fast payloads belong on `/guardian/settle`, not `/settle`.
    // Reject them up-front so the rest of this pipeline only deals with
    // settled-at-commit Public / Private variants.
    if matches!(payload.payload, MidenExactPayload::GuardianFast(_)) {
        return Err(FacilitatorError::BadRequest(
            "guardian-fast payloads must be sent to /guardian/settle (or /guardian/verify)"
                .to_owned(),
        ));
    }

    step_1_3_agreement(payload, requirements, config)?;
    let resolved = resolve_note(payload, node).await?;
    step_4_11_verify(&resolved, requirements, config, node).await?;
    Ok(resolved)
}

/// Steps 1–3: scheme/network/payTo/asset/amount agreement between the
/// `accepted` echo and the merchant's `requirements`, plus the faucet
/// allowlist and the note-type consistency check (the discriminator in
/// `payload.payload` must match `accepted.extra.noteType`).
fn step_1_3_agreement(
    payload: &MidenPaymentPayload,
    requirements: &MidenPaymentRequirements,
    config: &FacilitatorConfig,
) -> Result<(), FacilitatorError> {
    if !is_miden(&requirements.network) {
        return Err(FacilitatorError::UnsupportedNetwork);
    }
    if requirements.network != payload.accepted.network {
        return Err(FacilitatorError::BadRequest(
            "payload.accepted.network does not match requirements.network".to_owned(),
        ));
    }
    if requirements.pay_to != payload.accepted.pay_to {
        return Err(FacilitatorError::BadRequest(
            "payload.accepted.payTo does not match requirements.payTo".to_owned(),
        ));
    }
    if requirements.asset != payload.accepted.asset {
        return Err(FacilitatorError::BadRequest(
            "payload.accepted.asset does not match requirements.asset".to_owned(),
        ));
    }
    if requirements.amount != payload.accepted.amount {
        return Err(FacilitatorError::BadRequest(
            "payload.accepted.amount does not match requirements.amount".to_owned(),
        ));
    }
    if !config.allowed_faucets.allows(&requirements.asset) {
        return Err(FacilitatorError::AssetNotAllowed(
            requirements.asset.as_str().to_owned(),
        ));
    }

    let payload_kind = payload.payload.semantic_note_type();
    if payload_kind != payload.accepted.extra.note_type {
        return Err(FacilitatorError::BadRequest(
            "payload.payload.noteType does not match payload.accepted.extra.noteType".to_owned(),
        ));
    }

    // The per-variant payload's own asset/amount echo (independent from the
    // `accepted` envelope) must also agree. This guards against mismatched
    // SDKs that fail to carry the echo through both layers.
    let (echo_asset, echo_amount) = match &payload.payload {
        MidenExactPayload::Public(p) => (&p.asset, &p.amount),
        MidenExactPayload::Private(p) => (&p.asset, &p.amount),
        MidenExactPayload::GuardianFast(_) => {
            // Caller `check_payment` rejects GuardianFast before this step
            // — the commit-flow agreement is not its concern.
            unreachable!("guardian-fast payload reached commit-flow agreement step")
        }
        MidenExactPayload::Agentic(_) => {
            // Agentic settlement is handled by the separate
            // `agentic-guardian` binary on this branch — the M8
            // facilitator never sees it. If we got here, the caller
            // sent the wrong variant.
            return Err(FacilitatorError::BadRequest(
                "agentic payloads are handled by agentic-guardian, not the M8 facilitator"
                    .to_owned(),
            ));
        }
    };
    if echo_asset != &requirements.asset {
        return Err(FacilitatorError::BadRequest(
            "payload.payload.asset does not match requirements.asset".to_owned(),
        ));
    }
    if echo_amount != &requirements.amount {
        return Err(FacilitatorError::BadRequest(
            "payload.payload.amount does not match requirements.amount".to_owned(),
        ));
    }

    Ok(())
}

/// Resolves the payload + chain state into a uniform [`ResolvedNote`]. This
/// is the only function that branches on `noteType`.
async fn resolve_note<N: MidenNode + ?Sized>(
    payload: &MidenPaymentPayload,
    node: &N,
) -> Result<ResolvedNote, FacilitatorError> {
    match &payload.payload {
        MidenExactPayload::Public(public) => resolve_public(public, node).await,
        MidenExactPayload::Private(private) => resolve_private(private, node).await,
        MidenExactPayload::GuardianFast(_) => {
            unreachable!("guardian-fast payload reached commit-flow resolve_note")
        }
        MidenExactPayload::Agentic(_) => Err(FacilitatorError::BadRequest(
            "agentic payloads belong to agentic-guardian, not the M8 facilitator".to_owned(),
        )),
    }
}

async fn resolve_public<N: MidenNode + ?Sized>(
    public: &PublicP2idPayload,
    node: &N,
) -> Result<ResolvedNote, FacilitatorError> {
    let snapshot = node
        .fetch_public_p2id_note(&public.note_id)
        .await
        .map_err(map_node_err)?
        .ok_or_else(|| FacilitatorError::NoteNotFound(public.note_id.as_str().to_owned()))?;

    Ok(ResolvedNote {
        transaction_id: public.transaction_id.clone(),
        claimed_sender: public.sender.clone(),
        onchain_sender: snapshot.sender,
        recipient: snapshot.recipient,
        asset_faucet: snapshot.asset_faucet,
        asset_amount: snapshot.asset_amount,
        block_num: snapshot.block_num,
        is_consumed: snapshot.is_consumed,
    })
}

async fn resolve_private<N: MidenNode + ?Sized>(
    private: &PrivateP2idPayload,
    node: &N,
) -> Result<ResolvedNote, FacilitatorError> {
    // Decode the canonical NoteFile from the base64 blob.
    let decoded = decode_private_blob(&private.note_blob)?;

    // Bind the off-chain blob to the on-chain commitment: ask the node for
    // the header by the recomputed note id. Any tampering with the off-chain
    // recipient/asset/serial_num would change the id and miss here.
    let snapshot = node
        .fetch_private_p2id_note(&decoded.note_id_hex)
        .await
        .map_err(map_node_err)?
        .ok_or_else(|| FacilitatorError::NoteNotFound(decoded.note_id_hex.as_str().to_owned()))?;

    let is_consumed = node
        .is_nullifier_consumed(&decoded.nullifier_hex)
        .await
        .map_err(map_node_err)?;

    Ok(ResolvedNote {
        transaction_id: private.transaction_id.clone(),
        claimed_sender: private.sender.clone(),
        onchain_sender: snapshot.sender,
        recipient: decoded.recipient,
        asset_faucet: decoded.asset_faucet,
        asset_amount: decoded.asset_amount,
        block_num: snapshot.block_num,
        is_consumed,
    })
}

/// Steps 4–11: rules that depend only on the [`ResolvedNote`] and the
/// merchant's requirements — note-type agnostic.
async fn step_4_11_verify<N: MidenNode + ?Sized>(
    resolved: &ResolvedNote,
    requirements: &MidenPaymentRequirements,
    config: &FacilitatorConfig,
    node: &N,
) -> Result<(), FacilitatorError> {
    let required_amount: u64 = requirements.amount.parse().map_err(|_| {
        FacilitatorError::BadRequest(format!(
            "requirements.amount is not a decimal u64: {}",
            requirements.amount
        ))
    })?;

    // Recipient match.
    if resolved.recipient != requirements.pay_to {
        return Err(FacilitatorError::RecipientMismatch);
    }
    // Asset / amount match.
    if resolved.asset_faucet != requirements.asset || resolved.asset_amount != required_amount {
        return Err(FacilitatorError::AssetMismatch);
    }
    // Sender consistency: payload-claimed sender must match the on-chain
    // metadata sender. (For both note types, sender is observable on chain.)
    if resolved.onchain_sender != resolved.claimed_sender {
        return Err(FacilitatorError::SenderMismatch);
    }
    // Not yet consumed.
    if resolved.is_consumed {
        return Err(FacilitatorError::AlreadyConsumed);
    }
    // Freshness.
    let current = node.latest_block_num().await.map_err(map_node_err)?;
    if current.saturating_sub(resolved.block_num) > config.freshness_blocks {
        return Err(FacilitatorError::Expired {
            block_num: resolved.block_num,
            current,
        });
    }
    Ok(())
}

/// Result of decoding a `PrivateP2idPayload.note_blob`.
///
/// The fields here are derived purely from the off-chain blob. They are
/// bound to chain reality in [`resolve_private`] via the recomputed
/// `note_id_hex` lookup.
struct DecodedPrivateNote {
    note_id_hex: NoteIdHex,
    nullifier_hex: String,
    recipient: AccountIdHex,
    asset_faucet: AccountIdHex,
    asset_amount: u64,
}

/// Decodes a base64-encoded `NoteFile`, validates it carries a P2ID note,
/// and extracts the fields the verifier needs.
fn decode_private_blob(note_blob_b64: &str) -> Result<DecodedPrivateNote, FacilitatorError> {
    let bytes = BASE64
        .decode(note_blob_b64.trim())
        .map_err(|e| FacilitatorError::NoteBlobDecode(format!("base64 decode failed: {e}")))?;
    let note_file = NoteFile::read_from_bytes(&bytes)
        .map_err(|e| FacilitatorError::NoteBlobDecode(format!("NoteFile decode failed: {e}")))?;

    // Only NoteDetails-bearing variants are useful — bare NoteId carries no
    // information for the verifier to bind to chain state.
    let details: NoteDetails = match note_file {
        NoteFile::NoteDetails { details, .. } => details,
        NoteFile::NoteWithProof(note, _proof) => NoteDetails::from(note),
        NoteFile::NoteId(_) => return Err(FacilitatorError::NoteBlobUnsupportedVariant),
    };

    if details.recipient().script().root() != P2idNote::script_root() {
        return Err(FacilitatorError::NoteBlobScriptMismatch);
    }

    let storage =
        P2idNoteStorage::try_from(details.recipient().storage().items()).map_err(|_| {
            FacilitatorError::BadRequest("P2ID storage in note blob is malformed".to_owned())
        })?;
    let recipient = account_id_to_hex(&storage.target())?;

    let (asset_faucet, asset_amount) = details
        .assets()
        .iter()
        .find_map(|asset| match asset {
            Asset::Fungible(fa) => Some((fa.faucet_id(), fa.amount())),
            Asset::NonFungible(_) => None,
        })
        .ok_or(FacilitatorError::AssetMismatch)?;
    let asset_faucet = account_id_to_hex(&asset_faucet)?;

    let note_id: NoteId = (&details).into();
    let note_id_hex: NoteIdHex = note_id.to_hex().parse().map_err(|e: miden_x402_types::IdError| {
        FacilitatorError::Internal(format!("recomputed note id is not canonical hex: {e}"))
    })?;
    let nullifier: Nullifier = (&details).into();
    let nullifier_hex = nullifier.to_hex();

    Ok(DecodedPrivateNote {
        note_id_hex,
        nullifier_hex,
        recipient,
        asset_faucet,
        asset_amount,
    })
}

fn account_id_to_hex(id: &AccountId) -> Result<AccountIdHex, FacilitatorError> {
    id.to_hex().parse().map_err(|e: miden_x402_types::IdError| {
        FacilitatorError::BadRequest(format!("recipient account id is not canonical hex: {e}"))
    })
}

fn map_node_err(err: crate::node::NodeError) -> FacilitatorError {
    use crate::node::NodeError;
    match err {
        NodeError::InvalidIdentifier(msg) => FacilitatorError::BadRequest(msg),
        NodeError::NotePrivate => FacilitatorError::BadRequest(
            "on-chain note is private; payload should use noteType=private".to_owned(),
        ),
        NodeError::NotePublic => FacilitatorError::BadRequest(
            "on-chain note is public; payload should use noteType=public".to_owned(),
        ),
        NodeError::NotP2id => {
            FacilitatorError::BadRequest("on-chain note is not a P2ID note".to_owned())
        }
        NodeError::InvalidP2idStorage => {
            FacilitatorError::BadRequest("on-chain note's P2ID storage is malformed".to_owned())
        }
        NodeError::NoFungibleAsset => FacilitatorError::AssetMismatch,
        NodeError::Rpc(msg) => FacilitatorError::NodeRpc(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FaucetAllowlist;
    use crate::node::NoteSnapshot;
    use crate::node::tests::MockNode;
    use miden_x402_types::{
        AssetTransferMethodTag, ExactScheme, MidenExactExtra, MidenExactPayload, NoteIdHex,
        NoteKind, TransactionIdHex, miden_testnet,
    };
    use std::net::SocketAddr;
    use x402_types::proto::v2::X402Version2;

    fn account(s: &str) -> AccountIdHex {
        s.parse().unwrap()
    }

    fn word(c: char) -> String {
        format!("0x{}", c.to_string().repeat(64))
    }

    fn note_id(c: char) -> NoteIdHex {
        word(c).parse().unwrap()
    }

    fn tx_id(c: char) -> TransactionIdHex {
        word(c).parse().unwrap()
    }

    fn config(freshness: u32) -> FacilitatorConfig {
        FacilitatorConfig {
            listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            rpc_url: "http://localhost".to_owned(),
            rpc_timeout_ms: 1000,
            allowed_faucets: FaucetAllowlist::Any,
            freshness_blocks: freshness,
            guardian: crate::config::GuardianConfig::default(),
        }
    }

    fn requirements() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: ExactScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: account("0x103f8a1ad4b983104aec0412ab0b0d"),
            max_timeout_seconds: 120,
            asset: account("0x0a7d175ed63ec5200fb2ced86f6aa5"),
            extra: MidenExactExtra {
                asset_transfer_method: AssetTransferMethodTag,
                token_symbol: "USDC".to_owned(),
                decimals: 6,
                note_type: NoteKind::Public,
                settlement: miden_x402_types::SettlementKind::Commit,
                guardian_url: None,
                serial_num: None,
                agentic_guardian_url: None,
                mandate_id: None,
                note_tag: None,
            },
        }
    }

    fn payload(req: &MidenPaymentRequirements, sender: &str) -> MidenPaymentPayload {
        MidenPaymentPayload {
            accepted: req.clone(),
            payload: MidenExactPayload::Public(PublicP2idPayload {
                note_id: note_id('a'),
                transaction_id: tx_id('b'),
                sender: account(sender),
                block_num: 100,
                asset: req.asset.clone(),
                amount: req.amount.clone(),
            }),
            resource: None,
            x402_version: X402Version2,
            extensions: None,
        }
    }

    fn snapshot(req: &MidenPaymentRequirements, sender: &str, block_num: u32) -> NoteSnapshot {
        NoteSnapshot {
            block_num,
            sender: account(sender),
            recipient: req.pay_to.clone(),
            asset_faucet: req.asset.clone(),
            asset_amount: 1000,
            is_consumed: false,
        }
    }

    #[tokio::test]
    async fn happy_path_returns_valid() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        node.insert(
            note_id('a'),
            Some(snapshot(&req, "0x857b06519e91e3a54538791bdbb0e2", 100)),
        );

        let res = verify(&pay, &req, &node, &cfg).await.unwrap();
        match res {
            VerifyResponse::Valid { payer } => {
                assert_eq!(payer, "0x857b06519e91e3a54538791bdbb0e2");
            }
            VerifyResponse::Invalid { .. } => panic!("expected Valid"),
        }
    }

    #[tokio::test]
    async fn settle_returns_buyer_transaction_id() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        node.insert(
            note_id('a'),
            Some(snapshot(&req, "0x857b06519e91e3a54538791bdbb0e2", 100)),
        );

        let res = settle(&pay, &req, &node, &cfg).await.unwrap();
        match res {
            SettleResponse::Success {
                payer,
                transaction,
                network,
            } => {
                assert_eq!(payer, "0x857b06519e91e3a54538791bdbb0e2");
                assert_eq!(transaction, word('b'));
                assert_eq!(network, "miden:testnet");
            }
            SettleResponse::Error { .. } => panic!("expected Success"),
        }
    }

    #[tokio::test]
    async fn missing_note_returns_not_found() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        // intentionally do not insert.

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::NoteNotFound(_)));
    }

    #[tokio::test]
    async fn recipient_mismatch_is_rejected() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        let mut snap = snapshot(&req, "0x857b06519e91e3a54538791bdbb0e2", 100);
        snap.recipient = account("0x999999999999999999999999999999");
        node.insert(note_id('a'), Some(snap));

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::RecipientMismatch));
    }

    #[tokio::test]
    async fn amount_mismatch_is_rejected() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        let mut snap = snapshot(&req, "0x857b06519e91e3a54538791bdbb0e2", 100);
        snap.asset_amount = 999;
        node.insert(note_id('a'), Some(snap));

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::AssetMismatch));
    }

    #[tokio::test]
    async fn consumed_note_is_rejected() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        let mut snap = snapshot(&req, "0x857b06519e91e3a54538791bdbb0e2", 100);
        snap.is_consumed = true;
        node.insert(note_id('a'), Some(snap));

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::AlreadyConsumed));
    }

    #[tokio::test]
    async fn stale_note_is_rejected() {
        let cfg = config(5); // tiny window
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(1_000); // tip far ahead
        node.insert(
            note_id('a'),
            Some(snapshot(&req, "0x857b06519e91e3a54538791bdbb0e2", 100)),
        );

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(
            err,
            FacilitatorError::Expired {
                block_num: 100,
                current: 1_000
            }
        ));
    }

    #[tokio::test]
    async fn sender_mismatch_is_rejected() {
        let cfg = config(50);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);
        // Snapshot's sender differs from the claimed payload sender.
        node.insert(
            note_id('a'),
            Some(snapshot(&req, "0x000000000000000000000000000000", 100)),
        );

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::SenderMismatch));
    }

    #[tokio::test]
    async fn note_kind_mismatch_between_extra_and_payload_is_rejected() {
        let cfg = config(50);
        let req = requirements();
        let mut pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        // Force a discriminator mismatch: keep extra=Public, but inject a
        // private payload. The agreement step should catch this even before
        // any node lookup.
        pay.payload = MidenExactPayload::Private(PrivateP2idPayload {
            note_blob: "Zm9v".to_owned(),
            transaction_id: tx_id('b'),
            sender: account("0x857b06519e91e3a54538791bdbb0e2"),
            block_num: 100,
            asset: req.asset.clone(),
            amount: req.amount.clone(),
        });
        let node = MockNode::new(110);

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::BadRequest(_)));
    }

    #[tokio::test]
    async fn private_with_malformed_blob_is_rejected() {
        // extra now says private; payload also says private but the blob is
        // bogus base64 — the verifier should fail at decode, not at lookup.
        let mut req = requirements();
        req.extra.note_type = NoteKind::Private;
        let cfg = config(50);

        let pay = MidenPaymentPayload {
            accepted: req.clone(),
            payload: MidenExactPayload::Private(PrivateP2idPayload {
                note_blob: "@@@not base64@@@".to_owned(),
                transaction_id: tx_id('b'),
                sender: account("0x857b06519e91e3a54538791bdbb0e2"),
                block_num: 100,
                asset: req.asset.clone(),
                amount: req.amount.clone(),
            }),
            resource: None,
            x402_version: X402Version2,
            extensions: None,
        };
        let node = MockNode::new(110);

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::NoteBlobDecode(_)));
    }

    #[tokio::test]
    async fn private_blob_bare_note_id_is_rejected() {
        // A NoteFile carrying only a bare NoteId has no recipient/asset to
        // verify against, so the verifier rejects the variant.
        use miden_client::Serializable;
        use miden_client::note::{NoteFile, NoteId};

        let mut req = requirements();
        req.extra.note_type = NoteKind::Private;
        let cfg = config(50);

        let synthetic_note_id =
            NoteId::try_from_hex(&format!("0x{}", "a".repeat(64))).unwrap();
        let file = NoteFile::NoteId(synthetic_note_id);
        let blob = BASE64.encode(file.to_bytes());

        let pay = MidenPaymentPayload {
            accepted: req.clone(),
            payload: MidenExactPayload::Private(PrivateP2idPayload {
                note_blob: blob,
                transaction_id: tx_id('b'),
                sender: account("0x857b06519e91e3a54538791bdbb0e2"),
                block_num: 100,
                asset: req.asset.clone(),
                amount: req.amount.clone(),
            }),
            resource: None,
            x402_version: X402Version2,
            extensions: None,
        };
        let node = MockNode::new(110);

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::NoteBlobUnsupportedVariant));
    }

    // The "private happy path with a real P2ID blob" is covered end-to-end by
    // `bin/pay_and_verify.rs -- --note-type private` against live testnet
    // rather than in this unit suite, because constructing a real `P2idNote`
    // requires a Miden-native RNG and the round-trip is most informative
    // when bound to actual on-chain state.

    #[tokio::test]
    async fn asset_not_on_allowlist_is_rejected() {
        let mut cfg = config(50);
        cfg.allowed_faucets =
            FaucetAllowlist::Only(vec![account("0xdeadbeefdeadbeefdeadbeefdeadbe")]);
        let req = requirements();
        let pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        let node = MockNode::new(110);

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::AssetNotAllowed(_)));
    }
}
