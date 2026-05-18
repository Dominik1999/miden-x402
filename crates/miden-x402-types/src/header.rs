//! HTTP header contract for the public x402 payment exchange.
//!
//! When a buyer talks to a merchant over HTTP, three headers carry the
//! base64-encoded JSON pieces of the protocol. The canonical names match
//! x402 v2 as implemented by `x402-rs`:
//!
//! | Constant | Header name | Direction | Body |
//! |---|---|---|---|
//! | [`PAYMENT_REQUIRED_HEADER`] | `Payment-Required` | merchant → buyer (on 402) | base64(JSON([`MidenPaymentRequired`])) |
//! | [`PAYMENT_SIGNATURE_HEADER`] | `Payment-Signature` | buyer → merchant (retry) | base64(JSON([`MidenPaymentPayload`])) |
//! | [`PAYMENT_RESPONSE_HEADER`] | `Payment-Response` | merchant → buyer (success) | base64(JSON(`SettleResponse`)) |
//!
//! The encoding is identical for all three: serialise the value to JSON,
//! then base64-encode it (standard alphabet). The generic
//! [`encode_payment_header`] / [`decode_payment_header`] handle any
//! [`serde::Serialize`] / [`serde::de::DeserializeOwned`] value. Type-specific
//! convenience wrappers below pin the type for each header to remove
//! ambiguity in language-port SDKs.

use std::borrow::Cow;

use thiserror::Error;
use x402_types::proto::v1::SettleResponse;
use x402_types::util::Base64Bytes;

use crate::aliases::{MidenPaymentPayload, MidenPaymentRequired};

/// Name of the HTTP header carrying a base64-encoded `MidenPaymentRequired`
/// on a `402 Payment Required` response from a merchant.
pub const PAYMENT_REQUIRED_HEADER: &str = "Payment-Required";

/// Name of the HTTP header carrying a base64-encoded `MidenPaymentPayload`
/// on the buyer's retry request after a `402`.
pub const PAYMENT_SIGNATURE_HEADER: &str = "Payment-Signature";

/// Name of the HTTP header carrying a base64-encoded `SettleResponse` on a
/// successful merchant response (the body still holds the paid resource).
pub const PAYMENT_RESPONSE_HEADER: &str = "Payment-Response";

