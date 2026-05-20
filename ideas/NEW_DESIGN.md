## Proposed Design: Guardian-as-Facilitator for x402 on Miden

Following @bobbinth's [clarification on in-flight state](https://github.com/0xMiden/protocol/discussions/2919#discussioncomment-16976558), this is a concrete proposal that builds on the [original sketch](https://github.com/0xMiden/protocol/discussions/2919#discussioncomment-16961615) without requiring protocol changes. The goal is sub-second perceived latency for x402 payments on Miden by moving proving off the agent's critical path and into a Guardian-operated facilitator.

The pattern reuses primitives that already exist (the multisig PoC ships a "sign-without-prove" client and a coordinator that batches and submits) and applies them to the agent-payment shape. No new note types, no new account types, no MASM changes.

### Detailed flow

```
========================================================================
SETUP (once)
========================================================================

User creates agent account on Miden with two keys:
  - hot key (held by agent, used for signing payment txs)
  - cold key + Guardian co-sig required for any out-of-mandate action
User signs AP2 mandate, sends to Agentic Guardian
Agentic Guardian stores mandate, registers agent account


========================================================================
PER-PAYMENT (hot path, target sub-second perceived latency)
========================================================================

Agent             Agentic Client       Agentic Guardian          Merchant
  |                     |                     |                     |
  |-- GET /resource ----------------------------------------------->|
  |<-- 402 (x402 + Miden scheme in accepts) ------------------------|
  |                     |                     |                     |
  | Construct tx        |                     |                     |
  | against latest      |                     |                     |
  | pending state       |                     |                     |
  | Sign with hot key   |                     |                     |
  |                     |                     |                     |
  |-- signed unproven tx + 402 context ------>|                     |
  |                     |                     |                     |
  |                     |   1. Check initial_state matches latest   |
  |                     |      pending state for this agent         |
  |                     |   2. Verify hot-key signature             |
  |                     |   3. Check tx output: P2ID to merchant,   |
  |                     |      amount and asset match 402           |
  |                     |   4. Check AP2 mandate satisfied          |
  |                     |      (amount cap, merchant allowlist,     |
  |                     |       time window, daily total)           |
  |                     |   5. Compute output nullifiers            |
  |                     |   6. Check none are in reserved_nullifiers|
  |                     |   7. Reserve nullifiers (WAL persisted)   |
  |                     |   8. Advance pending state for this agent |
  |                     |                     |                     |
  |                     |<-- ack (tx accepted, pending state = X) --|
  |                     |                     |                     |
  |                     |        |-- x402 /verify OK -------------->|
  |                     |        |                                  |
  |                     |        |<-- 200 OK + resource ------------|
  |<-- resource ---------------------------------------------------|


========================================================================
BATCH SETTLEMENT (async, off the critical path)
========================================================================

Agentic Guardian                                              Chain
  |                                                            |
  | Every N seconds or M pending txs:                          |
  |   1. Take snapshot of pending txs across all agents        |
  |   2. Order them per agent (preserve state-transition order)|
  |   3. Generate STARK proofs (in parallel, per tx)           |
  |   4. Assemble batch tx submission                          |
  |                                                            |
  |-- submit proven txs -------------------------------------->|
  |                                                            |
  |<-- block inclusion confirmation ---------------------------|
  |                                                            |
  |   5. Move nullifiers from reserved -> spent                |
  |   6. Mark pending states as committed                      |
  |   7. Notify Agentic Clients of commitment                  |


========================================================================
FAILURE RECOVERY
========================================================================

If batch fails on submission (e.g. one tx invalid):
  - Identify failing tx, isolate it
  - Resubmit remaining batch
  - Release nullifiers reserved by failing tx
  - Notify Agentic Client of failure for that specific tx

If Agentic Guardian crashes:
  - Recover reserved_nullifiers from WAL
  - Recover pending state from persistent store
  - Replay any in-flight txs not yet committed
```

### What changes where

No Miden protocol changes. No new note types. No new account types. The work splits cleanly into two components.

---

### 1. `miden-agentic-client`

