# x402 on Miden — Detailed Flow Diagrams

Both approaches follow the Base x402 payment flow: the agent only
communicates with the merchant (the API server). The merchant relays
to the facilitator internally. On-chain settlement is async.

---

## Approach 1: AgentDebitNote (ADN)

### Trust model

Same as Base x402. The signed debit authorization is a bearer instrument.
Anyone holding it (merchant or facilitator) can consume the on-chain note.
The merchant doesn't depend on the facilitator — it can self-submit.

### Step 0: Prefund (once per agent, ~20s)

```
User                                          Miden Node
  |                                               |
  | 1. Generate agent Falcon-512 keypair          |
  |    agent_pk = public key commitment (Word)    |
  |    agent_sk = secret key (kept by agent)      |
  |                                               |
  | 2. Build AgentDebitNote:                      |
  |    Script:  agent_debit_note.masm             |
  |    Storage: [agent_pk[0..3],                  |
  |              user_account_suffix,             |
  |              user_account_prefix,             |
  |              expiry_block_height]             |
  |    Assets:  N USDC (fungible)                 |
  |    Type:    PRIVATE                           |
  |    Serial:  random Word                       |
  |                                               |
  | 3. Submit prefund transaction:                |
  |    TransactionRequest with                    |
  |    own_output_notes([adn_note])               |
  |    (moves USDC from user vault                |
  |     into the AgentDebitNote)                  |
  |                                               |
  |--- prove + submit ---------------------------->|
  |                                               |
  |<-- tx included in block ----------------------|
  |                                               |
  |    On-chain: note COMMITMENT only             |
  |    (hash of script + storage + assets)        |
  |    Off-chain: full note data kept by user     |
  |                                               |
  | 4. Hand off to agent:                         |
  |    - agent_sk (Falcon secret key)             |
  |    - note_id (hash of commitment + assets)    |
  |    - serial_num (Word, unique per note)        |
  |    - full private note data                   |
  |      { script, storage, assets, metadata }    |
  |    - balance (N USDC)                         |
  |    - expiry_block_height                      |
  |                                               |
  | Agent is ready to pay.                        |
  | Facilitator does NOT know about the note yet. |
```

### Per-Payment: Steady State (2 RTT)

The facilitator already knows the note (from a previous payment).
The agent just signs and sends to the merchant.

