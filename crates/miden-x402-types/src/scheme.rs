//! Miden-specific pieces of the x402 `exact` scheme.
//!
//! The structures here plug into the network- and scheme-agnostic types from
//! `x402-types`. See [`crate::aliases`] for the composed `PaymentRequirements`
//! and `PaymentPayload` aliases that downstream code should use.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use crate::ids::{AccountIdHex, NoteIdHex, TransactionIdHex};

/// String value carried by [`MidenExactExtra::asset_transfer_method`].
pub const ASSET_TRANSFER_METHOD_P2ID: &str = "miden-p2id";

/// Tag struct that always serialises and deserialises as the JSON string
/// `"exact"`.
///
/// This mirrors the pattern used by `x402-rs/x402-chain-eip155` for its own
/// `ExactScheme` marker so that `PaymentRequirements.scheme` is a strongly
/// typed constant rather than a loose string.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ExactScheme;

impl ExactScheme {
    /// The wire representation: `"exact"`.
    pub const VALUE: &'static str = "exact";
}

impl fmt::Display for ExactScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::VALUE)
    }
}

impl Serialize for ExactScheme {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(Self::VALUE)
    }
}

impl<'de> Deserialize<'de> for ExactScheme {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == Self::VALUE {
            Ok(ExactScheme)
        } else {
            Err(serde::de::Error::custom(format!(
                "expected scheme \"{}\", got \"{}\"",
                Self::VALUE,
                raw
            )))
        }
    }
}

/// Tag struct that always serialises as `"miden-p2id"`.
///
/// Used for the `assetTransferMethod` field inside [`MidenExactExtra`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AssetTransferMethodTag;

impl AssetTransferMethodTag {
    /// The wire representation: `"miden-p2id"`.
    pub const VALUE: &'static str = ASSET_TRANSFER_METHOD_P2ID;
}

impl fmt::Display for AssetTransferMethodTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::VALUE)
    }
}

impl Serialize for AssetTransferMethodTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(Self::VALUE)
    }
}

impl<'de> Deserialize<'de> for AssetTransferMethodTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == Self::VALUE {
            Ok(AssetTransferMethodTag)
        } else {
            Err(serde::de::Error::custom(format!(
                "expected assetTransferMethod \"{}\", got \"{}\"",
                Self::VALUE,
                raw
            )))
        }
    }
}

/// Whether a P2ID note is stored on chain (`Public`) or carried in the
/// payment payload (`Private`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    /// Public P2ID — full note + assets stored by the network.
    Public,
    /// Private P2ID — only the commitment and nullifier appear on chain.
    Private,
}

/// Settlement model the buyer expects: `Commit` (default) means
/// settled-at-commit — the buyer proves + submits the create-note tx itself
/// and the facilitator just verifies. `GuardianFast` means verify-before-prove
/// — the buyer hands the facilitator a signed-but-unproven transaction; the
/// facilitator verifies the Falcon signature offline, reserves the input
/// nullifiers, replies OK, then proves + submits asynchronously via a remote
/// prover. Trust model under `GuardianFast` is the same as Base's x402
/// facilitator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettlementKind {
    /// Settled-at-commit. The buyer proves + submits the create-note tx and
    /// the facilitator is a read-only verifier of the on-chain state.
    /// Default — matches the Phase A behaviour and is wire-compatible with
    /// pre-Phase-B clients that don't send this field at all.
    #[default]
    Commit,
    /// Verify-before-prove. The Guardian-facilitator verifies a signed
    /// unproven transaction, reserves the input nullifiers in memory, and
    /// proves + submits the tx asynchronously. Only meaningful with
    /// `noteType: "private"`.
    GuardianFast,
    /// Agentic flow per `ideas/NEW_DESIGN.md`. The buyer (agent) signs an
    /// unproven tx with a **hot key** authorised by an AP2 mandate; the
    /// agentic-guardian verifies the hot-key signature, enforces the
    /// mandate (amount cap, merchant allowlist, time window, daily total),
    /// tracks per-agent pending state (allowing multiple in-flight txs
    /// chained on each other), batches verified txs, then proves +
    /// submits via `SubmitProvenBatch`.
    Agentic,
}

fn is_default_settlement(s: &SettlementKind) -> bool {
    matches!(s, SettlementKind::Commit)
}

