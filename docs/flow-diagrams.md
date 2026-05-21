# x402 on Miden — Flow Diagrams

Both approaches follow the Base x402 payment flow: the agent only
communicates with the merchant. The merchant relays to the facilitator
internally. On-chain settlement is async (off the hot path).

---

## Approach 1: AgentDebitNote (ADN)

### Trust model
Same as Base x402. The signed debit authorization is a bearer instrument.
Anyone holding it (merchant or facilitator) can consume the on-chain note.
The merchant doesn't depend on the facilitator — it can self-submit.

### Step 0: Setup (once per agent)

```
User                                    Miden Node
  |                                         |
  | 1. Generate agent Falcon keypair        |
  |                                         |
  | 2. Build AgentDebitNote:                |
  |    - Storage: [agent_pk,                |
  |                user_id,                 |
  |                expiry_block]            |
  |    - Assets: N USDC                     |
  |    - Script: agent_debit_note.masm      |
  |    - Type: PRIVATE                      |
  |                                         |
  | 3. Submit prefund tx ------------------>|
  |    (moves USDC from user's              |
  |     vault into the ADN note)            |
  |                                         |
  |<--- tx included in block ---------------|
  |     (note commitment on-chain,          |
  |      full note data stays private)      |
  |                                         |
  | 4. Give agent:                          |
  |    - Falcon hot key                     |
  |    - note_id, serial_num                |
  |    - full private note data             |
  |      (script, storage, assets)          |
  |    - balance, expiry_block              |
  |                                         |
  | Agent is ready to pay.                  |
```

The full private note data stays with the agent. The facilitator
does NOT know about the note yet.

### Per-Payment Flow

```
Agent                   Merchant (API Server)              Facilitator
  |                          |                                  |
  | ── Step 1 ──             |                                  |
  |                          |                                  |
  |-- GET /resource -------->|                                  |
  |                          |                                  |
  | ── Step 2 ──             |                                  |
  |                          |                                  |
  |<-- 402 ------------------|                                  |
  |  Payment-Required:       |                                  |
  |  { accepts: [{           |                                  |
  |      scheme:             |                                  |
  |       "miden-adn-x402",  |                                  |
  |      merchant_id,        |                                  |
  |      faucet_id,          |                                  |
  |      amount: "100"       |                                  |
  |    }]                    |                                  |
  |  }                       |                                  |
  |                          |                                  |
  | ── Step 3 ──             |                                  |
  |                          |                                  |
  | sign_debit: ~2ms         |                                  |
  |  msg = merge(serial,     |                                  |
  |    [merchant, amount])   |                                  |
  |  sig = falcon_sign(msg)  |                                  |
  |                          |                                  |
  |-- GET /resource -------->|                                  |
  |  Payment-Signature:      |                                  |
  |  {                       |                                  |
  |    note_id,              |                                  |
  |    serial_num_hex,       |                                  |
  |    merchant_account_id,  |                                  |
  |    amount,               |                                  |
  |    signature_hex,        |                                  |
  |    expiry_block_height,  |                                  |
  |    agent_pubkey_hex      |                                  |
  |  }                       |                                  |
  |                          |                                  |
  | ── Step 4 ──             |                                  |
  |                          |                                  |
  |                          |--- POST /adn/pay --------------->|
  |                          |    (relay signed debit)          |
  |                          |                                  |
  |                          |    Facilitator (~1ms):           |
  |                          |    1. Look up note_id            |
  |                          |       (if unknown: 404 →         |
  |                          |        merchant asks agent       |
  |                          |        for note_data, see        |
  |                          |        "first payment" below)    |
  |                          |    2. Verify Falcon sig          |
  |                          |       against agent_pk in        |
  |                          |       stored note data           |
  |                          |    3. Check note on-chain        |
  |                          |    4. Check expiry gap           |
  |                          |    5. Check mandate              |
  |                          |    6. Sign facilitator ack       |
  |                          |                                  |
  |                          |<-- { ack, facilitator_sig } -----|
  |                          |                                  |
  |                          | Merchant:                        |
  |                          | - Verify facilitator_sig         |
  |                          | - Serve resource                 |
  |                          |                                  |
  |<-- 200 OK + resource ----|                                  |
  |  Payment-Response:       |                                  |
  |  { tx status }           |                                  |
  |                          |                                  |
  |                          |                                  |
  | ════════════════ ASYNC SETTLEMENT ═══════════════════════════
  |                          |                                  |
  |                          |              Facilitator:        |
  |                          |              1. Build consume tx |
  |                          |                 (note_args +     |
  |                          |                  Falcon sig on   |
  |                          |                  advice stack)   |
  |                          |              2. Prove (~4s)      |
  |                          |              3. Submit to node   |
  |                          |              4. Wait for block   |
  |                          |                                  |
  |                          |              Produces:           |
  |                          |              - P2ID note →       |
  |                          |                merchant (on-chain)|
  |                          |              - Remainder ADN     |
  |                          |                note (for next    |
  |                          |                payment)          |
  |                          |                                  |
  | Agent receives remainder info                               |
  | (new note_id, serial, balance)                              |
  | via facilitator → merchant → agent                          |
  | or via polling                                              |
```

