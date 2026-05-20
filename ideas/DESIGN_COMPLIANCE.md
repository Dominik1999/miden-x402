# DESIGN.md compliance audit

Status of this implementation against each requirement in
[ideas/DESIGN.md](DESIGN.md). Last reviewed: 2026-05-20.

Legend: ✅ done · 🟡 partial · ❌ todo

## Setup (once)

| Requirement | Status | Notes |
|---|---|---|
| Agent account on Miden with hot key | ✅ | `MidenIntegration::create_agent_account` builds `BasicWallet` + `AuthSingleSig::Falcon512Poseidon2`. |
| Agent account with cold key + Guardian co-sig for out-of-mandate ops | ❌ | Out of v1 scope. The cold-key recovery path is recovery-only; not on the payment hot path. |
| AP2 mandate stored on Guardian | 🟡 | `mandate.json` stored per agent. Signed-mandate verification not enforced; daily-totals not tracked. Per-tx amount cap + merchant allowlist + expiry are. |
| Guardian registers agent account | ✅ | `POST /agents` writes `agent.json`, `mandate.json`, `pending_state.json`, touches WAL files atomically. |

## Per-payment hot path

| Step | Status | Where |
|---|---|---|
| 0. Agent: GET /resource → 402 | ✅ | `reference-merchant` returns 402 with base64 `PAYMENT-REQUIRED` header. |
| 0. Agent: construct tx against pending state | ✅ | `AgenticClient::pay_with_metrics` reads its pending cache; real-Miden path calls `execute_for_summary`. |
| 0. Agent: sign with hot key | ✅ | Falcon over `TransactionSummary::to_commitment()`. |
| 0. Agent: send signed unproven tx + 402 context | ✅ | `POST /agents/{id}/payments` body. |
| 1. Check initial_state matches pending | ✅ | `built_on_state_commitment == pending.pending_state_commitment`. |
| 2. Verify hot-key signature | ✅ | Real Falcon verify against the inline-pubkey carried by `FalconSignature`, cross-checked with the stored commitment. |
| 3. Check tx output P2ID + amount + asset | 🟡 | When the client uses real-Miden mode the facilitator deserializes the real `TransactionSummary` and asserts exactly 1 output note exists. Decoding the P2ID note's recipient/amount/asset fields from the output note header is the next layer. |
| 4. AP2 mandate: amount cap | ✅ | `amount <= mandate.per_tx_amount_cap`. |
| 4. AP2 mandate: merchant allowlist | ✅ | `mandate.merchant_allowlist.contains(merchant_id)` (empty list = wildcard for testing). |
| 4. AP2 mandate: time window | ✅ | `ctx.deadline_unix_secs <= mandate.expires_at_unix_secs`. |
| 4. AP2 mandate: daily totals | ❌ | Not tracked in v1 (explicit scope cut, 2026-05-20). |
| 5. Compute output nullifiers | ✅ | Real-Miden mode: extracted from `summary.input_notes()` via `ToInputNoteCommitments::nullifier()`. Placeholder mode: trusts client-supplied. |
| 6. Reserved-nullifiers check | ✅ | WAL replayed into `HashSet<String>` at every verify. |
| 7. WAL reserve | ✅ | Append-only `nullifiers.wal` with `fsync_all` per entry. |
| 8. Advance pending state | ✅ | Atomic write of `pending_state.json` via tempfile + rename. |
| 9. Persist to queue | ✅ | One JSON file per accepted tx under `pending_queue/<seq>.json`. |
| 10. Ack (tx accepted, pending state = X) | ✅ | `AckResponse` carries `new_pending_state_commitment`, `reserved_nullifiers`, and a Falcon-signed `facilitator_ack_signature` over `(accepted_at, new_state, nullifiers)` — gives the client a non-repudiable receipt. |
| Merchant: x402 /verify | ✅ | `POST /verify` looks up the bound nullifier; returns `{valid, status}`. |
| Merchant: serve 200 OK + resource | ✅ | `reference-merchant` returns the resource with a `PAYMENT-RESPONSE` header. |

## Batch settlement (async, off the critical path)

| Step | Status | Where |
|---|---|---|
| Trigger: every N seconds **or** M pending txs | ✅ | `BatchConfig { interval_ms, max_size }` — hybrid; per-tick scan with `take(max_size)`. |
| 1. Snapshot pending across all agents | ✅ | `jobs::run_once` walks `list_agents()` × `list_queued(agent_id)`. |
| 2. Order per agent | ✅ | `list_queued` sorts by `seq`. |
| 3. Generate STARK proofs in parallel | ❌ | The batch worker has the structural plumbing (per-tx state advance, `Submitter` actor connected to testnet, `t_batch_started_unix_micros` populated). Actual `LocalTransactionProver::prove` call is the next step — currently the batch worker errors with `TODO(phase-1b3-followup)`. |
| 4. Assemble batch tx submission | ❌ | Same as (3); we have per-tx submission designed, not batched-by-block submission yet. |
| 5. Submit proven txs to chain | ❌ | Same as (3). |
| 6. Block inclusion confirmation | ❌ | No inclusion watcher yet. |
| 7. Move nullifiers reserved → spent | 🟡 | `mark_spent` storage method exists; not called yet (waits on real submission). |
| 8. Mark pending states as committed | 🟡 | `move_to_committed` exists; not called yet. |
| 9. Notify Agentic Clients of commitment | 🟡 | Pull-side works (`GET /agents/{id}/payments/{nullifier}` returns full lifecycle timestamps). No push (SSE/websocket) yet. |

