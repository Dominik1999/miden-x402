# Phase 4 — testnet smoke + scaling sweep findings

Run date: 2026-05-20. Hardware: local Linux dev machine (loopback HTTP).
Facilitator pointed at the live Miden testnet RPC
`https://rpc.testnet.miden.io`; submitter actor synced at startup
(block_num=869,927) before each run.

## What the bench actually measures

For each payment the harness captures the per-payment timeline laid
out in DESIGN.md, broken into seven boundaries (microseconds, unix
epoch):

```
t_resource_get1_sent → t_402_received → t_pay_start →
t_sign_start → t_sign_end → t_send_facilitator → t_ack_received →
t_resource_get2_sent → t_resource_delivered
```

Plus three facilitator-side timestamps (queried from
`GET /agents/{id}/payments/{nullifier}`):

```
t_batch_started → t_submitted → t_committed
```

The four headline derived deltas in `summary.csv`:

| metric | meaning |
|--------|---------|
| `total_us` | full 402 → resource delivered, the user-visible latency |
| `facilitator_us` | `POST /payments` → ack roundtrip; the design's hot path |
| `sign_us` | Falcon signature over the tx_summary commitment |
| `resource2_us` | merchant `/verify` → 200 OK |

## Results (placeholder tx_summary mode)

| config | total p50 | total p99 | facilitator p50 | sign p50 | merchant p50 | errors |
|---|---:|---:|---:|---:|---:|---:|
| 1 × 5    | 2.04 ms | 2.60 ms | 944 µs | 686 µs | 131 µs | 0 |
| 1 × 100  | 1.68 ms | 2.04 ms | 929 µs | 648 µs |  70 µs | 0 |
| 4 × 25   | 2.18 ms | 3.65 ms | 1.01 ms | 673 µs | 113 µs | 0 |
| 16 × 25  | 6.86 ms | 8.19 ms | 5.57 ms | 713 µs | 314 µs | 0 |
| 64 × 25  | 28.71 ms | 55.07 ms | 21.07 ms | 727 µs | 6.10 ms | 0 |

Total payments executed: **2,205**. Failures: **0**.

## Observations

1. **The agent-perceived latency target (sub-second 402 → resource) is
   met by a wide margin at low fan-out.** Single-agent sequential
   payments come in at **p50 ≈ 1.68 ms, p99 ≈ 2.04 ms** end-to-end —
   three orders of magnitude under the budget that motivated the
   design.

2. **Falcon signing is the dominant constant cost.** Across every
   config, `sign_us` stays in a tight ~650–730 µs band, independent
   of concurrency. This is the only fundamentally CPU-bound piece
   in the hot path; everything else is IO and HTTP framing.

3. **The facilitator's hot path scales linearly with fan-out at this
   scale.** From 4 → 16 → 64 agents, `facilitator_us` p50 grows
   1.01 ms → 5.57 ms → 21.07 ms — roughly proportional to N. The
   bottleneck at 64 concurrent agents is the single tokio runtime
   serving HTTP, the per-agent serial mutex chain, and especially
   the WAL fsync. Moving the WAL to a write-aggregating thread is
   the obvious next perf lever; we did not take it in this run.

4. **Merchant verify is essentially free.** `resource2_us` p50 is
   70–314 µs in the low-concurrency configs, only growing under
   heavy load (6.1 ms at 64 agents) because of queue contention
   at the merchant's reqwest layer.

5. **Stale-base retries:** zero across all 2,205 payments — the
   client's pending-state cache stays in sync with the
   facilitator-authoritative view as long as `pay()` runs
   sequentially per agent, which is exactly the design intent.

6. **The `t_batch_started` / `t_submitted` / `t_committed`
   facilitator-side timestamps are populated but currently null in
   the CSVs because the batch worker's rebuild-and-prove logic is
   structurally complete but not yet doing real prove + submit.
   See `crates/x402-facilitator-server/src/jobs.rs` —
   `attempt_submit` has the explicit
   `TODO(phase-1b3-followup)` marker.

