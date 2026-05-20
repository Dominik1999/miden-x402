# miden-x402 wire protocol

This document is the **normative contract** between the buyer, the merchant, and
the facilitator. Anyone porting the merchant middleware or buyer client to a
new language (Node, Python, Go, etc.) implements against this document.

There are two distinct HTTP exchanges:

- **A. Public payment exchange.** Buyer ↔ merchant. Three HTTP headers carry
  the payment data; the rest of the request is the buyer's normal API call.
- **B. Back-end facilitator API.** Merchant ↔ facilitator. JSON request/response
  bodies. The merchant calls this from its own server; buyers never see it.

The Rust reference implementation of (A) for merchants is forthcoming
(Node and Python SDKs in M4/M5). The reference implementation of (B) is
[`miden-x402-facilitator`](../crates/miden-x402-facilitator).

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
Header names are case-insensitive on the wire per RFC 9110; emit them in
TitleCase as shown so that case-sensitive HTTP/2 indexes hit the canonical
form.

### A.2 Step-by-step

#### A.2.1 Initial request

Buyer makes a normal HTTP request to the protected resource. No
payment-related headers attached:

```http
GET /weather HTTP/1.1
Host: api.example.com
Accept: application/json
```

#### A.2.2 `402 Payment Required`

Merchant detects the missing `Payment-Signature`, builds a `PaymentRequired`,
and responds:

```http
HTTP/1.1 402 Payment Required
Payment-Required: eyJ4NDAyVmVyc2lvbiI6Miwi...  # base64(JSON(PaymentRequired))
```

The header value, base64-decoded, is the JSON:

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

The response body MAY be empty. Merchants that want their `402` to be
introspection-friendly MAY duplicate the payload as the response body with
`Content-Type: application/json` — the buyer SDK MUST treat the header as
authoritative.

#### A.2.3 Buyer creates a P2ID note on Miden

Outside the HTTP exchange, the buyer uses a Miden client (Rust SDK,
`@miden-sdk/miden-sdk`, etc.) to:

1. Build a `P2idNote` whose recipient is `accepts[0].payTo` and whose single
   fungible asset has faucet ID `accepts[0].asset` and amount
   `accepts[0].amount` (atomic units, parsed as `u64`).
2. Prove and submit the create-note transaction to Miden testnet.
3. Wait for the note to enter a committed block. Record the resulting
   `noteId`, `transactionId`, and `blockNum`.

#### A.2.4 Retry with `Payment-Signature`

```http
GET /weather HTTP/1.1
Host: api.example.com
Accept: application/json
Payment-Signature: eyJhY2NlcHRlZCI6eyJzY2hl...  # base64(JSON(PaymentPayload))
```

The header value, base64-decoded:

```json
{
  "x402Version": 2,
  "accepted": {
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
  },
  "payload": {
    "noteType": "public",
    "noteId": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "transactionId": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "sender": "0x857b06519e91e3a54538791bdbb0e2",
    "blockNum": 1234567,
    "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
    "amount": "1000"
  }
}
```

The buyer SDK MUST set `accepted` to **the exact same object** the merchant
offered — including unknown future fields. The merchant compares the offered
requirements against the echoed `accepted` to detect tampering.

For `noteType: "private"`, the payload replaces `noteId` with `noteBlob`
(base64-encoded `NoteFile`); all other receipt fields are identical:

```json
{
  "x402Version": 2,
  "accepted": { /* same shape; extra.noteType is "private" */ },
  "payload": {
    "noteType": "private",
    "noteBlob": "bm90ZQEK...<base64 of canonical NoteFile>",
    "transactionId": "0xbbbb...",
    "sender":        "0x857b06519e91e3a54538791bdbb0e2",
    "blockNum":      1234567,
    "asset":         "0x0a7d175ed63ec5200fb2ced86f6aa5",
    "amount":        "1000"
  }
}
```

The facilitator decodes the `NoteFile`, extracts recipient/asset from the
off-chain note, recomputes the `noteId`, and binds the blob to the on-chain
commitment by looking it up via `GetNotesById` — the result is
`FetchedNote::Private(NoteHeader, NoteInclusionProof)` from which sender and
block number are read. The same 11 verification rules apply; only the source
of recipient/asset differs between the two note types.

#### A.2.5 Merchant verifies via the facilitator

The merchant POSTs to the facilitator (see section B). On success, the
facilitator returns a `SettleResponse::Success`.

#### A.2.6 Successful response

```http
HTTP/1.1 200 OK
Content-Type: application/json
Payment-Response: eyJzdWNjZXNzIjp0cnVlLCJw...  # base64(JSON(SettleResponse))

{ "temperature": 21.5, "city": "Istanbul" }
```

