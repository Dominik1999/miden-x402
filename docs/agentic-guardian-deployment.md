# Deploying the agentic-guardian

The `agentic-guardian` binary implements
[`ideas/NEW_DESIGN.md`](../ideas/NEW_DESIGN.md): high-throughput
verify-before-prove + batched settlement + AP2 mandate gating + per-agent
pending state. It is a **separate process** from any OZ Guardian
deployment (NEW_DESIGN §139 — different operational profiles).

## Branch context

This documentation lives on the `feat/agentic-guardian` branch, which
starts from M8 (`5018000`) and adds the agentic flow alongside the
existing public / private / guardian-fast variants. The M8 facilitator
(`miden-x402-facilitator`) is **preserved** for the existing variants;
the agentic-guardian is a parallel binary for the new variant only.

To compare branches:

```bash
git log --oneline feat/agentic-guardian...main
```

## Prerequisites

- Rust 1.93+ (matches inicio-labs MultiSig and OZ Guardian workspaces).
- `protoc` 25.x for the Miden node-proto build script.
- Access to a Miden node RPC + a Miden remote prover.
- Postgres 14+ (Diesel migrations target Postgres-only syntax; the
  default config falls back to in-memory storage for development).

## Build

```bash
cargo build --release -p agentic-guardian --bin agentic-guardian
```

Binary at `target/release/agentic-guardian`.

## Configuration

The binary reads `base_config.ron` (path overridable via
`MIDENX402_CONFIG_FILE`). Every nested field can be overridden via
`MIDENX402_<SECTION>__<FIELD>` env vars — see
[`crates/agentic-guardian/base_config.ron`](../crates/agentic-guardian/base_config.ron):

```ron
Config(
    app: AppConfig(
        listen: "0.0.0.0:8080",
        network_id: "miden:testnet",
        cors_allowed_origins: ["*"],
    ),
    db: DbConfig(
        db_url: "postgres://agentic:agentic_password@localhost:5432/agentic_guardian",
        max_conn: 10,
    ),
    miden: MidenConfig(
        node_url: "https://rpc.testnet.miden.io:443",
        remote_prover_url: "https://prover.testnet.miden.io:443",
        store_path: "/var/agentic-guardian/store",
        keystore_path: "/var/agentic-guardian/keystore",
        timeout: "30s",
    ),
    batch: BatchConfig(
        max_batch_size: 16,
        max_batch_age: "750ms",
        tick_interval: "100ms",
    ),
    mandate: MandateConfig(
        challenge_ttl: "120s",
        reservation_ttl: "60s",
    ),
)
```

Override examples:

```bash
export MIDENX402_APP__LISTEN=0.0.0.0:8080
export MIDENX402_DB__DB_URL="postgres://..."
export MIDENX402_MIDEN__NODE_URL=https://rpc.testnet.miden.io:443
export MIDENX402_BATCH__MAX_BATCH_SIZE=32
```

## Database setup

```bash
# from the workspace root
diesel migration run --database-url $MIDENX402_DB__DB_URL --migration-dir crates/agentic-guardian/migrations
```

The single initial migration creates all seven tables: `agents`,
`mandates`, `pending_states`, `reservations`, `batch_queue`,
`challenges`, `mandate_counters`.

## Run

```bash
target/release/agentic-guardian
```

Sanity checks:

```bash
$ curl -s http://localhost:8080/x402/health
{"status":"ok"}

$ curl -s http://localhost:8080/x402/supported
{"kinds":[{"x402Version":2,"scheme":"exact","network":"miden:testnet","settlement":"agentic"}],"extensions":["miden-agentic-guardian"]}
```

## HTTP endpoints

Two route groups on the same port.

