"""Framework-agnostic merchant logic for the Guardian-facilitator wire.

The merchant never verifies a payment itself — it forwards the decoded
payload + the matched requirements to the configured Guardian-facilitator
and acts on the response.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Callable, Optional, Union

import httpx
from pydantic import TypeAdapter, ValidationError

from .headers import decode_payment_signature_header
from .types import (
    MIDEN_P2ID_PRIVATE_SCHEME,
    MIDEN_TESTNET,
    MidenP2idPrivateExtra,
    MidenPaymentPayload,
    MidenPaymentRequired,
    MidenPaymentRequirements,
    ResourceInfo,
    SettleResponse,
    SettleSuccess,
    VerifyResponse,
)

log = logging.getLogger(__name__)

#: Callback that signs a Guardian-style request. Returns the
#: (x-pubkey, x-signature, x-timestamp) headers as a dict.
MerchantSigner = Callable[[str], dict[str, str]]


@dataclass
class PriceTag:
    """What the merchant wants paid for the gated resource."""

    amount: str
    asset: str
    pay_to: str
    note_tag: str
    network: str = MIDEN_TESTNET
    max_timeout_seconds: int = 120


@dataclass
class PaywallConfig:
    facilitator_url: str
    timeout_seconds: float = 10.0
    httpx_client: Optional[httpx.Client] = None
    #: Optional Guardian-style merchant auth. Required for
    #: ``POST /x402/challenge`` and ``POST /x402/settle``; the SDK
    #: passes the JSON request body to the callback, which returns the
    #: signed ``x-pubkey``/``x-signature``/``x-timestamp`` headers.
    sign_request: Optional[MerchantSigner] = None


@dataclass
class Paid:
    settle: SettleSuccess


@dataclass
class Reject:
    body: MidenPaymentRequired


PaymentOutcome = Union[Paid, Reject]


def build_requirements(price: PriceTag) -> MidenPaymentRequirements:
    return MidenPaymentRequirements(
        scheme=MIDEN_P2ID_PRIVATE_SCHEME,
        network=price.network,
        amount=price.amount,
        asset=price.asset,
        pay_to=price.pay_to,
        max_timeout_seconds=price.max_timeout_seconds,
        extra=MidenP2idPrivateExtra(note_tag=price.note_tag),
    )


def acquire_challenge(
    requirements: MidenPaymentRequirements,
    config: PaywallConfig,
) -> MidenPaymentRequirements:
    """Asks the Guardian-facilitator to issue a ``serial_num`` and returns
    the requirements with ``extra.serial_num`` populated.
    """
    body = {
        "paymentRequirements": requirements.model_dump(by_alias=True, exclude_none=True),
    }
    return _with_client(config, lambda client: _challenge_call(client, config, body, requirements))


def _challenge_call(
    client: httpx.Client,
    config: PaywallConfig,
    body: dict,
    requirements: MidenPaymentRequirements,
) -> MidenPaymentRequirements:
    base = config.facilitator_url.rstrip("/")
    body_json = httpx._content.json_dumps_for_request(body)  # type: ignore[attr-defined]
    headers = {"content-type": "application/json"}
    if config.sign_request is not None:
        headers.update(config.sign_request(body_json))
    resp = client.post(f"{base}/x402/challenge", content=body_json, headers=headers)
    resp.raise_for_status()
    data = resp.json()
    new_extra = MidenP2idPrivateExtra(
        note_tag=requirements.extra.note_tag,
        serial_num=data["serialNum"],
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
    serial_num: str | None = None,
) -> MidenPaymentRequired:
    requirements = build_requirements(price)
    if serial_num:
        requirements.extra.serial_num = serial_num
    return MidenPaymentRequired(
        x402_version=2,
        accepts=[requirements],
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
    return _with_client(config, lambda client: _post_endpoint(client, config, "/x402/verify", body, VerifyResponse))


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
    return _with_client(config, lambda client: _settle_call(client, config, body))


def _settle_call(client: httpx.Client, config: PaywallConfig, body: dict) -> _SettleResult:
    import json
    body_json = json.dumps(body)
    headers = {"content-type": "application/json"}
    if config.sign_request is not None:
        headers.update(config.sign_request(body_json))
    try:
        resp = client.post(
            _facilitator_url(config.facilitator_url, "/x402/settle"),
            content=body_json,
            headers=headers,
        )
    except httpx.HTTPError as e:
        return _SettleResult(ok=False, error=f"facilitator unreachable: {e}")

    if resp.is_success:
        parsed = TypeAdapter(SettleResponse).validate_json(resp.content)
        if isinstance(parsed, SettleSuccess):
            return _SettleResult(ok=True, settle=parsed)
        return _SettleResult(ok=False, error=parsed.error_reason)

    reason = f"facilitator returned {resp.status_code}"
    try:
        body_json = resp.json()
        reason = (
            body_json.get("invalidReasonDetails")
            or body_json.get("invalidReason")
            or reason
        )
    except Exception:  # noqa: BLE001
        pass
    return _SettleResult(ok=False, error=reason)


def _post_endpoint(
    client: httpx.Client,
    config: PaywallConfig,
    path: str,
    body: dict,
    response_type,
):
    import json
    body_json = json.dumps(body)
    headers = {"content-type": "application/json"}
    if config.sign_request is not None:
        headers.update(config.sign_request(body_json))
    resp = client.post(
        _facilitator_url(config.facilitator_url, path),
        content=body_json,
        headers=headers,
    )
    resp.raise_for_status()
    return TypeAdapter(response_type).validate_json(resp.content)


def _with_client(config: PaywallConfig, fn):
    if config.httpx_client is not None:
        return fn(config.httpx_client)
    with httpx.Client(timeout=config.timeout_seconds) as client:
        return fn(client)


def process_payment(
    *,
    signature_header: str | None,
    price: PriceTag,
    resource: ResourceInfo,
    config: PaywallConfig,
) -> PaymentOutcome:
    """Returns either ``Paid`` (call the gated handler + attach the
    Payment-Response header) or ``Reject`` (emit a 402 with this body).

    On the first request (no ``Payment-Signature`` header) this acquires a
    server-generated ``serial_num`` via ``POST /x402/challenge`` and embeds
    it in the 402's ``extra.serial_num``.
    """
    payload = try_decode_signature(signature_header)
    if payload is None:
        requirements = build_requirements(price)
        try:
            requirements = acquire_challenge(requirements, config)
        except Exception as e:  # noqa: BLE001
            return Reject(
                body=MidenPaymentRequired(
                    x402_version=2,
                    accepts=[requirements],
                    resource=resource,
                    error=f"failed to acquire challenge: {e}",
                )
            )
        return Reject(
            body=MidenPaymentRequired(
                x402_version=2,
                accepts=[requirements],
                resource=resource,
            )
        )
    requirements = payload.accepted
    result = settle_with_facilitator(payload, requirements, config)
    if result.ok and result.settle is not None:
        return Paid(settle=result.settle)
    return Reject(
        body=build_payment_required(
            price,
            resource,
            error=result.error or "verification failed",
        )
    )