```
Agent                     Merchant (API Server)              Facilitator
  |                            |                                  |
  | ── Step 1: Request ──      |                                  |
  |                            |                                  |
  |--- GET /resource --------->|                                  |
  |                            |                                  |
  | ── Step 2: 402 ──          |                                  |
  |                            |                                  |
  |<-- HTTP 402 ---------------|                                  |
  |    Payment-Required:       |                                  |
  |    (base64 JSON header)    |                                  |
  |    {                       |                                  |
  |      accepts: [{           |                                  |
  |        scheme:             |                                  |
  |          "miden-adn-x402", |                                  |
  |        network:            |                                  |
  |          "miden:testnet",  |                                  |
  |        merchant_account_id:|                                  |
  |          "0x86535d...",    |                                  |
  |        asset_faucet_id:    |                                  |
  |          "0x4ac917...",   |                                  |
  |        amount: "100"       |                                  |
  |      }]                    |                                  |
  |    }                       |                                  |
  |                            |                                  |
  | ── Step 3: Sign + Retry ── |                                  |
  |                            |                                  |
  | Agent signs (local, ~2ms): |                                  |
  |                            |                                  |
  |   message = poseidon2::    |                                  |
  |     merge(                 |                                  |
  |       serial_num,          |                                  |
  |       [merchant_suffix,    |                                  |
  |        merchant_prefix,    |                                  |
  |        amount, 0]          |                                  |
  |     )                      |                                  |
  |                            |                                  |
  |   signature =              |                                  |
  |     falcon_sign(           |                                  |
  |       message, agent_sk)   |                                  |
  |                            |                                  |
  |--- GET /resource --------->|                                  |
  |    Payment-Signature:      |                                  |
  |    (base64 JSON header)    |                                  |
  |    {                       |                                  |
  |      note_id:              |                                  |
  |        "0xab12...",       |                                  |
  |      serial_num_hex:       |                                  |
  |        ["0x..","0x..",     |                                  |
  |         "0x..","0x.."],   |                                  |
  |      merchant_account_id:  |                                  |
  |        "0x86535d...",     |                                  |
  |      amount: 100,          |                                  |
  |      signature_hex:        |                                  |
  |        "0xfa1c0n...",     |                                  |
  |      expiry_block_height:  |                                  |
  |        100000,             |                                  |
  |      agent_pubkey_         |                                  |
  |        commitment_hex:     |                                  |
  |        "0xpk..."          |                                  |
  |    }                       |                                  |
  |                            |                                  |
  | ── Step 4: Verify + Serve ─|                                  |
  |                            |                                  |
  |                  MERCHANT   |                                  |
  |                  INTERNAL:  |                                  |
  |                            |--- POST /adn/pay --------------->|
  |                            |    (relay signed debit JSON)     |
  |                            |                                  |
  |                            |    FACILITATOR (~1ms):           |
  |                            |                                  |
  |                            |    1. Look up note_id in store   |
  |                            |       ✓ found (known note)       |
  |                            |                                  |
  |                            |    2. Recompute message from     |
  |                            |       (serial, merchant, amount) |
  |                            |                                  |
  |                            |    3. Deserialize Falcon sig     |
  |                            |       from signature_hex         |
  |                            |                                  |
  |                            |    4. Verify sig against         |
  |                            |       agent_pk from stored       |
  |                            |       note data                  |
  |                            |       ✓ valid                    |
  |                            |                                  |
  |                            |    5. Check expiry:              |
  |                            |       current_block + gap        |
  |                            |       < expiry_block             |
  |                            |       ✓ sufficient margin        |
  |                            |                                  |
  |                            |    6. Check mandate              |
  |                            |       (amount cap, allowlist)    |
  |                            |       ✓ passes                   |
  |                            |                                  |
  |                            |    7. Sign facilitator ack:      |
  |                            |       ack_sig = falcon_sign(     |
  |                            |         hash(timestamp,          |
  |                            |              note_id,            |
  |                            |              merchant,           |
  |                            |              amount),            |
  |                            |         facilitator_sk)          |
  |                            |                                  |
  |                            |<-- {                        } ---|
  |                            |      accepted_at_unix_micros,    |
  |                            |      facilitator_ack_signature,  |
  |                            |      facilitator_pubkey_         |
  |                            |        commitment                |
  |                            |    }                             |
  |                            |                                  |
  |                  MERCHANT:  |                                  |
  |                  - Verify facilitator_ack_signature            |
  |                  - Serve resource                              |
  |                            |                                  |
  |<-- HTTP 200 + resource ----|                                  |
  |    Payment-Response:       |                                  |
  |    (base64 JSON header)    |                                  |
  |    { status: "accepted" }  |                                  |
  |                            |                                  |
  |                            |                                  |
  | ═══════════ ASYNC SETTLEMENT (off hot path) ═════════════════ |
  |                            |                                  |
  |                            |    Facilitator batch worker:     |
  |                            |                                  |
  |                            |    1. Build consume tx:          |
  |                            |       TransactionRequest with    |
  |                            |       input_notes([adn_note]),   |
  |                            |       note_args = [merchant_     |
  |                            |         suffix, merchant_prefix, |
  |                            |         amount, 0]               |
  |                            |       advice_stack = prepared    |
  |                            |         Falcon signature         |
  |                            |                                  |
  |                            |    2. MASM script executes:      |
  |                            |       - Verify Falcon sig        |
  |                            |       - Create P2ID output to    |
  |                            |         merchant for amount      |
  |                            |       - Create remainder ADN     |
  |                            |         note with (balance -     |
  |                            |         amount)                  |
  |                            |                                  |
  |                            |    3. Prove STARK (~4s on CPU)   |
  |                            |                                  |
  |                            |    4. Submit proven tx to node   |
  |                            |                                  |
  |                            |    5. Wait for block inclusion   |
  |                            |       (~3-6s)                    |
  |                            |                                  |
  |                            |    ON-CHAIN RESULT:              |
  |                            |    - Original ADN note consumed  |
  |                            |    - P2ID note created →         |
  |                            |      merchant's account          |
  |                            |    - Remainder ADN note created  |
  |                            |      (same script, same storage, |
  |                            |       reduced balance,           |
  |                            |       new serial_num)            |
  |                            |                                  |
  | Agent receives remainder:  |                                  |
  | { new_note_id,             |                                  |
  |   new_serial_num,          |                                  |
  |   remaining_balance }      |                                  |
  | → cached for next payment  |                                  |
```

### Per-Payment: First Time (facilitator doesn't know the note)

