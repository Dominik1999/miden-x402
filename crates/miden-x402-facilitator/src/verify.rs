//! Verify-before-prove logic for `POST /x402/verify` and `POST /x402/settle`.
//!
//! Given a `MidenP2idPrivatePayload` and the matching `MidenPaymentRequirements`,
//! this module:
//!
//! 1. Decodes the four base64 blobs (`tx_inputs`, `signature`, `signed_summary`,
//!    `expected_note_blob`).
//! 2. Consumes the challenge by `serial_num`. Replays fail at this step.
//! 3. Binds `signed_summary` ↔ `tx_inputs` (input-notes commitment) and
//!    ↔ `expected_note_blob` (output-notes membership).
//! 4. Validates the P2ID script, recipient, and asset against the requirements.
//! 5. Verifies the buyer's Falcon signature against one of the buyer's
//!    cosigner commitments stored in Guardian metadata.
//! 6. Calls `check_nullifiers` on the Miden node — backstop against
//!    replays of already-settled txs.
//! 7. Evaluates the `MandatePolicy`.
//! 8. Confirms the buyer has sufficient balance via [`BalanceLookup`].
//! 9. Atomically reserves the input + output nullifiers in
//!    [`crate::storage::ReservationRepo`].
//!
//! On success, returns a [`VerifiedX402Tx`] handle that the settle path
//! consumes to enqueue the tx onto the batch worker.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use miden_client::Deserializable;
use miden_client::account::AccountId;
use miden_client::asset::Asset;
use miden_client::note::{NoteDetails, NoteFile, NoteId, Nullifier};
use miden_protocol::Word;
use miden_protocol::account::auth::{PublicKey, Signature};
use miden_protocol::transaction::{TransactionInputs, TransactionSummary};
use miden_standards::note::{P2idNote, P2idNoteStorage};
use thiserror::Error;

use miden_x402_types::{
    AccountIdHex, MidenP2idPrivatePayload, MidenPaymentRequirements,
};

use crate::buyer_auth::{BuyerAuthError, BuyerAuthLookup};
use crate::balance::{BalanceError, BalanceLookup};
use crate::error::FacilitatorError;
use crate::mandate::{ArcMandatePolicy, MandateContext};
use crate::storage::{ChallengeRepo, ReservationRepo, unix_now};

/// Errors returned by the nullifier-backstop check.
#[derive(Debug, Error)]
pub enum NullifierCheckError {
    #[error("one or more nullifiers are already consumed on chain")]
    Consumed,
    #[error("node RPC error: {0}")]
    Backend(String),
}

/// Pings the Miden node to confirm that none of the supplied nullifiers
/// have already been observed on chain. The verify path runs this as a
/// backstop against replays of already-settled-and-included transactions.
#[async_trait]
pub trait NullifierBackstop: Send + Sync + 'static {
    async fn assert_unspent(&self, nullifiers: &[Nullifier]) -> Result<(), NullifierCheckError>;
}

/// All the inputs a single `verify_unproven` call needs. Packaged as one
/// struct so handler code doesn't have to pass nine arguments.
pub struct VerifyDeps<'a> {
    pub network: &'a str,
    pub challenges: &'a dyn ChallengeRepo,
    pub reservations: &'a dyn ReservationRepo,
    pub reservation_ttl: std::time::Duration,
    pub mandate: ArcMandatePolicy,
    pub buyer_auth: &'a dyn BuyerAuthLookup,
    pub balance: &'a dyn BalanceLookup,
    pub nullifier_backstop: &'a dyn NullifierBackstop,
}

/// Outcome of a successful verification, handed to the settle path.
#[derive(Debug)]
pub struct VerifiedX402Tx {
    /// The parsed-and-bound `TransactionInputs` — fed to the remote prover.
    pub tx_inputs: TransactionInputs,
    /// Raw base64 of `tx_inputs`, retained for the batch queue (so we
    /// don't have to re-serialise).
    pub tx_inputs_b64: String,
    /// Buyer's Falcon signature — re-injected into the advice map at
    /// prove time. Carried for completeness even though the in-tx advice
    /// map already has it.
    pub signature: Signature,
    /// Reserved nullifier hex strings — promoted on submit success,
    /// released on submit failure.
    pub reserved_nullifiers: Vec<String>,
    /// Buyer account id, echoed into `SettleResponse::Success { payer }`.
    pub payer: AccountIdHex,
    /// `signed_summary` commitment — used to compute the queued_id and
    /// (in tests) to anchor the receipt digest.
    pub signed_summary_commitment: Word,
}