### Agent-client facing (`/agentic/*`)

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/agentic/register` | One-time agent + AP2 mandate registration. |
| `POST` | `/agentic/submit` | Hot-path: verify signed-unproven tx + reserve nullifiers + advance pending state. Returns `(queued_id, new_pending_state_commitment)`. |
| `GET` | `/agentic/status/{queued_id}` | Current status of a queued tx (queued / submitted). |
| `GET` | `/agentic/pending_state/{agent_id}` | Agent's currently-tracked pending state (for client crash recovery). |

### Merchant-facing (`/x402/*`)

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/x402/challenge` | Issue a server-generated `serial_num` for the merchant's 402 response. |
| `POST` | `/x402/verify` | Verify an agentic payment without enqueueing. |
| `POST` | `/x402/settle` | Verify + enqueue the agentic payment for batch settle. |
| `GET` | `/x402/supported` | Declare the agentic kind. |
| `GET` | `/x402/health` | Liveness probe. |
| `GET` | `/x402/pubkey` | (TODO) Facilitator receipt-signing pubkey. |

## Implementation status on this branch

This branch ships the **architectural skeleton** — the entire crate
compiles and the test suite passes (100 tests across the workspace).
Specific gaps are marked with `// TODO:` comments and called out below
so the Miden team knows where to focus iteration.

| Layer | Status |
|---|---|
| Wire types ([`miden_x402_types::Agentic*`] + `Ap2*`) | ✅ shipped + round-trip tests |
| Storage trait surface | ✅ shipped |
| Postgres backend | ⚠️ delegated to in-memory; trait surface locked, Diesel impls pending |
| Diesel migrations | ✅ committed |
| `Ap2Policy` evaluator | ✅ all four NEW_DESIGN bullets + tests |
| Per-agent pending state CAS | ✅ in-memory; Postgres impl pending |
| Reservation TTL + sweeper | ✅ in-memory; Postgres impl pending |
| Batch worker loop | ✅ drain + reservation lifecycle; prove + submit forwards to runtime stub |
| `MidenAgenticClientRuntime` (LocalSet) | ⚠️ scaffolded; needs `miden-client` + inicio-labs `miden-multisig-client` wiring to actually prove/submit |
| Hot-key Falcon verify | ⚠️ stubbed (port from M8 facilitator's `guardian/auth.rs`) |
| Cold-key mandate signature verify | ⚠️ stubbed |
| WAL recovery on boot | ✅ scaffolded; runs reservation sweep |
| Failure isolation (NEW_DESIGN §83-87) | ⚠️ scaffolded; per-tx halve-and-retry pending |
| HTTP routes | ✅ all endpoints wired |
| `miden-agentic-client` SDK | ⚠️ skeleton — sign-without-prove returns `NotImplemented` until the LocalSet wiring lands |

## What's left to wire (in priority order)

1. **Falcon hot/cold signature verification.** Both M8's
   `guardian/auth.rs` and inicio-labs's `miden-multisig-client` already
   have working Falcon-512 verify code. Port either.
2. **`MidenAgenticClientRuntime` real wiring.** Spawn a `miden-client`
   on the dedicated thread; dispatch `ProposeTx`, `ExecuteTx`,
   `SubmitProvenBatch` messages against it. The pattern is in
   [`/.reference/inicio-multisig/engine/multisig_client_runtime.rs`](../.reference/inicio-multisig/engine/multisig_client_runtime.rs).
3. **Diesel-backed storage impls.** Trait surface in
   [`storage/mod.rs`](../crates/agentic-guardian/src/storage/mod.rs) is
   locked. Replace each `MemoryXxxRepo` delegate in
   [`storage/postgres.rs`](../crates/agentic-guardian/src/storage/postgres.rs)
   with `diesel_async` queries. Reference:
   [`/.reference/inicio-multisig/store/persistence/store.rs`](../.reference/inicio-multisig/store/persistence/store.rs).
4. **`SubmitProvenBatch` wiring.** The Miden node accepts
   `TransactionBatch` via its `SubmitProvenBatch` RPC; agentic-guardian's
   batch worker assembles N proven txs into a batch and submits in one
   call.
5. **`miden-agentic-client::sign::sign_unproven_payment`** — wraps
   `MultisigClient::propose_multisig_transaction` once the
   inicio-labs git dep is added.
