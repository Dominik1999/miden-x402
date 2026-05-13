# x402 Integration for Miden — MVP Plan

Status: **draft, awaiting approval before implementation**

Scope target: deliverable **#1** ("x402 facilitator on Miden") from
[0xMiden/protocol#2919](https://github.com/0xMiden/protocol/discussions/2919).
Other deliverables in that doc (MPP, AP2/Guardian, benchmarks) are out of scope here.

## 1. Background and protocol-fit analysis

### 1.1 x402 in one paragraph
x402 v2 is a transport-agnostic payment protocol most commonly carried over HTTP. Server replies `402 Payment Required` with a JSON `PaymentRequired` body. Client picks an acceptable `PaymentRequirements`, builds a `PaymentPayload`, retries the request with a base64-encoded `PAYMENT-SIGNATURE` header. The server verifies and settles via a **facilitator** service exposing `POST /verify`, `POST /settle`, `GET /supported`, then returns the resource and a base64-encoded `PAYMENT-RESPONSE` header.

### 1.2 How EVM `exact` works and why it doesn't port to Miden
`exact` on EVM uses **EIP-3009 `transferWithAuthorization`**: buyer signs an off-chain typed message, facilitator broadcasts the transfer paying gas. The whole premise — "off-chain authorization, third party submits the tx" — depends on a shared ERC-20 contract that anyone can call with a valid signature.

Miden has no such primitive. Assets are moved by **P2ID notes**: the sender creates an on-chain "envelope" addressed to a recipient account ID, and the recipient later consumes it. Transactions are **client-side proved (ZK)**; only the sender can prove the create-note tx, only the recipient can prove the consume-note tx. There is no relayer pattern.

### 1.3 The mapping we adopt
The natural Miden equivalent of an EIP-3009 signed authorization is **a committed P2ID note locked to the merchant**. Getting that note onto chain *is* the authorization — unforgeable because only the sender's account could have produced the proof.

| Stage | EVM `exact` | Miden `exact` |
|---|---|---|
| Buyer signs | EIP-3009 EIP-712 signature | Buyer proves a `CreateP2idNote` tx locally |
| On the wire | Signature + authorization | Note ID + tx ID + block number |
| Facilitator | Broadcasts tx, pays gas | Read-only verifier of node state |
| Settlement | Facilitator submits transfer | Already on chain at note commitment |
| Reclaim | EIP-3009 nonce + validBefore | Plain P2ID is non-reclaimable |

Consequences for the design:
- **Settlement = note committed on chain.** Merchant-side consumption is deferred and idempotent.
- **The facilitator never holds custody.** It is a read-only verifier.
- **The buyer must run a Miden-aware client.** They cannot use a passive wallet; they must prove their own transaction.

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│ this repo (Rust)                                                  │
│  crates/miden-x402-types         — serde types, base64 helpers   │
│  crates/miden-x402-facilitator   — binary + lib                  │
│       POST /verify  POST /settle  GET /supported                 │
│       ↕ gRPC to Miden testnet node                               │
└──────────────────────────────────────────────────────────────────┘
                       ▲ HTTP (public service)
        ┌──────────────┴──────────────┐
        │                             │
┌───────────────────┐         ┌───────────────────┐
│ sdks/node (TS)    │         │ sdks/python       │
│  merchant mw      │         │  merchant mw      │
│  agent client     │         │  (no agent — TBD) │
│  demo merchant    │         │  demo merchant    │
│  demo agent       │         │                   │
└───────────────────┘         └───────────────────┘
```

The facilitator is the heavy piece. The SDKs are thin HTTP clients of the
facilitator plus framework glue. Each language's merchant middleware only
needs to: (a) emit the 402 body when `PAYMENT-SIGNATURE` is missing,
(b) call the facilitator's `/verify` and `/settle` when it's present,
(c) attach `PAYMENT-RESPONSE` on success.

## 3. Decisions (locked from clarifying questions)

| Topic | Decision |
|---|---|
| Language for this repo | **Rust only.** Facilitator + types. |
| Example SDKs (separate from this repo's MVP cut, see §6) | **Node (TypeScript) and Python.** Node has merchant + agent. Python has merchant only — agent skipped in MVP because there is no official Python Miden client SDK. |
| Facilitator endpoints | **`POST /verify`, `POST /settle`, `GET /supported`.** |
| Settlement semantics | **Note committed on chain ⇒ settled.** Server-side consumption is deferred. |
| Note privacy (MVP) | **Public P2ID notes.** |
| Note privacy (Phase 2, immediately after MVP) | **Private P2ID notes** — first-class. Architecture must not paint us into a corner. See §7. |
| Token (default) | **Miden testnet faucet `0x0a7d175ed63ec5200fb2ced86f6aa5`.** Configurable. Symbol/decimals discovered at startup. |
| Networks | **Miden testnet only.** Mainnet reserved. |
| Protocol version | **x402 v2.** |
| Scheme | **`"exact"`** with `extra.assetTransferMethod = "miden-p2id"`. |

## 4. Wire-level protocol (Miden scheme draft)

### 4.1 Network identifier
Miden has no registered CAIP-2 namespace. We adopt provisional values, documented as non-standard:

- `miden:testnet`
- `miden:mainnet` (reserved, unused)

### 4.2 `PaymentRequirements` (server → client, inside the 402 body)
```json
{
  "scheme": "exact",
  "network": "miden:testnet",
  "amount": "1000",
  "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
  "payTo": "0x<merchant-account-id-hex>",
  "maxTimeoutSeconds": 120,
  "extra": {
    "assetTransferMethod": "miden-p2id",
    "tokenSymbol": "<TBD-on-startup>",
    "decimals": 8,
    "noteType": "public"
  }
}
```
Wrapped in the standard v2 `PaymentRequired` envelope (`x402Version`, `error`, `resource`, `accepts`, `extensions`).

### 4.3 `PaymentPayload.payload` (client → server, base64 in `PAYMENT-SIGNATURE`)
For `noteType: "public"`:
```json
{
  "noteType": "public",
  "noteId": "0x<note-id>",
  "transactionId": "0x<tx-hash>",
  "sender": "0x<buyer-account-id-hex>",
  "blockNum": 1234567,
  "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
  "amount": "1000"
}
```
No `signature` field. The cryptographic authorization is implicit in the fact that the note exists on chain — only the sender's account could have produced the STARK proof that put it there.

The payload type is designed as a tagged union on `noteType` so that the Phase 2 `"private"` variant slots in without breaking the wire format (see §7).

### 4.4 `SettlementResponse` (server → client, base64 in `PAYMENT-RESPONSE`)
```json
{
  "success": true,
  "payer": "0x<buyer-account-id-hex>",
  "transaction": "0x<tx-hash>",
  "network": "miden:testnet"
}
```
The `transaction` field is the **buyer's** create-note transaction hash — that's the on-chain event that settled the payment. The facilitator does not produce a new tx of its own.

## 5. Verification algorithm (facilitator `/verify` and `/settle`)

Given a `PaymentPayload` and a `PaymentRequirements`, run all of the following. Fail-fast.

1. **Scheme/network/asset/payTo agreement** between payload and requirements. Otherwise → `invalid_scheme` / `invalid_network` / `invalid_payload`.
2. **Note resolution.** Query the Miden node RPC for the note by `noteId`. It must exist, be **public**, and live in a **committed** block. Otherwise → `invalid_transaction_state`.
3. **Note contents.** Recipient account ID equals `payTo`; note carries exactly one fungible asset whose faucet ID equals `asset` and whose amount equals `requirements.amount`. Equality is enforced for the `exact` scheme — overpayment is rejected with a clear `errorReason`.
4. **Not yet consumed.** Note's nullifier is not in the node's nullifier set. Otherwise → `invalid_transaction_state` (likely double-spend or replay attempt).
5. **Freshness.** `currentBlockNum - blockNum` corresponds to ≤ `maxTimeoutSeconds`, using a configured block-time estimate. Otherwise → `invalid_payload` with explicit `expired_authorization` reason.
6. **Sender consistency.** `payload.sender` equals the note's sender as reported by the node. Otherwise → `invalid_payload`.

If all pass: `/verify` returns `{ isValid: true, payer: <sender> }`; `/settle` returns a `SettlementResponse` with the same content. Both endpoints are idempotent — under "settled-at-commit" semantics there is no extra side effect to perform.

## 6. Workspace layout (this repo only)

```
miden-x402/
├── Cargo.toml                       # workspace
├── PLAN.md                          # this file
├── README.md                        # added later
├── crates/
│   ├── miden-x402-types/            # x402 v2 types + Miden scheme types
│   └── miden-x402-facilitator/      # facilitator (lib + binary)
├── sdks/                            # example client SDKs (see §6.3)
│   ├── node/                        # TypeScript
│   └── python/                      # Python (merchant only)
└── docs/
    └── scheme_exact_miden.md        # scheme spec, mirrors x402-rs/specs style
```

### 6.1 `crates/miden-x402-types`
- Re-uses struct shapes from the x402 v2 spec: `PaymentRequired`, `PaymentRequirements`, `PaymentPayload`, `VerifyResponse`, `SettlementResponse`, `SupportedKind`, `SupportedResponse`, error enum.
- Miden-specific `MidenPayload` enum tagged on `noteType` with `Public` variant in MVP and `Private` variant **declared but unimplemented** (returns "unsupported" at the facilitator) so Phase 2 doesn't bump the wire format.
- `Network::Miden(MidenNetwork)` enum, `MidenNetwork::Testnet` (+ reserved `Mainnet`).
- Helpers: base64 header (de)serialization, CAIP-2 parsing for `miden:<reference>`, `payment_required` builder.
- No `miden-client` dependency. Pure types and serde.

### 6.2 `crates/miden-x402-facilitator`
- Library exposing a `Facilitator` trait and a concrete `MidenFacilitator` holding a `miden_client::rpc::GrpcClient` (read-only — no keystore, no merchant accounts).
- Binary exposing the three HTTP endpoints via `axum`.
- Config (file + env):
  - Miden node endpoint (default: `Endpoint::testnet()`).
  - Default faucet allowlist (seeded with `0x0a7d175ed63ec5200fb2ced86f6aa5`); deployments can extend.
  - Block-time estimate for freshness checks (default tuned to testnet).
  - HTTP listen address, CORS, request size limits, structured logging via `tracing`.
- Public service: intended to be deployed once and pointed at by many merchants. No per-merchant state.

### 6.3 SDKs (out of this repo's MVP delivery, but designed alongside)
The SDKs are not part of the Rust workspace; they live in `sdks/` and ship in their own languages. They will be implemented after the facilitator is up. Each is a thin HTTP client of this facilitator plus framework glue.

**`sdks/node` (TypeScript)**
- `merchant`: Express + Hono middleware. Given a `PriceTag` and a facilitator URL, emits 402 / verifies / settles / passes through. Modeled on `coinbase/x402` Node middleware.
- `agent`: a fetch wrapper that, on 402, calls `@miden-sdk/miden-sdk` (WASM) to build + prove + submit a P2ID note, then retries the request with `PAYMENT-SIGNATURE`.
- `demo/merchant`: Express server gated by the middleware.
- `demo/agent`: CLI that pays the demo merchant.

**`sdks/python`**
- `merchant`: FastAPI + Flask middleware. Same shape as the Node merchant.
- `demo/merchant`: FastAPI server gated by the middleware.
- **No agent in MVP.** Python has no official Miden client SDK; the Node agent can be used to test cross-language merchants (Node agent ↔ Python merchant ↔ Rust facilitator).

## 7. Phase 2: private notes (immediately after MVP)

This section is part of this plan, not a TBD, so the MVP doesn't paint us into a corner.

In Miden, **private notes are not stored by the network** — only commitments and nullifiers are. The note data (script, inputs, assets, recipient) must be transmitted directly between parties. To verify a private-note payment, the facilitator needs the full note data plus enough proof to confirm:
- the commitment is on chain in a committed block
- the recipient matches `payTo`
- the asset and amount match the requirements
- the nullifier is not yet recorded

Design implications:

1. **Wire format.** Add `noteType: "private"` to the `MidenPayload` tagged union with an additional field carrying the note blob (the canonical serialization from `miden-client`'s `Note` / `NoteFile`).
2. **Facilitator verification.** A new code path that imports the note blob, reconstructs the commitment, queries the node for that commitment, runs the same recipient/asset/amount/nullifier/freshness checks as the public path.
3. **Transport size.** Private note blobs are larger than the ~150-byte public payload. `PAYMENT-SIGNATURE` header limits become relevant; we document a fallback to body-carried payment (also in the x402 v2 spec) for transports that can't take large headers.
4. **Server-side persistence.** Merchant must persist the private note blob to be able to consume it later. This is a merchant-side responsibility but the SDKs should expose a hook.
5. **Privacy properties.** Public payloads still leak sender/recipient/amount on chain. Private notes are what actually delivers the "transaction graph is not exposed" line from the scope doc.

The MVP types crate declares the `Private` variant so Phase 2 is a non-breaking addition.

## 8. End-to-end demo (MVP, public notes)

1. Operator deploys the Rust facilitator pointed at Miden testnet.
2. Operator runs the Node demo merchant configured with the merchant account ID, asset `0x0a7d175ed63ec5200fb2ced86f6aa5`, amount, and the facilitator URL.
3. Operator runs the Node demo agent configured with a funded buyer account.
4. Expected sequence (visible in logs):
   - Agent → merchant: `GET /weather`
   - Merchant → agent: `402` with `PaymentRequired`
   - Agent locally builds + proves + submits a public P2ID note tx; waits for commitment
   - Agent → merchant: `GET /weather` with `PAYMENT-SIGNATURE: <base64>`
   - Merchant → facilitator: `POST /verify` then `/settle`
   - Facilitator queries node, returns `isValid: true`
   - Merchant → agent: `200 OK` + body + `PAYMENT-RESPONSE: <base64>`
5. Cross-language check: re-run with the Python demo merchant in place of the Node merchant. Same outcome.
6. Operator verifies the note on `testnet.midenscan.com`.

## 9. Milestones / execution order

1. **M1 — Workspace + types.** Cargo workspace; `miden-x402-types` compiling with the structs (including the unimplemented `Private` payload variant) and base64 round-trip tests. No network code.
2. **M2 — Facilitator core.** `miden-x402-facilitator` lib + bin against testnet RPC; `/verify` and `/supported` working against hand-crafted public P2ID notes created with the Miden CLI.
3. **M3 — `/settle` parity.** Same checks as `/verify` (idempotent), returns `SettlementResponse`. Header emission contract finalised.
4. **M4 — Node SDK (merchant + agent) + demo.** Express + Hono middleware; agent wrapper around `@miden-sdk/miden-sdk`; demo merchant and demo agent. Full E2E green.
5. **M5 — Python SDK (merchant only) + demo.** FastAPI + Flask middleware; demo merchant. Cross-language E2E green (Node agent ↔ Python merchant).
6. **M6 — Docs.** `README.md` with quickstart, `docs/scheme_exact_miden.md` modeled on the `x402-rs` scheme spec template, deployment notes for the facilitator.
7. **M7 (Phase 2, separate plan) — Private notes.** Extend the facilitator and the SDKs per §7.

## 10. Out of scope for the MVP

- Private P2ID notes (Phase 2, planned next — §7).
- `P2IDR` notes with sender-side reclaim window.
- Miden mainnet.
- Multi-chain / multi-scheme support.
- Discovery API / Bazaar.
- Gas sponsorship extensions (not meaningful in Miden's prover-side model).
- MPP and AP2/Guardian integration (scope items #2 and #3 in the discussion doc).
- Benchmarks (scope item #4).
- Python agent client (no official Miden Python client SDK).
- Browser embedding of the Node agent (works in principle via WASM, not part of the MVP demo).

## 11. Risks and open items

- **CAIP-2 namespace.** `miden:testnet` is provisional; revisit when Miden registers an official identifier.
- **Block-time / freshness check.** `maxTimeoutSeconds` needs a stable block-time estimate. Start with a conservative constant pulled from config; refine with on-chain measurements in Phase 2.
- **Token metadata.** The default faucet `0x0a7d175ed63ec5200fb2ced86f6aa5` is fixed; symbol and decimals will be queried from the node at startup and cached. If the lookup fails, the facilitator refuses to start.
- **Buyer needs a Miden client.** Real UX cost vs. EVM x402's "any wallet" model. Documented as a known constraint.
- **`miden-client` API stability.** Crate is v0.14 (alpha). Pin a specific version in `Cargo.toml` and re-test on bumps.
- **`@miden-sdk/miden-sdk` parity.** The Node agent relies on the WASM SDK's `transactions.send` / P2ID flow. Verify it produces a note shape the facilitator can resolve before relying on it in M4.
- **Merchant must consume notes eventually.** Out of MVP scope but examples should call this out so accumulated notes don't go stale.
- **Public-note privacy gap.** Public P2ID exposes sender/recipient/amount. We accept this for MVP and immediately address it in Phase 2.

## 12. Non-goals to be explicit about

- We are **not** building a generic x402-rs alternative. Only `exact` on `miden:testnet`.
- We are **not** building a wallet. Buyers bring their own funded Miden accounts.
- We are **not** standardizing a CAIP-2 namespace for Miden — that's an upstream concern.
- We are **not** trying to match every chain x402-rs supports. One chain, one scheme, done well.
