# miden-x402-types

x402 v2 wire-format types specialised for the Miden network.

This crate defines the JSON shapes that flow over the `402 Payment Required`
HTTP exchange when payment settles in a Miden P2ID note. It re-uses the
network- and scheme-agnostic types from [`x402-types`](https://crates.io/crates/x402-types)
and adds Miden-specific pieces:

- A CAIP-2 chain identifier for Miden (`miden:testnet`, `miden:mainnet`).
- Validated hex newtypes for `AccountId`, `NoteId`, and `TransactionId`.
- A `MidenExactPayload` enum tagged on `noteType` with three variants:
  `public` and `private` (both settled-at-commit) and `guardianFast`
  (verify-before-prove via a Guardian facilitator). The variant any
  particular merchant uses is signaled via `extra.settlement` (defaults to
  `"commit"`).
- Base64 helpers for the `PAYMENT-SIGNATURE` and `PAYMENT-RESPONSE` headers.

The crate is wire-format-only: no network I/O, no `miden-client` dependency.

## Example JSON

`PaymentRequired` (server → client, in the 402 body):

```json
{
  "x402Version": 2,
  "resource": {
    "url": "https://api.example.com/weather",
    "mimeType": "application/json"
  },
  "accepts": [
    {
      "scheme": "exact",
      "network": "miden:testnet",
      "amount": "1000",
      "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
      "payTo": "0x103f8a1ad4b983104aec0412ab0b0d",
      "maxTimeoutSeconds": 120,
      "extra": {
        "assetTransferMethod": "miden-p2id",
        "tokenSymbol": "USDC",
        "decimals": 6,
        "noteType": "public"
      }
    }
  ]
}
```

`PaymentPayload.payload` (client → server, base64-of-JSON in `PAYMENT-SIGNATURE`).

Public variant:

```json
{
  "noteType": "public",
  "noteId": "0x<64-hex>",
  "transactionId": "0x<64-hex>",
  "sender": "0x<30-hex>",
  "blockNum": 1234567,
  "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
  "amount": "1000"
}
```

Private variant (M7 — same trust + latency profile as public; only the
on-chain transaction graph is hidden):

```json
{
  "noteType": "private",
  "noteBlob": "<base64 of canonical NoteFile>",
  "transactionId": "0x<64-hex>",
  "sender": "0x<30-hex>",
  "blockNum": 1234567,
  "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
  "amount": "1000"
}
```

Guardian-fast variant (M8 — verify-before-prove; sub-second perceived
latency; Guardian trust model; `extra.settlement == "guardian-fast"` and
`extra.noteType == "private"` required):

```json
{
  "noteType": "guardianFast",
  "txInputs":         "<base64 of TransactionInputs>",
  "signature":        "<base64 of miden_protocol::account::auth::Signature>",
  "signedSummary":    "<base64 of TransactionSummary>",
  "expectedNoteBlob": "<base64 of NoteFile::NoteDetails>",
  "serialNum":        "0x<server-generated 32-byte word>",
  "transactionId":    "0x<pre-prove tx id>",
  "sender":           "0x<30-hex>",
  "asset":            "0x0a7d175ed63ec5200fb2ced86f6aa5",
  "amount":           "1000"
}
```

The full wire contract is [`docs/protocol.md`](../../docs/protocol.md);
the scheme-specific binding lives in
[`docs/scheme_exact_miden.md`](../../docs/scheme_exact_miden.md).

## Note on CAIP-2

`miden:testnet` is a **provisional** CAIP-2 identifier; Miden has not yet
registered an official namespace with the Chain Agnostic standards working
group. We will migrate when an official identifier is assigned.

## License

Apache-2.0.
