# Approach 2: Agentic Guardian (Hot-Key + Mandates) — Status

## Overview

Extends Approach 1 (Guardian verify-before-prove) with AI agent hot keys and AP2 spending mandates. Agent registers with hot key (Falcon) + cold key, the Agentic Guardian enforces per-tx caps, merchant allowlists, rolling time-window caps, and daily total limits before acking the payment.

## What's Done

- **Two new crates**: `agentic-guardian` (mandate policy + enrollment server), `miden-agentic-client` (agent SDK)
- **AP2 Mandate evaluation**: 4-tier gating (expiry, per-tx cap, merchant allowlist, rolling window + daily total) — implemented and unit-tested
- **Pending state tracking**: client-side optimistic state tracker (advance + rollback) — implemented and tested
- **Wire types**: `PaymentPayload` has `"agentic"` variant with `hotSignature`, `pendingStateCommitment`, `mandateId`
- **Documentation**: deployment guide, AP2 mandate spec, smoke test instructions
- **Smoke testnet binary**: `miden-x402-smoke-testnet` for manual operator runs

## What's Missing (Skeleton/Stub Code)

### 1. Hot-key Falcon signature verification
**File**: `crates/agentic-guardian/src/auth/hot_key.rs`
- Currently returns `Ok(())` — stub
- Need: port real Falcon-512 signature verification from `miden-crypto` against `TransactionSummary.to_commitment()`
- Blocked on: matching the exact same signature format as `guardian-shared::SignatureScheme::Falcon`

### 2. Cold-key mandate registration verification
**File**: `crates/agentic-guardian/src/auth/cold_key.rs`
- Currently returns `Ok(())` — stub
- Need: verify Falcon signature over `Ap2Mandate.canonical_bytes()` via `Rpo256::hash()`

### 3. Sign-without-prove (agent SDK)
**File**: `crates/miden-agentic-client/src/sign.rs`
- Currently returns `NotImplemented`
- Need: wire `miden-multisig-client` as dependency, call `execute_for_summary()` → sign → package
- Same prerequisite as Approach 1: MultisigGuardian account + signature advice format

### 4. Batch proving + submit
**File**: `crates/agentic-guardian/src/runtime.rs` (`MidenRuntimeHandle`)
- `SubmitProvenBatch` handler is a skeleton
- Need: call `RemoteTransactionProver::prove()` and `SubmitProvenBatch` RPC
- Depends on: `miden-remote-prover-client` (already imported but unused)

### 5. E2E testnet test
No automated e2e test exists. Need:
1. All 4 stubs above implemented
2. Test flow: register agent with AP2 mandate → 5 payments within mandate → verify 6th is rejected → merchant batch settlement
3. Same infrastructure as Approach 1 (MultisigGuardian account, execute_for_summary, etc.)

## Dependencies on Approach 1

Approach 2 builds on top of Approach 1. Once Approach 1's MultisigGuardian signature advice format is solved, the same solution applies here. The additional work for Approach 2 is:
- AP2 mandate enforcement (already implemented)
- Hot/cold key verification (stubs need real crypto)
- Agent SDK sign-without-prove (same as Approach 1 + mandate ID)

## Key Files

| Component | File | Status |
|-----------|------|--------|
| Hot-key auth | `crates/agentic-guardian/src/auth/hot_key.rs` | Stub |
| Cold-key auth | `crates/agentic-guardian/src/auth/cold_key.rs` | Stub |
| AP2 Mandate | `crates/agentic-guardian/src/mandate/ap2.rs` | Working + tests |
| Pending state | `crates/miden-agentic-client/src/pending_state.rs` | Working + tests |
| Sign-without-prove | `crates/miden-agentic-client/src/sign.rs` | NotImplemented |
| Batch worker | `crates/agentic-guardian/src/runtime.rs` | Skeleton |
| Smoke test | `src/bin/miden-x402-smoke-testnet.rs` | Manual, real testnet |
