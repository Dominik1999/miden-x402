//! End-to-end wire-format round-trip test for the `miden-p2id-private`
//! scheme.
//!
//! Simulates the full HTTP exchange between buyer, merchant, and
//! Guardian-facilitator using only the types and helpers exposed by
//! `miden-x402-types`. This is the contract SDKs (Node, Python) must
//! implement against.
//!
//! Steps:
//!
//! 1. Merchant builds a `PaymentRequired` and emits a `Payment-Required`
//!    header value. The `extra.serialNum` is the value the merchant already
//!    obtained from the Guardian-facilitator via `POST /x402/challenge`.
//! 2. Buyer decodes it, builds a signed-unproven `TransactionInputs` +
//!    `TransactionSummary` + Falcon signature (mocked here — actually
//!    constructing those requires the Miden VM), emits a `Payment-Signature`
//!    header value.
//! 3. Merchant decodes the signature header and POSTs the JSON body the
//!    facilitator expects on `POST /x402/verify` / `POST /x402/settle`.
//! 4. Facilitator returns a `SettleResponse::Success`; merchant encodes it
//!    for the `Payment-Response` header on the buyer-facing response.

use miden_x402_types::{
    AccountIdHex, MidenP2idPrivateExtra, MidenP2idPrivatePayload, MidenP2idPrivateScheme,
    MidenPaymentPayload, MidenPaymentRequired, MidenPaymentRequirements, MidenVerifyRequest,
    MidenWirePayload, NoteIdHex, PAYMENT_REQUIRED_HEADER, PAYMENT_RESPONSE_HEADER,
    PAYMENT_SIGNATURE_HEADER, ResourceInfo, SettleResponse, X402Version2,
    decode_payment_required_header, decode_payment_response_header,
    decode_payment_signature_header, encode_payment_required_header,
    encode_payment_response_header, encode_payment_signature_header, miden_testnet,
};

const SAMPLE_FAUCET: &str = "0x0a7d175ed63ec5200fb2ced86f6aa5";
const SAMPLE_MERCHANT: &str = "0x103f8a1ad4b983104aec0412ab0b0d";
const SAMPLE_BUYER: &str = "0x857b06519e91e3a54538791bdbb0e2";
const SAMPLE_QUEUED_ID: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn faucet() -> AccountIdHex {
    SAMPLE_FAUCET.parse().unwrap()
}
fn merchant() -> AccountIdHex {
    SAMPLE_MERCHANT.parse().unwrap()
}
fn buyer() -> AccountIdHex {
    SAMPLE_BUYER.parse().unwrap()
}
fn serial_num() -> NoteIdHex {
    format!("0x{}", "c".repeat(64)).parse().unwrap()
}

fn requirements() -> MidenPaymentRequirements {
    MidenPaymentRequirements {
        scheme: MidenP2idPrivateScheme,
        network: miden_testnet(),
        amount: "1000".to_owned(),
        pay_to: merchant(),
        max_timeout_seconds: 120,
        asset: faucet(),
        extra: MidenP2idPrivateExtra {
            note_tag: "weather.api".to_owned(),
            serial_num: Some(serial_num()),
        },
    }
}

fn payment_required() -> MidenPaymentRequired {
    MidenPaymentRequired {
        x402_version: X402Version2,
        error: None,
        resource: Some(ResourceInfo {
            url: "https://api.example.com/weather".to_owned(),
            description: Some("current weather".to_owned()),
            mime_type: Some("application/json".to_owned()),
        }),
        accepts: vec![requirements()],
    }
}

fn payment_payload(req: &MidenPaymentRequirements) -> MidenPaymentPayload {
    MidenPaymentPayload {
        accepted: req.clone(),
        payload: MidenWirePayload::from(MidenP2idPrivatePayload {
            tx_inputs: "dHhfaW5wdXRzX2Jsb2I=".to_owned(),
            signature: "c2lnX2Jsb2I=".to_owned(),
            signed_summary: "c3VtbWFyeV9ibG9i".to_owned(),
            expected_note_blob: "bm90ZV9ibG9i".to_owned(),
            serial_num: serial_num(),
            sender: buyer(),
            asset: faucet(),
            amount: req.amount.clone(),
        }),
        resource: None,
        x402_version: X402Version2,
        extensions: None,
    }
}

/// Step 1: a 402 response from the merchant carries the `Payment-Required`
/// header with a base64-encoded `MidenPaymentRequired` value.
#[test]
fn step_1_merchant_emits_payment_required_header() {
    let required = payment_required();
    let header_value = encode_payment_required_header(&required).unwrap();

    assert_eq!(PAYMENT_REQUIRED_HEADER, "Payment-Required");

    let decoded = decode_payment_required_header(&header_value).unwrap();
    assert_eq!(decoded.x402_version, X402Version2);
    assert_eq!(decoded.accepts.len(), 1);
    let accept = &decoded.accepts[0];
    assert_eq!(accept.network.to_string(), "miden:testnet");
    assert_eq!(accept.amount, "1000");
    assert_eq!(accept.asset.as_str(), SAMPLE_FAUCET);
    assert_eq!(accept.pay_to.as_str(), SAMPLE_MERCHANT);
    assert_eq!(accept.max_timeout_seconds, 120);
    assert_eq!(accept.extra.note_tag, "weather.api");
    assert_eq!(accept.extra.serial_num.as_ref().unwrap().as_str(), serial_num().as_str());
}

