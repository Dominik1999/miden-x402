# miden-x402

x402 payments on Miden using the **batch-settlement** scheme with AgentDebitNote.

## Why batch-settlement on Miden?

The [x402 spec](https://docs.x402.org) defines three payment schemes: `exact`, `upto`, and `batch-settlement`. This repo implements Miden as a new network binding for `batch-settlement` — the scheme designed for high-throughput micropayments.

### Direct alignment with the x402 spec

- **`batch-settlement` is a first-class x402 scheme.** We're not inventing a new protocol — we're adding `"network": "miden:testnet"` as a new network binding for an existing scheme.
- **The facilitator role matches exactly.** x402 facilitators expose `/verify` and `/settle`. Our facilitator has exactly these two endpoints. The facilitator does not hold funds or act as a custodian — it verifies and settles.
- **The lifecycle matches.** x402 batch-settlement has three phases: Commit → Accumulate → Redeem. Our flow: Channel Setup (commit ADN note) → Cumulative Vouchers (accumulate off-chain) → Settlement (redeem on-chain).
- **The wire format aligns.** `PAYMENT-SIGNATURE` headers with `scheme`, `network`, `payload` map directly to our voucher structs.

### Why Miden is a natural fit for batch-settlement

- **Private notes.** The AgentDebitNote is a private note — only a commitment is on-chain. The merchant, amount, and user are not visible to observers.
- **Atomic settlement.** Miden consumes the ADN note and creates P2ID output + remainder in a single atomic transaction. No separate claim/settle steps like EVM.
- **No gas fees per payment.** Vouchers are signed off-chain by the agent. The merchant verifies locally. Only the settlement transaction touches the chain.
- **Falcon signatures.** Miden's native Falcon-512 signatures are verified in the note script on-chain. No EVM signature recovery or gas-heavy verification.

### The flow

```
Phase 1 — Channel Setup (one on-chain tx):
  Agent creates AgentDebitNote → locks funds with committed merchant
  Agent sends noteCommitment + details to merchant
  Merchant calls facilitator /verify → confirms note is on-chain

Phase 2 — Per-Request (zero on-chain txs):
  Agent signs cumulative voucher: (serial, merchant, cumulativeAmount)
  Merchant verifies locally against stored userPubKey
  Resource served immediately — no facilitator involved

Phase 3 — Settlement (one on-chain tx):
  Merchant calls facilitator /settle with latest voucher
  Facilitator consumes ADN → P2ID(cumulativeAmount) to merchant + remainder
  Returns remainderNoteCommitment → next batch continues from remainder
```

## Benchmark Results

Tested on real Miden testnet with standalone binaries across 3 AWS EC2 instances.

### Same-region (us-east-1, 3x t3.xlarge)

50 payments, 2 settlement batches:

| Metric | Value |
|--------|-------|
| **Voucher latency p50** | **2.03ms** |
| Voucher latency avg | 2.49ms |
| Settlement #1 | ~36s (ZK proving on t3.xlarge) |
| Settlement #2 | ~32s |
| Multi-batch | Working (serial reset + remainder note) |

> Note: The ~2ms voucher latency is dominated by Falcon signing (~1.4ms). Network RTT within the same region adds ~0.5ms. Settlement latency is ZK proving — would be ~3s on a c5.4xlarge.

### Compared to other approaches

| | batch-settlement (this) | ADN dual-sig | Guardian verify-before-prove |
|---|---|---|---|
| On-chain txs per session | 2 (setup + settle) | N (one per payment) | N (one per payment) |
| Facilitator per-request | No | Yes (co-signs) | Yes (verifies + queues) |
| x402 scheme fit | Direct (`batch-settlement`) | None (custom) | Closest to `exact` |
| Trust model | Merchant verifies locally | Facilitator co-signs | Facilitator acks |
| Agent computation | ~1.4ms (Falcon sign) | ~1.4ms (Falcon sign) | ~15ms (kernel + sign) |
| Privacy | Private note | Private note | Public account state |

## What's proven

All tests pass on the real Miden testnet (`rpc.testnet.miden.io`):

- **10 MockChain tests** — consume path, reclaim path, attack vectors
- **5 voucher unit tests** — sign/verify, cumulative amounts, wrong key/amount
- **3 wire format tests** — x402 header encode/decode round-trip
- **1 e2e testnet test** — full batch-settlement flow (34s)
- **AWS multi-server benchmark** — 50 payments across 3 EC2 instances (2ms p50)

## Standalone binaries

```bash
# Build
cargo build --release -p adn-services

# Setup: create accounts + ADN note on testnet
./target/release/adn-agent setup --data-dir /tmp/agent --out-config /tmp/config.json

# Start facilitator (separate server)
./target/release/adn-facilitator --port 7002 --data-dir /tmp/fac --account-file fac.b64 --import-keystore /tmp/agent/keystore

# Start merchant (separate server)
./target/release/adn-merchant --port 7001 --facilitator-url http://facilitator:7002 --merchant-id 0x... --settle-after 25

# Run benchmark
./target/release/adn-agent benchmark --config /tmp/config.json --merchant-url http://merchant:7001 --payments 50
```

## One-command AWS benchmark

```bash
# Launches 3 EC2 instances, builds, runs 50 payments, prints results, terminates
./scripts/bench-full.sh --key-name my-key --key-file ~/.ssh/my-key.pem --payments 50
```

## Layout

```
crates/
  agent-debit-note/        ADN note script (MASM), Rust types, voucher, wire format, SDK
  adn-services/            Standalone binaries: adn-agent, adn-facilitator, adn-merchant
  adn-client/              Agent-side signing client

  # Vendored from OpenZeppelin Guardian
  shared/                  guardian-shared types
  client/                  guardian HTTP client
  server/                  guardian-server library + binary
  miden-rpc-client/        Miden node RPC wrapper
  miden-keystore/          Falcon key handling
  contracts/               multisig+guardian account components
  miden-multisig-client/   sign-without-prove client

scripts/
  bench-full.sh            One-command AWS benchmark (launch → build → run → terminate)
  bench-local.sh           Local benchmark using e2e test
  bench-aws.sh             Benchmark on existing EC2 instances by IP
```

## Quick start

```bash
# Run MockChain tests (no network needed)
cargo test -p agent-debit-note --test note_script
cargo test -p agent-debit-note --test voucher_test
cargo test -p agent-debit-note --test wire_test

# Run e2e on testnet (requires network access)
RUST_LOG=info cargo test --release -p agent-debit-note --test batch_settlement_e2e -- --ignored --nocapture

# Run local multi-server benchmark
./scripts/bench-local.sh
```

## Other approaches (feature branches)

| Branch | Approach | Status |
|--------|----------|--------|
| `feat/adn-dual-sig` | ADN with agent + facilitator co-signature per payment | Working (9 e2e tests) |
| `feat/guardian-verify-before-prove` | MultisigGuardian sign-without-prove | WIP |
| `feat/agentic-guardian` | Hot-key + AP2 mandate enforcement | WIP |

See [docs/x402-scheme-analysis.md](docs/x402-scheme-analysis.md) for analysis of how `exact` and `upto` schemes could be added.

## Spec reference

- [x402 batch-settlement scheme](https://docs.x402.org/schemes/batch-settlement)
- [Miden network binding spec (gist)](https://gist.github.com/VAIBHAVJINDAL3012/c2b8a19c160f8121918c630188c1d99b)
