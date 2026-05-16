# x402 scheme: `exact` × `miden:testnet`

This document is the formal scheme spec for x402 v2 `exact` payments on
Miden. It is what facilitators and SDK ports implement against. The
generic protocol-level transport (three HTTP headers, JSON request/response
bodies) is covered by [`protocol.md`](./protocol.md); this document
specifies what the `exact` scheme means when `network = "miden:testnet"`.

It is modelled on the `x402-rs` per-scheme spec template.

## 1. Identifiers

| Field | Value |
|---|---|
| `scheme` | `"exact"` |
| `network` | `"miden:testnet"` (provisional CAIP-2 namespace; `miden:mainnet` is reserved) |
| `extra.assetTransferMethod` | `"miden-p2id"` |

`miden:testnet` is **not** a registered CAIP-2 namespace. The chosen value
is provisional and will be revisited when Miden registers an official
identifier upstream.

## 2. `PaymentRequirements.extra` shape

```json
{
  "assetTransferMethod": "miden-p2id",
  "tokenSymbol": "USDC",
  "decimals": 6,
  "noteType": "public"
}
```

| Field | Type | Notes |
|---|---|---|
| `assetTransferMethod` | constant `"miden-p2id"` | locks scheme intent to a P2ID note |
| `tokenSymbol` | string | informational; buyer UX only |
| `decimals` | integer | informational; buyer UX only |
| `noteType` | `"public"` (or `"private"` once Phase 2 lands) | controls how the note is observable on chain |

`amount` is decimal string of atomic units (i.e. `"1000"` = 1000 micro-USDC
at `decimals: 6`). Both `asset` and `payTo` are lowercase `0x`-prefixed
hex Miden account IDs.

## 3. `PaymentPayload.payload` shape — `noteType: "public"`

```json
{
  "noteType": "public",
  "noteId": "0x<32-byte note id>",
  "transactionId": "0x<create-note tx id>",
  "sender": "0x<buyer account id>",
  "blockNum": 1234567,
  "asset": "0x<faucet account id>",
  "amount": "1000"
}
```

There is no separate `signature` field. The cryptographic authorisation
is implicit: a P2ID note exists on chain only because the sender's account
produced a STARK proof of the create-note transaction. No third party can
forge it.

The Phase 2 `"private"` variant carries the canonical NoteFile blob in a
`noteBlob: <base64>` field instead of `noteId`. The MVP facilitator
declares the variant in its types crate so the wire format is forward
compatible, but rejects requests carrying it with
`unsupported_scheme`.

## 4. Verification rules

The facilitator runs the following checks fail-fast in this order. The
canonical implementation lives in
[`crates/miden-x402-facilitator/src/verifier.rs`](../crates/miden-x402-facilitator/src/verifier.rs).

| # | Check | Failure → |
|---|---|---|
| 1 | `paymentPayload.accepted` matches `paymentRequirements` on `network`, `payTo`, `asset`, `amount` | `invalid_payload` |
| 2 | `requirements.network` is Miden | `unsupported_network` |
| 3 | `requirements.asset` is on the facilitator's faucet allowlist | `asset_mismatch` |
| 4 | `paymentPayload.payload.noteType == "public"` | `unsupported_scheme` |
| 5 | The node returns a public `FetchedNote` for `noteId`, committed | `invalid_transaction_state` |
| 6 | `note.recipient().script().root() == P2idNote::script_root()` | `invalid_payload` |
| 7 | `P2idNoteStorage::try_from(note.recipient().storage().items())` yields the `payTo` account id | `invalid_payload` / `recipient_mismatch` |
| 8 | Exactly one fungible asset, faucet equals `requirements.asset` and amount equals `requirements.amount` | `asset_mismatch` |
| 9 | `note.metadata().sender() == paymentPayload.payload.sender` | `invalid_signature` |
| 10 | The nullifier is not yet in the consumed set | `invalid_transaction_state` |
| 11 | `currentBlockNum - note.blockNum <= MIDEN_X402_FRESHNESS_BLOCKS` | `invalid_payment_expired` |

Any violation maps to a canonical x402
[`ErrorReason`](https://docs.rs/x402-types/latest/x402_types/proto/enum.ErrorReason.html);
HTTP status is `400` for client-side failures and `500` for unexpected
node failures. See
[`crates/miden-x402-facilitator/src/error.rs`](../crates/miden-x402-facilitator/src/error.rs).

## 5. Settlement semantics

A successful `/settle` does **not** produce a new on-chain transaction.
The buyer's create-note transaction is the on-chain event that settled
the payment, and the `transaction` field of `SettleResponse::Success` is
that buyer-side transaction id. Under this model `/settle` is idempotent
with `/verify` — the merchant may skip the `/verify` call entirely and
just hit `/settle`, which is what the reference middlewares do.

The merchant is responsible for eventually consuming the note. The
facilitator does not track consumption beyond the nullifier check at
verification time. Consumption can be deferred and batched at the
merchant's convenience without affecting buyers.

## 6. Privacy properties (MVP)

`noteType: "public"` is the MVP scheme. Public P2ID notes expose the
sender, recipient, asset, and amount on chain. This is the same privacy
posture as a standard EVM `transferWithAuthorization` and is documented
explicitly as a known constraint.

Phase 2 ships `noteType: "private"`, where the network stores only the
commitment and nullifier; the note body (script, inputs, assets,
recipient) travels in the `Payment-Signature` body, not on chain. That is
the variant that delivers the "transaction graph is not exposed" property
the scope document calls out. The MVP wire format already reserves the
`"private"` tag so the Phase 2 upgrade is non-breaking.

## 7. Reference implementations

| Layer | Reference |
|---|---|
| Wire types | [`crates/miden-x402-types`](../crates/miden-x402-types) (Rust); [`@miden-x402/types`](../sdks/node/packages/types) (TS); [`miden_x402.types`](../sdks/python/src/miden_x402/types.py) (Python) |
| Facilitator | [`crates/miden-x402-facilitator`](../crates/miden-x402-facilitator) (Rust, canonical) |
| Merchant middleware | [`@miden-x402/merchant`](../sdks/node/packages/merchant) (TS); [`miden_x402.fastapi`](../sdks/python/src/miden_x402/fastapi.py) / [`miden_x402.flask`](../sdks/python/src/miden_x402/flask.py) (Python) |
| Agent client | [`@miden-x402/agent`](../sdks/node/packages/agent) (TS) |
| Live smoke binary | [`crates/miden-x402-facilitator/src/bin/smoke_testnet.rs`](../crates/miden-x402-facilitator/src/bin/smoke_testnet.rs) |