/// Performs the full verify-before-prove pipeline.
pub async fn verify_unproven(
    payload: &MidenP2idPrivatePayload,
    requirements: &MidenPaymentRequirements,
    deps: VerifyDeps<'_>,
) -> Result<VerifiedX402Tx, FacilitatorError> {
    use miden_x402_types::network::is_miden;

    // -------------------- 0. requirements sanity ------------------------
    if !is_miden(&requirements.network) {
        return Err(FacilitatorError::UnsupportedNetwork);
    }
    if requirements.network.to_string() != deps.network {
        return Err(FacilitatorError::UnsupportedNetwork);
    }
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

    // -------------------- 1. consume the challenge ----------------------
    let challenge = deps
        .challenges
        .consume(payload.serial_num.as_str())
        .await
        .map_err(|_| FacilitatorError::ChallengeNotFound)?;
    let now = unix_now();
    if challenge.is_expired(now) {
        return Err(FacilitatorError::ChallengeExpired);
    }
    if challenge.requirements.pay_to != requirements.pay_to
        || challenge.requirements.asset != requirements.asset
        || challenge.requirements.amount != requirements.amount
    {
        return Err(FacilitatorError::BadRequest(
            "challenge requirements snapshot does not match live requirements".to_owned(),
        ));
    }

    // -------------------- 2. decode wire blobs --------------------------
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

    // -------------------- 3. bind summary ↔ tx_inputs -------------------
    if signed_summary.input_notes().commitment() != tx_inputs.input_notes().commitment() {
        return Err(FacilitatorError::InputNotesMismatch);
    }

    // -------------------- 4. bind summary ↔ expected blob ---------------
    let expected_note_id: NoteId = (&expected_details).into();
    let output_contains_expected =
        signed_summary.output_notes().iter().any(|n| n.id() == expected_note_id);
    if !output_contains_expected {
        return Err(FacilitatorError::OutputNoteMismatch);
    }

    // -------------------- 5. P2ID script + recipient + asset ------------
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

    // -------------------- 6. sender consistency -------------------------
    let tx_account_sender = account_id_to_hex(&tx_inputs.account().id())?;
    if tx_account_sender != payload.sender {
        return Err(FacilitatorError::SenderMismatch);
    }

    // -------------------- 7. Falcon signature against cosigners ---------
    let commitments = deps
        .buyer_auth
        .cosigner_commitments(&payload.sender)
        .await
        .map_err(|e| match e {
            BuyerAuthError::NotConfigured => {
                FacilitatorError::BadRequest("buyer account not configured on this Guardian".into())
            }
            BuyerAuthError::UnsupportedScheme => {
                FacilitatorError::UnsupportedAuthScheme("not Falcon-512 Poseidon2".into())
            }
            BuyerAuthError::Backend(s) => FacilitatorError::Storage(s),
            BuyerAuthError::InvalidCommitment(s) => FacilitatorError::Storage(s),
        })?;
    verify_falcon_against_cosigners(&signature, &signed_summary, &commitments)?;

    // -------------------- 8. nullifier backstop on chain ----------------
    let mut nullifiers: Vec<Nullifier> = tx_inputs
        .input_notes()
        .iter()
        .map(|n| n.note().nullifier())
        .collect();
    let output_nullifier: Nullifier = (&expected_details).into();
    nullifiers.push(output_nullifier);

    deps.nullifier_backstop
        .assert_unspent(&nullifiers)
        .await
        .map_err(|e| match e {
            NullifierCheckError::Consumed => FacilitatorError::AlreadyConsumed,
            NullifierCheckError::Backend(s) => FacilitatorError::NodeRpc(s),
        })?;

    // -------------------- 9. mandate evaluation -------------------------
    let resource_url = None::<&str>; // optional; not in the payload
    let mandate_ctx = MandateContext {
        buyer: &payload.sender,
        resource_url,
        asset: &payload.asset,
        amount: &payload.amount,
        requirements,
    };
    deps.mandate.evaluate(&mandate_ctx)?;

    // -------------------- 10. balance check -----------------------------
    let required_u128 = required_amount as u128;
    match deps
        .balance
        .check_sufficient(&payload.sender, &payload.asset, required_u128)
        .await
    {
        Ok(_) => {}
        Err(BalanceError::Insufficient { .. }) => {
            return Err(FacilitatorError::InsufficientBalance);
        }
        Err(BalanceError::Backend(s)) => return Err(FacilitatorError::Storage(s)),
    }

    // -------------------- 11. atomically reserve nullifiers -------------
    let reserved_hex: Vec<String> = nullifiers.iter().map(|n| n.to_hex()).collect();
    deps.reservations
        .try_reserve_all(&reserved_hex, deps.reservation_ttl)
        .await?;

    Ok(VerifiedX402Tx {
        tx_inputs,
        tx_inputs_b64: payload.tx_inputs.clone(),
        signature,
        reserved_nullifiers: reserved_hex,
        payer: payload.sender.clone(),
        signed_summary_commitment: signed_summary.to_commitment(),
    })
}