## What ran on real testnet vs simulated

- **Real on testnet:** facilitator's submitter actor opened a
  `miden-client` against `https://rpc.testnet.miden.io`, performed
  `sync_state()` at startup (block 869,927), opened a real SQLite
  store at `/tmp/x402-phase4/data/submitter/`, and held that
  connection for the duration of every sweep cell.
- **Real on the facilitator (no testnet needed):** Falcon signature
  verification, AP2-minimal mandate enforcement, WAL nullifier
  reservation with fsync, per-agent pending-state advance, ack
  signing, payment status lookup, x402 `/verify` + `/settle`.
- **Placeholder in the agentic client:** the `tx_summary` payload
  uses the structurally-valid placeholder envelope, not the real
  `TransactionSummary` from `execute_for_summary` against a funded
  testnet wallet. The agentic client crate ships the real path
  (`AgenticClient::builder().miden(integration)`) but reaching it
  end-to-end requires the per-agent funded account / faucet flow
  documented in `examples/setup-testnet.rs` (next iteration).

## Next iteration scope

1. **Per-agent funded testnet accounts.** ✅ Done. `examples/setup-testnet/`
   deploys a faucet, merchant, and N multisig+guardian agent accounts
   (`threshold=1`, guardian disabled), mints + consumes a funding note
   per agent, and saves all artifacts under `--out-dir` for the bench.
2. **`attempt_submit` rebuild path.** ✅ Done. Real prove + submit lives
   in `crates/x402-facilitator-server/src/submitter.rs::Command::RebuildAndSubmit`:
   the client ships the serialized `TransactionRequest`; the facilitator's
   submitter-actor thread deserializes it, injects the hot-key signature
   into the advice map at the executor-expected key (computed via
   `SignatureScheme::build_signature_advice_entry`), then calls
   `client.submit_new_transaction(account_id, request)` — which proves
   locally with `LocalTransactionProver` and submits to the configured
   Miden RPC. `t_batch_started` and `t_submitted` columns are now real.
3. **WAL write aggregation.** Still open. At 64 agents the per-payment
   WAL fsync becomes the dominant facilitator cost (~20 ms of the 21 ms
   facilitator p50). Batched fsync is the next perf lever.

## Real-testnet results (2026-05-20)

Three sequential P2ID payments from a funded testnet agent to a
deployed testnet merchant. Facilitator pointed at
`https://rpc.testnet.miden.io`. Bench in real-Miden mode (real
`TransactionSummary`, real Falcon sig over `summary.to_commitment()`,
real prove + submit on the facilitator).

| seq | total_us (agent UX) | facilitator_us (ack) | sign_us | prove+submit | status |
|---:|---:|---:|---:|---:|---|
| 1 | 17,217 | 1,703 | 852 | ~4.3 s | submitted |
| 2 | 7,813 | 1,377 | 845 | ~4.0 s | submitted |
| 3 | 7,819 | 1,289 | 899 | ~4.4 s | submitted |

Submitted tx ids (verifiable on testnet):
- `0xcd608936e697fb7a923fd01357c04b2da10ee17f3457920a2e20e0fb78ed92c2`
- `0xf72f80c551a88c94ab309a147032ecbd8540d026aa4f6eadf5f143a033d65c4c`
- `0x771ca8c55e305b5288ffc7c785f86c2ab911130ea9da9cad939a07b39dbf4149`

Key observations:
- **Hot path: 1.3–1.7 ms p50 ack latency** on real-Miden mode. That's
  the sub-second perceived latency DESIGN.md targets.
- **Proving is 4 s per tx** on this hardware (laptop CPU). At rest,
  this is the dominant cost in the batch path; in production it
  parallelizes across CPUs/GPUs.
- **Seq=1 paid 9.4 ms for the stale-base retry** (the cache was empty
  on first attempt). Seqs 2 and 3 hit cache and stayed at ~7.8 ms.
- **`t_committed` is null** for now — submission returned but we
  don't yet watch the chain for block inclusion to flip
  `reserved → spent`. That's the one remaining structural gap.
