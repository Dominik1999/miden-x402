# miden-x402

x402 v2 payments for the Miden network.

This is a Rust workspace that hosts the facilitator service and shared wire
types for the [x402 payment protocol](https://www.x402.org) on Miden. The
overall scope, design rationale, and milestones live in [`PLAN.md`](./PLAN.md).

## Status

Pre-alpha. Currently shipping milestone **M1** — the wire-format types crate
`miden-x402-types`. The facilitator binary (M2) and the Node / Python SDKs come
next.

## Crates

- [`miden-x402-types`](./crates/miden-x402-types) — x402 v2 types specialised
  for Miden's `exact` scheme.

## License

Apache-2.0.
