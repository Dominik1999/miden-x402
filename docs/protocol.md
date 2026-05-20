# miden-x402 wire protocol

This document is the **normative contract** between the buyer, the merchant,
and the Guardian-facilitator. Anyone porting the merchant middleware or
buyer client to a new language (Node, Python, Go, etc.) implements against
this document.

The design that drives this protocol lives at
[`ideas/DESIGN.md`](../ideas/DESIGN.md): use the OpenZeppelin Guardian as
the x402 facilitator, with verify-before-prove and batched settlement.

There are two distinct HTTP exchanges:

- **A. Public payment exchange.** Buyer ↔ merchant. Three HTTP headers carry
  the payment data; the rest of the request is the buyer's normal API call.
- **B. Back-end facilitator API.** Merchant ↔ Guardian-facilitator (and
  buyer ↔ Guardian-facilitator). JSON request/response bodies, plus
  Guardian-style auth headers on every authenticated route.

All JSON examples use `camelCase` keys. All identifiers are lowercase
`0x`-prefixed hex. All amounts are decimal strings of atomic token units.

---

## A. Public payment exchange (buyer ↔ merchant)

### A.1 Headers

| Constant | Header | Direction | Body |
|---|---|---|---|
| `PAYMENT_REQUIRED_HEADER` | `Payment-Required` | merchant → buyer (`402`) | base64(JSON(`PaymentRequired`)) |
| `PAYMENT_SIGNATURE_HEADER` | `Payment-Signature` | buyer → merchant (retry) | base64(JSON(`PaymentPayload`)) |
| `PAYMENT_RESPONSE_HEADER` | `Payment-Response` | merchant → buyer (`200`) | base64(JSON(`SettleResponse`)) |

Base64 alphabet is **standard** (RFC 4648 §4, `+`/`/` with `=` padding).

### A.2 Step-by-step

#### A.2.1 Initial request

Buyer makes a normal HTTP request. No payment headers attached:

```http
GET /weather HTTP/1.1
Host: api.example.com
Accept: application/json
```

#### A.2.2 Merchant calls the facilitator's `/x402/challenge`

Before emitting the 402, the merchant gets a server-generated `serial_num`
from the Guardian-facilitator (see §B.1). The merchant authenticates this
call as itself (Guardian-style `x-pubkey`/`x-signature`/`x-timestamp`).

#### A.2.3 `402 Payment Required`

```http
HTTP/1.1 402 Payment Required
Payment-Required: eyJ4NDAyVmVyc2lvbiI6Miwi...   # base64(JSON(PaymentRequired))
```

The header value, base64-decoded:

```json
{
  "x402Version": 2,
  "resource": {
    "url": "https://api.example.com/weather",
    "description": "current weather",
    "mimeType": "application/json"
  },
  "accepts": [
    {
      "scheme": "miden-p2id-private",
      "network": "miden:testnet",
      "amount": "1000",
      "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
      "payTo": "0x103f8a1ad4b983104aec0412ab0b0d",
      "maxTimeoutSeconds": 120,
      "extra": {
        "noteTag": "weather.api",
        "serialNum": "0xcccc...cc"
      }
    }
  ]
}
```

Notes:

- `scheme` is always `"miden-p2id-private"` — this protocol only carries
  the private-note, verify-before-prove path. There is no public-note or
  settled-at-commit variant.
- `network` uses x402 v2 CAIP-2 form (`namespace:reference`). DESIGN.md
  uses the casual `"miden-mainnet"` form; the on-the-wire value is
  `"miden:mainnet"` (or `"miden:testnet"`). See
  [`docs/UPSTREAM_WISHLIST.md`](./UPSTREAM_WISHLIST.md) for why.
- `extra.noteTag` is an opaque tag the merchant uses to route incoming
  P2ID notes.
- `extra.serialNum` is the server-issued `serial_num` the merchant
  acquired from `POST /x402/challenge`. The buyer MUST use this exact
  value when constructing the P2ID note.

#### A.2.4 Buyer constructs a signed-but-unproven Miden transaction

Outside the HTTP exchange, the buyer uses a Miden client (e.g.
`miden-multisig-client` from the OZ Guardian repo) to:

1. Build a `P2idNote` with recipient = `accepts[0].payTo`, asset =
   `accepts[0].asset`, amount = `accepts[0].amount`, and `serial_num`
   = `accepts[0].extra.serialNum`.
2. Build a `TransactionInputs` that consumes the buyer's funds and
   creates this P2ID note as output.
3. Execute the transaction locally **without proving** to obtain a
   canonical `TransactionSummary`.
4. Sign `TransactionSummary::to_commitment()` with a Falcon-512 cosigner
   key authorised on the buyer account.

The buyer does NOT prove or submit the tx to the Miden node. The
Guardian-facilitator does that asynchronously after verifying.

#### A.2.5 Retry with `Payment-Signature`

