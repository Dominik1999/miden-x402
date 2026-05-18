//! Composed type aliases that specialise `x402-types`' generic protocol
//! structures for the Miden `exact` scheme.
//!
//! Downstream code (facilitator, SDKs) should use these aliases rather than
//! constructing the generic `x402_types::proto::v2::*` types directly.

use x402_types::proto::v2;

use crate::ids::AccountIdHex;
use crate::scheme::{ExactScheme, MidenExactExtra, MidenExactPayload};

/// `PaymentRequirements` specialised for Miden's `exact` scheme.
///
/// - `scheme` is fixed to the [`ExactScheme`] tag (serialises as `"exact"`).
/// - `amount` is a decimal string of atomic units (Miden tokens are u64).
/// - `payTo` and `asset` are [`AccountIdHex`].
/// - `extra` is [`MidenExactExtra`].
pub type MidenPaymentRequirements =
    v2::PaymentRequirements<ExactScheme, String, AccountIdHex, MidenExactExtra>;

/// `PaymentPayload` specialised for Miden — embeds the Miden requirements and
/// the Miden scheme payload.
pub type MidenPaymentPayload = v2::PaymentPayload<MidenPaymentRequirements, MidenExactPayload>;

/// `PaymentRequired` (the 402 response body) specialised for Miden.
pub type MidenPaymentRequired = v2::PaymentRequired<MidenPaymentRequirements>;

/// `VerifyRequest` body for the facilitator's `POST /verify`.
pub type MidenVerifyRequest = v2::VerifyRequest<MidenPaymentPayload, MidenPaymentRequirements>;

/// `SettleRequest` body for the facilitator's `POST /settle`. Same shape as
/// the verify request.
pub type MidenSettleRequest = MidenVerifyRequest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::miden_testnet;
    use crate::scheme::{
        AssetTransferMethodTag, MidenExactPayload, NoteKind, PublicP2idPayload, SettlementKind,
    };
    use x402_types::proto::v2::{PaymentRequired, ResourceInfo, X402Version2};

    fn sample_account() -> AccountIdHex {
        "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap()
    }

    fn sample_word(byte: char) -> String {
        format!("0x{}", byte.to_string().repeat(64))
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
        assert_eq!(accept["scheme"], "exact");
        assert_eq!(accept["network"], "miden:testnet");
        assert_eq!(accept["amount"], "1000");
        assert_eq!(accept["asset"], "0x0a7d175ed63ec5200fb2ced86f6aa5");
        assert_eq!(accept["payTo"], "0x103f8a1ad4b983104aec0412ab0b0d");
        assert_eq!(accept["maxTimeoutSeconds"], 120);
        assert_eq!(accept["extra"]["assetTransferMethod"], "miden-p2id");
        assert_eq!(accept["extra"]["tokenSymbol"], "USDC");
        assert_eq!(accept["extra"]["decimals"], 6);
        assert_eq!(accept["extra"]["noteType"], "public");

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
        };

        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["x402Version"], 2);
        assert_eq!(value["accepted"]["scheme"], "exact");
        assert_eq!(value["payload"]["noteType"], "public");
        assert_eq!(value["payload"]["amount"], "1000");
        assert!(
            value["payload"]["noteId"]
                .as_str()
                .unwrap()
                .starts_with("0x")
        );

        // Round-trip back.
        let json = serde_json::to_string(&payload).unwrap();
        let decoded: MidenPaymentPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.accepted.amount, "1000");
    }
}