/// Verifies `signature` against the buyer's accepted `cosigner_commitments`.
/// The wire signature carries its public key bundled in; we compute the
/// commitment, confirm membership in `cosigner_commitments`, then run the
/// cryptographic verification.
pub fn verify_falcon_against_cosigners(
    signature: &Signature,
    signed_summary: &TransactionSummary,
    cosigner_commitments: &[Word],
) -> Result<(), FacilitatorError> {
    let falcon_sig = match signature {
        Signature::Falcon512Poseidon2(s) => s,
        Signature::EcdsaK256Keccak(_) => {
            return Err(FacilitatorError::UnsupportedAuthScheme(
                "wire signature is EcdsaK256Keccak; only Falcon-512 Poseidon2 supported".into(),
            ));
        }
    };

    let claimed_pk_high = PublicKey::Falcon512Poseidon2(falcon_sig.public_key().clone());
    let claimed_commitment_word: Word = claimed_pk_high.to_commitment().into();

    let known = cosigner_commitments
        .iter()
        .any(|c| *c == claimed_commitment_word);
    if !known {
        return Err(FacilitatorError::BadSignature);
    }

    let message = signed_summary.to_commitment();
    if !claimed_pk_high.verify(message, signature.clone()) {
        return Err(FacilitatorError::BadSignature);
    }
    Ok(())
}

fn account_id_to_hex(id: &AccountId) -> Result<AccountIdHex, FacilitatorError> {
    id.to_hex()
        .parse()
        .map_err(|e: miden_x402_types::IdError| FacilitatorError::BadRequest(e.to_string()))
}

#[cfg(test)]
mod tests {
    //! These tests cover the **fast-fail** paths of `verify_unproven` —
    //! malformed blobs, missing/expired challenges, network/asset
    //! mismatches. The happy path requires constructing a real signed
    //! `TransactionInputs` + `TransactionSummary` + Falcon signature
    //! against a fixture multisig account, which needs the Miden VM —
    //! covered by the end-to-end integration tests, not here.

    use super::*;
    use crate::balance::test_support::MockBalanceLookup;
    use crate::buyer_auth::test_support::MockBuyerAuthLookup;
    use crate::mandate::AllowAll;
    use crate::storage::memory::{MemoryChallengeRepo, MemoryReservationRepo};
    use crate::storage::{IssuedChallenge, SerializableWord};
    use std::sync::Arc;
    use miden_protocol::Felt;
    use miden_x402_types::{
        MidenP2idPrivateExtra, MidenP2idPrivatePayload, MidenP2idPrivateScheme,
        MidenPaymentRequirements, miden_testnet,
    };
    use std::time::Duration;

    struct AlwaysUnspent;
    #[async_trait]
    impl NullifierBackstop for AlwaysUnspent {
        async fn assert_unspent(&self, _: &[Nullifier]) -> Result<(), NullifierCheckError> {
            Ok(())
        }
    }

    fn sample_word(c: char) -> String {
        format!("0x{}", c.to_string().repeat(64))
    }

