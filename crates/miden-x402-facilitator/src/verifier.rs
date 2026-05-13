//! Pure verification logic for the Miden `exact` x402 scheme.
//!
//! The verifier is split from the HTTP handlers so it can be exercised in
//! tests without spinning up an axum router or a real Miden node — see the
//! `MockNode` impl in [`crate::node`]'s tests for the test pattern.
//!
//! The checks follow §5 of the project `PLAN.md`:
//!
//! 1. Scheme / network / asset / payTo agreement between payload and
//!    requirements.
//! 2. Note resolution against the node by id.
//! 3. Recipient extracted from canonical P2ID storage matches `payTo`.
//! 4. Asset (faucet + amount) matches the requirements exactly.
//! 5. Nullifier is not yet on chain.
//! 6. Block-number freshness within the configured window.
//! 7. Payload-claimed sender matches the on-chain note's sender.

use miden_x402_types::{
    AccountIdHex, MidenExactPayload, MidenPaymentPayload, MidenPaymentRequirements,
    PublicP2idPayload, network::is_miden,
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
    let (payer, _) = check_payment(payload, requirements, node, config).await?;
    Ok(VerifyResponse::valid(payer.into_inner()))
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
    let (payer, transaction_id) = check_payment(payload, requirements, node, config).await?;
    Ok(SettleResponse::Success {
        payer: payer.into_inner(),
        transaction: transaction_id.into_inner(),
        network: requirements.network.to_string(),
    })
}

/// Shared verification path used by both `verify` and `settle`.
///
/// Returns `(payer, buyer_create_tx_id)` on success.
async fn check_payment<N: MidenNode + ?Sized>(
    payload: &MidenPaymentPayload,
    requirements: &MidenPaymentRequirements,
    node: &N,
    config: &FacilitatorConfig,
) -> Result<(AccountIdHex, miden_x402_types::TransactionIdHex), FacilitatorError> {
    // 1. Scheme + network sanity.
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

    // 2. Asset allowlist (set at the facilitator deployment level).
    if !config.allowed_faucets.allows(&requirements.asset) {
        return Err(FacilitatorError::AssetNotAllowed(
            requirements.asset.as_str().to_owned(),
        ));
    }

    // 3. Payload-kind dispatch.
    let public = match &payload.payload {
        MidenExactPayload::Public(p) => p,
        MidenExactPayload::Private(_) => {
            return Err(FacilitatorError::PrivateNotSupported);
        }
    };

    check_public(public, requirements, node, config).await
}

async fn check_public<N: MidenNode + ?Sized>(
    public: &PublicP2idPayload,
    requirements: &MidenPaymentRequirements,
    node: &N,
    config: &FacilitatorConfig,
) -> Result<(AccountIdHex, miden_x402_types::TransactionIdHex), FacilitatorError> {
    if public.asset != requirements.asset {
        return Err(FacilitatorError::BadRequest(
            "payload.asset does not match requirements.asset".to_owned(),
        ));
    }
    if public.amount != requirements.amount {
        return Err(FacilitatorError::BadRequest(
            "payload.amount does not match requirements.amount".to_owned(),
        ));
    }

    // Parse the required amount once; we'll need it for the asset check.
    let required_amount: u64 = requirements.amount.parse().map_err(|_| {
        FacilitatorError::BadRequest(format!(
            "requirements.amount is not a decimal u64: {}",
            requirements.amount
        ))
    })?;

    // 4. Resolve the note on chain.
    let snapshot = node
        .fetch_public_p2id_note(&public.note_id)
        .await
        .map_err(map_node_err)?
        .ok_or_else(|| FacilitatorError::NoteNotFound(public.note_id.as_str().to_owned()))?;

    // 5. Recipient match.
    if snapshot.recipient != requirements.pay_to {
        return Err(FacilitatorError::RecipientMismatch);
    }

    // 6. Asset / amount match.
    if snapshot.asset_faucet != requirements.asset || snapshot.asset_amount != required_amount {
        return Err(FacilitatorError::AssetMismatch);
    }

    // 7. Sender consistency.
    if snapshot.sender != public.sender {
        return Err(FacilitatorError::SenderMismatch);
    }

    // 8. Not yet consumed.
    if snapshot.is_consumed {
        return Err(FacilitatorError::AlreadyConsumed);
    }

    // 9. Freshness.
    let current = node.latest_block_num().await.map_err(map_node_err)?;
    if current.saturating_sub(snapshot.block_num) > config.freshness_blocks {
        return Err(FacilitatorError::Expired {
            block_num: snapshot.block_num,
            current,
        });
    }

    Ok((snapshot.sender, public.transaction_id.clone()))
}

fn map_node_err(err: crate::node::NodeError) -> FacilitatorError {
    use crate::node::NodeError;
    match err {
        NodeError::InvalidIdentifier(msg) => FacilitatorError::BadRequest(msg),
        NodeError::NotePrivate => {
            FacilitatorError::NoteNotFound("note is private; cannot resolve via id".to_owned())
        }
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
    async fn private_payload_is_rejected_for_now() {
        let cfg = config(50);
        let req = requirements();
        let mut pay = payload(&req, "0x857b06519e91e3a54538791bdbb0e2");
        pay.payload = MidenExactPayload::Private(miden_x402_types::PrivateP2idPayload {
            note_blob: "Zm9v".to_owned(),
        });
        let node = MockNode::new(110);

        let err = verify(&pay, &req, &node, &cfg).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::PrivateNotSupported));
    }

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