/// Contents of the `extra` field on a Miden `exact` [`PaymentRequirements`].
///
/// [`PaymentRequirements`]: x402_types::proto::v2::PaymentRequirements
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidenExactExtra {
    /// Tag indicating the asset transfer mechanism. Always `"miden-p2id"`.
    pub asset_transfer_method: AssetTransferMethodTag,
    /// Symbol of the fungible token, e.g. `"USDC"`.
    pub token_symbol: String,
    /// Number of decimals the token uses.
    pub decimals: u8,
    /// Whether payments use public or private P2ID notes.
    pub note_type: NoteKind,
    /// Settlement model. Defaults to [`SettlementKind::Commit`] when absent
    /// on the wire — preserves Phase A's wire format byte-for-byte.
    #[serde(default, skip_serializing_if = "is_default_settlement")]
    pub settlement: SettlementKind,
    /// Guardian facilitator URL — only meaningful with
    /// `settlement: "guardian-fast"`. Tells the agent which endpoint to POST
    /// the signed unproven transaction to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_url: Option<String>,
    /// Server-generated `serial_num` (32-byte `Word`). Only meaningful with
    /// `settlement: "guardian-fast"`. The buyer MUST use this exact serial
    /// number when constructing the P2ID note so the Guardian knows the
    /// future nullifier the moment it issues the 402. Reused as the
    /// challenge id by the facilitator's `ChallengeStore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_num: Option<NoteIdHex>,
    /// Agentic-guardian endpoint URL — only meaningful with
    /// `settlement: "agentic"`. Tells the agent which agentic-guardian
    /// to POST the signed-unproven tx to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agentic_guardian_url: Option<String>,
    /// Mandate identifier — only meaningful with `settlement: "agentic"`.
    /// Lets the agentic-guardian look up the AP2 mandate that gates this
    /// payment. The agent client populates this from `/agentic/register`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mandate_id: Option<String>,
    /// Opaque tag the merchant attaches for routing incoming P2ID notes
    /// on its side. Optional in M8 (merchants can demultiplex by `payTo`
    /// alone); recommended with `settlement: "agentic"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_tag: Option<String>,
}

/// The scheme-specific payload that travels in `PAYMENT-SIGNATURE`.
///
/// Tagged on `noteType` with three variants:
///
/// - `"public"`: settled-at-commit, public P2ID note. Resolvable from the
///   on-chain commitment alone.
/// - `"private"`: settled-at-commit, private P2ID note. The body lives in the
///   off-chain `noteBlob`; the facilitator binds it to chain by recomputing
///   the commitment.
/// - `"guardianFast"`: verify-before-prove, private P2ID note. Carries a
///   signed-but-unproven `TransactionInputs` blob. The semantic note kind is
///   still private; the discriminator difference indicates the settlement
///   path (Guardian) the facilitator should route through. See
///   [`SettlementKind::GuardianFast`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "noteType", rename_all = "camelCase")]
pub enum MidenExactPayload {
    /// Payment by a public P2ID note. The note + assets are stored by the
    /// Miden network and verifiable by id alone.
    Public(PublicP2idPayload),
    /// Payment by a private P2ID note, settled-at-commit.
    Private(PrivateP2idPayload),
    /// Payment by a signed-but-unproven private P2ID transaction. The
    /// Guardian-facilitator verifies the Falcon signature offline and
    /// proves + submits asynchronously.
    GuardianFast(GuardianFastPayload),
    /// Payment by an agentic-guardian-facilitated signed-but-unproven
    /// private P2ID transaction. Per `ideas/NEW_DESIGN.md`: the buyer is
    /// an agent account with a **hot key** authorised by an AP2 mandate;
    /// the agentic-guardian verifies the hot-key signature, enforces the
    /// mandate, tracks per-agent pending state, then batches + submits.
    Agentic(AgenticPayload),
}

impl MidenExactPayload {
    /// Semantic note kind backing each variant. `Private`, `GuardianFast`,
    /// and `Agentic` are all semantically private notes; only the
    /// settlement flow differs.
    pub fn semantic_note_type(&self) -> NoteKind {
        match self {
            MidenExactPayload::Public(_) => NoteKind::Public,
            MidenExactPayload::Private(_)
            | MidenExactPayload::GuardianFast(_)
            | MidenExactPayload::Agentic(_) => NoteKind::Private,
        }
    }

