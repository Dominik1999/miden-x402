# miden-x402

A Guardian-operated x402 payment facilitator for the Miden L2 zk-rollup.

This repository implements the design accepted in
[Miden protocol discussion #2919](https://github.com/0xMiden/protocol/discussions/2919),
captured locally as [`ideas/DESIGN.md`](ideas/DESIGN.md):

> Agents sign Miden transactions **without proving**. A Guardian-operated
> facilitator verifies the signature, enforces an AP2 mandate, reserves
> output nullifiers, and acknowledges the payment within a network
> round-trip. Proving and on-chain settlement happen asynchronously in
> the background, off the agent's critical path.

The end-to-end target is sub-second perceived latency for x402 payments
on Miden, comparable to Base x402.

## Layout

```
crates/
  # Vendored from https://github.com/OpenZeppelin/guardian (do not edit)
  shared/                  guardian-shared types
  client/                  guardian HTTP client
  server/                  guardian-server library + binary
  miden-rpc-client/        lightweight Miden node RPC wrapper
  miden-keystore/          Falcon key handling
  contracts/               miden multisig+guardian account components
  miden-multisig-client/   reference sign-without-prove client

  # New here
  x402-facilitator-server/ x402 facilitator endpoints on top of guardian-server
  miden-agentic-client/    agent-paced single-signer client built on miden-keystore

examples/
  x402-bench/              N-agent x K-payment latency/throughput harness
  reference-merchant/      minimal x402 paywall server
ideas/
  DESIGN.md                the accepted design
```

The vendored Guardian crates are pinned to the upstream layout so we can
follow upstream patches by diffing against `/path/to/guardian/crates`.

## Quick start

```bash
cargo check --workspace
```
