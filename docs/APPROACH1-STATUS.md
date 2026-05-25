# Approach 1: Guardian Verify-Before-Prove — Status

## Overview

Agent has a MultisigGuardian account (threshold=1). On each payment the agent builds a P2ID transaction, executes locally WITHOUT proving to get a `TransactionSummary`, signs it with Falcon, and sends the signed summary to the facilitator. The facilitator re-builds the transaction WITH the signature injected as advice, proves it, and submits to chain.

## What's Done

- **Workspace deps**: miden-client (git main), miden-client-sqlite-store, miden-confidential-contracts (MultisigGuardianBuilder), miden-multisig-client, miden-keystore — all added and compiling
- **Guardian binary fixed**: added `auditor` field (LogAuditor) for upstream compatibility
- **WIP e2e test**: `crates/miden-x402-facilitator/tests/approach1_guardian_p2id.rs` — compiles, creates MultisigGuardian account, but fails at mint note consumption

## What's Missing

### 1. MultisigGuardian signature advice format
The MultisigGuardian auth module expects signatures injected via a specific advice map format. The standard `build_consume_notes` / `TransactionRequestBuilder` doesn't produce the right request for MultisigGuardian accounts. The multisig-client has private functions that handle this:

- `miden_multisig_client::transaction::build_consume_notes_transaction_request_from_notes()` — builds a consume request with the right custom script + advice format
- `miden_multisig_client::execution::collect_signature_advice()` — collects signatures into the right advice entries

**Fix options:**
1. Upstream PR to make `transaction` and `execution` modules public in miden-multisig-client
2. Inline the functions into the test (they're ~50 lines each)
3. Use the full `MultisigClient` API with a running Guardian server

### 2. P2ID payment flow (after consume works)
Once the agent can consume mint notes (funding), the P2ID payment flow uses:
```
execute_for_summary(client, agent_id, unsigned_p2id_request) → Unauthorized(TransactionSummary)
→ agent signs summary.to_commitment() with Falcon
→ build_p2id_transaction_request(account, merchant, assets, salt, signature_advice)
→ submit_new_transaction(agent_id, signed_request) → proves + submits
```
This pattern is already implemented in the test but untested because the mint step fails.

### 3. 5 sequential payments + merchant batch settlement
Same pattern as Approach 3's `adn_x402_merchant_settlement` test — loop 5 times, collect P2ID output notes, merchant batch-consumes.

### 4. HTTP layer (optional)
Wire the sign-without-prove flow through HTTP endpoints matching the x402 spec:
- GET /resource → 402
- POST /pay with signed TransactionSummary
- Facilitator /settle endpoint

## Key APIs

| Function | Source | Purpose |
|----------|--------|---------|
| `MultisigGuardianBuilder::new(config).build()` | miden-confidential-contracts | Create MultisigGuardian account |
| `execute_for_summary(client, id, req)` | miden-multisig-client (private) | Execute → Unauthorized → get TransactionSummary |
| `build_p2id_transaction_request(account, recipient, assets, salt, advice)` | miden-multisig-client (private) | Build P2ID tx with custom send script |
| `collect_signature_advice(sigs, required, commitment)` | miden-multisig-client (private) | Format Falcon sig for MultisigGuardian advice map |
| `SignatureScheme::Falcon.build_signature_advice_entry(...)` | guardian-shared | Build individual advice entry |

## Architecture Notes

- MultisigGuardian auth fires `AUTH_UNAUTHORIZED_EVENT` when signatures are missing — this is how `execute_for_summary` extracts the TransactionSummary
- The Guardian acts as a single serialization point — prevents double-spend by reserving nullifiers before proving
- Proving is async (batch worker) — merchant gets a signed receipt immediately
- The facilitator IS the Guardian server with x402 routes merged in
