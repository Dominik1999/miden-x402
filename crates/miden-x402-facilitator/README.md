# miden-x402-facilitator

The x402 v2 facilitator service for the Miden network — a read-only HTTP gateway between merchant servers and a Miden node.

## What it does

Exposes the four endpoints merchant SDKs expect from any x402 facilitator,
specialised for Miden's `exact` scheme with public P2ID notes:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness probe |
| `GET` | `/supported` | Lists the `(scheme=exact, network=miden:testnet)` kind this facilitator handles |
| `POST` | `/verify` | Verifies a Miden `exact` payment against on-chain state |
| `POST` | `/settle` | Same checks as `/verify`; returns the buyer's create-note transaction ID |

`/verify` and `/settle` perform the same checks because settlement on Miden
under this implementation is **at-commit**: once the buyer's P2ID note is in a
committed block, the payment is final from the chain's perspective. The
merchant consumes the note out-of-band on its own schedule.

## Verification rules

Given a request body of `{ x402Version, paymentPayload, paymentRequirements }`,
the facilitator runs the following checks (fail-fast). Each check maps to a
canonical x402 [`ErrorReason`](https://crates.io/crates/x402-types) on failure.

1. `paymentPayload.accepted` agrees with `paymentRequirements` on
   `network`, `payTo`, `asset`, and `amount`.
2. `requirements.network` is in the Miden CAIP-2 namespace.
3. `requirements.asset` is on the facilitator's configured faucet allowlist.
4. For `noteType: "public"` payloads:
   1. The note resolves on chain via `get_notes_by_id` and is in a committed block.
   2. `note.recipient().script().root() == P2idNote::script_root()` (canonical P2ID).
   3. The P2ID storage parses into an `AccountId` that equals `payTo`.
   4. The note carries exactly one fungible asset whose faucet equals `asset` and amount equals `amount`.
   5. The note's nullifier is **not** in the node's consumed set.
   6. `currentBlockNum - note.blockNum ≤ MIDEN_X402_FRESHNESS_BLOCKS`.
   7. The on-chain note's sender equals `paymentPayload.payload.sender`.
5. For `noteType: "private"` payloads, the facilitator returns
   `UnsupportedScheme`. Private-note support is Phase 2 of this project.

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

M2 of the [project plan](../../PLAN.md). The facilitator is functionally
complete against mocked nodes; live testnet smoke testing happens in M4 when
the Node SDK lands and can produce real P2ID notes.

## License

Apache-2.0.