```http
GET /weather HTTP/1.1
Host: api.example.com
Payment-Signature: eyJ4NDAyVmVyc2lvbiI6Miwi...  # base64(JSON(PaymentPayload))
```

The header value, base64-decoded:

```json
{
  "x402Version": 2,
  "accepted": { /* the exact `accepts[i]` the merchant offered */ },
  "payload": {
    "noteType": "miden-p2id-private",
    "txInputs":         "<base64(TransactionInputs)>",
    "signature":        "<base64(Signature)>",
    "signedSummary":    "<base64(TransactionSummary)>",
    "expectedNoteBlob": "<base64(NoteFile::NoteDetails)>",
    "serialNum":        "0xcccc...cc",
    "sender":           "0x857b06519e91e3a54538791bdbb0e2",
    "asset":            "0x0a7d175ed63ec5200fb2ced86f6aa5",
    "amount":           "1000"
  }
}
```

The buyer SDK MUST set `accepted` to **the exact same object** the merchant
offered, including unknown future fields.

#### A.2.6 Merchant calls the facilitator's `/x402/settle`

The merchant forwards the decoded payload + the matched requirements to
`POST /x402/settle` (see §B.3), authenticating itself.

#### A.2.7 Successful response

```http
HTTP/1.1 200 OK
Content-Type: application/json
Payment-Response: eyJzdWNjZXNzIjp0cnVlLCJw...   # base64(JSON(SettleResponse))

{ "temperature": 21.5, "city": "Istanbul" }
```

The header value, base64-decoded:

```json
{
  "success": true,
  "payer": "0x857b06519e91e3a54538791bdbb0e2",
  "transaction": "0xqueued_id...",
  "network": "miden:testnet",
  "receiptSig": "<base64(Falcon signature)>",
  "receiptPubkeyCommitment": "0xfacilitator_pubkey_commitment..."
}
```

- `transaction` is the deterministic **queued id** (`blake3(serial_num ||
  signed_summary.commitment())`), not yet the on-chain id. The facilitator
  resolves it to the post-prove `ProvenTransaction.id()` once the batch
  worker drains. Clients can look it up later via the facilitator's
  internal mapping (out of scope for this document).
- `receiptSig` is a Falcon-512 signature over `RPO256([payer, queuedId,
  networkHash])` produced by the facilitator's own receipt-signing key.
  Merchants cache the facilitator's pubkey via `GET /x402/pubkey` and
  retain receipts for accounting / auditing.

### A.3 Error responses

If verification fails the merchant returns another 402:

```json
{
  "x402Version": 2,
  "error": "input nullifier already reserved in pending window",
  "resource": { /* ... */ },
  "accepts": [ /* ... */ ]
}
```

Merchants MUST NOT return the resource body when verification fails.

---

## B. Back-end facilitator API (merchant/buyer ↔ Guardian-facilitator)

The Guardian-facilitator is an OZ Guardian server with the x402 module
mounted (single binary, single process, single port). Routes under `/`
are Guardian's standard routes (`/configure`, `/delta`, `/pubkey`, ...);
routes under `/x402/*` are the facilitator-specific ones documented below.

Default base URL during development: `http://localhost:8080`.

All `/x402/{challenge,verify,settle}` calls are **Guardian-authenticated**:
the caller signs the request with a Falcon-512 cosigner key that appears
in the account's `Auth::MidenFalconRpo::cosigner_commitments`. The merchant
authenticates as its merchant account; the buyer authenticates as its
buyer account. See OZ Guardian's [`spec/api.md`](../../guardian/spec/api.md)
§ Miden Request Signing for the canonical signing format.

### B.1 `POST /x402/challenge`

Issues a server-generated `serial_num`.

**Request body**

```json
{
  "paymentRequirements": { /* MidenPaymentRequirements without extra.serialNum */ }
}
```

**Auth**: signed by the merchant account.

**Success response** — `200 OK`

```json
{
  "serialNum": "0x<32-byte hex>",
  "expiresInSeconds": 120
}
```

### B.2 `POST /x402/verify`

Verifies a signed-but-unproven payment without enqueueing it for
settlement. Reserves the input + output nullifiers.

**Request body** (same shape as §B.3):

```json
{
  "x402Version": 2,
  "paymentPayload":      { /* MidenPaymentPayload */ },
  "paymentRequirements": { /* MidenPaymentRequirements */ }
}
```

**Auth**: signed by the buyer account.

**Success response** — `200 OK`

```json
{ "isValid": true, "payer": "0x857b06519e91e3a54538791bdbb0e2" }
```

**Failure response** — `400` / `409` / `412` / `503` (see §B.5):

```json
{
  "isValid": false,
  "invalidReason": "asset_mismatch",
  "invalidReasonDetails": "..."
}
```

### B.3 `POST /x402/settle`