    /// Settlement model implied by the variant.
    pub fn implied_settlement(&self) -> SettlementKind {
        match self {
            MidenExactPayload::Public(_) | MidenExactPayload::Private(_) => SettlementKind::Commit,
            MidenExactPayload::GuardianFast(_) => SettlementKind::GuardianFast,
            MidenExactPayload::Agentic(_) => SettlementKind::Agentic,
        }
    }
}

/// Public-note payment payload: just enough metadata for the facilitator to
/// resolve the note through node RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicP2idPayload {
    /// Miden `NoteId` (`Word`, 64 hex characters).
    pub note_id: NoteIdHex,
    /// Miden `TransactionId` of the note-creation transaction.
    pub transaction_id: TransactionIdHex,
    /// Account ID of the payer (the note's sender).
    pub sender: AccountIdHex,
    /// Block number in which the note was committed.
    pub block_num: u32,
    /// Faucet account ID of the fungible asset transferred.
    pub asset: AccountIdHex,
    /// Asset amount in atomic units, as a decimal string (x402 v2 convention).
    pub amount: String,
}

/// Private-note payment payload.
///
/// Carries the serialised Miden `NoteFile` so the facilitator can import the
/// note, recompute the commitment, and bind it to the on-chain commitment
/// fetched by `note_id`. The blob is base64-encoded for header safety. The
/// remaining fields mirror [`PublicP2idPayload`] so the wire envelope is
/// uniform across both note types and the facilitator can produce the same
/// `SettleResponse::Success` shape for both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateP2idPayload {
    /// Base64-encoded serialised `miden_protocol::note::NoteFile`.
    pub note_blob: String,
    /// Miden `TransactionId` of the note-creation transaction.
    pub transaction_id: TransactionIdHex,
    /// Account ID of the payer (the note's sender).
    pub sender: AccountIdHex,
    /// Block number in which the note's commitment was committed.
    pub block_num: u32,
    /// Faucet account ID of the fungible asset transferred.
    pub asset: AccountIdHex,
    /// Asset amount in atomic units, as a decimal string (x402 v2 convention).
    pub amount: String,
}

/// Guardian-flow payment payload.
///
/// Carries the signed-but-unproven `TransactionInputs` (base64 of the
/// canonical Miden serialisation), the `expected_note_blob` (so the
/// facilitator can recompute the recipient/asset binding without re-executing
/// the tx), and a copy of the server-issued `serial_num` (which the
/// facilitator uses to look up the challenge in its in-memory store).
///
/// Unlike the `Commit` variants, `transaction_id` here is the *pre-prove* id
/// derived deterministically from `TransactionInputs` — the post-prove
/// `ProvenTransaction` id will be the one returned in `SettleResponse`.
/// `block_num` is intentionally absent because the tx has not yet been
/// included in a block at the time the payload is constructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardianFastPayload {
    /// Base64-encoded canonical `miden_protocol::transaction::TransactionInputs`.
    /// The Guardian uses this for two things: reading the buyer's
    /// `PartialAccount.storage()` to locate the on-chain
    /// `PublicKeyCommitment`, and forwarding to `RemoteTransactionProver` at
    /// settle-time.
    pub tx_inputs: String,
    /// Base64-encoded `miden_protocol::account::auth::Signature` carrying the
    /// buyer's authorization over the tx summary. The Guardian re-injects
    /// this into `tx_args.advice_inputs.map` at prove-and-submit time. Sent
    /// as a separate field (rather than only via the advice map) because the
    /// advice map stores the prepared stack-reversed form which cannot be
    /// inverted back to the high-level [`Signature`] needed for offline
    /// verification.
    ///
    /// [`Signature`]: https://docs.rs/miden-protocol/0.14/miden_protocol/account/auth/enum.Signature.html
    pub signature: String,
    /// Base64-encoded `miden_protocol::transaction::TransactionSummary` —
    /// the exact value whose `.to_commitment()` digest the buyer signed.
    /// The Guardian:
    ///
    /// 1. Verifies `signed_summary.input_notes.commitment() ==
    ///    tx_inputs.input_notes().commitment()` to bind the signed summary
    ///    to the executable tx.
    /// 2. Verifies the recomputed output `NoteId` from `expected_note_blob`
    ///    appears in `signed_summary.output_notes`, binding the summary to
    ///    the buyer's claimed output P2ID note.
    /// 3. Verifies `signature` against `signed_summary.to_commitment()`.
    ///
    /// Carried explicitly because the salt component of the summary is
    /// generated inside the VM kernel during execution and is not
    /// derivable from `TransactionInputs` alone.
    pub signed_summary: String,
    /// Base64-encoded `miden_protocol::note::NoteFile::NoteDetails` for the
    /// expected output note. Used by the facilitator to recompute the
    /// recipient/asset binding and bind it against the tx's
    /// `output_notes_commitment` (which the signature already attests to).
    pub expected_note_blob: String,
    /// Echo of `requirements.extra.serialNum`. Must match the value the
    /// Guardian generated at challenge-issue time, and must match the
    /// `serial_num` baked into `expected_note_blob`.
    pub serial_num: NoteIdHex,
    /// Pre-prove `TransactionId` derived deterministically from
    /// `TransactionInputs`.
    pub transaction_id: TransactionIdHex,
    /// Account ID of the payer (the note's sender).
    pub sender: AccountIdHex,
    /// Faucet account ID of the fungible asset transferred.
    pub asset: AccountIdHex,
    /// Asset amount in atomic units, as a decimal string.
    pub amount: String,
}

