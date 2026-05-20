# Deploying the Guardian-facilitator

The `guardian-facilitator` binary runs an OpenZeppelin Guardian server with
the x402 module mounted on top. One binary, one process, one port. Routes
under `/` are Guardian's standard surface (`/configure`, `/delta`, ...);
routes under `/x402/*` are the facilitator-specific ones in
[`docs/protocol.md`](./protocol.md).

## Prerequisites

- Rust 1.93 or newer (matches OZ Guardian's workspace).
- `protoc` 25.x on the build host (transitive via `guardian-server`'s
  protobuf build script).
- Access to a Miden node RPC endpoint (`MIDEN_X402_RPC_URL`) and a Miden
  remote prover (`MIDEN_X402_REMOTE_PROVER_URL`).

## Build

```bash
cargo build --release -p miden-x402-facilitator --bin guardian-facilitator
```

The resulting binary is at `target/release/guardian-facilitator`.

## Configuration

The binary reads two sets of env vars:

### Guardian half

See OZ Guardian's [README](https://github.com/OpenZeppelin/guardian)
for the canonical list. Most relevant:

- `DATABASE_URL` — Postgres URL (only when running with the `postgres`
  feature; filesystem backend is the default).
- `GUARDIAN_KEYSTORE_PATH` — Guardian's ack-key keystore directory.
  Default `/var/guardian/keystore`.
- `GUARDIAN_RATE_LIMIT_ENABLED`, `GUARDIAN_RATE_BURST_PER_SEC`,
  `GUARDIAN_RATE_PER_MIN` — rate limiting knobs.
- `GUARDIAN_MAX_REQUEST_BYTES` — max body size; default 1 MiB.
- `RUST_LOG` — `tracing-subscriber` env filter.

### x402 half

| Var | Default | Description |
|---|---|---|
| `MIDEN_X402_LISTEN_ADDR` | `0.0.0.0:8080` | HTTP bind address. |
| `MIDEN_X402_RPC_URL` | `https://rpc.testnet.miden.io` | Miden node gRPC URL. Used by the batch worker (submit) and the verify-path nullifier backstop. |
| `MIDEN_X402_REMOTE_PROVER_URL` | _required_ | gRPC URL of the Miden remote prover. The binary refuses to start if unset. |
| `MIDEN_X402_NETWORK` | `miden:testnet` | CAIP-2 network id; `miden:mainnet` for prod. |
| `MIDEN_X402_STORAGE_ROOT` | `/var/x402/state` | Root directory for x402 state (challenges, reservations, batch queue, receipt key). The binary creates `<root>/guardian/` for Guardian-side state too. |
| `MIDEN_X402_CHALLENGE_TTL_SECS` | `120` | TTL for `POST /x402/challenge` records. Should be ≥ the merchant's advertised `maxTimeoutSeconds`. |
| `MIDEN_X402_RESERVATION_TTL_SECS` | `60` | TTL for reserved input nullifiers (defensive — the happy path releases explicitly). |
| `MIDEN_X402_BATCH_MAX_SIZE` | `8` | Maximum number of verified txs the batch worker drains per cycle. |
| `MIDEN_X402_BATCH_MAX_AGE_MS` | `750` | When the oldest queued tx is older than this, drain the batch even if shorter than `BATCH_MAX_SIZE`. |
| `MIDEN_X402_BATCH_TICK_MS` | `100` | How often the batch worker wakes to evaluate drain conditions. |
| `MIDEN_X402_MANDATE_POLICY` | `allow-all` | Selector for the `MandatePolicy`. Only `allow-all` is built in today; see [`docs/mandate.md`](./mandate.md). |

## Receipt key

On first boot the binary generates a fresh Falcon-512 keypair and persists
it under `${MIDEN_X402_STORAGE_ROOT}/keystore/receipt_key.bin`. Subsequent
boots load the same key so the `receiptPubkeyCommitment` returned by
`GET /x402/pubkey` is stable across restarts.

To rotate the key, stop the binary, delete the file, and restart — a fresh
keypair is generated. Merchants will need to re-fetch the pubkey.

## Verifying the deployment

After boot:

```bash
curl -s http://localhost:8080/x402/health
# {"status":"ok"}

curl -s http://localhost:8080/x402/supported
# {"kinds":[{"x402Version":2,"scheme":"miden-p2id-private","network":"miden:testnet"}],"extensions":["miden-guardian-facilitator"]}

curl -s http://localhost:8080/x402/pubkey
# {"commitment":"0x...","pubkeyB64":"..."}

# Guardian routes are mounted on the same port:
curl -s http://localhost:8080/pubkey
# {"commitment":"0x..."}
```

## Storage layout

Filesystem-backed (default):

```text
${MIDEN_X402_STORAGE_ROOT}/
├── guardian/               # Guardian state (accounts, deltas, proposals, ack key)
│   ├── accounts/...
│   ├── deltas/...
│   └── keystore/...        # Guardian's ack key
└── x402/                   # x402-specific state
    ├── challenges/<serial_num_hex>.json
    ├── reservations/<nullifier_hex>.json
    ├── batch_queue/<queued_id>.json
    └── keystore/receipt_key.bin
```

For multi-instance deployments swap the filesystem repos for a shared
backend behind the same traits (`ChallengeRepo`, `ReservationRepo`,
`BatchQueueRepo`, `FacilitatorKeyStore`). The default impl is filesystem +
single process.

## Operational notes

- The batch worker drops txs on prove/submit failure (DESIGN.md doesn't
  specify a retry policy). Reservations are released so the buyer can
  retry from `POST /x402/settle`. Add metrics on `batch worker: prove
  failed` / `node submit failed` log lines to alert on persistent
  failures.
- The reservation TTL is the only ceiling on the in-mempool window.
  Until the inclusion-bridge ships (see
  [`docs/UPSTREAM_WISHLIST.md`](./UPSTREAM_WISHLIST.md)), reservations
  fall out of the in-memory set when the TTL elapses, even for txs that
  are still in flight on the Miden node. Set `MIDEN_X402_RESERVATION_TTL_SECS`
  generously (5 × your typical submit-to-inclusion latency).
- The verify-path nullifier backstop is currently a no-op in the binary
  (`NoopNullifierBackstop`); replay protection rests on the reservation
  set + on-chain rejection at prove time. Wiring a real
  `check_nullifiers` RPC against `miden-rpc-client` is a small change
  tracked in `UPSTREAM_WISHLIST.md`.
