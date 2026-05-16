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

The facilitator runs the following checks, fail-fast, in this order. See
[`crate::verifier`](../crates/miden-x402-facilitator/src/verifier.rs) for the
canonical implementation.

1. **Agreement check.** `paymentPayload.accepted` matches
   `paymentRequirements` on `network`, `payTo`, `asset`, `amount`.
2. **Network.** `requirements.network.namespace == "miden"`.
3. **Allowlist.** `requirements.asset` is on the facilitator's faucet
   allowlist (env `MIDEN_X402_ALLOWED_FAUCETS`, default seeded to the
   testnet token).
4. **Note kind.** `paymentPayload.payload.noteType == "public"`. Private
   notes return `unsupported_scheme` until Phase 2.
5. **Resolve note.** `get_notes_by_id` returns a committed `FetchedNote::Public`.
6. **P2ID script root.** `note.recipient().script().root() == P2idNote::script_root()`.
7. **Recipient.** Extracted via `P2idNoteStorage::try_from(note.recipient().storage().items())`,
   compared to `requirements.payTo`.
8. **Asset.** Exactly one fungible asset whose faucet equals
   `requirements.asset` and amount equals `requirements.amount`.
9. **Sender.** `note.metadata().sender() == paymentPayload.payload.sender`.
10. **Nullifier.** Not yet in the consumed set.
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
| Private notes (`noteType: "private"`) | Phase 2 |

[`ErrorReason`]: https://docs.rs/x402-types/latest/x402_types/proto/enum.ErrorReason.html
