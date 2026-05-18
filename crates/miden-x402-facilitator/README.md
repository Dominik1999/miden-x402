# miden-x402-facilitator

The x402 v2 facilitator service for the Miden network — an HTTP gateway
between merchant servers and a Miden node.

## What it does

Phase A endpoints (always on, never custody keys):

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness probe |
| `GET` | `/supported` | Lists the `(scheme=exact, network=miden:testnet)` kind + advertised extensions |
| `POST` | `/verify` | Verifies a Miden `exact` payment (public or private P2ID) against on-chain state |
| `POST` | `/settle` | Same checks as `/verify`; returns the buyer's create-note transaction id |

Phase B (Guardian) endpoints, mounted only when
`MIDEN_X402_GUARDIAN_ENABLED=true`:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/guardian/challenge` | Issues a server-generated `serial_num` for the merchant to inline into the 402's `extra.serialNum` |
| `POST` | `/guardian/verify` | Verifies a signed-but-unproven private tx offline; reserves input nullifiers |
| `POST` | `/guardian/settle` | Verify + reserve + prove (via remote prover) + submit; returns the post-prove `ProvenTransaction.id()` |

`/verify` and `/settle` are idempotent because settlement on Miden under
the commit flow is **at-commit**: once the buyer's P2ID note is in a
committed block, the payment is final from the chain's perspective. The
merchant consumes the note out-of-band on its own schedule.

The Guardian flow trades on a different trust model (same as Base's x402
facilitator): the merchant trusts the Guardian's verification message and
delivers the resource without waiting for on-chain inclusion; the
Guardian proves and submits asynchronously.

## Verification rules

Given a request body of `{ x402Version, paymentPayload, paymentRequirements }`,
the facilitator runs a single unified pipeline that branches only on
`noteType` for the source of recipient/asset. Each check maps to a
canonical x402 [`ErrorReason`](https://crates.io/crates/x402-types) on
failure.

Shared steps (1–3, 4–11):

1. `paymentPayload.accepted` agrees with `paymentRequirements` on
   `network`, `payTo`, `asset`, `amount`, and `extra.noteType`. The
   `payload.payload` discriminator must match `extra.noteType`.
2. `requirements.network` is in the Miden CAIP-2 namespace.
3. `requirements.asset` is on the facilitator's configured faucet allowlist.

Note resolution (the only step that branches on `noteType`):

- *`public`*: the on-chain `Note` is fetched via `get_notes_by_id`
  (`FetchedNote::Public(note, proof)`). Recipient, asset, sender, and
  block number all come from chain.
- *`private`*: the off-chain `NoteFile` blob is base64-decoded and the
  `noteId` is recomputed. The node is queried via `get_notes_by_id` for
  the matching commitment (`FetchedNote::Private(header, proof)`).
  Recipient and asset come from the blob; sender and block number come
  from the on-chain header. The recomputed-`noteId` equality is the
  cryptographic bind.

Shared checks (4–11):

4. `note.recipient().script().root() == P2idNote::script_root()`.
5. The P2ID storage parses into an `AccountId` that equals `payTo`.
6. The note carries exactly one fungible asset whose faucet equals
   `asset` and amount equals `amount`.
7. The on-chain metadata's sender equals `paymentPayload.payload.sender`.
8. The nullifier is **not** in the node's consumed set. For private
   notes the nullifier is recomputed from the off-chain blob (a function
   of `serial_num, script_root, storage_commitment, asset_commitment`
   only — no consumer key needed).
9. `currentBlockNum - note.blockNum ≤ MIDEN_X402_FRESHNESS_BLOCKS`.

For `noteType: "guardianFast"` payloads, the request must hit
`/guardian/verify` or `/guardian/settle` instead; the commit endpoints
reject them with `BadRequest`. See
[`docs/protocol.md`](../../docs/protocol.md) §A.2.7 + §B.2.7 for the full
Guardian flow.

## Configuration

All settings come from environment variables. Sensible defaults make it
runnable against Miden testnet out of the box.

| Variable | Default | Notes |
|---|---|---|
| `MIDEN_X402_LISTEN_ADDR` | `0.0.0.0:8080` | HTTP bind address |
| `MIDEN_X402_RPC_URL` | `https://rpc.testnet.miden.io` | Miden node gRPC URL |
| `MIDEN_X402_RPC_TIMEOUT_MS` | `10000` | RPC timeout per call |
| `MIDEN_X402_ALLOWED_FAUCETS` | `0x0a7d175ed63ec5200fb2ced86f6aa5` | Comma-separated faucet account IDs. Use `*` to allow any. |
| `MIDEN_X402_FRESHNESS_BLOCKS` | `24` | Roughly ~2 minutes at ~5s blocks |
| `MIDEN_X402_GUARDIAN_ENABLED` | `false` | Enable the Phase B `/guardian/*` endpoints. When `false` they are absent from the router. |
| `MIDEN_X402_REMOTE_PROVER_URL` | _unset_ | gRPC URL of the Miden remote prover. Required when `MIDEN_X402_GUARDIAN_ENABLED=true`; without it `/guardian/settle` returns `503`. |
| `MIDEN_X402_GUARDIAN_CHALLENGE_TTL_SECS` | `120` | TTL for issued `serial_num` challenges. Should be ≥ the merchant's advertised `maxTimeoutSeconds`. |
| `MIDEN_X402_GUARDIAN_RESERVATION_TTL_SECS` | `60` | TTL for reserved input nullifiers. Defensive — the success/failure paths normally release reservations explicitly. |
| `RUST_LOG` | `info` | Standard `tracing-subscriber` env filter |

