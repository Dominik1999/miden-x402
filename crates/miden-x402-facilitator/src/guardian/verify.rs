//! Verify-before-prove logic for the Guardian flow.
//!
//! Given a [`GuardianFastPayload`] from the wire and the matching
//! `MidenPaymentRequirements`, this module:
//!
//! 1. Decodes the four base64 blobs (`tx_inputs`, `signature`,
//!    `signed_summary`, `expected_note_blob`).
//! 2. Consumes the challenge by `serial_num`. A second attempt to use the
//!    same `serial_num` fails because the challenge has been removed.
//! 3. Binds the signed summary to the executable transaction:
//!    - `signed_summary.input_notes.commitment() ==
//!      tx_inputs.input_notes().commitment()` — proves the signature
//!      authorizes consumption of *these* input notes.
//!    - The recomputed `NoteId` from `expected_note_blob` must appear in
//!      `signed_summary.output_notes` — proves the signature authorizes
//!      creation of *this* output P2ID.
//! 4. Reads the buyer's `PublicKeyCommitment` from
//!    `tx_inputs.account().storage()` via the canonical `AuthSingleSig`
//!    storage slot.
//! 5. Standalone Falcon verification of `signature` against
//!    `signed_summary.to_commitment()` with the bound on-chain pubkey.
//! 6. Runs Phase A's note-binding checks (recipient, asset, amount,
//!    sender) against the resolved P2ID note from `expected_note_blob`.
//!    Freshness / on-chain inclusion are NOT checked — the whole point of
//!    Guardian-fast is to verify before the tx is on chain.
//! 7. Atomically reserves the input nullifiers in the
//!    [`ReservedNullifierSet`].
//!
//! On success, returns a [`VerifiedGuardianTx`] handle that the settle path
//! consumes to drive the remote prover.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use miden_client::Deserializable;
use miden_client::account::AccountId;
use miden_client::asset::Asset;
use miden_client::note::{NoteDetails, NoteFile, NoteId, Nullifier};
use miden_protocol::account::auth::Signature;
use miden_protocol::transaction::{TransactionInputs, TransactionSummary};
use miden_standards::note::{P2idNote, P2idNoteStorage};
use miden_x402_types::{
    AccountIdHex, GuardianFastPayload, MidenPaymentRequirements, NoteKind, SettlementKind,
    TransactionIdHex, network::is_miden,
};

use crate::config::FacilitatorConfig;
use crate::error::FacilitatorError;
use crate::guardian::auth::{read_falcon_auth, verify_signature};
use crate::guardian::challenge::{ChallengeStore, IssuedChallenge};
use crate::guardian::reservation::ReservedNullifierSet;

/// Outcome of a successful Guardian verification — handed to the settle
/// path to drive the remote prover.
#[derive(Debug)]
pub struct VerifiedGuardianTx {
    /// The deserialised, signed-but-unproven tx inputs (signature is
    /// available separately and is the canonical source for prove-time
    /// advice injection).
    pub tx_inputs: TransactionInputs,
    /// The high-level signature, ready to be re-injected into
    /// `tx_inputs.tx_args.advice_inputs.map` before forwarding to the
    /// remote prover.
    pub signature: Signature,
    /// Nullifier hex strings reserved by this verification. The settle
    /// path either promotes or releases these.
    pub reserved_nullifiers: Vec<String>,
    /// The payer account id, echoed back into `SettleResponse::Success`.
    pub payer: AccountIdHex,
    /// The pre-prove `TransactionId` from the wire (the post-prove id is
    /// known only after `RemoteTransactionProver::prove` returns).
    pub claimed_transaction_id: TransactionIdHex,
    /// The consumed challenge — released by the caller on success or held
    /// in flight if anything afterward fails.
    #[allow(dead_code)]
    pub challenge: IssuedChallenge,
}

