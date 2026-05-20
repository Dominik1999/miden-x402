# miden-x402 — Python SDK

Merchant SDK for the `miden-p2id-private` x402 scheme. FastAPI + Flask
adapters, no agent (Python has no official Miden client SDK; use the
Rust `miden-multisig-client` from the OZ Guardian repo for agents).

## Installation

```bash
pip install miden-x402
```

## Merchant

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
    facilitator_url="https://facilitator.miden.example",
    sign_request=my_guardian_signer,
)

@app.get("/weather", dependencies=[Depends(paywall(price=price, config=config))])
def weather():
    return {"temperature": 21.5, "city": "Istanbul"}
```

`sign_request` is a callable that takes the JSON request body and
returns the Guardian-style auth headers (`x-pubkey`, `x-signature`,
`x-timestamp`). The merchant is a Guardian-registered account; the
operator provisions a Falcon cosigner key and wires up the signer.

For Flask, import `paywall` from `miden_x402.flask`. Same shape.

## Header codec

For ports of this SDK to other languages, the canonical reference is
[`docs/protocol.md`](../../docs/protocol.md). Pydantic models in
[`src/miden_x402/types.py`](src/miden_x402/types.py) mirror the Rust
types byte-for-byte (camelCase aliases via `populate_by_name`).

## See also

- Wire contract: [`docs/protocol.md`](../../docs/protocol.md)
- Design: [`ideas/DESIGN.md`](../../ideas/DESIGN.md)
- Deployment: [`docs/deploy.md`](../../docs/deploy.md)
- Upstream wishlist: [`docs/UPSTREAM_WISHLIST.md`](../../docs/UPSTREAM_WISHLIST.md)