## Running

```bash
cargo run -p miden-x402-facilitator --release
```

```bash
# Bind to localhost, accept any faucet, info logs:
RUST_LOG=info \
MIDEN_X402_LISTEN_ADDR=127.0.0.1:8080 \
MIDEN_X402_ALLOWED_FAUCETS='*' \
cargo run -p miden-x402-facilitator
```

Smoke check:

```bash
curl -s http://localhost:8080/health      # {"status":"ok"}
curl -s http://localhost:8080/supported   # {"kinds":[{"x402Version":2,"scheme":"exact","network":"miden:testnet"}],"extensions":[]}
```

When the Guardian is enabled the `extensions` field carries
`["miden-guardian-fast"]` and the three `/guardian/*` routes are mounted:

```bash
MIDEN_X402_GUARDIAN_ENABLED=true \
MIDEN_X402_REMOTE_PROVER_URL=https://prover.example.com:50051 \
cargo run -p miden-x402-facilitator
```

## Library use

The crate also exposes its components for embedding inside another binary or
for testing:

```rust
use miden_x402_facilitator::{AppState, FacilitatorConfig, GrpcMidenNode, build_router};

let config = FacilitatorConfig::from_env()?;
let node = GrpcMidenNode::from_url(&config.rpc_url, config.rpc_timeout_ms)?;
let app = build_router(AppState::new(node, config));
// serve `app` with axum however you like
```

The [`MidenNode`](src/node.rs) trait lets you plug a mock node into tests
without touching real RPC. Look at the in-crate tests for examples.

## Status

- **M2 (Phase A, public P2ID)** — shipped.
- **M4a live-testnet smoke** — shipped (`miden-x402-smoke-testnet` binary).
- **M7 (private P2ID via unified verifier)** — shipped.
- **M8 (Guardian verify-before-prove)** — shipped on the facilitator side
  (`/guardian/*` endpoints + offline Falcon verification + nullifier
  reservation + remote-prover wiring). Full end-to-end requires WASM SDK
  extensions on the buyer side to produce a signed-but-unproven
  `TransactionInputs` — see [`docs/protocol.md`](../../docs/protocol.md) §A.2.7.

## License

Apache-2.0.