### First payment (facilitator doesn't know the note yet)

If the facilitator responds "unknown note", the merchant asks the
agent for the full private note data via a `402` with a
`Note-Data-Required` indicator. The agent resends with note_data
attached:

```
Agent                   Merchant                           Facilitator
  |                          |                                  |
  |-- GET /resource -------->|                                  |
  |  Payment-Signature:      |                                  |
  |  { note_id, sig, ... }   |                                  |
  |                          |--- POST /adn/pay --------------->|
  |                          |    { note_id, sig, ... }         |
  |                          |                                  |
  |                          |<-- 404 "unknown note_id" --------|
  |                          |                                  |
  |<-- 402 ------------------|                                  |
  |  Payment-Required:       |                                  |
  |  { reason:               |                                  |
  |    "note_data_required", |                                  |
  |    note_id: "0x..."      |                                  |
  |  }                       |                                  |
  |                          |                                  |
  |-- GET /resource -------->|                                  |
  |  Payment-Signature:      |                                  |
  |  { note_id, sig, ...,   |                                  |
  |    note_data: {          |  ← full private note data        |
  |      script_hex,         |                                  |
  |      storage,            |                                  |
  |      assets              |                                  |
  |    }                     |                                  |
  |  }                       |                                  |
  |                          |--- POST /adn/pay --------------->|
  |                          |    { ..., note_data }            |
  |                          |                                  |
  |                          |    Facilitator: store note,      |
  |                          |    verify sig, ack               |
  |                          |                                  |
  |                          |<-- { ack } ----------------------|
  |<-- 200 + resource -------|                                  |
```

### Subsequent payments (same or different merchant)

The facilitator already has the note data. The agent sends just
the signed debit. No `note_data` needed:

```
  |-- GET /resource -------->|
  |  Payment-Signature:      |
  |  { note_id, sig, ... }   |  ← no note_data
  |                          |--- POST /adn/pay → ack -------->|
  |<-- 200 + resource -------|
```

If the agent pays a **different merchant**, same flow — the new
merchant relays to the same facilitator, which already has the note.

After async settlement produces a remainder note, the agent uses
the new note_id + serial. If the facilitator doesn't recognize
the remainder yet (it should, since it created it), the same
challenge-response flow applies.

### Timing (68ms transatlantic RTT)

```
STEADY-STATE (facilitator knows the note):
Step 1→2: GET → 402             68 ms  (1 RTT)
Step 3:   Falcon sign            2 ms  (local, no kernel)
Step 3→4: GET + Payment-Sig    ~70 ms  (1 RTT, includes merchant→facilitator
                                        relay on loopback ~1ms)
────────────────────────────────────────
Hot path total:               ~140 ms  (2 RTT + 2ms signing)

FIRST PAYMENT (note_data challenge adds 1 extra round-trip):
Step 1→2: GET → 402             68 ms  (1 RTT)
Step 3:   Sign + send           70 ms  (1 RTT, facilitator returns 404)
Step 3b:  402 note_data_req     68 ms  (1 RTT back to agent)
Step 3c:  Resend with data      70 ms  (1 RTT, facilitator stores + acks)
────────────────────────────────────────
First payment total:          ~278 ms  (4 RTT + 2ms signing)

Async settlement:             ~4-10s   (prove + block inclusion)
```

---

## Approach 2: Signed Transaction Request (Guardian-Facilitator)

### Trust model
Merchant trusts the facilitator's pre-finality ack. The facilitator is
the single serialization authority for the agent's account state.
Settlement happens asynchronously. If the facilitator disappears after
acking, the merchant has no independent path to settlement — the signed
tx request is not a bearer instrument (only the facilitator can prove
and submit it).

### Step 0: Setup (once per agent)

```
User                                    Miden Node
  |                                         |
  | 1. Deploy agent account on-chain        |
  |    (MultisigGuardian, threshold=1,      |
  |     guardian disabled, Falcon key)      |
  |                                         |
  | 2. Mint tokens to agent --------------->|
  | 3. Consume minted note (fund vault) --->|
  |                                         |
  | 4. Register agent with facilitator      |
  |    (account ID, hot key, mandate,       |
  |     account snapshot)                   |
  |                                         |
  | Agent is ready to pay.                  |
```

No pre-funded note. The agent's account vault holds the funds.

### Per-Payment Flow

