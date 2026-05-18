# miden-x402 — Python SDK

Merchant SDK for x402 v2 on Miden. FastAPI + Flask adapters, no agent
(Python has no official Miden client SDK; use the Node agent for E2E).

Wire format matches [`docs/protocol.md`](../../docs/protocol.md) of the
parent repo.

## Install

```
pip install -e '.[fastapi,flask]'
```

## Quickstart (FastAPI)

```python
from fastapi import FastAPI
from miden_x402 import PaywallConfig, PriceTag
from miden_x402.fastapi import install_paywall_exception_handler, paywall

app = FastAPI()
install_paywall_exception_handler(app)  # required so 402 bodies aren't wrapped in `detail`

price = PriceTag(
    amount="1000",
    asset="0x0a7d175ed63ec5200fb2ced86f6aa5",
    pay_to="0x...your-merchant-account-id...",
    token_symbol="USDC",
    decimals=6,
)
config = PaywallConfig(facilitator_url="http://localhost:8080")

@app.get("/weather")
def weather():
    return {"temperature": 21.5, "city": "Istanbul"}

# Wire the paywall in front of the route.
app.middleware("http")(paywall(price=price, config=config, path="/weather"))
```

## Quickstart (Flask)

```python
from flask import Flask, jsonify
from miden_x402 import PaywallConfig, PriceTag
from miden_x402.flask import paywall

app = Flask(__name__)

price = PriceTag(
    amount="1000",
    asset="0x0a7d175ed63ec5200fb2ced86f6aa5",
    pay_to="0x...your-merchant-account-id...",
    token_symbol="USDC",
    decimals=6,
)
config = PaywallConfig(facilitator_url="http://localhost:8080")

@app.get("/weather")
@paywall(price=price, config=config)
def weather():
    return jsonify(temperature=21.5, city="Istanbul")
```

### Private notes (M7)

Set `note_type="private"` on the ``PriceTag`` to opt into settled-at-commit
private P2ID. The merchant code is unchanged; the facilitator and Node
agent handle the off-chain ``NoteFile`` blob transport transparently.

```python
price_private = PriceTag(
    amount="1000",
    asset="0x0a7d175ed63ec5200fb2ced86f6aa5",
    pay_to="0x...your-merchant-account-id...",
    token_symbol="USDC",
    decimals=6,
    note_type="private",
)
```

A live `/weather-private` route is wired in
[`examples/demo-merchant-fastapi`](./examples/demo-merchant-fastapi) and
[`examples/demo-merchant-flask`](./examples/demo-merchant-flask).

### Guardian verify-before-prove (M8)

Set ``settlement="guardian-fast"`` to opt into the Guardian flow. The
merchant SDK auto-calls the facilitator's ``/guardian/challenge`` endpoint
on the first request and inlines the server-issued ``serial_num`` into the
402 response; on the retry it forwards to ``/guardian/settle``. Requires
the facilitator to be started with ``MIDEN_X402_GUARDIAN_ENABLED=true``.

```python
price_fast = PriceTag(
    amount="1000",
    asset="0x0a7d175ed63ec5200fb2ced86f6aa5",
    pay_to="0x...",
    token_symbol="USDC",
    decimals=6,
    note_type="private",
    settlement="guardian-fast",
)
```

See [`docs/protocol.md`](../../docs/protocol.md) §A.2.7 for the full
wire contract and [`docs/deploy.md`](../../docs/deploy.md) for the
Guardian environment variables on the facilitator side.

## Cross-language E2E

Use the Node agent (from `sdks/node`) to drive a Python merchant:

```
# in shell A — Rust facilitator
cargo run -p miden-x402-facilitator --bin miden-x402-facilitator

# in shell B — Python FastAPI merchant
MERCHANT_PAY_TO=0x... \
  uvicorn examples.demo-merchant-fastapi.server:app --port 3001

# in shell C — Node agent in mock mode
AGENT_MOCK=1 TARGET_URL=http://localhost:3001/weather \
  MOCK_NOTE_ID=0x... MOCK_TX_ID=0x... MOCK_SENDER=0x... MOCK_BLOCK_NUM=1 \
  pnpm --filter demo-agent start
```

## Demos

- [`examples/demo-merchant-fastapi`](./examples/demo-merchant-fastapi)
- [`examples/demo-merchant-flask`](./examples/demo-merchant-flask)