A new client crate (or extension of `miden-client`) that handles the agent-side flow. The reference architecture already exists in [`miden-multisig-client`](https://github.com/0xMiden/MultiSig/tree/main/crates/miden-multisig-client), which implements the same "sign without prove, send to coordinator" pattern for the human multisig case.

The multisig client is human-paced (collect N-of-M signatures from humans over minutes/hours). The agentic client needs the same architectural shape adapted for machine pace: one signer, sub-second cadence, many concurrent in-flight txs.

What it must do:

- **Sign-only mode.** Construct a transaction, sign with the agent's hot key, do not prove. Serialize the signed-but-unproven tx for transmission.
- **Track pending state per agent account.** After sending a signed tx to the Guardian and receiving ack, advance the client's view of the agent's account state. The next tx must be constructed against this pending state, not the on-chain committed state.
- **Handle multiple in-flight txs.** Bobbin's [clarification](https://github.com/0xMiden/protocol/discussions/2919#discussioncomment-16976558) is the foundational point: as long as the Guardian is the single serialization authority, no forks are possible, so the client can keep building on pending state without risk of invalidation.
- **Reconcile with on-chain commitment.** When the Guardian reports a tx as committed, mark that state transition as final. If a tx fails, roll back the pending state.
- **Light footprint.** Agent infrastructure (Crossmint, Skyfire, Payman) should be able to embed this in constrained environments. No proving keys, no kernel execution at runtime, just key management and state tracking.

Open decision: extend `miden-client` with a sign-only mode, or ship a separate `miden-agentic-client` crate that wraps the core client. The multisig precedent is "separate crate that wraps." Arguments either way:

- *Separate crate:* clear boundaries, doesn't burden core client users with agent-specific state machine
- *Core integration:* sign-only mode is generally useful (mobile, browser, any constrained signer), agentic-client becomes a thin wrapper that adds the high-throughput pending-state tracking

Needs input from @igamigo on which factoring fits the core client roadmap.

---

### 2. `agentic-guardian` (or extension of OpenZeppelin Guardian)

Likely a fork or extension of [OpenZeppelin Guardian](https://github.com/OpenZeppelin/guardian) plus the [coordinator server pattern from the multisig PoC](https://github.com/0xMiden/MultiSig/blob/main/bin/coordinator-server/README.md). The coordinator server is roughly the right shape (Rust/Axum, REST API, PostgreSQL persistence, accepts proposals, batches, submits to network). It needs adapting for the agent-payment workload.

What it must do:

- **Accept signed unproven txs.** New endpoint that takes a serialized signed tx, validates the signature against the registered agent's hot key, and queues for batch settlement.
- **Per-agent pending state tracking.** Maintain the latest acknowledged state for each registered agent account. Reject txs that don't build on the latest pending state for that agent (prevents forks).
- **Per-agent nullifier reservation DB.** Reserve nullifiers in memory at verify time, persist to WAL for crash recovery, move to spent at commitment. This prevents double-spend in the pending window.
- **AP2 mandate enforcement.** Per-agent mandate stored. Each incoming tx checked against mandate conditions (amount caps, merchant allowlists, time windows, daily totals). Reject if violated.
- **Concurrent multi-agent throughput.** Unlike the multisig coordinator which serves one account per proposal, the agentic guardian must handle N independent agents each with multiple concurrent in-flight txs. Lock contention design needs attention.
- **Batch proving.** Generate STARK proofs for pending txs in parallel and assemble a batch for submission. Proving infrastructure (CPU/GPU) sized for target throughput.
- **Batch submission and reconciliation.** Submit proven txs, watch for block inclusion, notify clients of commitment or failure.
- **Crash recovery.** All in-flight state (reserved nullifiers, pending account states, queued txs) must survive a process restart. PostgreSQL + WAL pattern from the coordinator server is a reasonable starting point.

Open decision: extend OZ Guardian directly, or ship as a separate `agentic-guardian` component that integrates with Guardian for custody and mandate verification but handles the high-throughput payment flow itself. The latter is probably cleaner because the operational profiles are different (Guardian: low-frequency high-value custody operations; agentic-facilitator: high-frequency low-value payment flows). Needs input from @bobbinth and OZ team on what fits their architecture.

---

### Why this design works

- **Sub-second perceived latency.** Agent's critical path is sign + send + ack. Proving and on-chain settlement happen asynchronously. Latency budget is dominated by network round-trips (~200-500ms), comparable to Base x402.
- **No protocol changes.** Stays inside the existing Miden account and note model. The "inverted proving flow" Bobbin described is the only architectural shift, and the primitives already exist in the multisig PoC.
- **Privacy preserved.** Notes can be private (per Digine-Labs' existing `noteType: "private"` variant). The Guardian sees its registered agents' txs (which it would anyway as co-signer), the merchant sees only what x402 verification needs.
- **Mandate enforcement composes.** Guardian's per-agent mandate check happens at verify time. AP2 enforcement at the chain layer rather than the merchant layer.
- **Cleanly reuses existing patterns.** Multisig client for sign-without-prove. Coordinator server for batched async submission. OZ Guardian for custody and mandate. New work is mostly gluing these together and adapting for machine-pace throughput.

### What this comment is for

If the design is acceptable in principle to @bobbinth, @igamigo, @partylikeits1983 and @ermvrs this can serve as the shared baseline for a more detailed implementation design across the four parties. Each component (agentic-client, agentic-guardian, x402 scheme variant, end-to-end testing) can then be scoped and assigned.

Open questions left for the design discussion:

1. Sign-only mode in core `miden-client` vs. separate `miden-agentic-client` crate
2. Extending OZ Guardian vs. separate `agentic-guardian` component
3. Where pending-state tracking primarily lives (client, Guardian, or duplicated)
4. Batch trigger (time window, size threshold, hybrid) and target latency-to-finality
5. WAL persistence design for nullifier reservations
6. How Digine-Labs' [miden-x402](https://github.com/Digine-Labs/miden-x402) scheme spec gets updated to express the new flow (likely a new variant alongside `noteType: "private"`)

The actor-model concerns raised in my [earlier follow-up](https://github.com/0xMiden/protocol/discussions/2919#discussioncomment-16976558) are resolved by the Guardian being the single serialization authority, as Bobbin explained. The `AgentDebitNote` proposal is withdrawn, sorry @partylikeits1983 