/// Agentic-flow payment payload (NEW_DESIGN.md §37-50).
///
/// Carries a signed-but-unproven Miden transaction where the signer is
/// the agent's **hot key**. The agentic-guardian:
///
/// 1. Looks up the buyer's pending state; verifies `pending_state_commitment` matches.
/// 2. Verifies `hot_signature` against `signed_summary.to_commitment()`
///    using the agent's registered hot pubkey.
/// 3. Decodes `expected_note_blob`, validates it's a P2ID to the merchant
///    in the 402; recomputes the note id and binds it to
///    `signed_summary.output_notes`.
/// 4. Evaluates the AP2 mandate (amount cap, merchant allowlist, time
///    window, daily total).
/// 5. Computes input + output nullifiers; reserves them transactionally.
/// 6. Advances pending state for the agent and acks.
///
/// Prove + submit happens later in the agentic-guardian's batch worker
/// (parallel per-tx `RemoteTransactionProver::prove` then a single
/// `SubmitProvenBatch` call).
///
/// Distinguishing fields vs [`GuardianFastPayload`]:
///
/// - `hot_signature` instead of generic `signature` — explicit naming so
///   downstream code makes the role clear.
/// - `pending_state_commitment` — the agent's local view of the
///   account's pending state. The Guardian rejects if it doesn't match
///   the Guardian's tracked pending state.
/// - No `transaction_id` field — the only meaningful id is the post-prove
///   `ProvenTransaction.id()`, which the agentic-guardian returns to the
///   client via `/agentic/status/{queued_id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticPayload {
    /// Base64-encoded canonical `miden_protocol::transaction::TransactionInputs`.
    pub tx_inputs: String,
    /// Base64-encoded `miden_protocol::account::auth::Signature` produced
    /// by the agent's **hot key** over `signed_summary.to_commitment()`.
    pub hot_signature: String,
    /// Base64-encoded `miden_protocol::transaction::TransactionSummary` —
    /// the exact value whose `.to_commitment()` digest the buyer signed.
    pub signed_summary: String,
    /// Base64-encoded `miden_protocol::note::NoteFile::NoteDetails` for
    /// the expected output P2ID note.
    pub expected_note_blob: String,
    /// Echo of `requirements.extra.serialNum`.
    pub serial_num: NoteIdHex,
    /// The agent client's view of the account's pending-state commitment
    /// — the commitment of the state this tx is built against. The
    /// agentic-guardian rejects if this does not match its tracked
    /// pending state for this agent (prevents forks; see NEW_DESIGN
    /// §39).
    pub pending_state_commitment: NoteIdHex,
    /// Echo of `requirements.extra.mandateId`. The agentic-guardian
    /// looks up the AP2 mandate by this id and enforces it.
    pub mandate_id: String,
    /// Account ID of the payer (the agent's Miden account).
    pub sender: AccountIdHex,
    /// Faucet account ID of the fungible asset transferred.
    pub asset: AccountIdHex,
    /// Asset amount in atomic units, as a decimal string.
    pub amount: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_account() -> AccountIdHex {
        "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap()
    }

    fn sample_word(prefix_nibble: char) -> String {
        format!("0x{}", prefix_nibble.to_string().repeat(64))
    }

    fn sample_public_payload() -> PublicP2idPayload {
        PublicP2idPayload {
            note_id: sample_word('a').parse().unwrap(),
            transaction_id: sample_word('b').parse().unwrap(),
            sender: sample_account(),
            block_num: 1_234_567,
            asset: sample_account(),
            amount: "1000".to_owned(),
        }
    }

    #[test]
    fn exact_scheme_serialises_as_string() {
        let json = serde_json::to_string(&ExactScheme).unwrap();
        assert_eq!(json, "\"exact\"");
    }

    #[test]
    fn exact_scheme_rejects_other_values() {
        let res: Result<ExactScheme, _> = serde_json::from_str("\"upto\"");
        assert!(res.is_err());
    }

    #[test]
    fn asset_transfer_method_tag_serialises() {
        let json = serde_json::to_string(&AssetTransferMethodTag).unwrap();
        assert_eq!(json, "\"miden-p2id\"");
    }

    #[test]
    fn note_kind_round_trips() {
        let public = serde_json::to_string(&NoteKind::Public).unwrap();
        assert_eq!(public, "\"public\"");
        let private = serde_json::to_string(&NoteKind::Private).unwrap();
        assert_eq!(private, "\"private\"");
    }

    #[test]
    fn public_payload_serialises_with_camel_case() {
        let payload = MidenExactPayload::Public(sample_public_payload());
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["noteType"], "public");
        assert!(json["noteId"].is_string());
        assert!(json["transactionId"].is_string());
        assert!(json["blockNum"].is_u64());
        assert_eq!(json["amount"], "1000");
    }

    fn sample_private_payload() -> PrivateP2idPayload {
        PrivateP2idPayload {
            note_blob: "Zm9v".to_owned(),
            transaction_id: sample_word('b').parse().unwrap(),
            sender: sample_account(),
            block_num: 1_234_567,
            asset: sample_account(),
            amount: "1000".to_owned(),
        }
    }

    #[test]
    fn private_payload_serialises_with_tag() {
        let payload = MidenExactPayload::Private(sample_private_payload());
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["noteType"], "private");
        assert_eq!(json["noteBlob"], "Zm9v");
        assert!(json["transactionId"].is_string());
        assert!(json["sender"].is_string());
        assert!(json["blockNum"].is_u64());
        assert_eq!(json["amount"], "1000");
    }

    #[test]
    fn private_payload_round_trips() {
        let original = MidenExactPayload::Private(sample_private_payload());
        let json = serde_json::to_string(&original).unwrap();
        let decoded: MidenExactPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn payload_round_trip() {
        let original = MidenExactPayload::Public(sample_public_payload());
        let json = serde_json::to_string(&original).unwrap();
        let decoded: MidenExactPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn unknown_note_type_is_rejected() {
        let json = r#"{"noteType":"escrow","noteBlob":"x"}"#;
        let res: Result<MidenExactPayload, _> = serde_json::from_str(json);
        assert!(res.is_err());
    }

    fn sample_guardian_fast_payload() -> GuardianFastPayload {
        GuardianFastPayload {
            tx_inputs: "AAA=".to_owned(),
            signature: "c2ln".to_owned(),
            signed_summary: "c3VtbWFyeQ==".to_owned(),
            expected_note_blob: "AAB=".to_owned(),
            serial_num: sample_word('c').parse().unwrap(),
            transaction_id: sample_word('b').parse().unwrap(),
            sender: sample_account(),
            asset: sample_account(),
            amount: "1000".to_owned(),
        }
    }

    #[test]
    fn guardian_fast_payload_serialises_with_tag() {
        let payload = MidenExactPayload::GuardianFast(sample_guardian_fast_payload());
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["noteType"], "guardianFast");
        assert_eq!(json["txInputs"], "AAA=");
        assert_eq!(json["expectedNoteBlob"], "AAB=");
        assert!(json["serialNum"].is_string());
        assert!(json["transactionId"].is_string());
        assert!(json["sender"].is_string());
        assert_eq!(json["amount"], "1000");
        assert!(json.get("blockNum").is_none(), "guardianFast payload omits blockNum");
    }

    #[test]
    fn guardian_fast_payload_round_trips() {
        let original = MidenExactPayload::GuardianFast(sample_guardian_fast_payload());
        let json = serde_json::to_string(&original).unwrap();
        let decoded: MidenExactPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn semantic_note_type_maps_correctly() {
        assert_eq!(
            MidenExactPayload::Public(sample_public_payload()).semantic_note_type(),
            NoteKind::Public,
        );
        assert_eq!(
            MidenExactPayload::Private(sample_private_payload()).semantic_note_type(),
            NoteKind::Private,
        );
        // GuardianFast is semantically a private note — only the settlement
        // path differs.
        assert_eq!(
            MidenExactPayload::GuardianFast(sample_guardian_fast_payload()).semantic_note_type(),
            NoteKind::Private,
        );
    }

    #[test]
    fn implied_settlement_maps_correctly() {
        assert_eq!(
            MidenExactPayload::Public(sample_public_payload()).implied_settlement(),
            SettlementKind::Commit,
        );
        assert_eq!(
            MidenExactPayload::Private(sample_private_payload()).implied_settlement(),
            SettlementKind::Commit,
        );
        assert_eq!(
            MidenExactPayload::GuardianFast(sample_guardian_fast_payload()).implied_settlement(),
            SettlementKind::GuardianFast,
        );
    }

    #[test]
    fn settlement_kind_serialises_as_kebab_case() {
        assert_eq!(serde_json::to_string(&SettlementKind::Commit).unwrap(), "\"commit\"");
        assert_eq!(
            serde_json::to_string(&SettlementKind::GuardianFast).unwrap(),
            "\"guardian-fast\"",
        );
    }

    #[test]
    fn extra_round_trips() {
        let extra = MidenExactExtra {
            asset_transfer_method: AssetTransferMethodTag,
            token_symbol: "USDC".to_owned(),
            decimals: 6,
            note_type: NoteKind::Public,
            settlement: SettlementKind::Commit,
            guardian_url: None,
            serial_num: None,
            agentic_guardian_url: None,
            mandate_id: None,
            note_tag: None,
        };
        let json = serde_json::to_string(&extra).unwrap();
        let decoded: MidenExactExtra = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, extra);
        assert!(json.contains("\"assetTransferMethod\":\"miden-p2id\""));
        assert!(json.contains("\"noteType\":\"public\""));
        // Default settlement is omitted on the wire to preserve Phase A
        // byte-for-byte compatibility for any consumer that doesn't know
        // about the field.
        assert!(!json.contains("settlement"));
        assert!(!json.contains("guardianUrl"));
        assert!(!json.contains("serialNum"));
        assert!(!json.contains("agenticGuardianUrl"));
        assert!(!json.contains("mandateId"));
        assert!(!json.contains("noteTag"));
    }

    #[test]
    fn extra_guardian_fast_round_trips_and_emits_fields() {
        let extra = MidenExactExtra {
            asset_transfer_method: AssetTransferMethodTag,
            token_symbol: "USDC".to_owned(),
            decimals: 6,
            note_type: NoteKind::Private,
            settlement: SettlementKind::GuardianFast,
            guardian_url: Some("https://facilitator.miden.io".to_owned()),
            serial_num: Some(sample_word('c').parse().unwrap()),
            agentic_guardian_url: None,
            mandate_id: None,
            note_tag: None,
        };
        let json = serde_json::to_string(&extra).unwrap();
        let decoded: MidenExactExtra = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, extra);
        assert!(json.contains("\"settlement\":\"guardian-fast\""));
        assert!(json.contains("\"guardianUrl\":\"https://facilitator.miden.io\""));
        assert!(json.contains("\"serialNum\":"));
    }

    #[test]
    fn extra_accepts_phase_a_wire_without_settlement_fields() {
        // A Phase A consumer never sent settlement/guardianUrl/serialNum.
        // Phase B types must accept that wire bit-perfect.
        let json = r#"{
            "assetTransferMethod": "miden-p2id",
            "tokenSymbol": "USDC",
            "decimals": 6,
            "noteType": "public"
        }"#;
        let extra: MidenExactExtra = serde_json::from_str(json).unwrap();
        assert_eq!(extra.settlement, SettlementKind::Commit);
        assert!(extra.guardian_url.is_none());
        assert!(extra.serial_num.is_none());
    }
}
