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
| `noteType` | `"public"` or `"private"` | controls how the note is observable on chain |

`amount` is decimal string of atomic units (i.e. `"1000"` = 1000 micro-USDC
at `decimals: 6`). Both `asset` and `payTo` are lowercase `0x`-prefixed
hex Miden account IDs.

## 3. `PaymentPayload.payload` shape

Both note kinds use a discriminated union on `noteType`. The receipt fields
(`transactionId`, `sender`, `blockNum`, `asset`, `amount`) are uniform; only
the binding to the on-chain commitment differs.

### 3.1 `noteType: "public"`

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

### 3.2 `noteType: "private"`

```json
{
  "noteType": "private",
  "noteBlob": "bm90ZQEK...<base64 of canonical NoteFile>",
  "transactionId": "0x<create-note tx id>",
  "sender": "0x<buyer account id>",
  "blockNum": 1234567,
  "asset": "0x<faucet account id>",
  "amount": "1000"
}
```

### 3.3 `noteType: "guardianFast"` (Phase B — verify-before-prove)

```json
{
  "noteType": "guardianFast",
  "txInputs":         "<base64(TransactionInputs)>",
  "signature":        "<base64(Signature)>",
  "signedSummary":    "<base64(TransactionSummary)>",
  "expectedNoteBlob": "<base64(NoteFile::NoteDetails)>",
  "serialNum":        "0x<server-generated>",
  "transactionId":    "0x<pre-prove tx id>",
  "sender":           "0x<buyer account id>",
  "asset":            "0x<faucet account id>",
  "amount":           "1000"
}
```

This variant is only valid when `extra.settlement == "guardian-fast"` and
`extra.noteType == "private"`. It carries a signed-but-unproven transaction
that the Guardian facilitator verifies offline and proves + submits
asynchronously. See [protocol.md §A.2.7](./protocol.md) for the full flow.

There is no `blockNum` because the tx is not yet on chain. The
`transactionId` is the pre-prove id; the post-prove id is returned in
`SettleResponse.transaction` by `/guardian/settle`.

`noteBlob` is the base64 encoding of a Miden canonical
[`NoteFile`](https://docs.rs/miden-protocol/0.14.5/miden_protocol/note/enum.NoteFile.html).
Acceptable variants are `NoteDetails { details, after_block_num, tag }` and
`NoteWithProof(Note, NoteInclusionProof)`. A bare `NoteId` is rejected
(it carries no information the facilitator can bind to chain state).

There is no separate `signature` field in either variant. The cryptographic
authorisation is implicit: a P2ID note exists on chain only because the
sender's account produced a STARK proof of the create-note transaction. No
third party can forge it.

## 4. Verification rules

The facilitator runs the following checks fail-fast in this order. The
canonical implementation lives in
[`crates/miden-x402-facilitator/src/verifier.rs`](../crates/miden-x402-facilitator/src/verifier.rs).

Both `noteType` values flow through a single pipeline — only rule 5 branches
on the discriminator (recipient and asset come from the on-chain note for
`public`, and from the off-chain `noteBlob` for `private`, bound to chain
state by recomputing the `noteId`).

| # | Check | Failure → |
|---|---|---|
| 1 | `paymentPayload.accepted` matches `paymentRequirements` on `network`, `payTo`, `asset`, `amount`; `payload.payload` discriminator matches `extra.noteType` | `invalid_payload` |
| 2 | `requirements.network` is Miden | `unsupported_network` |
| 3 | `requirements.asset` is on the facilitator's faucet allowlist | `asset_mismatch` |
| 4 | `paymentPayload.payload.noteType` ∈ `{"public", "private"}` | `unsupported_scheme` |
| 5a | *Public:* `get_notes_by_id` returns `FetchedNote::Public(note, proof)` for `noteId`, committed | `invalid_transaction_state` |
| 5b | *Private:* `noteBlob` base64-decodes to a `NoteFile::{NoteDetails, NoteWithProof}`; the recomputed `noteId` resolves to `FetchedNote::Private(header, proof)` | `invalid_payload` / `invalid_transaction_state` |
| 6 | `note.recipient().script().root() == P2idNote::script_root()` (from on-chain note for public; from `NoteFile` body for private) | `invalid_payload` |
| 7 | `P2idNoteStorage::try_from(note.recipient().storage().items())` yields the `payTo` account id | `invalid_payload` / `recipient_mismatch` |
| 8 | Exactly one fungible asset, faucet equals `requirements.asset` and amount equals `requirements.amount` | `asset_mismatch` |
| 9 | `metadata.sender() == paymentPayload.payload.sender` (always read from chain — public for both note kinds) | `invalid_signature` |
| 10 | The nullifier is not yet in the consumed set (recomputed off-chain for private; deterministic in serial_num/script/storage/asset only) | `invalid_transaction_state` |
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

## 6. Privacy properties

`noteType: "public"`: P2ID notes expose the sender, recipient, asset, and
amount on chain. Same privacy posture as a standard EVM
`transferWithAuthorization`. The note's NoteId is queryable by anyone with
`get_notes_by_id` and returns the full body. Choose this when the merchant
or buyer needs the public auditability of every payment.

`noteType: "private"`: only the note commitment and (post-consumption) the
nullifier appear on chain. The note body (script, inputs, assets,
recipient) travels in the `Payment-Signature` body, never on chain. The
on-chain `NoteHeader` exposes the sender account id and the note tag (a
recipient-discovery hint), which is the expected and documented exposure
surface. This variant delivers the "transaction graph is not exposed"
property the scope document calls out: an external observer can see *that*
a tx happened and who initiated it, but not *what* it transferred nor *to
whom* (beyond the tag hint).

## 7. Reference implementations

| Layer | Reference |
|---|---|
| Wire types | [`crates/miden-x402-types`](../crates/miden-x402-types) (Rust); [`@miden-x402/types`](../sdks/node/packages/types) (TS); [`miden_x402.types`](../sdks/python/src/miden_x402/types.py) (Python) |
| Facilitator | [`crates/miden-x402-facilitator`](../crates/miden-x402-facilitator) (Rust, canonical) |
| Merchant middleware | [`@miden-x402/merchant`](../sdks/node/packages/merchant) (TS); [`miden_x402.fastapi`](../sdks/python/src/miden_x402/fastapi.py) / [`miden_x402.flask`](../sdks/python/src/miden_x402/flask.py) (Python) |
| Agent client | [`@miden-x402/agent`](../sdks/node/packages/agent) (TS) |
| Live smoke binary | [`crates/miden-x402-facilitator/src/bin/smoke_testnet.rs`](../crates/miden-x402-facilitator/src/bin/smoke_testnet.rs) |
