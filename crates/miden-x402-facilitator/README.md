# miden-x402-facilitator

Guardian-as-x402-facilitator — module bolted onto an OpenZeppelin
[Guardian](https://github.com/OpenZeppelin/guardian) server. Ships the
`guardian-facilitator` binary that runs a Guardian server with the
`/x402/*` routes mounted on the same port.

For the design that drives this crate, see
[`ideas/DESIGN.md`](../../ideas/DESIGN.md). For the wire contract, see
[`docs/protocol.md`](../../docs/protocol.md).

## What ships

The crate is split into:

| Module | Purpose |
|---|---|
| [`config`](src/config.rs) | env-driven facilitator config |
| [`storage`](src/storage/mod.rs) | pluggable repos for challenges, reservations, batch queue, receipt key |
| [`mandate`](src/mandate.rs) | `MandatePolicy` trait + `AllowAll` default |
| [`buyer_auth`](src/buyer_auth.rs) | buyer cosigner pubkey lookup |
| [`balance`](src/balance.rs) | buyer balance lookup against Guardian's persisted state |
| [`verify`](src/verify.rs) | verify-before-prove pipeline |
| [`settle`](src/settle.rs) | enqueue + sign receipt |
| [`batch`](src/batch.rs) | async batch settle worker |
| [`receipt`](src/receipt.rs) | facilitator-owned Falcon receipt signer |
| [`handlers`](src/handlers.rs) | axum `/x402/*` router |
| [`bin/guardian_facilitator.rs`](src/bin/guardian_facilitator.rs) | the single binary |

## HTTP routes

All `/x402/*` routes are mounted on top of the standard Guardian router
in a single process / single port. See
[`docs/protocol.md`](../../docs/protocol.md) §B for the wire contract.

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/x402/challenge` | Issue a server-generated `serial_num` |
| `POST` | `/x402/verify` | Verify a signed-but-unproven payment |
| `POST` | `/x402/settle` | Verify + enqueue for batch settle + return receipt |
| `GET` | `/x402/pubkey` | Return the facilitator's receipt-signing pubkey |
| `GET` | `/x402/supported` | Declare the supported `(scheme, network)` kind |
| `GET` | `/x402/health` | Liveness probe |

Guardian routes (`/configure`, `/delta`, `/pubkey`, etc.) are mounted on
the same port — the facilitator IS the Guardian.

## Running

```bash
export MIDEN_X402_REMOTE_PROVER_URL=https://prover.testnet.miden.io
cargo run --release -p miden-x402-facilitator --bin guardian-facilitator
```

See [`docs/deploy.md`](../../docs/deploy.md) for env vars and the
filesystem-state layout.

## Tests

```bash
PROTOC=/path/to/protoc cargo test -p miden-x402-facilitator
```

The unit tests cover storage repos (filesystem + memory), receipt signing,
mandate evaluation, batch queue mechanics, verify-path fast-fail paths,
and queued-id derivation. The happy path of `verify_unproven` requires
constructing a real signed `TransactionInputs` against a fixture multisig
account and is exercised end-to-end in a separate integration harness.
