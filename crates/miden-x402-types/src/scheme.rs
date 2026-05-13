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
    /// Phase 2 of this project; declared here so the wire format does not
    /// have to change later.
    Private,
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
}

/// The scheme-specific payload that travels in `PAYMENT-SIGNATURE`.
///
/// Tagged on `noteType`. The `Private` variant is declared so the wire format
/// is stable across Phase 1 (MVP) and Phase 2; a Phase 1 facilitator rejects
/// it with `ErrorReason::UnsupportedScheme`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "noteType", rename_all = "lowercase")]
pub enum MidenExactPayload {
    /// Payment by a public P2ID note. The note + assets are stored by the
    /// Miden network and verifiable by id alone.
    Public(PublicP2idPayload),
    /// Payment by a private P2ID note. Phase 2 only.
    Private(PrivateP2idPayload),
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

/// Private-note payment payload — Phase 2.
///
/// Carries the serialised Miden `NoteFile` so the facilitator can import the
/// note, recompute the commitment, and verify against the node. The blob is
/// base64-encoded for header safety. Not used by Phase 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateP2idPayload {
    /// Base64-encoded serialised `miden_protocol::note::NoteFile`.
    pub note_blob: String,
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

    #[test]
    fn private_payload_serialises_with_tag() {
        let payload = MidenExactPayload::Private(PrivateP2idPayload {
            note_blob: "Zm9v".to_owned(),
        });
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["noteType"], "private");
        assert_eq!(json["noteBlob"], "Zm9v");
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

    #[test]
    fn extra_round_trips() {
        let extra = MidenExactExtra {
            asset_transfer_method: AssetTransferMethodTag,
            token_symbol: "USDC".to_owned(),
            decimals: 6,
            note_type: NoteKind::Public,
        };
        let json = serde_json::to_string(&extra).unwrap();
        let decoded: MidenExactExtra = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, extra);
        assert!(json.contains("\"assetTransferMethod\":\"miden-p2id\""));
        assert!(json.contains("\"noteType\":\"public\""));
    }
}
