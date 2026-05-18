# miden-x402

x402 v2 payments for the Miden network. This repo ships:

- **`miden-x402-facilitator`** — Rust facilitator service that verifies
  Miden P2ID payments against live testnet. `POST /verify`, `POST /settle`,
  `GET /supported`, `GET /health`.
- **`@miden-x402/merchant`** and **`@miden-x402/agent`** — Node.js SDK.
  Express + Hono merchant middleware; agent client wrapping `fetch` with
  P2ID payment via `@miden-sdk/miden-sdk` (WASM).
- **`miden-x402`** (Python) — FastAPI + Flask merchant middleware. No
  Python agent (no official Miden Python client SDK); use the Node agent
  for E2E.

Wire format is the [x402 v2 protocol](https://x402.org) over three
HTTP headers, base64-of-JSON encoded:

| Header | Direction | Body |
|---|---|---|
| `Payment-Required` | merchant → buyer (402) | `MidenPaymentRequired` |
| `Payment-Signature` | buyer → merchant (retry) | `MidenPaymentPayload` |
| `Payment-Response` | merchant → buyer (200) | `SettleResponse` |

The settled-at-commit model: a P2ID note in a committed Miden block is
the settlement event. The facilitator is a read-only verifier — it never
custodies keys.

The full wire contract is [`docs/protocol.md`](./docs/protocol.md). The
scheme is documented in [`docs/scheme_exact_miden.md`](./docs/scheme_exact_miden.md).
Deployment notes live in [`docs/deploy.md`](./docs/deploy.md).

## Quickstart

### 1. Run the facilitator (Rust)

```
cargo run -p miden-x402-facilitator --bin miden-x402-facilitator
```

By default it listens on `0.0.0.0:8080` and talks to
`https://rpc.testnet.miden.io`. See [`docs/deploy.md`](./docs/deploy.md)
for env vars.

Sanity checks:

```
$ curl -s http://localhost:8080/health
{"status":"ok"}

$ curl -s http://localhost:8080/supported
{"kinds":[{"x402Version":2,"scheme":"exact","network":"miden:testnet"}],"extensions":[]}
```

### 2. Add a paywall to your server

**Node (Express):**

```ts
import express from 'express';
import { paywall } from '@miden-x402/merchant/express';

const app = express();
app.get(
  '/weather',
  paywall({
    facilitatorUrl: 'http://localhost:8080',
    price: {
      amount: '1000',
      asset: '0x0a7d175ed63ec5200fb2ced86f6aa5',
      payTo: '0x...your-merchant-account-id...',
      tokenSymbol: 'USDC',
      decimals: 6,
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
    token_symbol="USDC",
    decimals=6,
)
config = PaywallConfig(facilitator_url="http://localhost:8080")

@app.get("/weather", dependencies=[Depends(paywall(price=price, config=config))])
def weather():
    return {"temperature": 21.5, "city": "Istanbul"}
```

Hono + Flask variants in [`sdks/node`](./sdks/node) and
[`sdks/python`](./sdks/python).

### 3. Pay a 402 from an agent

```ts
import { withMidenX402, createWasmSdkPayer } from '@miden-x402/agent';

const payer = createWasmSdkPayer({
  buyerAccountId: '0x...your-funded-buyer...',
  storePath: './buyer-store',
});

const fetchPaid = withMidenX402(fetch, { payer });
const r = await fetchPaid('http://localhost:3000/weather');
console.log(await r.json());
```

`withMidenX402` is a drop-in `fetch` wrapper: on `402`, it pays the P2ID
note, retries, and returns the resource.

For a no-Miden-network smoke check of the full HTTP wiring, see the
**Mock-mode demo** in [`sdks/node/README.md`](./sdks/node/README.md).

For a live-testnet smoke against a real on-chain P2ID note, see
[`docs/smoke-testnet.md`](./docs/smoke-testnet.md).

## Status

| Component | Milestone | Status |
|---|---|---|
| Wire types (`miden-x402-types`) | M1 | shipped |
| Facilitator (`miden-x402-facilitator`) | M2 | shipped |
| Header contract + `docs/protocol.md` | M3 | shipped |
| Live testnet smoke binary | M4a | shipped |
| Node SDK (merchant + agent + demos) | M4b | shipped |
| Python SDK (merchant + demos) | M5 | shipped |
| Quickstart + deploy + scheme docs | M6 | shipped |
| Private P2ID notes (unified verifier; `noteType: "private"`) | M7 | shipped |
| Guardian verify-before-prove (`settlement: "guardian-fast"`) | M8 | shipped (server-side; full E2E requires WASM SDK extensions) |

## License

Apache-2.0.
