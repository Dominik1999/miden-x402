"""Base64 + JSON header codecs for the x402 v2 wire headers.

The header constants and the standard-alphabet base64 encoding match
``crates/miden-x402-types/src/header.rs`` exactly.
"""

from __future__ import annotations

import base64
import json
from typing import Type, TypeVar

from pydantic import TypeAdapter

from .types import (
    MidenPaymentPayload,
    MidenPaymentRequired,
    SettleResponse,
)

PAYMENT_REQUIRED_HEADER = "Payment-Required"
PAYMENT_SIGNATURE_HEADER = "Payment-Signature"
PAYMENT_RESPONSE_HEADER = "Payment-Response"

_T = TypeVar("_T")


def _encode_model(value: object) -> str:
    """Generic: serialise a Pydantic model to camelCase JSON, then base64."""
    if hasattr(value, "model_dump_json"):
        json_str = value.model_dump_json(by_alias=True, exclude_none=True)  # type: ignore[attr-defined]
    else:
        json_str = json.dumps(value)
    return base64.b64encode(json_str.encode("utf-8")).decode("ascii")


def _decode_to(header_value: str, type_: Type[_T]) -> _T:
    raw = base64.b64decode(header_value.encode("ascii"))
    return TypeAdapter(type_).validate_json(raw)


def encode_payment_required_header(value: MidenPaymentRequired) -> str:
    return _encode_model(value)


def decode_payment_required_header(value: str) -> MidenPaymentRequired:
    return _decode_to(value, MidenPaymentRequired)


def encode_payment_signature_header(value: MidenPaymentPayload) -> str:
    return _encode_model(value)


def decode_payment_signature_header(value: str) -> MidenPaymentPayload:
    return _decode_to(value, MidenPaymentPayload)


def encode_payment_response_header(value: SettleResponse) -> str:
    return _encode_model(value)


def decode_payment_response_header(value: str) -> SettleResponse:
    return _decode_to(value, SettleResponse)
