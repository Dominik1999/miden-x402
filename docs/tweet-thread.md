# x402 on Miden — Measured Latency Tweet Thread

---

**1/**

We measured real network latency for x402 payments on Miden.

Setup: agent in eu-west-1, facilitator+merchant in us-east-1, 68ms RTT between them. Real Miden kernel execution, real Falcon signing, real testnet RPC.

Result: 230ms end-to-end for an agent to pay and receive a resource.

---

**2/**

How it works:

x402 is an HTTP payment protocol. An agent requests a resource, gets a 402 response with payment terms, pays, and gets the resource.

On EVM chains (Base), the facilitator submits an on-chain tx and waits for block inclusion before responding. That takes 1.5-2.5s.

---

**3/**

On Miden, we move STARK proving off the critical path.

The agent executes the transaction kernel locally (13ms), signs the resulting TransactionSummary with Falcon (2ms), and sends the signed-but-unproven tx to a Guardian-Facilitator.

The facilitator verifies the signature, checks the P2ID output, reserves nullifiers, and acks — all in 11ms server-side.

---

**4/**

The measured breakdown per payment (steady-state, 68ms transatlantic RTT):

```
Miden kernel execution:     13 ms
Falcon signing:              2 ms
Facilitator round-trip:     79 ms  (68ms wire + 11ms verification)
Merchant verify round-trip: 69 ms  (68ms wire + 1ms loopback)
─────────────────────────────────
Total:                     230 ms
```

The system is 3x RTT-dominated. On a closer link the total drops proportionally.

---

**5/**

The design uses OpenZeppelin's Guardian as the foundation. The Guardian acts as the single serialization authority for each agent's account state — solving Miden's actor-model constraint without protocol changes.

Per-agent pending state, WAL-backed nullifier reservation, and per-agent locking ensure concurrent multi-agent throughput without double-spend.

---

**6/**

The trust model matters.

On Base x402, the merchant serves the resource AFTER on-chain settlement. The facilitator just reads chain state — the merchant can independently verify.

On Miden x402, the merchant serves the resource BEFORE settlement. The facilitator promises to prove and submit asynchronously (~4s per tx on CPU).

---

**7/**

This means the merchant trusts the facilitator to actually settle. If the facilitator and agent collude, the merchant gives away the resource for free.

Same trust model as credit cards: serve now, settle later, dispute if needed. Acceptable for small automated API payments between known parties. Not a replacement for settlement-before-delivery when that guarantee is needed.

---

**8/**

What's real:
- Full Miden VM kernel execution (sign-without-prove via MultisigGuardian's Unauthorized path)
- Real Falcon-512 signature verification on the facilitator
- P2ID output validation (recipient, asset, amount)
- WAL-persisted nullifier reservation with fsync
- Real STARK proving + submission to Miden testnet (async, ~4s/tx)
- Measured on real AWS infra across the Atlantic

---

**9/**

What's still needed:
- Block-inclusion watcher (reserved nullifiers never become "spent" yet)
- Nullifier release on batch failure
- The sign-without-prove pattern requires MultisigGuardian accounts — an upstream `execute_for_summary()` API in miden-client would make this cleaner

Design by @aspect_build, implementation by @DigineLabs, built on @OpenZeppelin Guardian + @0xMiden protocol.

---

**10/**

The full audit, benchmark scripts, and measurement data are at:
github.com/Digine-Labs/miden-x402 (branch: oz-guardian-latest-flow)

Anyone can reproduce: deploy a facilitator, point the bench at it, and measure your own RTT-dominated latency.
