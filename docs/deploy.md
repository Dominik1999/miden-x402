# Deploying `miden-x402-facilitator`

The facilitator is a public, read-only HTTP service: one process per
deployment, no per-merchant state, no keystore. Many merchants point at
the same facilitator URL.

## Binary

Build:

```
cargo build -p miden-x402-facilitator --release --bin miden-x402-facilitator
```

The release binary lives at `target/release/miden-x402-facilitator`.

Run:

```
./target/release/miden-x402-facilitator
```

## Configuration (env vars)

All optional; defaults documented in
[`crates/miden-x402-facilitator/src/config.rs`](../crates/miden-x402-facilitator/src/config.rs).

| Variable | Default | Description |
|---|---|---|
| `MIDEN_X402_LISTEN_ADDR` | `0.0.0.0:8080` | HTTP bind address. |
| `MIDEN_X402_RPC_URL` | `https://rpc.testnet.miden.io` | Miden node gRPC URL. |
| `MIDEN_X402_RPC_TIMEOUT_MS` | `10000` | Per-call gRPC timeout. |
| `MIDEN_X402_ALLOWED_FAUCETS` | `0x0a7d175ed63ec5200fb2ced86f6aa5` | Comma-separated faucet account ids accepted as payment assets. Use `*` to allow any (dev only). |
| `MIDEN_X402_FRESHNESS_BLOCKS` | `24` | Max blocks between note commit and chain tip before a note is rejected as expired. ~2 minutes at ~5s blocks. |
| `MIDEN_X402_GUARDIAN_ENABLED` | `false` | Enable the Phase B `/guardian/*` endpoints. When `false` (default) they are absent from the router and the facilitator behaves byte-for-byte like Phase A. |
| `MIDEN_X402_REMOTE_PROVER_URL` | _unset_ | gRPC URL of the Miden remote prover. Required when the Guardian is enabled; without it `/guardian/settle` returns `503 Service Unavailable`. |
| `MIDEN_X402_GUARDIAN_CHALLENGE_TTL_SECS` | `120` | TTL applied to issued `serial_num` challenges. Should be ≥ the merchant's advertised `maxTimeoutSeconds`. |
| `MIDEN_X402_GUARDIAN_RESERVATION_TTL_SECS` | `60` | TTL applied to reserved input nullifiers. Defensive — the verify/settle success and failure paths normally release reservations explicitly. |
| `RUST_LOG` | `info` | Standard `tracing-subscriber` env filter. |

The freshness window has a direct UX effect: too tight and slow buyers
get their notes rejected; too loose and a stale signed payload could be
replayed long after the merchant offered it. Start with the default and
tune from production traces.

## Enabling the Guardian (Phase B)

The `/guardian/*` endpoints implement the verify-before-prove flow
documented in [`ideas/GUARDIAN.md`](../ideas/GUARDIAN.md) and
[`docs/protocol.md`](./protocol.md) §A.2.7 + §B.2.7. They are
**disabled by default**. To turn them on:

```bash
MIDEN_X402_GUARDIAN_ENABLED=true \
MIDEN_X402_REMOTE_PROVER_URL=https://prover.miden.host:50051 \
./target/release/miden-x402-facilitator
```

When enabled:

- `/supported` advertises `"miden-guardian-fast"` in `extensions`.
- `POST /guardian/challenge` accepts merchant requests for a
  server-generated `serial_num`.
- `POST /guardian/verify` and `POST /guardian/settle` accept signed
  unproven transactions; settle forwards to the remote prover and
  submits the resulting `ProvenTransaction` to the configured Miden node.

Guardian state (issued challenges + reserved nullifiers) is held in
memory inside the process. A single Guardian deployment is one process
— horizontal scaling requires either sticky routing or moving state to
a shared store (not implemented in this iteration).

## Health probes

`GET /health` returns `{"status":"ok"}`. Use this for liveness and
readiness probes — the binary starts the HTTP listener immediately, so
a successful response also implies the configured RPC URL is at least
syntactically valid.

`GET /supported` returns the list of `(x402Version, scheme, network)`
tuples the facilitator handles. Currently the single tuple is
`(2, "exact", "miden:testnet")`. Useful for discovery and for x402
client libraries that probe before issuing a payment.

## Docker (sketch)

A minimal Dockerfile is not shipped in this repo, but the binary is
self-contained and runs in any glibc base image. Suggested shape:

```dockerfile
FROM rust:1.85 AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p miden-x402-facilitator --bin miden-x402-facilitator

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/miden-x402-facilitator /usr/local/bin/
EXPOSE 8080
ENV MIDEN_X402_LISTEN_ADDR=0.0.0.0:8080
ENTRYPOINT ["miden-x402-facilitator"]
```

`ca-certificates` is required for outbound TLS to the public testnet RPC.

## Multi-tenant deployment

The facilitator does not authenticate callers. In production:

- Put it behind a reverse proxy / API gateway that handles TLS, rate
  limits, and (optionally) merchant-side auth.
- Use `MIDEN_X402_ALLOWED_FAUCETS` to lock down the asset universe.
  Setting this to `*` is a development convenience only.
- Consider running one facilitator per network the deployment supports.
  This service only speaks Miden testnet.

## Observability

`RUST_LOG=info` or `RUST_LOG=miden_x402_facilitator=debug` is sufficient
for local debugging. Every verification logs its outcome and (on
failure) the reason. The default `tracing-subscriber` writes structured
JSON when `RUST_LOG_FORMAT=json` is set in your wrapper — not built into
the binary but trivial to swap in via a small fork of
`bin/miden_x402_facilitator.rs` if you need it.

## Validating a deployment

After deploying:

1. `curl /health` — expect `{"status":"ok"}`.
2. `curl /supported` — expect the `miden:testnet` kind.
3. Run [`miden-x402-smoke-testnet`](./smoke-testnet.md) against the
   facilitator's RPC config with a real on-chain note. Expect
   `verify: VALID` and `settle: SUCCESS`.
4. Wire a merchant + agent demo pair against the public URL. Expect the
   demo `GET /weather` flow to return `200 OK` with `Payment-Response`
   attached.

Step 3 plus the M4b demos are the production sign-off check. If they
pass, an API provider can drop the facilitator URL into their existing
x402 wiring.