The header value, base64-decoded:

```json
{
  "success": true,
  "payer": "0x857b06519e91e3a54538791bdbb0e2",
  "transaction": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "network": "miden:testnet"
}
```

`transaction` is the **buyer's create-note transaction id**, because under
the Miden settled-at-commit model that is the on-chain event that finalised
the payment. The merchant never produces a new on-chain tx of its own
(consumption of the note happens out of band, on the merchant's own
schedule).

#### A.2.7 Guardian-fast variant (verify-before-prove)

When `accepts[*].extra.settlement == "guardian-fast"`, the buyer hands the
facilitator a *signed but not proven* transaction; the facilitator verifies
the Falcon signature offline, reserves the input nullifier(s), and proves +
submits the tx asynchronously via a configured remote prover.

The merchant's 402 acquires a server-issued `serial_num` from the
facilitator's `POST /guardian/challenge` endpoint before emitting the
response. The buyer must use that exact `serial_num` when constructing the
P2ID note so the Guardian's pre-computed nullifier matches.

402 `extra` shape for `guardian-fast`:

```json
{
  "assetTransferMethod": "miden-p2id",
  "tokenSymbol": "USDC",
  "decimals": 6,
  "noteType": "private",
  "settlement": "guardian-fast",
  "guardianUrl": "https://facilitator.miden.io",
  "serialNum": "0x<32-byte server-generated word>"
}
```

`Payment-Signature` payload variant for `guardian-fast` (base64-decoded):

```json
{
  "noteType": "guardianFast",
  "txInputs":         "<base64(TransactionInputs)>",
  "signature":        "<base64(miden_protocol::account::auth::Signature)>",
  "signedSummary":    "<base64(miden_protocol::transaction::TransactionSummary)>",
  "expectedNoteBlob": "<base64(NoteFile::NoteDetails)>",
  "serialNum":        "0x...",
  "transactionId":    "0x...",
  "sender":           "0x...",
  "asset":            "0x...",
  "amount":           "1000"
}
```

`signedSummary` is the canonical `TransactionSummary` whose
`to_commitment()` digest the buyer's Falcon signature authorizes. The
facilitator binds it to `txInputs` (via `input_notes_commitment`) and to
`expectedNoteBlob` (via membership in `output_notes`), then verifies the
signature against the on-chain `PublicKeyCommitment` extracted from
`txInputs.account()` storage.

Trust model is identical to Base's x402 facilitator: the merchant trusts
the Guardian's `Payment-Response` verification message and delivers the
resource. On-chain inclusion is post-hoc; the returned `transaction` field
is the post-prove `ProvenTransaction.id()`, NOT the pre-prove id echoed in
`txInputs`.

### A.3 Error responses

If verification fails the merchant returns the same `402 Payment Required`
flow as A.2.2, with `error` populated:

```json
{
  "x402Version": 2,
  "error": "note already consumed",
  "resource": { /* ... */ },
  "accepts": [ /* ... */ ]
}
```

Merchants MUST NOT return the resource body when verification fails.

---

## B. Back-end facilitator API (merchant ↔ facilitator)

The facilitator is a plain HTTP+JSON service. There are no `Payment-*` headers
in this exchange — the merchant simply forwards the decoded payload + the
matched requirements as a JSON body.

Default base URL during development: `http://localhost:8080`.

### B.1 `POST /verify`

Verifies a payment without producing any new on-chain effect.

**Request body**

```json
{
  "x402Version": 2,
  "paymentPayload":      { /* MidenPaymentPayload, see A.2.4 */ },
  "paymentRequirements": { /* MidenPaymentRequirements, the chosen accept */ }
}
```

**Success response** — `200 OK`

```json
{
  "isValid": true,
  "payer": "0x857b06519e91e3a54538791bdbb0e2"
}
```

**Failure response** — `400 Bad Request` (or `500` for internal errors)

```json
{
  "isValid": false,
  "invalidReason": "asset_mismatch",
  "invalidReasonDetails": "on-chain note asset does not match requirements"
}
```

The `invalidReason` field is one of the canonical x402 [`ErrorReason`]
values; the merchant uses it to decide how to respond to the buyer.

### B.2 `POST /settle`

Same request body and verification logic as `/verify`. Returns
`SettleResponse` directly — this is the value the merchant base64-encodes
into the `Payment-Response` header.

**Success response** — `200 OK`

```json
{
  "success": true,
  "payer": "0x857b06519e91e3a54538791bdbb0e2",
  "transaction": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "network": "miden:testnet"
}
```

**Failure response** — `400` / `500` with the same error body as `/verify`.

Under settled-at-commit semantics `/settle` is idempotent and produces no
new on-chain tx of its own: it just re-verifies and returns the buyer's
create-note tx id.