/// Errors that may occur while encoding or decoding a payment header.
#[derive(Debug, Error)]
pub enum HeaderError {
    /// The header value was not valid base64.
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// The decoded bytes were not valid JSON for the requested type.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Generic encoder: serialises any value to JSON and returns its base64
/// representation, suitable for placing in one of the payment headers.
pub fn encode_payment_header<T: serde::Serialize>(value: &T) -> Result<String, HeaderError> {
    let json = serde_json::to_vec(value)?;
    let bytes = Base64Bytes::encode(json);
    // `Base64Bytes::Display` prints the base64 string. We collect it into an
    // owned `String` so the caller doesn't have to worry about lifetimes.
    Ok(bytes.to_string())
}

/// Generic decoder: parses a base64-encoded header value into a typed value.
pub fn decode_payment_header<T: serde::de::DeserializeOwned>(
    header: &str,
) -> Result<T, HeaderError> {
    let bytes = Base64Bytes(Cow::Borrowed(header.as_bytes())).decode()?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Encodes a [`MidenPaymentRequired`] for the [`PAYMENT_REQUIRED_HEADER`].
pub fn encode_payment_required_header(value: &MidenPaymentRequired) -> Result<String, HeaderError> {
    encode_payment_header(value)
}

/// Decodes the value of a [`PAYMENT_REQUIRED_HEADER`] into a
/// [`MidenPaymentRequired`].
pub fn decode_payment_required_header(header: &str) -> Result<MidenPaymentRequired, HeaderError> {
    decode_payment_header(header)
}

/// Encodes a [`MidenPaymentPayload`] for the [`PAYMENT_SIGNATURE_HEADER`].
pub fn encode_payment_signature_header(value: &MidenPaymentPayload) -> Result<String, HeaderError> {
    encode_payment_header(value)
}

/// Decodes the value of a [`PAYMENT_SIGNATURE_HEADER`] into a
/// [`MidenPaymentPayload`].
pub fn decode_payment_signature_header(header: &str) -> Result<MidenPaymentPayload, HeaderError> {
    decode_payment_header(header)
}

/// Encodes a [`SettleResponse`] for the [`PAYMENT_RESPONSE_HEADER`].
pub fn encode_payment_response_header(value: &SettleResponse) -> Result<String, HeaderError> {
    encode_payment_header(value)
}

/// Decodes the value of a [`PAYMENT_RESPONSE_HEADER`] into a [`SettleResponse`].
pub fn decode_payment_response_header(header: &str) -> Result<SettleResponse, HeaderError> {
    decode_payment_header(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aliases::MidenPaymentRequirements;
    use crate::ids::AccountIdHex;
    use crate::network::miden_testnet;
    use crate::scheme::{
        AssetTransferMethodTag, ExactScheme, MidenExactExtra, MidenExactPayload, NoteKind,
        PublicP2idPayload, SettlementKind,
    };
    use x402_types::proto::v2::{PaymentRequired, ResourceInfo, X402Version2};

    fn sample_account() -> AccountIdHex {
        "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap()
    }

    fn sample_word(c: char) -> String {
        format!("0x{}", c.to_string().repeat(64))
    }

    fn sample_requirements() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: ExactScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: "0x103f8a1ad4b983104aec0412ab0b0d".parse().unwrap(),
            max_timeout_seconds: 120,
            asset: sample_account(),
            extra: MidenExactExtra {
                asset_transfer_method: AssetTransferMethodTag,
                token_symbol: "USDC".to_owned(),
                decimals: 6,
                note_type: NoteKind::Public,
                settlement: SettlementKind::Commit,
                guardian_url: None,
                serial_num: None,
            },
        }
    }

    fn sample_payload() -> MidenPaymentPayload {
        MidenPaymentPayload {
            accepted: sample_requirements(),
            payload: MidenExactPayload::Public(PublicP2idPayload {
                note_id: sample_word('a').parse().unwrap(),
                transaction_id: sample_word('b').parse().unwrap(),
                sender: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
                block_num: 1_234_567,
                asset: sample_account(),
                amount: "1000".to_owned(),
            }),
            resource: None,
            x402_version: X402Version2,
            extensions: None,
        }
    }

    fn sample_required() -> MidenPaymentRequired {
        PaymentRequired {
            x402_version: X402Version2,
            error: None,
            resource: Some(ResourceInfo {
                url: "https://api.example.com/weather".to_owned(),
                description: None,
                mime_type: Some("application/json".to_owned()),
            }),
            accepts: vec![sample_requirements()],
        }
    }

    #[test]
    fn header_names_are_stable() {
        assert_eq!(PAYMENT_REQUIRED_HEADER, "Payment-Required");
        assert_eq!(PAYMENT_SIGNATURE_HEADER, "Payment-Signature");
        assert_eq!(PAYMENT_RESPONSE_HEADER, "Payment-Response");
    }

    #[test]
    fn round_trip_payment_payload_through_header() {
        let original = sample_payload();
        let header = encode_payment_header(&original).unwrap();
        let decoded: MidenPaymentPayload = decode_payment_header(&header).unwrap();
        assert_eq!(decoded.accepted.amount, original.accepted.amount);
        assert_eq!(decoded.payload, original.payload);
    }

    #[test]
    fn type_specific_signature_helpers_round_trip() {
        let original = sample_payload();
        let header = encode_payment_signature_header(&original).unwrap();
        let decoded = decode_payment_signature_header(&header).unwrap();
        assert_eq!(decoded.payload, original.payload);
    }

    #[test]
    fn type_specific_required_helpers_round_trip() {
        let original = sample_required();
        let header = encode_payment_required_header(&original).unwrap();
        let decoded = decode_payment_required_header(&header).unwrap();
        assert_eq!(decoded.accepts.len(), 1);
        assert_eq!(decoded.accepts[0].network.to_string(), "miden:testnet");
    }

    #[test]
    fn type_specific_response_helpers_round_trip() {
        let original = SettleResponse::Success {
            payer: "0x857b06519e91e3a54538791bdbb0e2".to_owned(),
            transaction: sample_word('b'),
            network: "miden:testnet".to_owned(),
        };
        let header = encode_payment_response_header(&original).unwrap();
        let decoded = decode_payment_response_header(&header).unwrap();
        match decoded {
            SettleResponse::Success {
                payer,
                transaction,
                network,
            } => {
                assert_eq!(payer, "0x857b06519e91e3a54538791bdbb0e2");
                assert_eq!(transaction, sample_word('b'));
                assert_eq!(network, "miden:testnet");
            }
            SettleResponse::Error { .. } => panic!("expected Success"),
        }
    }

    #[test]
    fn invalid_base64_is_reported() {
        let res: Result<MidenPaymentPayload, _> = decode_payment_header("!!! not base64 !!!");
        assert!(matches!(res, Err(HeaderError::Base64(_))));
    }

    #[test]
    fn invalid_json_after_base64_is_reported() {
        // Valid base64 of "not json".
        let header = "bm90IGpzb24=";
        let res: Result<MidenPaymentPayload, _> = decode_payment_header(header);
        assert!(matches!(res, Err(HeaderError::Json(_))));
    }
}
