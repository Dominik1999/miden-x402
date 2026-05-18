"""Flask demo merchant gated by Miden x402.

Run:

    MERCHANT_PAY_TO=0x... \\
    ASSET=0x0a7d175ed63ec5200fb2ced86f6aa5 \\
    FACILITATOR_URL=http://localhost:8080 \\
    PORT=3001 \\
    .venv/bin/python examples/demo-merchant-flask/server.py
"""

from __future__ import annotations

import os
import sys

from flask import Flask, jsonify

from miden_x402 import PaywallConfig, PriceTag
from miden_x402.flask import paywall


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
PORT = int(os.environ.get("PORT", "3001"))

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

app = Flask(__name__)


@app.get("/weather")
@paywall(
    price=price,
    config=config,
    description="current weather (public note)",
    mime_type="application/json",
)
def weather():
    return jsonify(temperature=21.5, city="Istanbul")


# Private-note variant. Same merchant code path; only ``note_type`` differs.
@app.get("/weather-private")
@paywall(
    price=price_private,
    config=config,
    description="current weather (private note)",
    mime_type="application/json",
)
def weather_private():
    return jsonify(temperature=21.5, city="Istanbul")


if __name__ == "__main__":
    app.run(port=PORT)