### B.2.7 Guardian endpoints (Phase B)

The Guardian endpoints are mounted only when the facilitator is started
with `MIDEN_X402_GUARDIAN_ENABLED=true`. When disabled (default), the
endpoints below are absent from the router and the facilitator behaves
byte-for-byte like Phase A.

#### `POST /guardian/challenge`

**Request body**

```json
{ "paymentRequirements": { /* MidenPaymentRequirements */ } }
```

**Success response** — `200 OK`

```json
{
  "serialNum": "0x<32-byte hex>",
  "expiresInSeconds": 120
}
```

Issues a single-use server-generated `serial_num`. The merchant inlines
this value into `extra.serialNum` and `extra.guardianUrl` of the 402
response.

#### `POST /guardian/verify`

Same request body as `/verify`. The payload's `noteType` must be
`"guardianFast"`. Performs the offline Falcon verification and reserves
the input nullifiers, but does NOT prove or submit — the reservation is
held until `/guardian/settle` is called (success) or the TTL sweeper
releases it (timeout).

Returns `VerifyResponse::Valid { payer }` on success.

#### `POST /guardian/settle`

Same request body. Runs the same checks as `/guardian/verify`, then
forwards the verified `TransactionInputs` to the configured remote prover
(`MIDEN_X402_REMOTE_PROVER_URL`) and submits the resulting
`ProvenTransaction` to the Miden node. On success the reservation is
promoted to consumed; on any failure it is released. Returns
`SettleResponse::Success { payer, transaction, network }` where
`transaction` is the post-prove `ProvenTransaction.id()`.

Returns `503 Service Unavailable` if `MIDEN_X402_REMOTE_PROVER_URL` is
unset; `501 Not Implemented` if `MIDEN_X402_GUARDIAN_ENABLED=false`.

### B.3 `GET /supported`

```json
{
  "kinds": [
    {
      "x402Version": 2,
      "scheme": "exact",
      "network": "miden:testnet"
    }
  ],
  "extensions": []
}
```

### B.4 `GET /health`

```json
{ "status": "ok" }
```

---

## C. Verification rules (informative)

The facilitator runs the following checks, fail-fast, in this order, through
a single note-type-agnostic pipeline. The only step that branches on
`noteType` is rule 5 — recipient and asset come from the on-chain note for
`public` and from the off-chain `noteBlob` for `private`, bound to chain
state by recomputing the `noteId`. See
[`crate::verifier`](../crates/miden-x402-facilitator/src/verifier.rs) for the
canonical implementation.

1. **Agreement check.** `paymentPayload.accepted` matches
   `paymentRequirements` on `network`, `payTo`, `asset`, `amount`, and
   `extra.noteType`. The `payload.payload` discriminator must match
   `extra.noteType`.
2. **Network.** `requirements.network.namespace == "miden"`.
3. **Allowlist.** `requirements.asset` is on the facilitator's faucet
   allowlist (env `MIDEN_X402_ALLOWED_FAUCETS`, default seeded to the
   testnet token).
4. **Note kind.** Both `"public"` and `"private"` are supported.
5. **Resolve note.**
   - *Public:* `get_notes_by_id` returns `FetchedNote::Public(Note, proof)`;
     recipient, asset, sender, and block num are read directly from the
     on-chain note.
   - *Private:* base64-decode `noteBlob` → `NoteFile`. Accept only
     `NoteDetails` / `NoteWithProof` variants (a bare `NoteId` is rejected).
     Recompute the `noteId` and call `get_notes_by_id`; expect
     `FetchedNote::Private(NoteHeader, proof)`. Recipient and asset come
     from the off-chain `NoteDetails`; sender and block num come from the
     on-chain header. The `noteId` equality is the cryptographic bind —
     any tampering with the off-chain note changes the commitment and
     misses on the lookup.
6. **P2ID script root.** `note.recipient().script().root() == P2idNote::script_root()`.
7. **Recipient.** Extracted via `P2idNoteStorage::try_from(note.recipient().storage().items())`,
   compared to `requirements.payTo`.
8. **Asset.** Exactly one fungible asset whose faucet equals
   `requirements.asset` and amount equals `requirements.amount`.
9. **Sender.** On-chain `metadata().sender() == paymentPayload.payload.sender`.
10. **Nullifier.** Not yet in the consumed set. For private notes, the
    nullifier is recomputed off-chain from the `NoteFile` (a function of
    serial_num, script_root, storage_commitment, asset_commitment — no
    consumer key needed) and checked via the same `check_nullifiers` RPC.
11. **Freshness.** `currentBlockNum - note.blockNum <= MIDEN_X402_FRESHNESS_BLOCKS`.

