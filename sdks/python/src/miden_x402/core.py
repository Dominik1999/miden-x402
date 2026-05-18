"""Framework-agnostic merchant logic for x402 v2 on Miden.

The merchant never verifies a payment itself — it just forwards the
decoded payload + the matched requirements to the configured facilitator
and acts on the response.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Optional, Union

import httpx
from pydantic import ValidationError

from .headers import (
    decode_payment_signature_header,
)
from .types import (
    ASSET_TRANSFER_METHOD_P2ID,
    EXACT_SCHEME,
    MIDEN_TESTNET,
    MidenExactExtra,
    MidenPaymentPayload,
    MidenPaymentRequired,
    MidenPaymentRequirements,
    ResourceInfo,
    SettleResponse,
    SettleSuccess,
    VerifyResponse,
)

log = logging.getLogger(__name__)


@dataclass
class PriceTag:
    """What the merchant wants paid for the gated resource."""

    amount: str
    asset: str
    pay_to: str
    token_symbol: str
    decimals: int
    network: str = MIDEN_TESTNET
    note_type: str = "public"
    max_timeout_seconds: int = 120
    # Settlement model. ``"commit"`` (default) = Phase A; ``"guardian-fast"``
    # = Phase B verify-before-prove. The merchant code is identical for
    # both; the only difference is that on a guardian-fast 402 we acquire a
    # server-issued ``serial_num`` from the facilitator beforehand.
    settlement: str = "commit"


@dataclass
class PaywallConfig:
    facilitator_url: str
    timeout_seconds: float = 10.0
    httpx_client: Optional[httpx.Client] = None


@dataclass
class Paid:
    settle: SettleSuccess


@dataclass
class Reject:
    body: MidenPaymentRequired


PaymentOutcome = Union[Paid, Reject]


def build_requirements(price: PriceTag) -> MidenPaymentRequirements:
    return MidenPaymentRequirements(
        scheme=EXACT_SCHEME,
        network=price.network,
        amount=price.amount,
        asset=price.asset,
        pay_to=price.pay_to,
        max_timeout_seconds=price.max_timeout_seconds,
        extra=MidenExactExtra(
            asset_transfer_method=ASSET_TRANSFER_METHOD_P2ID,
            token_symbol=price.token_symbol,
            decimals=price.decimals,
            note_type=price.note_type,  # type: ignore[arg-type]
            settlement=price.settlement,  # type: ignore[arg-type]
        ),
    )


def acquire_guardian_challenge(
    requirements: MidenPaymentRequirements,
    config: PaywallConfig,
) -> MidenPaymentRequirements:
    """For ``settlement: 'guardian-fast'``, asks the facilitator to issue a
    server-generated ``serial_num`` and returns the requirements with
    ``serial_num`` and ``guardian_url`` populated in ``extra``."""
    body = {
        "paymentRequirements": requirements.model_dump(by_alias=True, exclude_none=True),
    }
    client = config.httpx_client or httpx.Client(timeout=config.timeout_seconds)
    owns_client = config.httpx_client is None
    base = config.facilitator_url.rstrip("/")
    try:
        resp = client.post(f"{base}/guardian/challenge", json=body)
    finally:
        if owns_client:
            client.close()
    resp.raise_for_status()
    data = resp.json()
    new_extra = MidenExactExtra(
        **{
            **requirements.extra.model_dump(by_alias=False, exclude_none=True),
            "serial_num": data["serialNum"],
            "guardian_url": base,
        }
    )
    return MidenPaymentRequirements(
        scheme=requirements.scheme,
        network=requirements.network,
        amount=requirements.amount,
        asset=requirements.asset,
        pay_to=requirements.pay_to,
        max_timeout_seconds=requirements.max_timeout_seconds,
        extra=new_extra,
    )


def build_payment_required(
    price: PriceTag,
    resource: ResourceInfo,
    error: str | None = None,
) -> MidenPaymentRequired:
    return MidenPaymentRequired(
        x402_version=2,
        accepts=[build_requirements(price)],
        resource=resource,
        error=error,
    )


def try_decode_signature(header_value: str | None) -> MidenPaymentPayload | None:
    """Returns ``None`` if the header is missing or unparseable."""
    if not header_value:
        return None
    try:
        return decode_payment_signature_header(header_value)
    except (ValidationError, ValueError) as e:
        log.debug("payment signature header rejected: %s", e)
        return None


def _facilitator_url(base: str, path: str) -> str:
    return f"{base.rstrip('/')}{path}"


def verify_with_facilitator(
    payload: MidenPaymentPayload,
    requirements: MidenPaymentRequirements,
    config: PaywallConfig,
) -> VerifyResponse:
    body = {
        "x402Version": 2,
        "paymentPayload": payload.model_dump(by_alias=True, exclude_none=True),
        "paymentRequirements": requirements.model_dump(by_alias=True, exclude_none=True),
    }
    client = config.httpx_client or httpx.Client(timeout=config.timeout_seconds)
    owns_client = config.httpx_client is None
    try:
        resp = client.post(_facilitator_url(config.facilitator_url, "/verify"), json=body)
    finally:
        if owns_client:
            client.close()
    return _parse_facilitator_response(resp, VerifyResponse)  # type: ignore[arg-type]


@dataclass
class _SettleResult:
    ok: bool
    settle: SettleSuccess | None = field(default=None)
    error: str | None = field(default=None)


def settle_with_facilitator(
    payload: MidenPaymentPayload,
    requirements: MidenPaymentRequirements,
    config: PaywallConfig,
) -> _SettleResult:
    body = {
        "x402Version": 2,
        "paymentPayload": payload.model_dump(by_alias=True, exclude_none=True),
        "paymentRequirements": requirements.model_dump(by_alias=True, exclude_none=True),
    }
    # Route to the right endpoint based on the negotiated settlement model.
    settlement = getattr(requirements.extra, "settlement", "commit") or "commit"
    path = "/guardian/settle" if settlement == "guardian-fast" else "/settle"
    client = config.httpx_client or httpx.Client(timeout=config.timeout_seconds)
    owns_client = config.httpx_client is None
    try:
        try:
            resp = client.post(_facilitator_url(config.facilitator_url, path), json=body)
        except httpx.HTTPError as e:
            return _SettleResult(ok=False, error=f"facilitator unreachable: {e}")
    finally:
        if owns_client:
            client.close()

    if resp.is_success:
        parsed = _parse_facilitator_response(resp, SettleResponse)  # type: ignore[arg-type]
        if isinstance(parsed, SettleSuccess):
            return _SettleResult(ok=True, settle=parsed)
        return _SettleResult(ok=False, error=parsed.error_reason)

    # Non-2xx: facilitator returns x402-shaped error body.
    reason = f"facilitator returned {resp.status_code}"
    try:
        body_json = resp.json()
        reason = body_json.get("invalidReasonDetails") or body_json.get("invalidReason") or reason
    except Exception:  # noqa: BLE001
        pass
    return _SettleResult(ok=False, error=reason)


def _parse_facilitator_response(resp: httpx.Response, type_):  # type: ignore[no-untyped-def]
    """Internal: validate a 2xx response into the discriminated union."""
    from pydantic import TypeAdapter

    return TypeAdapter(type_).validate_json(resp.content)


def process_payment(
    *,
    signature_header: str | None,
    price: PriceTag,
    resource: ResourceInfo,
    config: PaywallConfig,
) -> PaymentOutcome:
    """Returns either ``Paid`` (call into the gated handler, attach the
    Payment-Response header) or ``Reject`` (emit a 402 with this body).

    For ``price.settlement == 'guardian-fast'`` and the first request (no
    ``Payment-Signature`` header), this also acquires a server-generated
    ``serial_num`` from the facilitator and embeds it in the 402's
    ``extra.serial_num``.
    """
    payload = try_decode_signature(signature_header)
    if payload is None:
        requirements = build_requirements(price)
        if price.settlement == "guardian-fast":
            try:
                requirements = acquire_guardian_challenge(requirements, config)
            except Exception as e:  # noqa: BLE001
                # Surface the failure in the 402 body so the buyer sees an
                # actionable error rather than a generic "verification
                # failed".
                return Reject(
                    body=MidenPaymentRequired(
                        x402_version=2,
                        accepts=[requirements],
                        resource=resource,
                        error=f"failed to acquire guardian challenge: {e}",
                    )
                )
        return Reject(
            body=MidenPaymentRequired(
                x402_version=2,
                accepts=[requirements],
                resource=resource,
            )
        )
    # Retry path: use the requirements echoed back in `payload.accepted` so
    # we honour the guardian challenge serial_num issued in the 402.
    requirements = payload.accepted
    result = settle_with_facilitator(payload, requirements, config)
    if result.ok and result.settle is not None:
        return Paid(settle=result.settle)
    return Reject(
        body=build_payment_required(price, resource, error=result.error or "verification failed")
    )
