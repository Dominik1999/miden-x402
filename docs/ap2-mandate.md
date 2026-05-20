# AP2 mandate

Per [`ideas/NEW_DESIGN.md`](../ideas/NEW_DESIGN.md) §15-19, §42-44: when
the user sets up an agent account, they sign an **AP2 mandate** with
their cold key. The mandate is stored at the agentic-guardian and
enforced on every incoming `POST /agentic/submit`.

## Schema

Defined in [`crates/miden-x402-types/src/mandate.rs`](../crates/miden-x402-types/src/mandate.rs):

```rust
pub struct Ap2Mandate {
    pub mandate_id: String,
    pub agent_account_id: AccountIdHex,
    pub amount_cap_per_tx: u64,         // bullet 1
    pub merchant_allowlist: Vec<AccountIdHex>, // bullet 2 (by payTo)
    pub time_window_secs: u64,          // bullet 3
    pub daily_total_cap: u64,           // bullet 4
    pub issued_at_unix_secs: u64,
    pub expires_at_unix_secs: u64,
}
```

The user wraps it with their cold-key Falcon-512 signature:

```rust
pub struct Ap2SignedMandate {
    pub mandate: Ap2Mandate,
    pub user_signature_b64: String,
    pub user_pubkey_b64: String,
}
```

The signed message is `Rpo256::hash(mandate.canonical_bytes())`. The
canonical encoding is documented inline in
[`mandate.rs`](../crates/miden-x402-types/src/mandate.rs).

## Enforcement

Implemented in [`crates/agentic-guardian/src/mandate/ap2.rs`](../crates/agentic-guardian/src/mandate/ap2.rs)
as `Ap2Policy::evaluate`. Each incoming tx runs through these checks in
order, fail-fast:

1. **Expiry.** `now_unix_secs < mandate.expires_at_unix_secs`.
2. **Per-tx amount cap.** `payload.amount ≤ amount_cap_per_tx`. Error:
   `MandateSchemaError::AmountCapExceeded`.
3. **Merchant allowlist.** `requirements.payTo ∈ merchant_allowlist`.
   Error: `MandateSchemaError::MerchantNotAllowed`.
4. **Rolling time window.** Sum of (this tx + all txs in the last
   `time_window_secs`) must not exceed `daily_total_cap × (window_secs /
   86_400)`. Error: `MandateSchemaError::TimeWindowExceeded`. For
   window ≥ 1 day the cap is `daily_total_cap` directly (the daily
   check below becomes redundant in that case).
5. **24h daily total.** Sum of txs in the last 86 400 s must not exceed
   `daily_total_cap`. Error: `MandateSchemaError::DailyTotalExceeded`.

On success the evaluator increments the per-agent counter:

```sql
INSERT INTO mandate_counters (agent_account_id, window_start_unix_secs, total_amount)
VALUES ($agent, floor_minute($now), $amount)
ON CONFLICT (agent_account_id, window_start_unix_secs)
  DO UPDATE SET total_amount = mandate_counters.total_amount + EXCLUDED.total_amount;
```

Window queries sum across buckets in the lookback range. Bucketing by
minute keeps the table small even at high throughput.

## Cold-path delegation

When a payment exceeds the mandate (e.g. the user wants to send more
than `amount_cap_per_tx` once), the agentic-guardian refuses to
cosign. The user falls back to the cold-key path through a
**separately deployed** OZ Guardian instance (NEW_DESIGN §139): submit
the tx via `POST /delta/proposal` against the OZ Guardian, the user
signs with the cold key, OZ Guardian co-signs as a second cosigner.

The agentic-guardian and the cold-path OZ Guardian are two different
processes with different operational profiles (high-frequency vs
low-frequency, high-volume vs high-value). This branch ships the
agentic-guardian; the cold-path OZ Guardian is the off-the-shelf
[OpenZeppelin/guardian](https://github.com/OpenZeppelin/guardian)
running unmodified.

## Implementation status on this branch

- ✅ Schema + canonical encoding ([`miden_x402_types::Ap2Mandate`]).
- ✅ Evaluator with the four bullets + counters
  ([`Ap2Policy`](../crates/agentic-guardian/src/mandate/ap2.rs)).
- ✅ Per-agent counter store ([`MandateCounterRepo`](../crates/agentic-guardian/src/storage/mod.rs)).
- ⚠️ User signature verification is **stubbed** —
  [`crates/agentic-guardian/src/auth/cold_key.rs`](../crates/agentic-guardian/src/auth/cold_key.rs)
  returns `Ok(())` until the Falcon-512 + Rpo256 wiring is plumbed.
  Schema + dispatch are in place; only the crypto call is missing.

## Why no commitment helper in `miden-x402-types`

`Ap2Mandate::canonical_bytes()` returns deterministic bytes. Hashing
to a `Word` via `Rpo256` is delegated to the consumer
(agentic-guardian + miden-agentic-client) to keep
`miden-x402-types` free of Miden-crypto deps — it stays a tiny crate
suitable for embedding in JS/Python ports.
