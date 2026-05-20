## Idea: Guardian as x402 Facilitator (Batched Settlement)

Following the latency discussion in [node#1796](https://github.com/0xMiden/node/issues/1796), I want to float an architectural alternative that I think collapses three roles into the Guardian and changes our competitive position on M2M latency.

### The pattern

Standard x402 on Miden (per [Digine-Labs/miden-x402](https://github.com/Digine-Labs/miden-x402)): agent constructs note, proves locally, submits to network, waits for block inclusion, sends `{note_id, inclusion_proof}` to merchant, merchant verifies via the facilitator. End-to-end ~5-10s, dominated by proving + block time. Settled-at-commit model: a P2ID note in a committed block is the settlement event, and the facilitator is a read-only verifier.

Standard x402 on Base: agent signs an EIP-3009 authorization, facilitator verifies signature, merchant delivers, facilitator settles on-chain async. Sub-second perceived latency, but trust assumption that facilitator submits and merchant delivers.

Proposed pattern: use the Guardian as the x402 facilitator with batched settlement.

The 402 response follows the standard x402 spec structure, with Miden-specific note parameters carried in the `extra` field:

```json
{
  "x402Version": 1,
  "accepts": [
    {
      "scheme": "miden-p2id-private",
      "network": "miden-mainnet",
      "payTo": "<recipient_account_id>",
      "asset": "<faucet_id>",
      "maxAmountRequired": "100000",
      "resource": "https://api.example.com/premium",
      "maxTimeoutSeconds": 60,
      "extra": {
        "note_tag": "<tag>",
        "serial_num": "<server_generated>"
      }
    }
  ]
}
```

```
Agent                Guardian-Facilitator             Merchant
  |                          |                           |
  |-- GET /resource ------------------------------------>|
  |<- 402 (standard x402 with Miden scheme in accepts) --|
  |                          |                           |
  | Build private P2ID note  |                           |
  | + sign tx (NOT proven)   |                           |
  |                          |                           |
  |-- Signed unproven note ->|                           |
  |                          |                           |
  |       Verify: signature valid, note matches 402,     |
  |       agent has balance?, mandate satisfied,         |
  |       input note reserved (nullifier lock)           |
  |                          |                           |
  |                          |-- Verification OK ------->|
  |                          |                           |
  |<-- Resource delivered -------------------------------|
  |                          |                           |
  |                          |<-- Resource delivered ----|
  |                          |                           |
  |       Batch with other pending payments              |
  |       Co-sign + prove batch                          |
  |       Submit batch to network                        |
```

The merchant's trust model here is the same as on Base: trust the facilitator's verification message and deliver. The merchant does not independently verify on-chain inclusion at delivery time. Batch settlement produces an auditable on-chain record post-hoc, useful for accounting and reconciliation but not on the critical path.

### Why so?

**Same trust assumptions as Base, better latency profile.** The agent trusts the Guardian (which it already does, by definition of Guardian). The merchant trusts the Guardian to settle (same trust model as trusting Coinbase's CDP facilitator). No new trust assumptions added, one trust assumption reused.

**Proving moves off the critical path.** Steps 1-5 take maybe 200-500ms. Proving and settlement happen asynchronously, amortized across a batch. Per-transaction proving cost drops to 1/N. This is what makes sub-cent micropayments economically viable.

**Guardian is doing 3 jobs at once.** Mandate enforcement (already), payment verification (new), batch settlement (new). For users who already have a Guardian, the facilitator role comes for free. For Guardian operators, this is a new revenue stream that fits the "custody economics without custody liability" framing of Phase 1b.

**Privacy doesn't degrade.** Guardian sees its users' transactions (it would anyway). Merchant sees `{note_id, inclusion_proof}` post-batch. No facilitator-as-privacy-leak issue that exists when a third party like Coinbase facilitates across all merchants.

### Competition

Right now our honest benchmark story is "Miden settles privately in 5-10s, Tempo in ~500ms, Base in ~2-4s." That's cool, but we can do better.

With Guardian-as-facilitator the story becomes "Miden settles privately with sub-second perceived latency. Faster and more private than base and tempo." That sounds better if we make it happen :).

### Open question

**1. Nullifier reservation for verify-before-prove.** To prevent the Guardian from verifying two transactions that consume the same input note, the Guardian maintains an in-memory `reserved_nullifiers` set:

- At verify time, compute the nullifiers the note would produce
- If any are already in the reserved set, reject (agent is trying to double-spend in the pending window)
- Otherwise add them to the reserved set and continue
- On batch settlement success, nullifiers move from "reserved" to spent (and are now on-chain via the batch tx)
- On batch settlement failure or timeout, release the reservations

This is structurally the same mechanism [`EIP-3009`](https://eips.ethereum.org/EIPS/eip-3009) (the basis for `x402`) uses with nonces, just enforced at the Guardian rather than the contract. Since the Guardian co-signs anyway, it's the natural serialization point for the agent's pending transactions.

Does that make sense? (@bobbinth, @partylikeits1983, @ermvrs, @Mirko-von-Leipzig )

### Plan
**For the next 4 weeks:** Miden team / Digine Labs operates one Guardian with a facilitator module as the reference implementation. Single endpoint (e.g. `facilitator.miden.io`), branded as Miden. Merchants integrate against it the same way they integrate against Coinbase's CDP.

**For year 1:** package the facilitator module as a drop-in component that existing x402 facilitator operators (PayAI, Dexter, x402.rs) can integrate to add Miden as a supported chain. They already run facilitator infrastructure across Base, Solana, Polygon. Adding Miden is a module, not a new operation. This is how we get to facilitator diversity without asking anyone to stand up a Guardian.