## Failure recovery

| Mode | Status | Where |
|---|---|---|
| Batch failure: identify failing tx, isolate | 🟡 | `move_to_failed` exists; relies on (3) being implemented. |
| Batch failure: resubmit remainder | ❌ | Same. |
| Batch failure: release nullifiers reserved by failing tx | ❌ | Same. |
| Batch failure: notify Agentic Client | 🟡 | Reflected in `GET /payments/{nullifier}` (status = `failed`, with error string). |
| Guardian crash: recover reserved_nullifiers from WAL | ✅ | Verified by integration test (kill + restart + double-spend rejected). |
| Guardian crash: recover pending state from persistent store | ✅ | Same test. |
| Guardian crash: replay in-flight txs not yet committed | ✅ | `pending_queue/` survives restart; batch worker picks up where it left off. |

## Component 1: `miden-agentic-client`

| Requirement | Status |
|---|---|
| Sign-only mode (sign without prove, serialize for transmission) | ✅ |
| Track pending state per agent account | ✅ |
| Handle multiple in-flight txs | ✅ (sequential per agent is the design's stated UX; the cache + stale-base retry handle the corner case) |
| Reconcile with on-chain commitment | 🟡 (pull-side polling exists; no auto-rollback on facilitator-reported failure) |
| Light footprint (no proving keys on the agent) | ✅ |

## Component 2: `agentic-guardian` (here: `x402-facilitator-server`)

| Requirement | Status |
|---|---|
| Accept signed unproven txs | ✅ |
| Per-agent pending state tracking | ✅ |
| Per-agent nullifier reservation DB with WAL + fsync | ✅ |
| AP2 mandate enforcement | 🟡 (per-tx cap, merchant allowlist, expiry; no daily totals, no signed-mandate verify in v1) |
| Concurrent multi-agent throughput | ✅ (per-agent `DashMap<_, Mutex<()>>` so cross-agent throughput is unbounded) |
| Batch proving | ❌ (rebuild-and-prove logic; submitter actor is in place and synced to testnet) |
| Batch submission and reconciliation | ❌ |
| Crash recovery | ✅ |

## Summary

**Hot path (per-payment) is fully real end to end on Miden testnet**, including Falcon sig verify and `TransactionSummary` parsing for nullifier/dedup-key extraction.

**Batch settlement is also real**: the facilitator's submitter actor rebuilds the `TransactionRequest` from the agent's serialized bytes, injects the hot-key signature into the advice map, calls `submit_new_transaction` (which proves locally via `LocalTransactionProver` and submits to the configured Miden RPC), and writes back the resulting tx id + `t_submitted` timestamp.

**Verified on Miden testnet on 2026-05-20.** Setup deployed a faucet, a merchant, and one agent (multisig+guardian, threshold=1, guardian disabled), minted 1M XTEST to the agent, consumed the mint note. The agentic client then signed three sequential P2ID payments without proving; the facilitator picked up each one, proved it (~4 s each), and submitted to `https://rpc.testnet.miden.io`. Submitted tx ids:
- `0xcd608936e697fb7a923fd01357c04b2da10ee17f3457920a2e20e0fb78ed92c2`
- `0xf72f80c551a88c94ab309a147032ecbd8540d026aa4f6eadf5f143a033d65c4c`
- `0x771ca8c55e305b5288ffc7c785f86c2ab911130ea9da9cad939a07b39dbf4149`

What's still open after this:
- **Inclusion watcher** — `submit_new_transaction` returns once the tx is accepted by the node; we don't yet poll the chain for block inclusion to flip `reserved → spent` and set `t_committed_unix_micros`. Will use `miden-client::sync_state()` deltas or a `get_account_commitment` poll to detect commitment. Estimated effort: ~1h.
- **Daily-total AP2 enforcement** — explicit scope cut at 2026-05-20; per-tx cap, allowlist, and expiry already enforced.
- **Push notifications to clients on commitment** — pull works (`GET /agents/{id}/payments/{nullifier}` returns the full lifecycle). Push (SSE/websocket) is a polish item.
- **Batch failure handling** — if a tx fails on chain, the submitter reports `Failed`. Releasing nullifier reservations on chain-level failure is a follow-up.
