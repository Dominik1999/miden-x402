"""FastAPI demo merchant gated by Miden x402.

Run:

    MERCHANT_PAY_TO=0x... \\
    ASSET=0x0a7d175ed63ec5200fb2ced86f6aa5 \\
    FACILITATOR_URL=http://localhost:8080 \\
    .venv/bin/uvicorn examples.demo-merchant-fastapi.server:app --port 3001
"""

from __future__ import annotations

import os
import sys

from fastapi import Depends, FastAPI

from miden_x402 import PaywallConfig, PriceTag
from miden_x402.fastapi import install_paywall_exception_handler, paywall


def _required(key: str) -> str:
    v = os.environ.get(key)
    if not v:
        print(f"missing required env var: {key}", file=sys.stderr)
        sys.exit(1)
    return v


PAY_TO = _required("MERCHANT_PAY_TO")
ASSET = os.environ.get("ASSET", "0x0a7d175ed63ec5200fb2ced86f6aa5")
FACILITATOR_URL = os.environ.get("FACILITATOR_URL", "http://localhost:8080")
AMOUNT = os.environ.get("AMOUNT", "1000")

price = PriceTag(
    amount=AMOUNT,
    asset=ASSET,
    pay_to=PAY_TO,
    token_symbol="USDC",
    decimals=6,
)
price_private = PriceTag(
    amount=AMOUNT,
    asset=ASSET,
    pay_to=PAY_TO,
    token_symbol="USDC",
    decimals=6,
    note_type="private",
)
config = PaywallConfig(facilitator_url=FACILITATOR_URL)

app = FastAPI()
install_paywall_exception_handler(app)


@app.get(
    "/weather",
    dependencies=[
        Depends(
            paywall(
                price=price,
                config=config,
                description="current weather (public note)",
                mime_type="application/json",
            )
        )
    ],
)
def weather() -> dict[str, object]:
    return {"temperature": 21.5, "city": "Istanbul"}


# Private-note variant of the same paywall. Same merchant code path; only the
# ``PriceTag.note_type`` differs. The buyer's agent picks up the
# ``noteType: 'private'`` hint from the 402 response and routes its payer
# accordingly. The facilitator runs the same unified verification pipeline
# for both note types.
@app.get(
    "/weather-private",
    dependencies=[
        Depends(
            paywall(
                price=price_private,
                config=config,
                description="current weather (private note)",
                mime_type="application/json",
            )
        )
    ],
)
def weather_private() -> dict[str, object]:
    return {"temperature": 21.5, "city": "Istanbul"}
