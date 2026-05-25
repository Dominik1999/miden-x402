# Batch-Settlement Spec Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align the AgentDebitNote with the x402 `batch-settlement` spec — cumulative voucher model, committed merchant in note, single-sig consume, identity-based reclaim, verify/settle HTTP endpoints.

**Architecture:** The MASM note script is simplified: remove facilitator co-signature, commit merchant_account_id in note storage (7 items: user_pk(4), merchant_suffix, merchant_prefix, reclaim_block_height). Consume path verifies a single agent signature over merge(serial_num, [merchant_suffix, merchant_prefix, cumulativeAmount, 0]). Reclaim path verifies agent signature over merge(serial_num, [user_suffix, user_prefix, 0, 0]) (unchanged). Off-chain, the agent signs cumulative vouchers; the merchant verifies locally; the facilitator settles on-chain.

**Tech Stack:** Miden MASM, Rust, miden-testing (MockChain), axum, reqwest, miden-client

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/agent-debit-note/masm/agent_debit_note.masm` | Modify | Remove facilitator PK, add merchant to storage, single-sig consume |
| `crates/agent-debit-note/src/types.rs` | Modify | New `AgentDebitNoteStorage` (7 items, merchant committed) |
| `crates/agent-debit-note/src/message.rs` | Modify | Update `debit_message` to use committed merchant, add `voucher_message` |
| `crates/agent-debit-note/src/note.rs` | Modify | Adapt to new storage layout |
| `crates/agent-debit-note/src/voucher.rs` | Create | `CumulativeVoucher` struct, sign/verify helpers |
| `crates/agent-debit-note/src/lib.rs` | Modify | Add `pub mod voucher` |
| `crates/agent-debit-note/tests/note_script.rs` | Rewrite | All MockChain tests updated for new MASM |
| `crates/agent-debit-note/tests/voucher_test.rs` | Create | Off-chain voucher sign/verify tests |
| `crates/agent-debit-note/tests/batch_settlement_e2e.rs` | Create | Full e2e: setup → 5 vouchers → settle → merchant consumes |

---

### Task 1: Create branch and update MASM

**Files:**
- Modify: `crates/agent-debit-note/masm/agent_debit_note.masm`

- [ ] **Step 1: Create branch**
```bash
git checkout main
git checkout -b feat/batch-settlement-spec
```

- [ ] **Step 2: Rewrite the MASM script**

New storage layout (7 items):
```
[0-3]  user_pub_key commitment (Word) — agent's Falcon public key
[4]    merchant_account_id_suffix
[5]    merchant_account_id_prefix
[6]    reclaim_block_height
```

New consume path (single-sig):
- Load merchant from storage (committed, not note_args)
- Note_args: `[cumulativeAmount, 0, 0, 0]`
- Message: `merge(serial_num, [merchant_suffix, merchant_prefix, cumulativeAmount, 0])`
- Verify single agent signature (no facilitator sig)
- Create P2ID(cumulativeAmount) to committed merchant
- Create remainder ADN(balance - cumulativeAmount)

Reclaim path unchanged:
- After expiry, verify agent signature over reclaim message
- Create P2ID(full_balance) to user

Replace the full MASM with:
```masm
#! AgentDebitNote — batch-settlement spec.
#!
#! Consume path (before expiry): agent sig over (serial, merchant, cumulativeAmount).
#! Creates P2ID(cumulativeAmount) to committed merchant + remainder note.
#!
#! Reclaim path (at or after expiry): agent sig over reclaim message.
#! Creates P2ID(full_balance) to user.
#!
#! Note storage (7 items at STORAGE_PTR):
#!   [0-3]  user_pub_key_commitment (Word)
#!   [4]    merchant_account_id_suffix
#!   [5]    merchant_account_id_prefix
#!   [6]    reclaim_block_height
#!
#! Note args (consume): [cumulativeAmount, 0, 0, 0]
#! Note args (reclaim): [0, 0, 0, 0]