```
Agent                     Merchant                         Facilitator
  |                            |                                |
  |--- GET /resource --------->|                                |
  |<-- 402 --------------------|                                |
  |                            |                                |
  | sign debit (~2ms)          |                                |
  |                            |                                |
  |--- GET /resource --------->|                                |
  |    Payment-Signature:      |                                |
  |    { note_id, sig, ... }   |                                |
  |                            |--- POST /adn/pay ------------->|
  |                            |    { note_id, sig, ... }       |
  |                            |                                |
  |                            |<-- 404 "unknown note_id" ------|
  |                            |                                |
  |<-- 402 --------------------|                                |
  |    Payment-Required:       |                                |
  |    { reason:               |                                |
  |      "note_data_required", |                                |
  |      note_id: "0x..." }   |                                |
  |                            |                                |
  |--- GET /resource --------->|                                |
  |    Payment-Signature:      |                                |
  |    { note_id, sig, ...,    |                                |
  |      note_data: {          | ← full private note preimage   |
  |        script_hex,         |                                |
  |        storage,            |                                |
  |        assets,             |                                |
  |        serial_num          |                                |
  |      }                     |                                |
  |    }                       |                                |
  |                            |--- POST /adn/pay ------------->|
  |                            |    { ..., note_data }          |
  |                            |                                |
  |                            |    Facilitator:                |
  |                            |    - Store note data           |
  |                            |      (implicit registration)  |
  |                            |    - Verify sig, ack           |
  |                            |                                |
  |                            |<-- { ack } --------------------|
  |<-- 200 + resource ---------|                                |
  |                            |                                |
  | Subsequent payments:       |                                |
  | facilitator knows the note,|                                |
  | no note_data needed.       |                                |
```

### Timing (measured on us-east-1 ↔ eu-west-1, 68ms RTT)

```
STEADY STATE (2 RTT):

  GET → 402                       68 ms   (1 RTT agent↔merchant)
  Falcon sign                      2 ms   (local, no kernel execution)
  GET + Payment-Sig → 200        ~70 ms   (1 RTT agent↔merchant,
                                           includes merchant→facilitator
                                           loopback relay ~1ms)
  ─────────────────────────────────────
  Hot-path total:               ~140 ms

FIRST PAYMENT (4 RTT, note_data challenge):

  GET → 402                       68 ms   (1 RTT)
  Sign + GET → 402 (unknown)      70 ms   (1 RTT, facilitator 404)
  GET + note_data → 200           70 ms   (1 RTT, facilitator stores + acks)
  ─────────────────────────────────────
  First payment:                ~278 ms   (includes extra challenge RTT)

ASYNC SETTLEMENT (off hot path):

  Build consume tx                ~1 ms
  STARK prove                    ~4 s     (CPU; faster with GPU/dedicated)
  Submit + block inclusion       ~3-6 s
  ─────────────────────────────────────
  Settlement total:              ~7-10 s
```

---

## Approach 2: Signed Transaction Request (Guardian-Facilitator)

### Trust model

Merchant trusts the facilitator's pre-finality ack. The facilitator is
the single serialization authority for the agent's account state.
Settlement happens async. If the facilitator disappears after acking,
the merchant has no independent path to settlement — the signed tx
request is not a bearer instrument (only the facilitator can prove
and submit it, because it requires the MultisigGuardian account's
execution context).

### Step 0: Deploy + Register (once per agent)

```
User                                          Miden Node
  |                                               |
  | 1. Generate agent Falcon-512 keypair          |
  |                                               |
  | 2. Deploy MultisigGuardian account:           |
  |    - threshold = 1                            |
  |    - guardian_enabled = false                  |
  |    - single signer = agent_pk                 |
  |    - component: BasicWallet                   |
  |    - storage_mode: Public                     |
  |                                               |
  |--- prove + submit ---------------------------->|
  |<-- account on-chain --------------------------|
  |                                               |
  | 3. Mint tokens to agent:                      |
  |    faucet → P2ID note → agent                 |
  |                                               |
  |--- mint tx ---------------------------------->|
  |<-- included ----------------------------------|
  |                                               |
  | 4. Agent consumes minted note:                |
  |    (uses MultisigGuardian sign-inject flow)   |
  |    execute_for_summary → Unauthorized(summary)|
  |    sign summary.to_commitment()               |
  |    inject sig into advice map                 |
  |    submit_new_transaction                     |
  |                                               |
  |--- consume tx -------------------------------->|
  |<-- included (agent vault now funded) ---------|
  |                                               |
  | 5. Register with facilitator:                 |
  |    POST /agents                               |
  |    { agent_id, account_id, hot_key,           |
  |      mandate, account_snapshot_b64 }          |
  |                                               |
  | Agent is ready to pay.                        |
```

### Per-Payment: Steady State (2 RTT)