```
Agent                   Merchant (API Server)              Facilitator
  |                          |                                  |
  | ── Step 1 ──             |                                  |
  |                          |                                  |
  |-- GET /resource -------->|                                  |
  |                          |                                  |
  | ── Step 2 ──             |                                  |
  |                          |                                  |
  |<-- 402 ------------------|                                  |
  |  Payment-Required:       |                                  |
  |  { accepts: [{           |                                  |
  |      scheme:             |                                  |
  |       "miden-p2id-x402", |                                  |
  |      merchant_id,        |                                  |
  |      faucet_id,          |                                  |
  |      amount: "100"       |                                  |
  |    }]                    |                                  |
  |  }                       |                                  |
  |                          |                                  |
  | ── Step 3 ──             |                                  |
  |                          |                                  |
  | Build + sign: ~15ms      |                                  |
  |  1. execute_for_summary  |                                  |
  |     (Miden kernel, 13ms) |                                  |
  |  2. falcon_sign(         |                                  |
  |     summary.commitment,  |                                  |
  |     2ms)                 |                                  |
  |                          |                                  |
  |-- GET /resource -------->|                                  |
  |  Payment-Signature:      |                                  |
  |  {                       |                                  |
  |    agent_id,             |                                  |
  |    tx_summary: {         |                                  |
  |      version:            |                                  |
  |       "miden-real-v1",   |                                  |
  |      summary_base64,     |                                  |
  |      tx_request_base64   |                                  |
  |    },                    |                                  |
  |    hot_key_signature,    |                                  |
  |    x402_context,         |                                  |
  |    built_on_state,       |                                  |
  |    new_state,            |                                  |
  |    claimed_nullifiers    |                                  |
  |  }                       |                                  |
  |                          |                                  |
  | ── Step 4 ──             |                                  |
  |                          |                                  |
  |                          |--- POST /agents/{id}/payments -->|
  |                          |    (relay full payload)          |
  |                          |                                  |
  |                          |    Facilitator (10-step          |
  |                          |    verify, ~11ms):               |
  |                          |    1. Stale-base check           |
  |                          |    2. Falcon sig verify          |
  |                          |    3. P2ID output validation     |
  |                          |       (recipient, asset, amount) |
  |                          |    4. Mandate enforcement         |
  |                          |    5. Compute nullifiers         |
  |                          |    6. Check reserved set         |
  |                          |    7. Reserve nullifiers (WAL)   |
  |                          |    8. Advance pending state      |
  |                          |    9. Sign ack                   |
  |                          |                                  |
  |                          |<-- { ack, facilitator_sig,  } ---|
  |                          |                                  |
  |                          | Merchant:                        |
  |                          | - Verify facilitator_sig         |
  |                          | - Serve resource                 |
  |                          |                                  |
  |<-- 200 OK + resource ----|                                  |
  |  Payment-Response:       |                                  |
  |  { tx status }           |                                  |
  |                          |                                  |
  |                          |                                  |
  | ════════════════ ASYNC SETTLEMENT ═══════════════════════════
  |                          |                                  |
  |                          |              Batch worker:       |
  |                          |              1. Decode tx request|
  |                          |              2. Inject sig into  |
  |                          |                 advice map       |
  |                          |              3. Prove (~4s)      |
  |                          |              4. Submit to node   |
  |                          |                                  |
  |                          |              Produces:           |
  |                          |              - P2ID note →       |
  |                          |                merchant (on-chain)|
  |                          |              - Agent account     |
  |                          |                state updated     |
```

### Timing (68ms transatlantic RTT)

```
Step 1→2: GET → 402             68 ms  (1 RTT)
Step 3:   Kernel + sign         15 ms  (local: 13ms kernel + 2ms Falcon)
Step 3→4: GET + Payment-Sig    ~70 ms  (1 RTT, includes merchant→facilitator
                                        relay on loopback ~1ms)
────────────────────────────────────────
Hot path total:               ~153 ms  (2 RTT + 15ms computation)

Async settlement:             ~4-10s   (prove + block inclusion)
```

**Note on RTT count:** With the corrected flow (agent only talks to
merchant, merchant relays internally), both approaches use 2 RTTs.
The previous measurement of 231ms / 3 RTTs was from the old flow
where the agent contacted the facilitator directly.

---

## Side-by-Side Comparison

| Dimension | Approach 1: AgentDebitNote | Approach 2: Signed Tx Request |
|-----------|---------------------------|-------------------------------|
| **Hot-path latency** | ~140ms (2 RTT + 2ms) | ~153ms (2 RTT + 15ms) |
| **Agent computation** | 2ms (Falcon sign only) | 15ms (kernel 13ms + sign 2ms) |
| **Agent dependencies** | Falcon key only | miden-client + MultisigGuardian |
| **Trust model** | Same as Base x402 (bearer instrument) | Merchant trusts facilitator |
| **Facilitator failure** | Merchant can self-submit | Merchant has no recourse |
| **Setup cost** | Prefund tx on-chain (~20s) | Account deployment + registration |
| **Capital efficiency** | Locked in note until expiry | Vault-based, no lock-up |
| **Privacy** | Private note (commitment only on-chain) | Account state transitions visible |
| **Note discovery** | Implicit via first payment relay | N/A (account-based) |
| **Protocol surface** | Custom MASM note script | Existing MultisigGuardian component |
| **On-chain footprint** | 1 note commitment (private) | Account state updates (public) |
