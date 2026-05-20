//! End-to-end wire-format round-trip test.
//!
//! Simulates the full HTTP exchange between buyer, merchant, and facilitator
//! using only the types and helpers exposed by `miden-x402-types`. This is
//! the contract M4/M5 SDKs (Node, Python) must implement against.
//!
//! The simulation walks every header and JSON shape:
//!
//! 1. Merchant builds a `PaymentRequired` and emits a `Payment-Required`
//!    header value.
//! 2. Buyer decodes it, picks an accept, builds a `PaymentPayload`, emits a
//!    `Payment-Signature` header value.
//! 3. Merchant decodes the signature header and POSTs the JSON body the
//!    facilitator expects (the `VerifyRequest` / `SettleRequest` shape).
//! 4. Facilitator returns a `SettleResponse`; merchant encodes it for the
//!    `Payment-Response` header on the successful HTTP response.
//!
//! Field-level assertions on each step ensure the canonical JSON shape stays
//! stable — language-port SDKs can use this file as a normative reference.

use miden_x402_types::{
    ASSET_TRANSFER_METHOD_P2ID, AccountIdHex, AssetTransferMethodTag, ExactScheme, MidenExactExtra,
    MidenExactPayload, MidenPaymentPayload, MidenPaymentRequired, MidenPaymentRequirements,
    MidenVerifyRequest, NoteIdHex, NoteKind, PAYMENT_REQUIRED_HEADER, PAYMENT_RESPONSE_HEADER,
    PAYMENT_SIGNATURE_HEADER, PrivateP2idPayload, PublicP2idPayload, ResourceInfo, SettleResponse,
    TransactionIdHex, X402Version2, decode_payment_required_header,
    decode_payment_response_header, decode_payment_signature_header,
    encode_payment_required_header, encode_payment_response_header,
    encode_payment_signature_header, miden_testnet,
};

const SAMPLE_FAUCET: &str = "0x0a7d175ed63ec5200fb2ced86f6aa5";
const SAMPLE_MERCHANT: &str = "0x103f8a1ad4b983104aec0412ab0b0d";
const SAMPLE_BUYER: &str = "0x857b06519e91e3a54538791bdbb0e2";
const SAMPLE_NOTE_ID: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SAMPLE_TX_ID: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn faucet() -> AccountIdHex {
    SAMPLE_FAUCET.parse().unwrap()
}
fn merchant() -> AccountIdHex {
    SAMPLE_MERCHANT.parse().unwrap()
}
fn buyer() -> AccountIdHex {
    SAMPLE_BUYER.parse().unwrap()
}
fn note_id() -> NoteIdHex {
    SAMPLE_NOTE_ID.parse().unwrap()
}
fn tx_id() -> TransactionIdHex {
    SAMPLE_TX_ID.parse().unwrap()
}

