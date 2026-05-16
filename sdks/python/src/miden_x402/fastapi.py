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
    from fastapi import FastAPI, Request, Response
    from fastapi.responses import JSONResponse
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
from .types import MidenPaymentRequired, ResourceInfo


class PaywallRejected(Exception):
    """Raised by the paywall dependency when a request must be answered
    with a 402. Pair with :func:`install_paywall_exception_handler` so
    FastAPI emits the canonical x402 PaymentRequired body (and not the
    default ``{"detail": ...}`` wrapper).
    """

    def __init__(self, body: MidenPaymentRequired, header_value: str) -> None:
        super().__init__("payment required")
        self.body = body
        self.header_value = header_value


async def _paywall_exception_handler(
    _request: Request, exc: PaywallRejected
) -> JSONResponse:
    return JSONResponse(
        status_code=402,
        content=exc.body.model_dump(by_alias=True, exclude_none=True),
        headers={PAYMENT_REQUIRED_HEADER: exc.header_value},
    )


def install_paywall_exception_handler(app: FastAPI) -> None:
    """Register the exception handler that emits the canonical 402 body.

    Call once at app setup. Without this, FastAPI's default handler would
    wrap the body in ``{"detail": ...}`` which buyers shouldn't have to
    parse around.
    """
    app.add_exception_handler(PaywallRejected, _paywall_exception_handler)


def paywall(
    price: PriceTag,
    config: PaywallConfig,
    *,
    description: str | None = None,
    mime_type: str | None = None,
) -> Callable[[Request, Response], None]:
    """Returns a FastAPI dependency that enforces the paywall.

    The dependency raises :class:`PaywallRejected` on the 402 path; this
    only renders correctly if the app has called
    :func:`install_paywall_exception_handler`.
    """

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
            raise PaywallRejected(outcome.body, header_value)
        raise RuntimeError(f"unreachable PaymentOutcome variant: {outcome!r}")

    return dependency
