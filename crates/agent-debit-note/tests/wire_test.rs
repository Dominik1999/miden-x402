use agent_debit_note::wire::{
    decode_header, encode_header, MidenExtra, PaymentRequired, PaymentRequirements,
};

#[test]
fn round_trip_payment_required() {
    let original = PaymentRequired {
        x402_version: 1,
        accepts: vec![PaymentRequirements {
            scheme: "batch-settlement".into(),
            network: "miden:testnet".into(),
            amount: "1000".into(),
            asset: "0xabcdef1234567890".into(),
            pay_to: "0x1234567890abcdef".into(),
            max_timeout_seconds: 3600,
            extra: Some(MidenExtra {
                min_reclaim_block_buffer: 100,
            }),
        }],
    };

    let encoded = encode_header(&original);
    let decoded: PaymentRequired = decode_header(&encoded).expect("round-trip should succeed");

    assert_eq!(decoded.x402_version, original.x402_version);
    assert_eq!(decoded.accepts.len(), 1);
    let req = &decoded.accepts[0];
    assert_eq!(req.scheme, "batch-settlement");
    assert_eq!(req.network, "miden:testnet");
    assert_eq!(req.amount, "1000");
    assert_eq!(req.asset, "0xabcdef1234567890");
    assert_eq!(req.pay_to, "0x1234567890abcdef");
    assert_eq!(req.max_timeout_seconds, 3600);
    assert_eq!(req.extra.as_ref().unwrap().min_reclaim_block_buffer, 100);
}

#[test]
fn decode_invalid_base64_returns_error() {
    let result: Result<PaymentRequired, _> = decode_header("not-valid-base64!!!");
    assert!(result.is_err());
}

#[test]
fn decode_invalid_json_returns_error() {
    let b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        b"not json",
    );
    let result: Result<PaymentRequired, _> = decode_header(&b64);
    assert!(result.is_err());
}