/// Step 2: buyer constructs a signed-unproven tx + emits a
/// `Payment-Signature` header value for the retry request.
#[test]
fn step_2_buyer_emits_payment_signature_header() {
    let required = payment_required();
    let chosen = required.accepts[0].clone();
    let payload = payment_payload(&chosen);

    let header_value = encode_payment_signature_header(&payload).unwrap();
    assert_eq!(PAYMENT_SIGNATURE_HEADER, "Payment-Signature");

    let decoded = decode_payment_signature_header(&header_value).unwrap();
    assert_eq!(decoded.x402_version, X402Version2);
    assert_eq!(decoded.accepted.amount, "1000");
    let inner = decoded.payload.inner();
    assert_eq!(inner.tx_inputs, "dHhfaW5wdXRzX2Jsb2I=");
    assert_eq!(inner.serial_num.as_str(), serial_num().as_str());
    assert_eq!(inner.sender.as_str(), SAMPLE_BUYER);
    assert_eq!(inner.amount, "1000");
}

/// Step 3: merchant repackages the decoded payload + the offered requirements
/// into the JSON body the facilitator expects on `POST /x402/verify` or
/// `POST /x402/settle`. This is the back-end (merchant → facilitator) wire
/// shape.
#[test]
fn step_3_merchant_builds_facilitator_request_body() {
    let required = payment_required();
    let chosen = required.accepts[0].clone();
    let payload = payment_payload(&chosen);

    let signature_header = encode_payment_signature_header(&payload).unwrap();
    let decoded_payload = decode_payment_signature_header(&signature_header).unwrap();

    let request = MidenVerifyRequest {
        x402_version: X402Version2,
        payment_payload: decoded_payload,
        payment_requirements: chosen,
    };

    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["x402Version"], 2);
    assert_eq!(json["paymentRequirements"]["scheme"], "miden-p2id-private");
    assert_eq!(json["paymentRequirements"]["network"], "miden:testnet");
    assert_eq!(json["paymentPayload"]["accepted"]["amount"], "1000");
    assert_eq!(json["paymentPayload"]["payload"]["noteType"], "miden-p2id-private");

    let restored: MidenVerifyRequest = serde_json::from_value(json).unwrap();
    assert_eq!(restored.payment_requirements.amount, "1000");
}

/// Step 4: facilitator returns a `SettleResponse::Success`; merchant encodes
/// it for the `Payment-Response` header on the buyer-facing response.
#[test]
fn step_4_merchant_emits_payment_response_header() {
    let settle = SettleResponse::Success {
        payer: SAMPLE_BUYER.to_owned(),
        transaction: SAMPLE_QUEUED_ID.to_owned(),
        network: "miden:testnet".to_owned(),
    };

    let header_value = encode_payment_response_header(&settle).unwrap();
    assert_eq!(PAYMENT_RESPONSE_HEADER, "Payment-Response");

    let decoded = decode_payment_response_header(&header_value).unwrap();
    match decoded {
        SettleResponse::Success { payer, transaction, network } => {
            assert_eq!(payer, SAMPLE_BUYER);
            assert_eq!(transaction, SAMPLE_QUEUED_ID);
            assert_eq!(network, "miden:testnet");
        }
        SettleResponse::Error { .. } => panic!("expected Success"),
    }
}

/// Whole flow stitched together: every JSON shape, every header, round-trips
/// cleanly. This is the test the Node and Python SDKs should pass against
/// shared fixtures.
#[test]
fn full_flow_round_trips_through_all_three_headers() {
    let required = payment_required();
    let required_header = encode_payment_required_header(&required).unwrap();
    let buyer_view = decode_payment_required_header(&required_header).unwrap();
    let chosen = buyer_view.accepts[0].clone();
    assert!(matches!(chosen.scheme, MidenP2idPrivateScheme));
    assert_eq!(chosen.network.to_string(), "miden:testnet");

    let payload = payment_payload(&chosen);
    let signature_header = encode_payment_signature_header(&payload).unwrap();
    let merchant_view = decode_payment_signature_header(&signature_header).unwrap();
    assert_eq!(merchant_view.accepted, chosen);

    let verify_body = MidenVerifyRequest {
        x402_version: X402Version2,
        payment_payload: merchant_view,
        payment_requirements: chosen.clone(),
    };
    let verify_json = serde_json::to_string(&verify_body).unwrap();
    let _restored: MidenVerifyRequest = serde_json::from_str(&verify_json).unwrap();

    let settle = SettleResponse::Success {
        payer: SAMPLE_BUYER.to_owned(),
        transaction: SAMPLE_QUEUED_ID.to_owned(),
        network: "miden:testnet".to_owned(),
    };
    let response_header = encode_payment_response_header(&settle).unwrap();
    let buyer_settle = decode_payment_response_header(&response_header).unwrap();
    match buyer_settle {
        SettleResponse::Success { payer, transaction, .. } => {
            assert_eq!(payer, SAMPLE_BUYER);
            assert_eq!(transaction, SAMPLE_QUEUED_ID);
        }
        SettleResponse::Error { .. } => panic!("expected Success"),
    }
}
