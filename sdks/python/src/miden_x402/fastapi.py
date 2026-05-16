"""FastAPI dependency that gates a route behind a Miden x402 payment.

Usage:

    from fastapi import Depends, FastAPI
    from miden_x402 import PaywallConfig, PriceTag
    from miden_x402.fastapi import paywall

    app = FastAPI()
    price = PriceTag(...)
    config = PaywallConfig(facilitator_url="http://localhost:8080")

    @app.get("/weather", dependencies=[Depends(paywall(price, config))])
    def weather():
        return {"temperature": 21.5}

The dependency emits a ``402 Payment Required`` (via ``HTTPException`` on
the failure path) when no signature is present or when the facilitator
rejects, and attaches the ``Payment-Response`` header on success.
"""

from __future__ import annotations

from typing import Callable

try:
    from fastapi import HTTPException, Request, Response
except ImportError as e:  # pragma: no cover
    raise ImportError(
        "FastAPI is not installed. `pip install miden-x402[fastapi]`."
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
) -> Callable[[Request, Response], None]:
    """Returns a FastAPI dependency that enforces the paywall."""

    def dependency(request: Request, response: Response) -> None:
        resource = ResourceInfo(
            url=str(request.url),
            description=description or request.url.path,
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
            response.headers[PAYMENT_RESPONSE_HEADER] = encode_payment_response_header(
                outcome.settle
            )
            return
        if isinstance(outcome, Reject):
            header_value = encode_payment_required_header(outcome.body)
            body_dict = outcome.body.model_dump(by_alias=True, exclude_none=True)
            raise HTTPException(
                status_code=402,
                detail=body_dict,
                headers={PAYMENT_REQUIRED_HEADER: header_value},
            )
        raise RuntimeError(f"unreachable PaymentOutcome variant: {outcome!r}")

    return dependency
