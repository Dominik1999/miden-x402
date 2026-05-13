//! Base64 transport helpers for the `PAYMENT-SIGNATURE` and `PAYMENT-RESPONSE`
//! headers.
//!
//! The x402 v2 wire convention is: serialise the payload to JSON, then
//! base64-encode it before placing it in an HTTP header. These two functions
//! wrap [`x402_types::util::Base64Bytes`] so callers don't need to manage the
//! intermediate `Vec<u8>`.

use std::borrow::Cow;

use thiserror::Error;
use x402_types::util::Base64Bytes;

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

/// Serialises a value to JSON and returns the base64 representation suitable
/// for an HTTP header (e.g. `PAYMENT-SIGNATURE`).
pub fn encode_payment_header<T: serde::Serialize>(value: &T) -> Result<String, HeaderError> {
    let json = serde_json::to_vec(value)?;
    let bytes = Base64Bytes::encode(json);
    // `Base64Bytes::Display` prints the base64 string. We collect it into an owned `String`
    // so the caller doesn't have to worry about the wrapper's lifetime.
    Ok(bytes.to_string())
}

/// Decodes a base64-encoded header back into a deserialisable value.
pub fn decode_payment_header<T: serde::de::DeserializeOwned>(
    header: &str,
) -> Result<T, HeaderError> {
    let bytes = Base64Bytes(Cow::Borrowed(header.as_bytes())).decode()?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aliases::MidenPaymentPayload;
    use crate::ids::AccountIdHex;
    use crate::network::miden_testnet;
    use crate::scheme::{
        AssetTransferMethodTag, ExactScheme, MidenExactExtra, MidenExactPayload, NoteKind,
        PublicP2idPayload,
    };
    use x402_types::proto::v2::X402Version2;

    fn sample_account() -> AccountIdHex {
        "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap()
    }

    fn sample_word(c: char) -> String {
        format!("0x{}", c.to_string().repeat(64))
    }

    fn sample_payload() -> MidenPaymentPayload {
        MidenPaymentPayload {
            accepted: crate::aliases::MidenPaymentRequirements {
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
                },
            },
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

    #[test]
    fn round_trip_payment_payload_through_header() {
        let original = sample_payload();
        let header = encode_payment_header(&original).unwrap();
        let decoded: MidenPaymentPayload = decode_payment_header(&header).unwrap();
        assert_eq!(decoded.accepted.amount, original.accepted.amount);
        assert_eq!(decoded.payload, original.payload);
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