    fn random_word() -> Word {
        Word::new([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)])
    }

    fn requirements() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: MidenP2idPrivateScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: "0x103f8a1ad4b983104aec0412ab0b0d".parse().unwrap(),
            max_timeout_seconds: 120,
            asset: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            extra: MidenP2idPrivateExtra {
                note_tag: "t".into(),
                serial_num: Some(sample_word('c').parse().unwrap()),
            },
        }
    }

    fn payload() -> MidenP2idPrivatePayload {
        MidenP2idPrivatePayload {
            tx_inputs: "AAA=".to_owned(),
            signature: "c2ln".to_owned(),
            signed_summary: "c3VtbWFyeQ==".to_owned(),
            expected_note_blob: "Zm9v".to_owned(),
            serial_num: sample_word('c').parse().unwrap(),
            sender: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
            asset: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            amount: "1000".to_owned(),
        }
    }

    async fn deps_for_tests<'a>(
        challenges: &'a MemoryChallengeRepo,
        reservations: &'a MemoryReservationRepo,
        buyer_auth: &'a MockBuyerAuthLookup,
        balance: &'a MockBalanceLookup,
        nullifier_backstop: &'a AlwaysUnspent,
    ) -> VerifyDeps<'a> {
        VerifyDeps {
            network: "miden:testnet",
            challenges,
            reservations,
            reservation_ttl: Duration::from_secs(60),
            mandate: Arc::new(AllowAll),
            buyer_auth,
            balance,
            nullifier_backstop,
        }
    }

    #[tokio::test]
    async fn rejects_unknown_serial_num() {
        let challenges = MemoryChallengeRepo::new();
        let reservations = MemoryReservationRepo::new();
        let buyer_auth = MockBuyerAuthLookup::new();
        let balance = MockBalanceLookup::new();
        let backstop = AlwaysUnspent;
        let deps = deps_for_tests(&challenges, &reservations, &buyer_auth, &balance, &backstop)
            .await;

        let req = requirements();
        let pay = payload();
        let err = verify_unproven(&pay, &req, deps).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::ChallengeNotFound));
    }

    #[tokio::test]
    async fn rejects_garbage_tx_inputs_blob() {
        let challenges = MemoryChallengeRepo::new();
        let reservations = MemoryReservationRepo::new();
        let buyer_auth = MockBuyerAuthLookup::new();
        let balance = MockBalanceLookup::new();
        let backstop = AlwaysUnspent;

        let req = requirements();
        let serial = sample_word('c').parse().unwrap();
        challenges
            .put(IssuedChallenge {
                serial_num: SerializableWord(random_word()),
                serial_num_hex: serial,
                requirements: req.clone(),
                issued_at_unix_secs: unix_now(),
                expires_at_unix_secs: unix_now() + 60,
            })
            .await
            .unwrap();

        let mut pay = payload();
        pay.tx_inputs = "@@not base64@@".into();

        let deps = deps_for_tests(&challenges, &reservations, &buyer_auth, &balance, &backstop)
            .await;
        let err = verify_unproven(&pay, &req, deps).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::TxInputsDecode(_)));
    }

    #[tokio::test]
    async fn rejects_asset_echo_mismatch() {
        let challenges = MemoryChallengeRepo::new();
        let reservations = MemoryReservationRepo::new();
        let buyer_auth = MockBuyerAuthLookup::new();
        let balance = MockBalanceLookup::new();
        let backstop = AlwaysUnspent;

        let req = requirements();
        let serial = sample_word('c').parse().unwrap();
        challenges
            .put(IssuedChallenge {
                serial_num: SerializableWord(random_word()),
                serial_num_hex: serial,
                requirements: req.clone(),
                issued_at_unix_secs: unix_now(),
                expires_at_unix_secs: unix_now() + 60,
            })
            .await
            .unwrap();

        let mut pay = payload();
        pay.asset = "0xdeadbeefdeadbeefdeadbeefdeadbe".parse().unwrap();

        let deps = deps_for_tests(&challenges, &reservations, &buyer_auth, &balance, &backstop)
            .await;
        let err = verify_unproven(&pay, &req, deps).await.unwrap_err();
        assert!(matches!(err, FacilitatorError::BadRequest(_)));
    }
}