Any violation maps to a canonical x402 [`ErrorReason`]; the HTTP status code
is `400` for client-side failures and `500` for unexpected node failures.

---

## D. Reference implementations

| Layer | Status |
|---|---|
| Wire types (`miden-x402-types`) | M1 — shipped |
| Facilitator (`miden-x402-facilitator`) | M2 — shipped |
| Header contract (this document + helpers) | M3 — shipped |
| Live testnet smoke binary (`miden-x402-smoke-testnet`) | M4a — shipped |
| Node SDK (merchant + agent) | M4b — shipped |
| Python SDK (merchant) | M5 — shipped |
| Quickstart README + scheme + deploy docs | M6 — shipped |
| Private notes (`noteType: "private"`) | M7 — shipped |
| Guardian verify-before-prove (`settlement: "guardian-fast"`) | M8 — shipped (verify path; settle path requires WASM SDK extensions to drive the buyer side end-to-end) |
| Agentic flow (`settlement: "agentic"`) — separate `agentic-guardian` binary per [`ideas/NEW_DESIGN.md`](../ideas/NEW_DESIGN.md) | feat/agentic-guardian branch — see [§A.2.8](#a28-agentic-variant-newdesignmd) and [`docs/agentic-guardian-deployment.md`](agentic-guardian-deployment.md) |

[`ErrorReason`]: https://docs.rs/x402-types/latest/x402_types/proto/enum.ErrorReason.html

---

## A.2.8 Agentic variant (NEW_DESIGN.md)

The `feat/agentic-guardian` branch adds a fourth payload variant on top
of M8's `public` / `private` / `guardianFast`. Wire-level it is
additive: the existing variants are byte-for-byte unchanged.

When `accepts[*].extra.settlement == "agentic"`, the buyer is an
**agent account** with a hot key (for in-mandate payments) + cold key
(for out-of-mandate ops). The agent signs an unproven tx with the hot
key and posts it to a separate `agentic-guardian` binary (NOT the M8
`miden-x402-facilitator`), which:

1. Verifies the hot-key Falcon signature.
2. Looks up + enforces the **AP2 mandate** the user signed at setup
   (amount cap, merchant allowlist, time window, daily total — see
   [`docs/ap2-mandate.md`](ap2-mandate.md)).
3. CAS-advances the **per-agent pending state** (allowing the agent
   to submit multiple in-flight txs that chain on each other, since
   the Guardian is the single serialization authority).
4. Reserves nullifiers (Postgres-WAL-backed).
5. Acks the client (sub-second perceived latency).
6. Asynchronously: parallel-proves N queued txs and submits as a
   `TransactionBatch` via `SubmitProvenBatch`.

402 `extra` shape for `agentic`:

```json
{
  "assetTransferMethod": "miden-p2id",
  "tokenSymbol": "USDC",
  "decimals": 6,
  "noteType": "private",
  "settlement": "agentic",
  "agenticGuardianUrl": "https://agentic-guardian.example",
  "mandateId": "m-abc",
  "noteTag": "weather.api",
  "serialNum": "0x<32-byte server-generated word>"
}
```

`Payment-Signature` payload variant for `agentic`:

```json
{
  "noteType": "agentic",
  "txInputs":               "<base64(TransactionInputs)>",
  "hotSignature":           "<base64(Falcon Signature)>",
  "signedSummary":          "<base64(TransactionSummary)>",
  "expectedNoteBlob":       "<base64(NoteFile::NoteDetails)>",
  "serialNum":              "0x...",
  "pendingStateCommitment": "0x...",
  "mandateId":              "m-abc",
  "sender":                 "0x...",
  "asset":                  "0x...",
  "amount":                 "1000"
}
```

Distinguishing fields vs the M8 `guardianFast` variant:

- `hotSignature` (explicit naming for the role).
- `pendingStateCommitment` — the agent's local view of the account's
  pending state; the agentic-guardian rejects if it doesn't match.
- `mandateId` — looks up the AP2 mandate to gate the tx.
- No `transactionId` — the only meaningful id is the post-prove
  `ProvenTransaction.id()`, returned later via
  `GET /agentic/status/{queued_id}`.

The agentic-guardian's HTTP API (see
[`docs/agentic-guardian-deployment.md`](agentic-guardian-deployment.md)):

- `POST /agentic/register` — one-time agent + mandate registration.
- `POST /agentic/submit` — hot-path 8-step verify + enqueue.
- `GET /agentic/status/{queued_id}` — current state of a queued tx.
- `GET /agentic/pending_state/{agent_id}` — current pending state.
- `POST /x402/challenge`, `POST /x402/verify`, `POST /x402/settle` —
  merchant-facing wrappers.