use miden::protocol::active_note
use miden::protocol::note
use miden::protocol::output_note
use miden::protocol::tx
use miden::core::sys
use miden::core::crypto::dsa::falcon512_poseidon2
use miden::core::crypto::hashes::poseidon2
use miden::standards::wallets::basic->wallet
use miden::standards::notes::p2id

const STORAGE_PTR=100
const USER_PK_PTR=100
const MERCHANT_SUFFIX_PTR=104
const MERCHANT_PREFIX_PTR=105
const RECLAIM_BLOCK_PTR=106

const CUMULATIVE_AMOUNT=8

const ASSET_KEY_PTR=12
const ASSET_VALUE_PTR=16

@note_script
pub proc main
    # Store note_args: [cumulativeAmount, 0, 0, 0]
    mem_store.CUMULATIVE_AMOUNT
    drop drop drop

    # Load storage
    push.STORAGE_PTR exec.active_note::get_storage
    drop drop

    # Route by block height
    exec.tx::get_block_number
    mem_load.RECLAIM_BLOCK_PTR
    swap gt

    if.true
        exec.consume_path
    else
        exec.reclaim_path
    end

    exec.sys::truncate_stack
end

proc consume_path
    # 1. Compute MESSAGE = merge(serial_num, [merchant_suffix, merchant_prefix, cumulativeAmount, 0])
    push.0 mem_load.CUMULATIVE_AMOUNT mem_load.MERCHANT_PREFIX_PTR mem_load.MERCHANT_SUFFIX_PTR
    exec.active_note::get_serial_number
    exec.poseidon2::merge

    # 2. Verify agent signature (advice map lookup)
    padw push.USER_PK_PTR mem_loadw_le

    dupw.1 dupw.1
    exec.poseidon2::merge
    # => [SIG_KEY, USER_PK, MESSAGE]

    adv.has_mapkey
    adv_push.1

    if.true
        adv.push_mapval
        dropw
    else
        dropw
    end

    exec.falcon512_poseidon2::verify

    # 3. Add assets to vault
    exec.wallet::add_assets_to_account
    push.ASSET_KEY_PTR exec.active_note::get_assets drop drop

    # 4. Create P2ID(cumulativeAmount) to committed merchant
    exec.active_note::get_serial_number
    swap.3 push.1 add swap.3
    push.1 push.0
    mem_load.MERCHANT_PREFIX_PTR mem_load.MERCHANT_SUFFIX_PTR
    exec.p2id::new

    push.0.0.0 mem_load.CUMULATIVE_AMOUNT
    padw push.ASSET_KEY_PTR mem_loadw_le
    call.wallet::move_asset_to_note
    dropw dropw dropw dropw

    # 5. Create remainder AgentDebitNote
    exec.active_note::get_script_root
    exec.active_note::get_serial_number
    push.1 add
    push.7 push.STORAGE_PTR
    exec.note::build_recipient
    push.1 push.0
    exec.output_note::create

    push.ASSET_VALUE_PTR mem_load mem_load.CUMULATIVE_AMOUNT sub
    push.0.0.0 movup.3
    padw push.ASSET_KEY_PTR mem_loadw_le
    call.wallet::move_asset_to_note
    dropw dropw dropw dropw
end

proc reclaim_path
    # Message = merge(serial_num, [user_suffix, user_prefix, 0, 0])
    push.0.0
    mem_load.MERCHANT_PREFIX_PTR  # reuse these temp slots — but we need user_id
    # Actually load user_id — but we don't have it in storage anymore!
    # The spec says "caller must be user_account_id" — we keep sig-based for now
    # with user_suffix/prefix from... hmm, we removed user from storage.
    # We need to keep user_account_id in storage OR use caller identity check.
    #
    # Decision: the consume path doesn't need user_id. But reclaim does.
    # Options:
    #   A) Keep user_id in storage (add 2 more items → 9 total)
    #   B) Use tx::get_account_id to check caller identity (spec approach)
    #
    # Going with B: check caller == note sender (no signature needed for reclaim)
    # But we need to know the user's account ID... it's the note sender.
    # active_note::get_sender might not exist. Let's keep sig-based reclaim
    # and store user_id. Update storage to 9 items.
    #
    # Actually, let's just keep user_id in storage. Updated layout:
    # [0-3] user_pub_key, [4] merchant_suffix, [5] merchant_prefix,
    # [6] user_suffix, [7] user_prefix, [8] reclaim_block_height = 9 items.
    
    # This needs a revised storage layout. See Task 1 Step 3.
    push.0.0
