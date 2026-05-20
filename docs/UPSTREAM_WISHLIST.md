# Upstream wishlist

The `guardian-facilitator` binary works against OpenZeppelin Guardian's
**existing** public API surface — no upstream PR is required to ship. But
the integration has rough edges that would smooth out with a few small
additions upstream. This doc enumerates them, in priority order, and
describes what would change in this repo after each lands.

If you can land one of these in
[`OpenZeppelin/guardian`](https://github.com/OpenZeppelin/guardian),
please file a PR linked back to here so we can update the impl.

---

## PR 1 — `pub fn server::api::build_router(...)`

### What

Lift the inline router construction from `ServerHandle::run()` (in
[`crates/server/src/builder/handle.rs`](../../guardian/crates/server/src/builder/handle.rs))
into a `pub fn` on the `server::api` module:

```rust
pub fn build_router(
    state: AppState,
    cors: Option<CorsLayer>,
    body_limit: BodyLimitConfig,
    rate_limit: RateLimitConfig,
) -> axum::Router {
    // current inline router building, lifted into a pub fn
}
```

Roughly a 30-line PR; the inline code already exists.

### Why

Today our binary
([`bin/guardian_facilitator.rs`](../crates/miden-x402-facilitator/src/bin/guardian_facilitator.rs))
manually enumerates Guardian's routes:

```rust
let guardian_router = Router::new()
    .route("/configure", post(configure))
    .route("/delta", post(push_delta).get(get_delta))
    .route("/delta/since", get(get_delta_since))
    .route("/delta/proposal", post(push_delta_proposal).get(get_delta_proposals).put(sign_delta_proposal))
    .route("/delta/proposal/single", get(get_delta_proposal))
    .route("/state", get(get_state))
    .route("/state/lookup", get(lookup))
    .route("/pubkey", get(get_pubkey))
    .with_state(guardian_state);
```

This drifts every time OZ adds a Guardian route, changes a middleware
layer, or tweaks rate-limit defaults.

### What would change after merge

`bin/guardian_facilitator.rs` collapses the 9-line route enumeration to:

```rust
let guardian_router = server::api::build_router(
    guardian_state,
    /* cors */ None,
    /* body_limit */ BodyLimitConfig::from_env(),
    /* rate_limit */ RateLimitConfig::from_env(),
);
```

Operators get Guardian's rate limiting + body-size limits applied to
Guardian routes for free (currently absent in our binary — see "Caveats"
below).

---

## PR 2 — `pub fn AckRegistry::sign_digest(...)`

### What

Expose [`AckRegistry::sign_with_server_key`](../../guardian/crates/server/src/ack/mod.rs)
(currently `pub(crate)`) as a `pub` wrapper that takes an arbitrary
`Word` digest:

```rust
impl AckRegistry {
    pub fn sign_digest(
        &self,
        scheme: &SignatureScheme,
        digest: Word,
    ) -> Result<Signature> {
        self.sign_with_server_key(scheme, digest)
    }
}
```

### Why

Today the facilitator runs a **second** Falcon-512 keypair (separate from
Guardian's ack key) just to sign settle receipts. See
[`crates/miden-x402-facilitator/src/receipt.rs`](../crates/miden-x402-facilitator/src/receipt.rs).

This is the consequence of `sign_with_server_key` being `pub(crate)` —
we cannot ask Guardian's ack key to sign `(payer, queued_id, network)`
from outside `guardian-server`. The high-level `ack_delta` API only signs
canonical `DeltaObject.new_commitment`.

Trade-offs of the current two-key arrangement:

- Merchants cache **two** pubkeys per Guardian operator (one for delta
  acks via `GET /pubkey`, one for x402 receipts via `GET /x402/pubkey`).
- We carry ~150 lines of keystore boilerplate (`FilesystemKeyStore`,
  `FacilitatorKeyStore` trait, `ReceiptSigner::load_or_generate`,
  bootstrap logic in the binary).
- Key rotation requires deleting two files instead of one.

### What would change after merge

- [`receipt.rs`](../crates/miden-x402-facilitator/src/receipt.rs) shrinks
  to a thin wrapper around `AckRegistry::sign_digest`. No `SecretKey`
  field, no `load_or_generate`.
- `GET /x402/pubkey` collapses into a thin proxy to Guardian's
  `GET /pubkey` (or we drop the route entirely and document that
  merchants should fetch from `/pubkey`).
- `FacilitatorKeyStore` trait and the `keystore/` filesystem path
  disappear.
- The bootstrap step "load or generate receipt key" comes out of the
  binary.
- Net deletion: ~150 lines.

The trust model from the merchant's side is **functionally identical**;
the only difference is one pubkey instead of two.

---

## PR 3 — `check_nullifiers` wiring on the verify path

### What

Not strictly an upstream PR — just a wire-up task in this repo. But
calling it out alongside the upstream items because the seam is in place
and the missing piece is just plumbing.

Replace [`NoopNullifierBackstop`](../crates/miden-x402-facilitator/src/bin/guardian_facilitator.rs)
in the binary with a real implementation that calls
`miden_rpc_client::MidenRpcClient::check_nullifiers(...)`.

### Why

The reservation set protects against in-flight double-spends within the
facilitator's pending window. The `check_nullifiers` backstop is the
guard against replays of **already-settled-and-included** transactions
(where the reservation has long since expired).

### What would change after merge

The verify path catches one more failure mode (`FacilitatorError::AlreadyConsumed`,
`HTTP 409`). No structural changes to anything else.

---

## PR 4 — Inclusion-bridge

### What

Wire the facilitator's reservation promotion lifecycle to Guardian's
canonicalization worker. When the worker observes that a candidate delta
has landed on chain (`DeltaStatus::Candidate → Canonical`), promote the
corresponding reservation in the facilitator's `ReservationRepo` to
"consumed" and let the reservation TTL go away (it's no longer needed).

Today the reservation TTL is the only ceiling — see
[`docs/deploy.md`](./deploy.md) § "Operational notes."

### Why

Without this bridge, reservations either:

- Fall out of the in-memory set when their TTL elapses (which is the
  current default 60s — racy for slow chains).
- Have to be set very long (5+ minutes) to be safe, blowing up the
  in-memory state for high-throughput facilitators.

### What would change after merge

- A new background task in the binary that subscribes to
  canonicalization events and promotes/clears reservations.
- `MIDEN_X402_RESERVATION_TTL_SECS` becomes purely a defensive
  fallback; the canonical signal is on-chain inclusion.

This needs either (a) Guardian to expose a hook on the canonicalization
worker (currently `start_canonicalization_worker` doesn't take a
callback), or (b) the facilitator to poll Guardian's storage for delta
status transitions. Option (a) is the cleaner upstream PR.

---

## What stays the same regardless of upstream changes

These don't change whether the PRs above land or not:

- Wire format (`miden-p2id-private` scheme, `extra.noteTag` +
  `extra.serialNum`, the three HTTP headers, the four `/x402/*`
  routes).
- The verify pipeline (decode + bind + sign-check + balance + mandate
  + reserve).
- The batch settle worker and its config knobs.
- The merchant SDKs' interface.
- The mandate hook.

The PRs are quality-of-life improvements, not protocol changes.

---

## Other improvements not pinned to upstream

These could be done in this repo without OZ involvement:

- **Node agent SDK.** Building a signed-unproven `TransactionInputs`
  from JS requires `@miden-sdk/miden-sdk` to expose a "build + sign + STOP
  before proving" seam. Currently the WASM SDK can prove + submit but
  not stop in the middle. Tracked in
  [`sdks/node/packages/agent/src/payer.ts`](../sdks/node/packages/agent/src/payer.ts).
- **Postgres backends for the four x402 repos.** The trait shape is
  ready (`ChallengeRepo`, `ReservationRepo`, `BatchQueueRepo`,
  `FacilitatorKeyStore`); the impls would parallel Guardian's
  Postgres-vs-filesystem split.
- **A concrete mandate policy.** See [`docs/mandate.md`](./mandate.md).
