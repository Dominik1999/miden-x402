# miden-x402

x402 v2 payments for the Miden network, realised as a module bolted onto
the OpenZeppelin [Guardian](https://github.com/OpenZeppelin/guardian).
One operator runs one Guardian process that handles both the multisig
private-state lifecycle (Guardian's existing job) and the x402 facilitator
lifecycle (verify-before-prove + batched settlement) — a single trust
anchor for buyers, merchants, and AI agents.

Design write-up: [`ideas/DESIGN.md`](./ideas/DESIGN.md). Wire contract:
[`docs/protocol.md`](./docs/protocol.md). What it would take to integrate
this upstream into OZ Guardian itself:
[`docs/UPSTREAM_WISHLIST.md`](./docs/UPSTREAM_WISHLIST.md).

## What ships

- **`miden-x402-facilitator`** — Rust binary `guardian-facilitator` that
  runs a Guardian server with the `/x402/*` routes mounted on the same
  port. Verify-before-prove, async batch settlement via a remote prover,
  pluggable mandate policy, persistent challenge / reservation / batch
  storage.
- **`@miden-x402/merchant` + `@miden-x402/types`** — Node merchant SDK
  with Express + Hono adapters.
- **`miden-x402` (Python)** — FastAPI + Flask merchant middleware.
- **`@miden-x402/agent`** — Node agent client stub. End-to-end agent
  payments from JS need `@miden-sdk/miden-sdk` extensions tracked in
  [`docs/UPSTREAM_WISHLIST.md`](./docs/UPSTREAM_WISHLIST.md); use the
  `miden-multisig-client` Rust crate from the OZ Guardian repo today.

## Wire format at a glance

The 402 response carries one entry in `accepts[]`:

```json
{
  "scheme": "miden-p2id-private",
  "network": "miden:testnet",
  "amount": "1000",
  "asset": "0x0a7d175ed63ec5200fb2ced86f6aa5",
  "payTo": "0x103f8a1ad4b983104aec0412ab0b0d",
  "maxTimeoutSeconds": 120,
  "extra": {
    "noteTag": "weather.api",
    "serialNum": "0x<server-issued 32-byte word>"
  }
}
```

The `Payment-Signature` payload carries a base64-encoded
`TransactionInputs` + Falcon signature + `TransactionSummary` +
`NoteFile::NoteDetails` blob — i.e., a signed-but-not-yet-proven Miden
transaction. The Guardian-facilitator verifies the signature against the
buyer's cosigner commitments, reserves the nullifiers, and replies with a
signed receipt; the actual prove + submit happens in a background batch
worker.

Full protocol: [`docs/protocol.md`](./docs/protocol.md).

## Quickstart

### 1. Run the Guardian-facilitator

```bash
export MIDEN_X402_REMOTE_PROVER_URL=https://prover.testnet.miden.io
export MIDEN_X402_NETWORK=miden:testnet
export MIDEN_X402_STORAGE_ROOT=/var/x402/state
cargo run --release -p miden-x402-facilitator --bin guardian-facilitator
```

Listens on `0.0.0.0:8080` by default; Guardian routes mount under `/`
and x402 routes under `/x402/*`. See [`docs/deploy.md`](./docs/deploy.md)
for the full env-var reference.

Sanity checks:

```bash
$ curl -s http://localhost:8080/x402/health
{"status":"ok"}

$ curl -s http://localhost:8080/x402/supported
{"kinds":[{"x402Version":2,"scheme":"miden-p2id-private","network":"miden:testnet"}],"extensions":["miden-guardian-facilitator"]}

$ curl -s http://localhost:8080/x402/pubkey
{"commitment":"0x...","pubkeyB64":"..."}
```

### 2. Gate a route with the merchant SDK

**Node (Express):**

```ts
import express from 'express';
import { paywall } from '@miden-x402/merchant/express';

const app = express();
app.get(
  '/weather',
  paywall({
    facilitatorUrl: 'http://localhost:8080',
    merchantAuth: myGuardianAuth, // signs x-pubkey/x-signature/x-timestamp
    price: {
      amount: '1000',
      asset: '0x0a7d175ed63ec5200fb2ced86f6aa5',
      payTo: '0x...your-merchant-account-id...',
      noteTag: 'weather.api',
    },
  }),
  (_req, res) => res.json({ temperature: 21.5, city: 'Istanbul' }),
);
app.listen(3000);
```

**Python (FastAPI):**

```python
from fastapi import Depends, FastAPI
from miden_x402 import PaywallConfig, PriceTag
from miden_x402.fastapi import paywall

app = FastAPI()
price = PriceTag(
    amount="1000",
    asset="0x0a7d175ed63ec5200fb2ced86f6aa5",
    pay_to="0x...your-merchant-account-id...",
    note_tag="weather.api",
)
config = PaywallConfig(
    facilitator_url="http://localhost:8080",
    sign_request=my_guardian_signer,
)

@app.get("/weather", dependencies=[Depends(paywall(price=price, config=config))])
def weather():
    return {"temperature": 21.5, "city": "Istanbul"}
```

The merchant must authenticate `POST /x402/challenge` and `POST /x402/settle`
as its own Guardian-registered account — see
[`docs/protocol.md`](./docs/protocol.md) §B.

### 3. Pay a 402 from an agent

End-to-end agent flow from JS is blocked on WASM SDK extensions
([UPSTREAM_WISHLIST.md](./docs/UPSTREAM_WISHLIST.md)). For Rust agents,
use [`miden-multisig-client`](https://github.com/OpenZeppelin/guardian/tree/main/crates/miden-multisig-client)
from the OZ Guardian repo today.

## Status

| Component | Status |
|---|---|
| Wire types (`miden-x402-types`) | shipped |
| Guardian-facilitator binary (`guardian-facilitator`) | shipped |
| Verify-before-prove pipeline | shipped |
| Async batch settle worker | shipped |
| Mandate policy hook (`AllowAll` default) | shipped |
| Falcon-signed settle receipts | shipped (facilitator-owned key; see UPSTREAM_WISHLIST.md) |
| Node merchant SDK (Express + Hono) | shipped |
| Python merchant SDK (FastAPI + Flask) | shipped |
| Node agent SDK | stub (blocked on WASM SDK extensions) |
| `check_nullifiers` backstop | wired in lib, no-op in binary (see UPSTREAM_WISHLIST.md) |
| Inclusion-bridge (reservation → consumed on canonical) | see UPSTREAM_WISHLIST.md |
| Postgres-backed x402 repos | future work (trait ready) |

## License

Apache-2.0.