```
Agent                     Merchant (API Server)              Facilitator
  |                            |                                  |
  | ── Step 1: Request ──      |                                  |
  |                            |                                  |
  |--- GET /resource --------->|                                  |
  |                            |                                  |
  | ── Step 2: 402 ──          |                                  |
  |                            |                                  |
  |<-- HTTP 402 ---------------|                                  |
  |    Payment-Required:       |                                  |
  |    {                       |                                  |
  |      accepts: [{           |                                  |
  |        scheme:             |                                  |
  |          "miden-p2id-x402",|                                  |
  |        network:            |                                  |
  |          "miden:testnet",  |                                  |
  |        merchant_account_id:|                                  |
  |          "0x86535d...",    |                                  |
  |        asset_faucet_id:    |                                  |
  |          "0x4ac917...",   |                                  |
  |        amount: "100"       |                                  |
  |      }]                    |                                  |
  |    }                       |                                  |
  |                            |                                  |
  | ── Step 3: Build + Sign ── |                                  |
  |                            |                                  |
  | Agent builds tx (~15ms):   |                                  |
  |                            |                                  |
  |  1. build_p2id_request(    |                                  |
  |       merchant, faucet,    |                                  |
  |       amount)              |                                  |
  |                            |                                  |
  |  2. execute_for_summary(   |                                  |
  |       request)             |                                  |
  |     → runs Miden kernel    |                                  |
  |     → fails at auth with   |                                  |
  |       Unauthorized(summary)|                                  |
  |     → extracts             |                                  |
  |       TransactionSummary   |                                  |
  |     (~13ms)                |                                  |
  |                            |                                  |
  |  3. falcon_sign(           |                                  |
  |       summary.             |                                  |
  |       to_commitment())     |                                  |
  |     (~2ms)                 |                                  |
  |                            |                                  |
  |--- GET /resource --------->|                                  |
  |    Payment-Signature:      |                                  |
  |    (base64 JSON header)    |                                  |
  |    {                       |                                  |
  |      agent_id:             |                                  |
  |        "run-...-agent-0",  |                                  |
  |      payload: {            |  ← full AgenticPayload           |
  |        tx_summary: {       |                                  |
  |          version:          |                                  |
  |           "miden-real-v1", |                                  |
  |          summary_base64:   |                                  |
  |           "...",           |                                  |
  |          tx_request_base64:|                                  |
  |           "..."            |                                  |
  |        },                  |                                  |
  |        hot_key_signature: {|                                  |
  |          signer_id: "0x..",|                                  |
  |          signature: {      |                                  |
  |            Falcon: "0x.."  |                                  |
  |          }                 |                                  |
  |        },                  |                                  |
  |        x402_context: {     |                                  |
  |          merchant_id,      |                                  |
  |          faucet_id,        |                                  |
  |          amount: "100"     |                                  |
  |        },                  |                                  |
  |        built_on_state:     |                                  |
  |          "0x...",          |                                  |
  |        new_state:          |                                  |
  |          "0x...",          |                                  |
  |        claimed_nullifiers: |                                  |
  |          ["0x..."]         |                                  |
  |      }                     |                                  |
  |    }                       |                                  |
  |                            |                                  |
  | ── Step 4: Verify + Serve ─|                                  |
  |                            |                                  |
  |                  MERCHANT   |                                  |
  |                  INTERNAL:  |                                  |
  |                            |                                  |
  |                            |--- POST /agents/{id}/payments -->|
  |                            |    (relay full payload JSON)     |
  |                            |                                  |
  |                            |    FACILITATOR (~11ms):          |
  |                            |                                  |
  |                            |    1. Stale-base check:          |
  |                            |       payload.built_on_state     |
  |                            |       == server pending_state    |
  |                            |       ✓ matches                  |
  |                            |                                  |
  |                            |    2. Falcon sig verify:         |
  |                            |       deserialize signature,     |
  |                            |       verify against registered  |
  |                            |       hot_key_commitment         |
  |                            |       over summary.commitment    |
  |                            |       ✓ valid                    |
  |                            |                                  |
  |                            |    3. P2ID output validation:    |
  |                            |       - exactly 1 output note    |
  |                            |       - script root == P2ID      |
  |                            |       - recipient == merchant    |
  |                            |       - asset == faucet + amount |
  |                            |       ✓ all match                |
  |                            |                                  |
  |                            |    4. Mandate enforcement:       |
  |                            |       - amount ≤ per_tx_cap      |
  |                            |       - merchant in allowlist    |
  |                            |       - deadline ≤ mandate expiry|
  |                            |       ✓ passes                   |
  |                            |                                  |
  |                            |    5. Compute output nullifiers  |
  |                            |       from TransactionSummary    |
  |                            |                                  |
  |                            |    6. Check reserved set         |
  |                            |       (no double-spend)          |
  |                            |                                  |
  |                            |    7. Reserve nullifiers         |
  |                            |       (WAL + fsync)              |
  |                            |                                  |
  |                            |    8. Advance pending state      |
  |                            |       for this agent             |
  |                            |                                  |
  |                            |    9. Falcon-sign ack:           |
  |                            |       hash(timestamp, state,     |
  |                            |            nullifiers)           |
  |                            |                                  |
  |                            |<-- {                        } ---|
  |                            |      accepted_at_unix_micros,    |
  |                            |      new_pending_state,          |
  |                            |      reserved_nullifiers,        |
  |                            |      seq,                        |
  |                            |      facilitator_ack_signature   |
  |                            |    }                             |
  |                            |                                  |
  |                  MERCHANT:  |                                  |
  |                  - Verify facilitator_ack_signature            |
  |                  - Serve resource                              |
  |                            |                                  |
  |<-- HTTP 200 + resource ----|                                  |
  |    Payment-Response:       |                                  |
  |    { status: "accepted" }  |                                  |
  |                            |                                  |
  |                            |                                  |
  | ═══════════ ASYNC SETTLEMENT (off hot path) ═════════════════ |
  |                            |                                  |
  |                            |    Facilitator batch worker      |
  |                            |    (runs every 1s):              |
  |                            |                                  |
  |                            |    1. Decode TransactionRequest  |
  |                            |       from tx_request_base64     |
  |                            |                                  |
  |                            |    2. Parse Falcon signature,    |
  |                            |       build advice map entry:    |
  |                            |       key = pubkey_commitment    |
  |                            |       val = prepared_signature   |
  |                            |                                  |
  |                            |    3. Inject into request's      |
  |                            |       advice_map                 |
  |                            |                                  |
  |                            |    4. client.sync_state()        |
  |                            |       (fetch latest chain state) |
  |                            |                                  |
  |                            |    5. client.submit_new_         |
  |                            |       transaction(account_id,    |
  |                            |                   request)       |
  |                            |       → LocalTransactionProver   |
  |                            |         generates STARK proof    |
  |                            |         (~4s on CPU)             |
  |                            |       → submits proven tx to     |
  |                            |         Miden node RPC           |
  |                            |                                  |
  |                            |    ON-CHAIN RESULT:              |
  |                            |    - P2ID note created →         |
  |                            |      merchant's account          |
  |                            |    - Agent's account state       |
  |                            |      updated (vault debited)     |
```

