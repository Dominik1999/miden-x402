"""Flask decorator that gates a view function behind a Miden x402 payment.

Usage:

    from flask import Flask, jsonify
    from miden_x402 import PaywallConfig, PriceTag
    from miden_x402.flask import paywall

    app = Flask(__name__)
    price = PriceTag(...)
    config = PaywallConfig(facilitator_url="http://localhost:8080")

    @app.get("/weather")
    @paywall(price=price, config=config)
    def weather():
        return jsonify(temperature=21.5)
"""

from __future__ import annotations

from functools import wraps
from typing import Callable

try:
    from flask import Response, jsonify, request
except ImportError as e:  # pragma: no cover
    raise ImportError(
        "Flask is not installed. `pip install miden-x402[flask]`."
    ) from e

from .core import (
    Paid,
    PaywallConfig,
    PriceTag,
    Reject,
    process_payment,
)
from .headers import (
    PAYMENT_REQUIRED_HEADER,
    PAYMENT_RESPONSE_HEADER,
    PAYMENT_SIGNATURE_HEADER,
    encode_payment_required_header,
    encode_payment_response_header,
)
from .types import ResourceInfo


def paywall(
    price: PriceTag,
    config: PaywallConfig,
    *,
    description: str | None = None,
    mime_type: str | None = None,
) -> Callable:
    def decorator(view_fn: Callable) -> Callable:
        @wraps(view_fn)
        def wrapper(*args, **kwargs):
            resource = ResourceInfo(
                url=request.url,
                description=description or request.path,
                mime_type=mime_type,
            )
            signature = request.headers.get(PAYMENT_SIGNATURE_HEADER)
            outcome = process_payment(
                signature_header=signature,
                price=price,
                resource=resource,
                config=config,
            )
            if isinstance(outcome, Paid):
                resp = view_fn(*args, **kwargs)
                # Coerce non-Response returns through jsonify so we can attach
                # the Payment-Response header before the view returns.
                if not isinstance(resp, Response):
                    if isinstance(resp, (dict, list)):
                        resp = jsonify(resp)
                    else:
                        resp = Response(resp)
                resp.headers[PAYMENT_RESPONSE_HEADER] = encode_payment_response_header(
                    outcome.settle
                )
                return resp
            if isinstance(outcome, Reject):
                body = outcome.body.model_dump(by_alias=True, exclude_none=True)
                response = jsonify(body)
                response.status_code = 402
                response.headers[PAYMENT_REQUIRED_HEADER] = encode_payment_required_header(
                    outcome.body
                )
                return response
            raise RuntimeError(f"unreachable PaymentOutcome variant: {outcome!r}")

        return wrapper

    return decorator