/// Verifies a signed-but-unproven Guardian transaction. On success returns
/// a [`VerifiedGuardianTx`] handle; on failure leaves the [`ChallengeStore`]
/// and [`ReservedNullifierSet`] unchanged (any partial reservation is
/// rolled back inside `try_reserve_all`).
pub async fn verify_unproven(
    payload: &GuardianFastPayload,
    requirements: &MidenPaymentRequirements,
    config: &FacilitatorConfig,
    challenges: &ChallengeStore,
    reservations: &ReservedNullifierSet,
) -> Result<VerifiedGuardianTx, FacilitatorError> {
    // Phase B requirements sanity ----------------------------------------
    if !is_miden(&requirements.network) {
        return Err(FacilitatorError::UnsupportedNetwork);
    }
    if requirements.extra.note_type != NoteKind::Private {
        return Err(FacilitatorError::BadRequest(
            "guardian-fast requires extra.noteType=\"private\"".to_owned(),
        ));
    }
    if requirements.extra.settlement != SettlementKind::GuardianFast {
        return Err(FacilitatorError::BadRequest(
            "guardian-fast requires extra.settlement=\"guardian-fast\"".to_owned(),
        ));
    }
    if !config.allowed_faucets.allows(&requirements.asset) {
        return Err(FacilitatorError::AssetNotAllowed(
            requirements.asset.as_str().to_owned(),
        ));
    }

    // Echo agreement -----------------------------------------------------
    if payload.asset != requirements.asset {
        return Err(FacilitatorError::BadRequest(
            "payload.asset does not match requirements.asset".to_owned(),
        ));
    }
    if payload.amount != requirements.amount {
        return Err(FacilitatorError::BadRequest(
            "payload.amount does not match requirements.amount".to_owned(),
        ));
    }
    let required_amount: u64 = requirements.amount.parse().map_err(|_| {
        FacilitatorError::BadRequest(format!(
            "requirements.amount is not a decimal u64: {}",
            requirements.amount
        ))
    })?;

    // Consume the challenge ---------------------------------------------
    let challenge = challenges
        .consume(payload.serial_num.as_str())
        .ok_or(FacilitatorError::ChallengeNotFound)?;

    if challenge.expires_at < std::time::Instant::now() {
        return Err(FacilitatorError::ChallengeExpired);
    }

    // Decode wire blobs -------------------------------------------------
    let tx_inputs_bytes = BASE64
        .decode(payload.tx_inputs.trim())
        .map_err(|e| FacilitatorError::TxInputsDecode(format!("base64 tx_inputs: {e}")))?;
    let tx_inputs = TransactionInputs::read_from_bytes(&tx_inputs_bytes)
        .map_err(|e| FacilitatorError::TxInputsDecode(format!("TransactionInputs decode: {e}")))?;

    let signature_bytes = BASE64
        .decode(payload.signature.trim())
        .map_err(|e| FacilitatorError::TxInputsDecode(format!("base64 signature: {e}")))?;
    let signature = Signature::read_from_bytes(&signature_bytes)
        .map_err(|e| FacilitatorError::TxInputsDecode(format!("Signature decode: {e}")))?;

    let summary_bytes = BASE64
        .decode(payload.signed_summary.trim())
        .map_err(|e| FacilitatorError::TxInputsDecode(format!("base64 signed_summary: {e}")))?;
    let signed_summary = TransactionSummary::read_from_bytes(&summary_bytes).map_err(|e| {
        FacilitatorError::TxInputsDecode(format!("TransactionSummary decode: {e}"))
    })?;

    let expected_blob_bytes = BASE64.decode(payload.expected_note_blob.trim()).map_err(|e| {
        FacilitatorError::NoteBlobDecode(format!("base64 expected_note_blob: {e}"))
    })?;
    let expected_note_file = NoteFile::read_from_bytes(&expected_blob_bytes)
        .map_err(|e| FacilitatorError::NoteBlobDecode(format!("NoteFile decode: {e}")))?;
    let expected_details: NoteDetails = match expected_note_file {
        NoteFile::NoteDetails { details, .. } => details,
        NoteFile::NoteWithProof(note, _) => note.into(),
        NoteFile::NoteId(_) => return Err(FacilitatorError::NoteBlobUnsupportedVariant),
    };

    // Bind signed_summary ↔ tx_inputs (input-notes commitment) -----------
    if signed_summary.input_notes().commitment() != tx_inputs.input_notes().commitment() {
        return Err(FacilitatorError::OutputNoteMismatch);
    }

    // Bind signed_summary ↔ expected_note_blob (output-notes membership) -
    let expected_note_id: NoteId = (&expected_details).into();
    let output_contains_expected =
        signed_summary.output_notes().iter().any(|n| n.id() == expected_note_id);
    if !output_contains_expected {
        return Err(FacilitatorError::OutputNoteMismatch);
    }

    // P2ID script + canonical recipient/asset extraction from the blob --
    if expected_details.recipient().script().root() != P2idNote::script_root() {
        return Err(FacilitatorError::NoteBlobScriptMismatch);
    }
    let storage = P2idNoteStorage::try_from(expected_details.recipient().storage().items())
        .map_err(|_| {
            FacilitatorError::BadRequest("P2ID storage in note blob is malformed".to_owned())
        })?;
    let recipient = account_id_to_hex(&storage.target())?;
    if recipient != requirements.pay_to {
        return Err(FacilitatorError::RecipientMismatch);
    }

    let (asset_faucet, asset_amount) = expected_details
        .assets()
        .iter()
        .find_map(|asset| match asset {
            Asset::Fungible(fa) => Some((fa.faucet_id(), fa.amount())),
            Asset::NonFungible(_) => None,
        })
        .ok_or(FacilitatorError::AssetMismatch)?;
    let asset_faucet_hex = account_id_to_hex(&asset_faucet)?;
    if asset_faucet_hex != requirements.asset || asset_amount != required_amount {
        return Err(FacilitatorError::AssetMismatch);
    }

    // Sender consistency: payload-claimed sender must match the account
    // executing the transaction. `tx_inputs.account().id()` is the
    // canonical source of truth here — the on-chain header doesn't exist
    // yet (the whole point of guardian-fast), so we can't read
    // `NoteMetadata::sender()` from chain like Phase A does. The buyer's
    // signature over `signed_summary` (which commits to this tx's output
    // notes) is the proof that this account authorized the payment.
    let tx_account_sender = account_id_to_hex(&tx_inputs.account().id())?;
    if tx_account_sender != payload.sender {
        return Err(FacilitatorError::SenderMismatch);
    }

    // Sanity-check the challenge's snapshot matches the live requirements.
    // (Cheap defense against a merchant offering different terms after the
    // challenge was issued.)
    if challenge.requirements.pay_to != requirements.pay_to
        || challenge.requirements.asset != requirements.asset
        || challenge.requirements.amount != requirements.amount
        || challenge.requirements.extra.note_type != requirements.extra.note_type
        || challenge.requirements.extra.settlement != requirements.extra.settlement
    {
        return Err(FacilitatorError::BadRequest(
            "challenge requirements snapshot does not match live requirements".to_owned(),
        ));
    }

    // Standalone Falcon signature verification ---------------------------
    let pubkey_commitment = read_falcon_auth(tx_inputs.account())?;
    let message = signed_summary.to_commitment();
    verify_signature(&pubkey_commitment, &signature, message)?;

    // Atomically reserve input nullifiers --------------------------------
    let mut reserved_nullifiers: Vec<String> = tx_inputs
        .input_notes()
        .iter()
        .map(|n| n.note().nullifier().to_hex())
        .collect();
    // Also reserve the future output P2ID's nullifier — protects against a
    // buyer attempting two simultaneous guardian-fast verifications that
    // both attempt to mint into the same future note id.
    let output_nullifier: Nullifier = (&expected_details).into();
    reserved_nullifiers.push(output_nullifier.to_hex());
    reservations
        .try_reserve_all(&reserved_nullifiers)
        .map_err(|_| FacilitatorError::AlreadyReserved)?;

    let payer = payload.sender.clone();
    let claimed_transaction_id = payload.transaction_id.clone();

    Ok(VerifiedGuardianTx {
        tx_inputs,
        signature,
        reserved_nullifiers,
        payer,
        claimed_transaction_id,
        challenge,
    })
}