### Timing (measured on us-east-1 ↔ eu-west-1, 68ms RTT)

```
STEADY STATE (2 RTT):

  GET → 402                       68 ms   (1 RTT agent↔merchant)
  Kernel execution                13 ms   (local, Miden VM runs tx)
  Falcon sign                      2 ms   (local)
  GET + Payment-Sig → 200        ~70 ms   (1 RTT agent↔merchant,
                                           includes merchant→facilitator
                                           loopback relay + 11ms verify)
  ─────────────────────────────────────
  Hot-path total:               ~153 ms

ASYNC SETTLEMENT (off hot path):

  Decode + inject signature       ~1 ms
  Sync chain state                ~1 s
  STARK prove                    ~4 s     (CPU; faster with GPU/dedicated)
  Submit + block inclusion       ~3-6 s
  ─────────────────────────────────────
  Settlement total:              ~7-10 s
```

---

## Side-by-Side Comparison

```
                        Approach 1: ADN          Approach 2: Signed Tx
                        ───────────────          ─────────────────────
Hot-path latency        ~140ms (2 RTT + 2ms)     ~153ms (2 RTT + 15ms)
Agent computation       2ms (Falcon only)        15ms (kernel 13ms + sign 2ms)
Agent dependencies      Falcon key only          miden-client + MultisigGuardian
Trust model             Bearer instrument        Facilitator-dependent
                        (same as Base x402)      (merchant trusts facilitator)
Facilitator failure     Merchant self-submits    Merchant has no recourse
Setup cost              Prefund tx (~20s)        Account deploy + registration
Capital efficiency      Locked until expiry      Vault-based, no lock-up
Privacy                 Private note             Public account state
                        (commitment on-chain)    (state transitions visible)
Note discovery          Lazy (first payment)     N/A (account-based)
Protocol surface        Custom MASM script       Existing MultisigGuardian
On-chain footprint      1 note commitment        Account state updates
Payment payload size    ~500 bytes (sig only)    ~5KB (full tx summary)
```
