//! Composed type aliases that specialise `x402-types`' generic protocol
//! structures for the `miden-p2id-private` scheme.
//!
//! Downstream code (facilitator, SDKs) should use these aliases rather than
//! constructing the generic `x402_types::proto::v2::*` types directly.

use x402_types::proto::v2;

use crate::ids::AccountIdHex;
use crate::scheme::{MidenP2idPrivateExtra, MidenP2idPrivateScheme, MidenWirePayload};

/// `PaymentRequirements` specialised for the `miden-p2id-private` scheme.
///
/// - `scheme` is fixed to [`MidenP2idPrivateScheme`] (serialises as
///   `"miden-p2id-private"`).
/// - `amount` is a decimal string of atomic units (Miden tokens are `u64`).
/// - `payTo` and `asset` are [`AccountIdHex`].
/// - `extra` is [`MidenP2idPrivateExtra`] (carries `note_tag` + optional
///   server-issued `serial_num`).
pub type MidenPaymentRequirements =
    v2::PaymentRequirements<MidenP2idPrivateScheme, String, AccountIdHex, MidenP2idPrivateExtra>;

/// `PaymentPayload` specialised for Miden — embeds the Miden requirements and
/// the tagged wire payload.
pub type MidenPaymentPayload = v2::PaymentPayload<MidenPaymentRequirements, MidenWirePayload>;

/// `PaymentRequired` (the 402 response body) specialised for Miden.
pub type MidenPaymentRequired = v2::PaymentRequired<MidenPaymentRequirements>;

/// `VerifyRequest` body for `POST /x402/verify`.
pub type MidenVerifyRequest = v2::VerifyRequest<MidenPaymentPayload, MidenPaymentRequirements>;

/// `SettleRequest` body for `POST /x402/settle`. Same shape as the verify
/// request.
pub type MidenSettleRequest = MidenVerifyRequest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::miden_testnet;
    use crate::scheme::{MidenP2idPrivatePayload, MidenWirePayload};
    use x402_types::proto::v2::{PaymentRequired, ResourceInfo, X402Version2};

    fn sample_account() -> AccountIdHex {
        "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap()
    }

    fn sample_word(byte: char) -> String {
        format!("0x{}", byte.to_string().repeat(64))
    }

    fn sample_requirements() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: MidenP2idPrivateScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: "0x103f8a1ad4b983104aec0412ab0b0d".parse().unwrap(),
            max_timeout_seconds: 120,
            asset: sample_account(),
            extra: MidenP2idPrivateExtra {
                note_tag: "weather.api".to_owned(),
                serial_num: Some(sample_word('c').parse().unwrap()),
            },
        }
    }

    fn sample_payload_inner() -> MidenP2idPrivatePayload {
        MidenP2idPrivatePayload {
            tx_inputs: "AAA=".to_owned(),
            signature: "c2ln".to_owned(),
            signed_summary: "c3VtbWFyeQ==".to_owned(),
            expected_note_blob: "Zm9v".to_owned(),
            serial_num: sample_word('c').parse().unwrap(),
            sender: "0x857b06519e91e3a54538791bdbb0e2".parse().unwrap(),
            asset: sample_account(),
            amount: "1000".to_owned(),
        }
    }

    #[test]
    fn payment_required_matches_documented_wire_shape() {
        let required: MidenPaymentRequired = PaymentRequired {
            x402_version: X402Version2,
            error: None,
            resource: Some(ResourceInfo {
                url: "https://api.example.com/weather".to_owned(),
                description: None,
                mime_type: Some("application/json".to_owned()),
            }),
            accepts: vec![sample_requirements()],
        };

        let value = serde_json::to_value(&required).unwrap();

        assert_eq!(value["x402Version"], 2);
        let accept = &value["accepts"][0];
        assert_eq!(accept["scheme"], "miden-p2id-private");
        assert_eq!(accept["network"], "miden:testnet");
        assert_eq!(accept["amount"], "1000");
        assert_eq!(accept["asset"], "0x0a7d175ed63ec5200fb2ced86f6aa5");
        assert_eq!(accept["payTo"], "0x103f8a1ad4b983104aec0412ab0b0d");
        assert_eq!(accept["maxTimeoutSeconds"], 120);
        assert_eq!(accept["extra"]["noteTag"], "weather.api");
        assert!(accept["extra"]["serialNum"].is_string());

        // Round-trip back.
        let json = serde_json::to_string(&required).unwrap();
        let decoded: MidenPaymentRequired = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.accepts.len(), 1);
        assert_eq!(decoded.accepts[0].asset, sample_account());
    }

    #[test]
    fn payment_payload_matches_documented_wire_shape() {
        let payload = MidenPaymentPayload {
            accepted: sample_requirements(),
            payload: MidenWirePayload::from(sample_payload_inner()),
            resource: None,
            x402_version: X402Version2,
            extensions: None,
        };

        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["x402Version"], 2);
        assert_eq!(value["accepted"]["scheme"], "miden-p2id-private");
        assert_eq!(value["payload"]["noteType"], "miden-p2id-private");
        assert_eq!(value["payload"]["amount"], "1000");
        assert_eq!(value["payload"]["txInputs"], "AAA=");

        // Round-trip back.
        let json = serde_json::to_string(&payload).unwrap();
        let decoded: MidenPaymentPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.accepted.amount, "1000");
        assert_eq!(decoded.payload.payer().as_str(), "0x857b06519e91e3a54538791bdbb0e2");
    }
}
