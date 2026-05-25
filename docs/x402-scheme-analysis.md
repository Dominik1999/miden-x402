# x402 Scheme Analysis: `exact`, `upto`, and `batch-settlement` on Miden

## 1. Scheme Definitions (from the x402 spec)

### `exact`
Fixed-price payment. The buyer authorizes exactly the advertised amount. One
payment per HTTP request. On EVM this uses EIP-3009 `transferWithAuthorization`
or Permit2: the buyer signs a transfer for a specific `(from, to, value, nonce,
validAfter, validBefore)`, the facilitator verifies the signature off-chain,
then executes the on-chain transfer during or after the request.

### `upto`
Usage-based payment. The buyer signs an authorization for *up to* X tokens. The
server determines the actual charge after serving the request (actual <= max).
Settlement includes the real amount, not the authorized maximum. On EVM this
uses the same signature primitives but the settlement call passes
`actualAmount`.

### `batch-settlement`
High-throughput micropayment scheme. The buyer commits funds once, then issues
signed cumulative vouchers per request (off-chain). The merchant verifies
locally. Settlement happens asynchronously when the merchant redeems the latest
voucher. Multiple requests share a single on-chain settlement.

---

## 2. Can We Implement `exact` on Miden?

**Yes.** The core mechanic — "buyer signs a one-shot transfer authorization,
facilitator verifies off-chain, settles on-chain" — maps cleanly to Miden.

### Option A: P2ID via Guardian (Approach 2 / `feat/guardian-verify-before-prove`)

This is the closest existing match. The agent:

1. Builds a `TransactionRequest` with a single P2ID output note paying the
   merchant exactly the requested amount.
2. Runs `execute_for_summary()` locally (~13ms) to get a `TransactionSummary`.
3. Signs `summary.to_commitment()` with its Falcon hot key (~2ms).
4. Sends the signed summary + tx request to the merchant (in the
   `Payment-Signature` header).

The merchant relays to the facilitator, which:

1. Verifies the Falcon signature against the registered hot key.
2. Validates the P2ID output (script root, recipient, asset, amount).
3. Acks immediately (pre-finality guarantee).
4. Proves and submits the transaction asynchronously.

**This is `exact`.** The amount in the P2ID note is precisely the advertised
price. The facilitator cannot change it — the `TransactionSummary` is
cryptographically bound to that specific output. The agent authorized exactly
one transfer of exactly that amount.

**What's missing to call it a spec-compliant `exact` binding:**

- Wire format: rename `scheme` from `"miden-p2id-x402"` to `"exact"` and add
  `"network": "miden:testnet"` in the standard x402 `PaymentRequirements`.
- Nonce / replay protection: EVM `exact` uses a nonce in the signed message.
  Our equivalent is the nullifier — each `TransactionSummary` produces a unique
  nullifier, and the facilitator's reserved-nullifier set prevents replay.
- `validAfter` / `validBefore`: map to block-height ranges. The agent can
  encode a deadline in the `x402_context`; the facilitator enforces it.

**Effort:** ~1 week on top of the existing `feat/guardian-verify-before-prove`
branch. Mostly wire-format alignment + spec documentation.

### Option B: AgentDebitNote single-debit (Approach 1 variant)

We could use the ADN consume path for a single payment: agent signs a debit
message for the exact amount, facilitator consumes the note, creates a P2ID to
the merchant for that amount plus a remainder note.

This works but is overkill for `exact` — the ADN's strength is reusability
(multiple debits from one note), which `exact` doesn't need. It also requires
a prefund step that locks capital, whereas `exact` should be as lightweight as
possible.

**Not recommended for `exact`.**

---

## 3. Can We Implement `upto` on Miden?

**Yes, but it requires more design work.** The challenge: the buyer signs an
authorization for amount X, but the server charges Y <= X. On EVM this is
straightforward because the settlement contract accepts an `actualAmount`
parameter. On Miden, the amount is baked into the note at creation time.

### Option A: Two-phase with facilitator-determined amount

1. Agent signs an `upto` authorization: a Falcon signature over
   `(serial_num, merchant, maxAmount)`.
2. Server processes the request, determines actual usage = Y.
3. Server tells the facilitator: "settle for Y" (where Y <= maxAmount).
4. Facilitator builds the P2ID note for Y (not maxAmount).

**The problem:** the agent's signature covers `maxAmount`, but the on-chain
note is for `Y`. We need a note script or transaction pattern where:

- The agent's signature authorizes *up to* X.
- The actual settlement amount Y <= X is chosen later by the facilitator.
- The facilitator cannot exceed X.

**Miden implementation — ADN with variable debit:**

The AgentDebitNote already supports this pattern naturally. The consume path
takes `cumulativeAmount` as a note argument, and the agent's signature covers
`(serial_num, merchant, cumulativeAmount)`. For `upto`:

1. Agent pre-signs a voucher for `maxAmount`.
2. Server determines `actualAmount`.
3. Facilitator uses `actualAmount` as the cumulative amount in the consume
   transaction — but this won't verify because the signature covers
   `maxAmount`, not `actualAmount`.

**This doesn't work directly.** The signature is bound to the exact amount.

### Option B: Modified note script with max-amount authorization

Create a variant note script where:

- The agent signs `(serial_num, merchant, maxAmount)`.
- The note script verifies the signature against `maxAmount`.
- The note script accepts `actualAmount` as a note argument.
- The script enforces `actualAmount <= maxAmount` (both are on the stack).
- P2ID output is created for `actualAmount`; remainder gets `balance -
  actualAmount`.

This is a new MASM script — call it `AgentUptoNote`. The signature message
format changes to include the max, while the note args carry the actual:

```
note_args: [actualAmount, maxAmount, 0, 0]
signature covers: merge(serial_num, [merchant_suffix, merchant_prefix, maxAmount, 0])
script enforces: actualAmount <= maxAmount
P2ID output:     actualAmount to merchant
remainder:       balance - actualAmount
```

The facilitator (or merchant) chooses `actualAmount` at settlement time. The
agent's signature remains valid for any amount up to `maxAmount`. The on-chain
script enforces the cap.

**Effort:** ~2 weeks.

- ~3 days: new MASM note script (`agent_upto_note.masm`) with the
  `actualAmount <= maxAmount` check.
- ~3 days: Rust types, wire format, client-side signing for `upto` scheme.
- ~3 days: facilitator endpoint changes (accept `maxAmount` from agent,
  `actualAmount` from server).
- ~3 days: tests (MockChain + testnet).

### Option C: Guardian P2ID with facilitator-chosen amount

With the Guardian approach, the facilitator builds the transaction, so it can
choose any amount. But the agent's signature covers a specific
`TransactionSummary` with a specific P2ID output amount. The facilitator
cannot change the amount without invalidating the signature.

To support `upto` with Guardian:

1. Agent does NOT sign a specific transaction. Instead, agent signs an
   "authorization envelope": `(merchant, maxAmount, nonce, expiry)`.
2. Facilitator builds the P2ID transaction for `actualAmount` itself.
3. Facilitator uses its own authority (as Guardian co-signer) to execute
   the transaction.

**This changes the trust model significantly.** The agent is no longer
authorizing a specific transaction — it's delegating spend authority up to a
cap. This is closer to the AP2 mandate model from `feat/agentic-guardian`.

**Not recommended as the primary `upto` path** — the ADN-based approach
(Option B) is simpler and doesn't change the trust model.

---

## 4. Mapping Existing Variants to Schemes

| Variant | Branch | Closest x402 Scheme | Notes |
|---------|--------|-------------------|-------|
| ADN batch-settlement | `main` | `batch-settlement` | Direct match. Cumulative vouchers, single settlement. This IS the batch-settlement Miden binding. |
| ADN dual-sig | `feat/adn-dual-sig` | `exact` (per-payment variant) | Each payment is independently authorized (agent + facilitator co-sign). One on-chain tx per payment. Functionally `exact` but uses ADN note rather than P2ID. |
| Guardian verify-before-prove | `feat/guardian-verify-before-prove` | `exact` | Closest to `exact`. Agent signs a specific P2ID transaction for the exact amount. Facilitator verifies and settles. |
| Agentic Guardian | `feat/agentic-guardian` | `upto` (with AP2 mandates) | AP2 mandates define spending caps. With mandate enforcement, the facilitator could settle for <= authorized amount. Currently stubs only. |

---

## 5. Implementation Effort Summary

| Scheme | Best Miden Pattern | Base Branch | Effort | Status |
|--------|-------------------|-------------|--------|--------|
| `batch-settlement` | ADN cumulative vouchers | `main` | Done | Fully working, tested on testnet |
| `exact` | Guardian P2ID (sign-without-prove) | `feat/guardian-verify-before-prove` | ~1 week | Wire format alignment, spec compliance. Core flow already works. |
| `exact` (alt) | ADN single-debit | `main` | ~1 week | Possible but less natural than Guardian P2ID. |
| `upto` | New `AgentUptoNote` MASM script | New branch from `main` | ~2 weeks | New note script with max-amount authorization + actual-amount settlement. |
| `upto` (alt) | AP2 mandate + Guardian | `feat/agentic-guardian` | ~3-4 weeks | Requires completing AP2 mandate enforcement + facilitator-built transactions. Higher trust assumptions. |

---

## 6. Recommendation

### Priority order

1. **`batch-settlement` — already done.** This is our strongest differentiator.
   No other chain can do private, zero-gas micropayments with off-chain
   vouchers and atomic on-chain settlement. Ship this as the primary Miden
   network binding.

2. **`exact` — do next, ~1 week.** The Guardian P2ID path
   (`feat/guardian-verify-before-prove`) is 80% there. Aligning the wire
   format to the x402 `exact` spec is straightforward. Having both
   `batch-settlement` and `exact` makes Miden a complete x402 network —
   merchants can choose per-request settlement (`exact`) or high-throughput
   batching (`batch-settlement`) depending on their use case.

3. **`upto` — do later, ~2 weeks.** Usage-based pricing is important for AI
   API providers (token-based billing, metered compute). The `AgentUptoNote`
   approach is clean and doesn't change trust assumptions. But it requires a
   new MASM script and end-to-end testing. Defer until after `exact` ships.

### Is `batch-settlement` sufficient alone?

For the x402 launch: **yes.** Batch-settlement covers the primary use case
(agent making many small payments to a single merchant). Most x402 deployments
today use `exact` on EVM, but that's because EVM has no good batch primitive.
On Miden, batch-settlement is strictly better for repeated payments.

However, `exact` is needed for:
- One-off purchases (no session setup overhead).
- Interoperability with merchants that only implement `exact`.
- The x402 spec's `PaymentRequirements.accepts` array — merchants can
  advertise both `exact` and `batch-settlement`, and the agent picks the
  best one.

And `upto` is needed for:
- Token-based AI billing (pay up to 10k tokens, actual charge based on usage).
- Metered APIs where the cost isn't known until after serving.
- Any variable-pricing scenario.

### Bottom line

Ship `batch-settlement` now. Add `exact` in a fast follow (~1 week). Plan
`upto` for the release after that (~2 weeks). All three schemes map to Miden
primitives we already have or can build with small extensions to the existing
codebase.