end
```

Hmm, I realize the reclaim path needs the user's account ID, which means the storage layout needs to include it. Let me revise.

- [ ] **Step 3: Finalize storage layout**

Revised storage (9 items):
```
[0-3]  user_pub_key_commitment (Word)
[4]    merchant_account_id_suffix
[5]    merchant_account_id_prefix
[6]    user_account_id_suffix
[7]    user_account_id_prefix
[8]    reclaim_block_height
```

Update MASM constants and reclaim path to use the user_id from storage for the reclaim message. The reclaim path computes `merge(serial_num, [user_suffix, user_prefix, 0, 0])` and verifies the agent's signature — same pattern as current, just different storage indices.

- [ ] **Step 4: Commit MASM changes**
```bash
git add crates/agent-debit-note/masm/agent_debit_note.masm
git commit -m "feat: align MASM with batch-settlement spec — single-sig, committed merchant"
```

---

### Task 2: Update Rust types and message functions

**Files:**
- Modify: `crates/agent-debit-note/src/types.rs`
- Modify: `crates/agent-debit-note/src/message.rs`
- Modify: `crates/agent-debit-note/src/note.rs`
- Modify: `crates/agent-debit-note/src/lib.rs`

- [ ] **Step 1: Update `AgentDebitNoteStorage`**

```rust
// types.rs
pub struct AgentDebitNoteStorage {
    pub user_pubkey_commitment: Word,      // [0-3]
    pub merchant_account_id: AccountId,    // [4-5]
    pub user_account_id: AccountId,        // [6-7]
    pub reclaim_block_height: u32,         // [8]
}

impl From<AgentDebitNoteStorage> for NoteStorage {
    fn from(s: AgentDebitNoteStorage) -> Self {
        NoteStorage::new(vec![
            s.user_pubkey_commitment[0],
            s.user_pubkey_commitment[1],
            s.user_pubkey_commitment[2],
            s.user_pubkey_commitment[3],
            s.merchant_account_id.suffix(),
            s.merchant_account_id.prefix().as_felt(),
            s.user_account_id.suffix(),
            s.user_account_id.prefix().as_felt(),
            Felt::new(s.reclaim_block_height as u64),
        ]).unwrap()
    }
}
```

- [ ] **Step 2: Update `debit_message` to use committed merchant**

```rust
// message.rs
/// Cumulative voucher message = merge(serial_num, [merchant_suffix, merchant_prefix, cumulativeAmount, 0])
pub fn debit_message(
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
) -> Word {
    let debit_word = Word::from([
        merchant_account_id.suffix(),
        merchant_account_id.prefix().as_felt(),
        Felt::new(cumulative_amount),
        Felt::ZERO,
    ]);
    merge_words(note_serial_num, debit_word)
}
```

Note: function signature changes — `amount` becomes `cumulative_amount`. The message format stays compatible with the MASM.

- [ ] **Step 3: Update `note.rs` for new storage**

Adapt the `create` function to use the new `AgentDebitNoteStorage`.

- [ ] **Step 4: Commit**
```bash
git add crates/agent-debit-note/src/
git commit -m "feat: update Rust types for batch-settlement — committed merchant, 9-item storage"
```

---

### Task 3: Add cumulative voucher types

**Files:**
- Create: `crates/agent-debit-note/src/voucher.rs`
- Modify: `crates/agent-debit-note/src/lib.rs`

- [ ] **Step 1: Create voucher module**

```rust
// voucher.rs
use miden_protocol::{Felt, Word};
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_poseidon2::{SecretKey, PublicKey, Signature};
use serde::{Serialize, Deserialize};