Same request body as `/x402/verify`. Runs the same checks, then enqueues
the verified tx on the batch worker for prove + submit and returns a
signed receipt.

**Auth**: signed by the buyer account.

**Success response** — `200 OK`

```json
{
  "success": true,
  "payer": "0x...",
  "transaction": "0x<queued_id>",
  "network": "miden:testnet",
  "receiptSig": "<base64>",
  "receiptPubkeyCommitment": "0x..."
}
```

The response returns immediately — prove + submit happen asynchronously
in the background batch worker. The merchant can deliver the resource as
soon as it has the receipt; the facilitator's signature on the receipt is
the trust anchor (DESIGN.md "same trust assumptions as Base").

### B.4 `GET /x402/pubkey`

Returns the facilitator's settle-receipt pubkey. No authentication
required.

```json
{
  "commitment": "0x<falcon pubkey commitment>",
  "pubkeyB64": "<base64 of raw Falcon public key>"
}
```

Merchants cache this once per facilitator operator and verify every
`receiptSig` against it.

### B.5 `GET /x402/supported`

```json
{
  "kinds": [
    { "x402Version": 2, "scheme": "miden-p2id-private", "network": "miden:testnet" }
  ],
  "extensions": ["miden-guardian-facilitator"]
}
```

### B.6 `GET /x402/health`

```json
{ "status": "ok" }
```

---

## C. Verification rules (informative)

The Guardian-facilitator runs the following checks in order on
`POST /x402/{verify,settle}`. Each failure maps to a canonical x402
`ErrorReason` and an HTTP status code. See
[`crates/miden-x402-facilitator/src/verify.rs`](../crates/miden-x402-facilitator/src/verify.rs)
for the canonical implementation.

1. **Network.** `requirements.network` is in the Miden namespace and
   matches the facilitator's configured network.
2. **Asset/amount agreement.** `payload.asset == requirements.asset` and
   `payload.amount == requirements.amount`.
3. **Challenge consumption.** `payload.serialNum` is in the
   facilitator's challenge store and not expired. The challenge is
   consumed atomically (replays fail).
4. **Wire decoding.** `txInputs`, `signature`, `signedSummary`, and
   `expectedNoteBlob` deserialise as their canonical Miden types.
5. **Summary ↔ tx_inputs binding.**
   `signedSummary.input_notes.commitment() == txInputs.input_notes.commitment()`.
6. **Summary ↔ output blob binding.** Recomputed `noteId` from
   `expectedNoteBlob` appears in `signedSummary.output_notes`.
7. **P2ID script root.** The note in `expectedNoteBlob` carries the
   canonical P2ID script root.
8. **Recipient + asset extraction.** Recipient and faucet+amount in
   `expectedNoteBlob` match `requirements.payTo` / `requirements.asset` /
   `requirements.amount`.
9. **Sender consistency.** `payload.sender == txInputs.account().id()`.
10. **Falcon signature.** `signature.public_key` commitment is in the
    buyer's `Auth::MidenFalconRpo::cosigner_commitments`; the signature
    verifies against `signedSummary.to_commitment()`.
11. **Nullifier backstop.** None of the input/output nullifiers have
    been observed on chain (`check_nullifiers` via the Miden node).
12. **Mandate policy.** `MandatePolicy::evaluate(...)` returns `Ok`.
    See [`docs/mandate.md`](./mandate.md).
13. **Balance check.** Buyer's Guardian-persisted vault state shows ≥
    `requirements.amount` of `requirements.asset`. Unrecognised state
    shapes degrade to "allow" (the on-chain check at prove time is the
    real enforcement).
14. **Atomic reservation.** Input + output nullifiers reserved in the
    facilitator's reservation store; concurrent attempts fail with
    `409 Conflict`.

HTTP status codes:

- `400 Bad Request` — input/blob/binding/signature failures.
- `409 Conflict` — nullifier already reserved or already-consumed on
  chain.
- `412 Precondition Failed` — buyer balance insufficient.
- `500 Internal Server Error` — facilitator-internal failures
  (storage, receipt signer).
- `503 Service Unavailable` — Miden node RPC failure, remote prover
  unreachable, or mandate backend down.

---

## D. Reference implementations

| Layer | Status |
|---|---|
| Wire types (`miden-x402-types`) | shipped |
| Guardian-facilitator binary (`guardian-facilitator`) | shipped (server-side) |
| Node merchant SDK (Express + Hono) | shipped |
| Node agent SDK | stub (blocked on WASM SDK extensions — see [`docs/UPSTREAM_WISHLIST.md`](./UPSTREAM_WISHLIST.md)) |
| Python merchant SDK (FastAPI + Flask) | shipped |
| Mandate policy hook | shipped (default `AllowAll`) |
| Batch settle worker | shipped |
| Inclusion-bridge (reservation → consumed on canonical block) | partial (see UPSTREAM_WISHLIST.md) |
