# miden-x402-types

x402 v2 wire-format types specialised for the `miden-p2id-private`
scheme.

This crate defines the JSON shapes that flow over the `402 Payment
Required` / `Payment-Signature` / `Payment-Response` headers, plus the
back-end JSON the merchant exchanges with the Guardian-facilitator.

The normative wire spec lives in
[`docs/protocol.md`](../../docs/protocol.md); this crate is the Rust
projection of it.

## What ships

| Module | Surface |
|---|---|
| [`scheme`](src/scheme.rs) | `MidenP2idPrivateScheme`, `MidenP2idPrivateExtra`, `MidenP2idPrivatePayload`, `MidenWirePayload` |
| [`network`](src/network.rs) | `miden_testnet()`, `miden_mainnet()` — CAIP-2 chain ids |
| [`ids`](src/ids.rs) | `AccountIdHex`, `NoteIdHex`, `TransactionIdHex` validated hex newtypes |
| [`aliases`](src/aliases.rs) | composed `MidenPaymentRequirements`, `MidenPaymentPayload`, `MidenPaymentRequired`, `MidenVerifyRequest`, `MidenSettleRequest` |
| [`header`](src/header.rs) | base64 codecs + canonical header-name constants |

There is exactly one scheme (`"miden-p2id-private"`) and one payload
variant. The protocol does not carry public-note or settled-at-commit
flows — see [`ideas/DESIGN.md`](../../ideas/DESIGN.md) and
[`docs/protocol.md`](../../docs/protocol.md).

## Tests

```bash
cargo test -p miden-x402-types
```

The integration test
[`tests/wire_round_trip.rs`](tests/wire_round_trip.rs) is the normative
shared fixture for SDK ports: it walks the four wire shapes
(`PaymentRequired`, `PaymentPayload`, `VerifyRequest`,
`SettleResponse`) through encode + decode and asserts every field of
the canonical JSON.