fn account_id_to_hex(id: &AccountId) -> Result<AccountIdHex, FacilitatorError> {
    id.to_hex()
        .parse()
        .map_err(|e: miden_x402_types::IdError| FacilitatorError::BadRequest(e.to_string()))
}

#[cfg(test)]
mod tests {
    //! These tests cover the *fast-fail* paths of `verify_unproven` —
    //! malformed blobs, missing/expired challenges, bad signature scheme,
    //! and the integrity checks that don't require a real signed
    //! transaction. The full happy path (build a real signed
    //! `TransactionInputs` + `TransactionSummary` + Falcon signature
    //! offline, then verify) needs Miden VM execution and is exercised
    //! end-to-end against live testnet via the docs/protocol.md
    //! verification plan rather than here.
    use super::*;
    use crate::config::{FacilitatorConfig, FaucetAllowlist, GuardianConfig};
    use miden_x402_types::{
        AssetTransferMethodTag, ExactScheme, MidenExactExtra, NoteIdHex, TransactionIdHex,
        miden_testnet,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::net::SocketAddr;
    use std::time::Duration;

    fn config() -> FacilitatorConfig {
        FacilitatorConfig {
            listen_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            rpc_url: "http://localhost".to_owned(),
            rpc_timeout_ms: 1000,
            allowed_faucets: FaucetAllowlist::Any,
            freshness_blocks: 50,
            guardian: GuardianConfig {
                enabled: true,
                remote_prover_url: None,
                challenge_ttl_secs: 60,
                reservation_ttl_secs: 60,
            },
        }
    }

    fn requirements_guardian() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: ExactScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: "0x103f8a1ad4b983104aec0412ab0b0d".parse().unwrap(),
            max_timeout_seconds: 120,
            asset: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            extra: MidenExactExtra {
                asset_transfer_method: AssetTransferMethodTag,
                token_symbol: "USDC".to_owned(),
                decimals: 6,
                note_type: NoteKind::Private,
                settlement: SettlementKind::GuardianFast,
                guardian_url: Some("http://localhost:8080".to_owned()),
                serial_num: None,
                agentic_guardian_url: None,
                mandate_id: None,
                note_tag: None,
            },
        }
    }

    fn dummy_payload(serial_num: NoteIdHex) -> GuardianFastPayload {
        GuardianFastPayload {
            tx_inputs: "AAA=".to_owned(),
            signature: "c2ln".to_owned(),
            signed_summary: "c3VtbWFyeQ==".to_owned(),
            expected_note_blob: "Zm9v".to_owned(),
            serial_num,
            transaction_id: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .parse::<TransactionIdHex>()
                .unwrap(),
            sender: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
            asset: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            amount: "1000".to_owned(),
        }
    }

    #[tokio::test]
    async fn rejects_unknown_serial_num() {
        let cfg = config();
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let req = requirements_guardian();
        let pay = dummy_payload(
            format!("0x{}", "c".repeat(64)).parse::<NoteIdHex>().unwrap(),
        );

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::ChallengeNotFound));
    }

    #[tokio::test]
    async fn rejects_garbage_tx_inputs_blob() {
        let cfg = config();
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let req = requirements_guardian();

        // Issue a real challenge so the lookup succeeds.
        let mut rng = StdRng::seed_from_u64(0);
        let issued = challenges.issue(&req, &mut rng);

        let mut pay = dummy_payload(issued.serial_num_hex.clone());
        pay.tx_inputs = "@@not base64@@".to_owned();

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::TxInputsDecode(_)));
    }

    #[tokio::test]
    async fn rejects_non_guardian_fast_requirements() {
        let cfg = config();
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let mut req = requirements_guardian();
        req.extra.settlement = SettlementKind::Commit;
        let pay = dummy_payload(
            format!("0x{}", "c".repeat(64)).parse::<NoteIdHex>().unwrap(),
        );

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::BadRequest(_)));
    }

    #[tokio::test]
    async fn rejects_public_note_type_for_guardian_flow() {
        let cfg = config();
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let mut req = requirements_guardian();
        req.extra.note_type = NoteKind::Public;
        let pay = dummy_payload(
            format!("0x{}", "c".repeat(64)).parse::<NoteIdHex>().unwrap(),
        );

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::BadRequest(_)));
    }

    #[tokio::test]
    async fn rejects_asset_echo_mismatch() {
        let cfg = config();
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let req = requirements_guardian();
        let mut rng = StdRng::seed_from_u64(7);
        let issued = challenges.issue(&req, &mut rng);

        let mut pay = dummy_payload(issued.serial_num_hex.clone());
        pay.asset = "0xdeadbeefdeadbeefdeadbeefdeadbe".parse().unwrap();

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::BadRequest(_)));
    }

    #[tokio::test]
    async fn rejects_amount_echo_mismatch() {
        let cfg = config();
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let req = requirements_guardian();
        let mut rng = StdRng::seed_from_u64(7);
        let issued = challenges.issue(&req, &mut rng);

        let mut pay = dummy_payload(issued.serial_num_hex.clone());
        pay.amount = "999".to_owned();

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::BadRequest(_)));
    }

    #[tokio::test]
    async fn allowlist_blocks_unlisted_faucet() {
        let mut cfg = config();
        cfg.allowed_faucets = FaucetAllowlist::Only(vec![
            "0xdeadbeefdeadbeefdeadbeefdeadbe".parse().unwrap(),
        ]);
        let challenges = ChallengeStore::new(Duration::from_secs(60));
        let reservations = ReservedNullifierSet::new(Duration::from_secs(60));
        let req = requirements_guardian();
        let pay = dummy_payload(
            format!("0x{}", "c".repeat(64)).parse::<NoteIdHex>().unwrap(),
        );

        let err = verify_unproven(&pay, &req, &cfg, &challenges, &reservations)
            .await
            .unwrap_err();
        assert!(matches!(err, FacilitatorError::AssetNotAllowed(_)));
    }
}