use crate::message::debit_message;

/// Off-chain cumulative voucher — agent signs, merchant verifies locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CumulativeVoucher {
    pub note_commitment_hex: String,
    pub cumulative_amount: u64,
    pub block_height_expiry: u32,
    pub merchant_account_id_hex: String,
    pub signature_hex: String,
}

/// Computes the message to sign for a cumulative voucher.
/// This matches the MASM consume path message computation.
pub fn voucher_message(
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
) -> Word {
    debit_message(note_serial_num, merchant_account_id, cumulative_amount)
}

/// Sign a cumulative voucher.
pub fn sign_voucher(
    secret_key: &SecretKey,
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
) -> Signature {
    let message = voucher_message(note_serial_num, merchant_account_id, cumulative_amount);
    secret_key.sign(message)
}

/// Verify a cumulative voucher signature against the agent's public key.
pub fn verify_voucher(
    public_key: &PublicKey,
    note_serial_num: Word,
    merchant_account_id: AccountId,
    cumulative_amount: u64,
    signature: &Signature,
) -> bool {
    let message = voucher_message(note_serial_num, merchant_account_id, cumulative_amount);
    public_key.verify(message, signature)
}
```

- [ ] **Step 2: Add `pub mod voucher` to lib.rs**

- [ ] **Step 3: Commit**
```bash
git add crates/agent-debit-note/src/voucher.rs crates/agent-debit-note/src/lib.rs
git commit -m "feat: add cumulative voucher types — sign/verify helpers"
```

---

### Task 4: MockChain tests for new MASM

**Files:**
- Rewrite: `crates/agent-debit-note/tests/note_script.rs`

- [ ] **Step 1: Update test setup for 9-item storage**

Update `setup_test()` to use the new storage layout (no facilitator key, add merchant to storage). Update `dual_sig_advice` → `agent_sig_advice` (single sig via advice map, matching MASM).

Key helper changes:
- Remove `facilitator_sk` from TestSetup
- `agent_sig_advice(agent_sk, message)` → returns AdviceInputs with agent sig in advice map (matching the new MASM `adv.has_mapkey` pattern)
- `setup_test(agent_pk, balance, serial, expiry, merchant_id)` → adds merchant to storage

- [ ] **Step 2: Write core consume tests**

| Test | What it verifies |
|------|-----------------|
| `test_valid_consume` | Single agent sig, P2ID to committed merchant, remainder note |
| `test_wrong_agent_sig` | Wrong key → fails |
| `test_cumulative_exceeds_balance` | Amount > balance → fails |
| `test_remainder_value_correct` | P2ID + remainder = original balance |
| `test_wrong_merchant_in_note_args` | Note args merchant ≠ committed merchant → N/A (merchant comes from storage now, note args only has amount) |

- [ ] **Step 3: Write reclaim tests**

| Test | What it verifies |
|------|-----------------|
| `test_valid_reclaim` | After expiry, agent sig → P2ID to user |
| `test_reclaim_before_expiry` | Before expiry → routes to consume, fails |
| `test_reclaim_wrong_sig` | Wrong key → fails |

- [ ] **Step 4: Write attack vector tests**

| Test | What it verifies |
|------|-----------------|
| `test_attack_unauthorized_consumer` | Third party sig → fails |
| `test_attack_amount_inflation` | Sig for 100, note_args say 999 → fails |
| `test_no_facilitator_needed` | Single agent sig suffices (no dual-sig) |

- [ ] **Step 5: Run all MockChain tests**
```bash
cargo test -p agent-debit-note --test note_script -- --nocapture
```
Expected: ALL PASS

- [ ] **Step 6: Commit**
```bash
git add crates/agent-debit-note/tests/note_script.rs
git commit -m "test: MockChain tests for batch-settlement MASM — single-sig, committed merchant"
```

---

### Task 5: Voucher unit tests

**Files:**
- Create: `crates/agent-debit-note/tests/voucher_test.rs`

- [ ] **Step 1: Write voucher sign/verify tests**

```rust
#[test]
fn test_voucher_sign_verify() {
    let sk = SecretKey::new();
    let pk = sk.public_key();
    let serial = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
    let merchant = AccountId::from_hex("0x...").unwrap();
    
    let sig = sign_voucher(&sk, serial, merchant, 5000);
    assert!(verify_voucher(&pk, serial, merchant, 5000, &sig));
    assert!(!verify_voucher(&pk, serial, merchant, 9999, &sig)); // wrong amount
}

