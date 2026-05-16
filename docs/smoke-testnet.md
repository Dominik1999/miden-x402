# Live testnet smoke test

The `miden-x402-smoke-testnet` binary validates the facilitator's verification
pipeline end-to-end against the real Miden testnet. It takes a public P2ID
note that already exists on chain, constructs the matching
`MidenPaymentRequirements` / `MidenPaymentPayload`, and calls
`verifier::verify()` and `verifier::settle()` directly (no HTTP layer).

This is a manual operator tool — the operator brings a funded Miden buyer
account, a merchant account, and a public P2ID note they have already
submitted via `miden-client-cli` or the M4b Node agent.

## Prerequisites

You need:

1. A funded buyer account on Miden testnet (faucet
   `0x0a7d175ed63ec5200fb2ced86f6aa5`, default token).
2. A merchant account ID (any account; for a smoke run a second wallet you
   control is fine).
3. A public P2ID note already on chain, paying the merchant from the buyer,
   one fungible asset, in atomic units (1000 = default).

The simplest way to create the note is the official `miden-client-cli`. From
its repo:

```
miden-client account new wallet --network testnet     # buyer
miden-client account new wallet --network testnet     # merchant
miden-client faucet mint --network testnet            # fund buyer
miden-client tx new pay-to-id \
    --network testnet \
    --sender <buyer-id> \
    --target <merchant-id> \
    --asset <faucet-id> \
    --amount 1000 \
    --note-type public
miden-client sync --network testnet
miden-client tx list --network testnet                # note id, tx id, block num
```

Record the resulting `noteId`, `transactionId`, and the block in which the
note was committed.

## Running

Set the env vars and run:

```
export MIDEN_X402_SMOKE_BUYER=0x...        # buyer account id
export MIDEN_X402_SMOKE_MERCHANT=0x...     # merchant account id (payTo)
export MIDEN_X402_SMOKE_NOTE_ID=0x...      # note id (32 bytes / 64 hex chars)
export MIDEN_X402_SMOKE_TX_ID=0x...        # create-note tx id
export MIDEN_X402_SMOKE_BLOCK_NUM=12345    # block in which the note was committed

# optional overrides
# export MIDEN_X402_SMOKE_FAUCET=0x0a7d175ed63ec5200fb2ced86f6aa5
# export MIDEN_X402_SMOKE_AMOUNT=1000
# export MIDEN_X402_SMOKE_TOKEN_SYMBOL=USDC
# export MIDEN_X402_SMOKE_DECIMALS=6
# export MIDEN_X402_SMOKE_RPC_URL=https://rpc.testnet.miden.io
# export MIDEN_X402_SMOKE_FRESHNESS=1000000   # very wide window for manual runs

cargo run -p miden-x402-facilitator --bin miden-x402-smoke-testnet
```

Expected output on success:

```
verify: VALID payer=0x...
settle: SUCCESS payer=0x... tx=0x... network=miden:testnet
```

Any failure prints the underlying `FacilitatorError` and exits non-zero.

## HTTP probes against the live facilitator

Run the facilitator binary in another shell and probe the bare endpoints:

```
cargo run -p miden-x402-facilitator --bin miden-x402-facilitator
```

```
$ curl -s http://localhost:8080/health
{"status":"ok"}

$ curl -s http://localhost:8080/supported
{"kinds":[{"x402Version":2,"scheme":"exact","network":"miden:testnet"}],"extensions":[]}
```

These two probes plus a green smoke run are the M4a sign-off.

## When this is useful

- After a `miden-client` version bump — re-run to confirm the on-chain
  P2ID note shape still matches `P2idNoteStorage::try_from`.
- After changing `verifier.rs` checks — re-run to confirm the verifier still
  resolves real notes (the unit tests use a `MockNode`).
- Before cutting a release — confirm the binary still starts and the
  `/health` + `/supported` endpoints are reachable.