fn requirements() -> MidenPaymentRequirements {
    MidenPaymentRequirements {
        scheme: ExactScheme,
        network: miden_testnet(),
        amount: "1000".to_owned(),
        pay_to: merchant(),
        max_timeout_seconds: 120,
        asset: faucet(),
        extra: MidenExactExtra {
            asset_transfer_method: AssetTransferMethodTag,
            token_symbol: "USDC".to_owned(),
            decimals: 6,
            note_type: NoteKind::Public,
            settlement: miden_x402_types::SettlementKind::Commit,
            guardian_url: None,
            serial_num: None,
            agentic_guardian_url: None,
            mandate_id: None,
            note_tag: None,
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
        payload: MidenExactPayload::Public(PublicP2idPayload {
            note_id: note_id(),
            transaction_id: tx_id(),
            sender: buyer(),
            block_num: 1_234_567,
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
    // Merchant side: encode the 402 body for the header.
    let required = payment_required();
    let header_value = encode_payment_required_header(&required).unwrap();

    // Sanity: the header name is the canonical one.
    assert_eq!(PAYMENT_REQUIRED_HEADER, "Payment-Required");

    // Buyer side: decode and inspect the canonical JSON shape.
    let decoded = decode_payment_required_header(&header_value).unwrap();
    assert_eq!(decoded.x402_version, X402Version2);
    assert_eq!(decoded.accepts.len(), 1);
    let accept = &decoded.accepts[0];
    assert_eq!(accept.network.to_string(), "miden:testnet");
    assert_eq!(accept.amount, "1000");
    assert_eq!(accept.asset.as_str(), SAMPLE_FAUCET);
    assert_eq!(accept.pay_to.as_str(), SAMPLE_MERCHANT);
    assert_eq!(accept.max_timeout_seconds, 120);
    assert_eq!(
        accept.extra.asset_transfer_method.to_string(),
        ASSET_TRANSFER_METHOD_P2ID
    );
    assert_eq!(accept.extra.token_symbol, "USDC");
    assert_eq!(accept.extra.decimals, 6);
}

/// Step 2: buyer creates a P2ID note on chain, then encodes a
/// `Payment-Signature` header value for the retry request.
#[test]
fn step_2_buyer_emits_payment_signature_header() {
    let required = payment_required();
    let chosen = required.accepts[0].clone();
    let payload = payment_payload(&chosen);

    let header_value = encode_payment_signature_header(&payload).unwrap();
    assert_eq!(PAYMENT_SIGNATURE_HEADER, "Payment-Signature");

    // Merchant side: decode the signature header back.
    let decoded = decode_payment_signature_header(&header_value).unwrap();
    assert_eq!(decoded.x402_version, X402Version2);
    assert_eq!(decoded.accepted.amount, "1000");
    match &decoded.payload {
        MidenExactPayload::Public(p) => {
            assert_eq!(p.note_id.as_str(), SAMPLE_NOTE_ID);
            assert_eq!(p.transaction_id.as_str(), SAMPLE_TX_ID);
            assert_eq!(p.sender.as_str(), SAMPLE_BUYER);
            assert_eq!(p.block_num, 1_234_567);
            assert_eq!(p.amount, "1000");
        }
        MidenExactPayload::Private(_)
        | MidenExactPayload::GuardianFast(_)
        | MidenExactPayload::Agentic(_) => panic!("expected public payload"),
    }
}

/// Step 3: merchant repackages the decoded payload + the offered requirements
/// into the JSON body the facilitator expects on `POST /verify` or
/// `POST /settle`. This is the back-end (merchant → facilitator) wire shape.
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
    assert_eq!(json["paymentRequirements"]["scheme"], "exact");
    assert_eq!(json["paymentRequirements"]["network"], "miden:testnet");
    assert_eq!(json["paymentPayload"]["accepted"]["amount"], "1000");
    assert_eq!(json["paymentPayload"]["payload"]["noteType"], "public");

    // Round-trip back into the typed request.
    let restored: MidenVerifyRequest = serde_json::from_value(json).unwrap();
    assert_eq!(restored.payment_requirements.amount, "1000");
}

/// Step 4: facilitator returns a `SettleResponse::Success`; merchant encodes
/// it for the `Payment-Response` header on the buyer-facing response.
#[test]
fn step_4_merchant_emits_payment_response_header() {
    let settle = SettleResponse::Success {
        payer: SAMPLE_BUYER.to_owned(),
        transaction: SAMPLE_TX_ID.to_owned(),
        network: "miden:testnet".to_owned(),
    };

    let header_value = encode_payment_response_header(&settle).unwrap();
    assert_eq!(PAYMENT_RESPONSE_HEADER, "Payment-Response");

    // Buyer side: decode the response header.
    let decoded = decode_payment_response_header(&header_value).unwrap();
    match decoded {
        SettleResponse::Success {
            payer,
            transaction,
            network,
        } => {
            assert_eq!(payer, SAMPLE_BUYER);
            assert_eq!(transaction, SAMPLE_TX_ID);
            assert_eq!(network, "miden:testnet");
        }
        SettleResponse::Error { .. } => panic!("expected Success"),
    }
}

/// Same wire shape, private-note variant: the buyer carries the canonical
/// `NoteFile` blob in `noteBlob` (base64) and the rest of the receipt fields
/// mirror the public variant.
#[test]
fn private_payload_round_trips_through_signature_header() {
    let mut req = requirements();
    req.extra.note_type = NoteKind::Private;

    let payload = MidenPaymentPayload {
        accepted: req.clone(),
        payload: MidenExactPayload::Private(PrivateP2idPayload {
            note_blob: "Zm9v".to_owned(),
            transaction_id: tx_id(),
            sender: buyer(),
            block_num: 1_234_567,
            asset: faucet(),
            amount: req.amount.clone(),
        }),
        resource: None,
        x402_version: X402Version2,
        extensions: None,
    };

    let header = encode_payment_signature_header(&payload).unwrap();
    let decoded = decode_payment_signature_header(&header).unwrap();
    match &decoded.payload {
        MidenExactPayload::Private(p) => {
            assert_eq!(p.note_blob, "Zm9v");
            assert_eq!(p.transaction_id.as_str(), SAMPLE_TX_ID);
            assert_eq!(p.sender.as_str(), SAMPLE_BUYER);
            assert_eq!(p.block_num, 1_234_567);
            assert_eq!(p.amount, "1000");
        }
        MidenExactPayload::Public(_)
        | MidenExactPayload::GuardianFast(_)
        | MidenExactPayload::Agentic(_) => panic!("expected private payload"),
    }
}

/// Guardian-fast wire variant: the signed-but-unproven flow. Same scheme,
/// same network — just a new payload variant and three optional `extra`
/// fields telling the agent which endpoint to POST to and which serial_num
/// to use.
#[test]
fn guardian_fast_payload_round_trips_through_signature_header() {
    use miden_x402_types::{GuardianFastPayload, SettlementKind};

    let mut req = requirements();
    req.extra.note_type = NoteKind::Private;
    req.extra.settlement = SettlementKind::GuardianFast;
    req.extra.guardian_url = Some("https://facilitator.miden.io".to_owned());
    let serial_num: miden_x402_types::NoteIdHex = format!("0x{}", "c".repeat(64)).parse().unwrap();
    req.extra.serial_num = Some(serial_num.clone());

    let payload = MidenPaymentPayload {
        accepted: req.clone(),
        payload: MidenExactPayload::GuardianFast(GuardianFastPayload {
            tx_inputs: "dHhfaW5wdXRzX2Jsb2I=".to_owned(),
            signature: "c2lnX2Jsb2I=".to_owned(),
            signed_summary: "c3VtbWFyeV9ibG9i".to_owned(),
            expected_note_blob: "Zm9v".to_owned(),
            serial_num,
            transaction_id: tx_id(),
            sender: buyer(),
            asset: faucet(),
            amount: req.amount.clone(),
        }),
        resource: None,
        x402_version: X402Version2,
        extensions: None,
    };

    let header = encode_payment_signature_header(&payload).unwrap();
    let decoded = decode_payment_signature_header(&header).unwrap();
    assert_eq!(
        decoded.accepted.extra.settlement,
        SettlementKind::GuardianFast,
    );
    assert_eq!(
        decoded.accepted.extra.guardian_url.as_deref(),
        Some("https://facilitator.miden.io"),
    );
    match &decoded.payload {
        MidenExactPayload::GuardianFast(p) => {
            assert_eq!(p.tx_inputs, "dHhfaW5wdXRzX2Jsb2I=");
            assert_eq!(p.expected_note_blob, "Zm9v");
            assert_eq!(p.serial_num.as_str(), &format!("0x{}", "c".repeat(64)));
            assert_eq!(p.sender.as_str(), SAMPLE_BUYER);
            assert_eq!(p.amount, "1000");
        }
        MidenExactPayload::Public(_)
        | MidenExactPayload::Private(_)
        | MidenExactPayload::Agentic(_) => panic!("expected guardianFast payload"),
    }
}

/// Agentic wire variant: hot-signed unproven tx + pending state commitment +
/// mandate id. New variant added on `feat/agentic-guardian` branch per
/// `ideas/NEW_DESIGN.md`. Existing `public` / `private` / `guardianFast`
/// variants are untouched.
#[test]
fn agentic_payload_round_trips_through_signature_header() {
    use miden_x402_types::{AgenticPayload, SettlementKind};

    let mut req = requirements();
    req.extra.note_type = NoteKind::Private;
    req.extra.settlement = SettlementKind::Agentic;
    req.extra.agentic_guardian_url = Some("https://agentic-guardian.example".to_owned());
    req.extra.mandate_id = Some("m-abc".to_owned());
    req.extra.note_tag = Some("weather.api".to_owned());
    let serial_num: miden_x402_types::NoteIdHex = format!("0x{}", "c".repeat(64)).parse().unwrap();
    req.extra.serial_num = Some(serial_num.clone());
    let pending_state: miden_x402_types::NoteIdHex =
        format!("0x{}", "d".repeat(64)).parse().unwrap();

    let payload = MidenPaymentPayload {
        accepted: req.clone(),
        payload: MidenExactPayload::Agentic(AgenticPayload {
            tx_inputs: "dHhfaW5wdXRz".to_owned(),
            hot_signature: "aG90X3NpZw==".to_owned(),
            signed_summary: "c3VtbWFyeQ==".to_owned(),
            expected_note_blob: "Zm9v".to_owned(),
            serial_num,
            pending_state_commitment: pending_state.clone(),
            mandate_id: "m-abc".to_owned(),
            sender: buyer(),
            asset: faucet(),
            amount: req.amount.clone(),
        }),
        resource: None,
        x402_version: X402Version2,
        extensions: None,
    };

    let header = encode_payment_signature_header(&payload).unwrap();
    let decoded = decode_payment_signature_header(&header).unwrap();
    assert_eq!(decoded.accepted.extra.settlement, SettlementKind::Agentic);
    assert_eq!(
        decoded.accepted.extra.agentic_guardian_url.as_deref(),
        Some("https://agentic-guardian.example"),
    );
    assert_eq!(decoded.accepted.extra.mandate_id.as_deref(), Some("m-abc"));
    assert_eq!(decoded.accepted.extra.note_tag.as_deref(), Some("weather.api"));
    match &decoded.payload {
        MidenExactPayload::Agentic(p) => {
            assert_eq!(p.tx_inputs, "dHhfaW5wdXRz");
            assert_eq!(p.hot_signature, "aG90X3NpZw==");
            assert_eq!(p.expected_note_blob, "Zm9v");
            assert_eq!(p.mandate_id, "m-abc");
            assert_eq!(p.pending_state_commitment.as_str(), pending_state.as_str());
            assert_eq!(p.sender.as_str(), SAMPLE_BUYER);
            assert_eq!(p.amount, "1000");
        }
        MidenExactPayload::Public(_)
        | MidenExactPayload::Private(_)
        | MidenExactPayload::GuardianFast(_) => panic!("expected agentic payload"),
    }
}

/// Whole flow stitched together: every JSON shape, every header, round-trips
/// cleanly. This is the test M4/M5 SDKs should pass against shared fixtures.
#[test]
fn full_flow_round_trips_through_all_three_headers() {
    // 1. Merchant → buyer: 402 with Payment-Required header.
    let required = payment_required();
    let required_header = encode_payment_required_header(&required).unwrap();
    let buyer_view = decode_payment_required_header(&required_header).unwrap();
    let chosen = buyer_view.accepts[0].clone();
    assert!(matches!(chosen.scheme, ExactScheme));
    assert_eq!(chosen.network.to_string(), "miden:testnet");

    // 2. Buyer → merchant: retry with Payment-Signature header.
    let payload = payment_payload(&chosen);
    let signature_header = encode_payment_signature_header(&payload).unwrap();
    let merchant_view = decode_payment_signature_header(&signature_header).unwrap();
    assert_eq!(merchant_view.accepted, chosen);

    // 3. Merchant → facilitator: POST /verify (or /settle) with JSON body.
    let verify_body = MidenVerifyRequest {
        x402_version: X402Version2,
        payment_payload: merchant_view,
        payment_requirements: chosen.clone(),
    };
    let verify_json = serde_json::to_string(&verify_body).unwrap();
    let _restored: MidenVerifyRequest = serde_json::from_str(&verify_json).unwrap();

    // 4. Facilitator → merchant → buyer: Payment-Response header.
    let settle = SettleResponse::Success {
        payer: SAMPLE_BUYER.to_owned(),
        transaction: SAMPLE_TX_ID.to_owned(),
        network: "miden:testnet".to_owned(),
    };
    let response_header = encode_payment_response_header(&settle).unwrap();
    let buyer_settle = decode_payment_response_header(&response_header).unwrap();
    match buyer_settle {
        SettleResponse::Success {
            payer, transaction, ..
        } => {
            assert_eq!(payer, SAMPLE_BUYER);
            assert_eq!(transaction, SAMPLE_TX_ID);
        }
        SettleResponse::Error { .. } => panic!("expected Success"),
    }
}