#[test]
fn test_voucher_cumulative_amounts() {
    // Sign vouchers with increasing cumulative amounts
    // Each should verify independently
    let sk = SecretKey::new();
    for amount in [1000, 2000, 3000, 4000, 5000] {
        let sig = sign_voucher(&sk, serial, merchant, amount);
        assert!(verify_voucher(&pk, serial, merchant, amount, &sig));
    }
}
```

- [ ] **Step 2: Commit**
```bash
git add crates/agent-debit-note/tests/voucher_test.rs
git commit -m "test: voucher sign/verify unit tests"
```

---

### Task 6: E2E testnet test — full batch-settlement flow

**Files:**
- Create: `crates/agent-debit-note/tests/batch_settlement_e2e.rs`

- [ ] **Step 1: Write the 3-phase e2e test**

The test implements the full spec flow:

**Phase 1 — Channel Setup:**
1. Agent creates ADN note on-chain (with committed merchant)
2. Agent sends noteCommitment + noteDetails to merchant
3. Merchant calls facilitator `/verify` → confirms on-chain
4. Merchant stores userPubKey

**Phase 2 — 5 Per-Request Vouchers (off-chain):**
1. Agent signs cumulative vouchers: 1000, 2000, 3000, 4000, 5000
2. Merchant verifies each locally against stored userPubKey
3. No facilitator involvement — instant

**Phase 3 — Settlement:**
1. Merchant calls facilitator `/settle` with latest voucher (cumulativeAmount=5000)
2. Facilitator consumes ADN note → P2ID(5000) to merchant + remainder(balance-5000)
3. Returns remainderNoteCommitment

**Phase 4 — Merchant Consumes P2ID:**
1. Merchant imports P2ID note
2. Merchant consumes it

Test structure: single test function with all 3 clients (agent, merchant/verifier, facilitator), HTTP servers for verify + settle endpoints.

- [ ] **Step 2: Run on testnet**
```bash
RUST_LOG=info cargo test --release -p agent-debit-note --test batch_settlement_e2e -- --ignored --nocapture
```

- [ ] **Step 3: Commit**
```bash
git add crates/agent-debit-note/tests/batch_settlement_e2e.rs
git commit -m "test: e2e batch-settlement flow — setup, 5 vouchers, settle, merchant consumes"
```

- [ ] **Step 4: Push branch**
```bash
git push fork feat/batch-settlement-spec
```

---

## Spec Coverage Check

| Spec Section | Task |
|-------------|------|
| Note storage (merchant committed) | Task 1, 2 |
| Single-sig consume (no facilitator) | Task 1, 4 |
| Reclaim path (agent sig) | Task 1, 4 |
| Cumulative voucher model | Task 3, 5 |
| Off-chain merchant verification | Task 6 (Phase 2) |
| POST /verify (session setup) | Task 6 (Phase 1) |
| POST /settle (settlement) | Task 6 (Phase 3) |
| P2ID to merchant + remainder | Task 1, 4, 6 |
| HTTP wire format alignment | Task 6 |
| MockChain tests | Task 4 |
| E2E testnet test | Task 6 |
