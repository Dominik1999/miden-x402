# Mandate policy

DESIGN.md frames the Guardian as doing three jobs at once: "mandate
enforcement (already), payment verification (new), batch settlement
(new)." In practice no mandate concept exists in OpenZeppelin Guardian
today, and there is no canonical mandate schema for agent payments.

This crate defines the **shape** of mandate enforcement (a trait +
context object) and ships an `AllowAll` default so the verify path is
operational from day one. Real policies plug in via
`MIDEN_X402_MANDATE_POLICY` at the binary boundary.

## The trait

[`crates/miden-x402-facilitator/src/mandate.rs`](../crates/miden-x402-facilitator/src/mandate.rs)

```rust
pub trait MandatePolicy: Send + Sync + 'static {
    fn evaluate(&self, ctx: &MandateContext<'_>) -> Result<(), MandateError>;
}

pub struct MandateContext<'a> {
    pub buyer: &'a AccountIdHex,
    pub resource_url: Option<&'a str>,
    pub asset: &'a AccountIdHex,
    pub amount: &'a str,
    pub requirements: &'a MidenPaymentRequirements,
}

pub enum MandateError {
    Rejected { reason: String },
    EvaluationFailed(String),
}
```

The verify path calls `mandate.evaluate(...)` after signature + binding
checks and before nullifier reservation. Failures map to:

- `Rejected { reason }` → HTTP `400 Bad Request`, `ErrorReason::InvalidPaymentAmount`.
- `EvaluationFailed(_)` → HTTP `503 Service Unavailable`,
  `ErrorReason::UnexpectedError`.

## The default policy

`AllowAll` passes every payment. Suitable for the reference deployment
and for any case where the merchant or agent operator runs its own
mandate gate upstream.

## Plugging in a real policy

The current binary's selector is wired for `allow-all` only. To add a
new policy:

1. Implement `MandatePolicy` for your type in a new module (e.g.
   `src/mandate_policies/amount_cap.rs`).
2. Extend [`config::MandatePolicyConfig`](../crates/miden-x402-facilitator/src/config.rs)
   with a new variant (e.g. `AmountCap { ... }`).
3. Extend [`bin/guardian_facilitator.rs`](../crates/miden-x402-facilitator/src/bin/guardian_facilitator.rs)
   to construct the policy from the config variant.

A real policy you might implement:

- **Per-account amount cap per `(faucet_id, time_window)`.** Reject if
  the buyer's spend over the last 24h on this asset would exceed a
  configured cap. Backing storage: a small per-buyer counter store
  (filesystem-backed, like the other repos).
- **Merchant allowlist.** Reject if `requirements.payTo` is not in a
  per-buyer-account allowlist. Backing storage: read from Guardian's
  account metadata `extra` field (Guardian supports opaque extra
  metadata on `AccountMetadata`).

Both examples need additional storage; both are out of scope for the
v1 facilitator but the trait is shaped to accept them without changing
the verify path.

## Why no concrete policy ships with v1

DESIGN.md calls mandate enforcement "already" — but it is not in OZ
Guardian today, and the agent-mandate ecosystem hasn't yet converged on
a canonical schema. Shipping a placeholder policy and locking in a
schema would constrain future work; shipping the trait + `AllowAll`
default lets operators add policies without churning the verify path.
