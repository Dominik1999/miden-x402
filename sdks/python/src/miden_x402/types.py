"""Pydantic models for the x402 v2 Miden wire format.

JSON field names are camelCase to match ``docs/protocol.md``; Python
attribute names are snake_case for ergonomics. Pydantic's ``populate_by_name``
+ ``alias_generator`` mode lets both styles co-exist.

The shapes mirror :mod:`miden_x402_types` in the Rust workspace.
"""

from __future__ import annotations

from typing import Annotated, Any, Literal, Union

from pydantic import BaseModel, ConfigDict, Field
from pydantic.alias_generators import to_camel

MIDEN_TESTNET = "miden:testnet"
MIDEN_MAINNET = "miden:mainnet"  # reserved, unused in MVP

EXACT_SCHEME = "exact"
ASSET_TRANSFER_METHOD_P2ID = "miden-p2id"


class _WireModel(BaseModel):
    """Common base: camelCase aliases, allow population by Python names."""

    model_config = ConfigDict(
        alias_generator=to_camel,
        populate_by_name=True,
        extra="allow",
    )


class MidenExactExtra(_WireModel):
    asset_transfer_method: Literal["miden-p2id"] = ASSET_TRANSFER_METHOD_P2ID
    token_symbol: str
    decimals: int
    note_type: Literal["public", "private"] = "public"


class MidenPaymentRequirements(_WireModel):
    scheme: Literal["exact"] = EXACT_SCHEME
    network: str
    amount: str
    asset: str
    pay_to: str
    max_timeout_seconds: int = 120
    extra: MidenExactExtra


class PublicP2idPayload(_WireModel):
    note_type: Literal["public"] = "public"
    note_id: str
    transaction_id: str
    sender: str
    block_num: int
    asset: str
    amount: str


class PrivateP2idPayload(_WireModel):
    note_type: Literal["private"] = "private"
    note_blob: str


MidenExactPayload = Annotated[
    Union[PublicP2idPayload, PrivateP2idPayload],
    Field(discriminator="note_type"),
]


class ResourceInfo(_WireModel):
    url: str
    description: str | None = None
    mime_type: str | None = None


class MidenPaymentRequired(_WireModel):
    x402_version: Literal[2] = 2
    error: str | None = None
    resource: ResourceInfo | None = None
    accepts: list[MidenPaymentRequirements]
    extensions: dict[str, Any] | None = None


class MidenPaymentPayload(_WireModel):
    x402_version: Literal[2] = 2
    accepted: MidenPaymentRequirements
    payload: MidenExactPayload
    resource: ResourceInfo | None = None
    extensions: dict[str, Any] | None = None


# ---------- Facilitator response envelopes ----------


class VerifyValid(_WireModel):
    is_valid: Literal[True] = True
    payer: str


class VerifyInvalid(_WireModel):
    is_valid: Literal[False] = False
    invalid_reason: str
    invalid_reason_details: str | None = None


VerifyResponse = Annotated[
    Union[VerifyValid, VerifyInvalid],
    Field(discriminator="is_valid"),
]


class SettleSuccess(_WireModel):
    success: Literal[True] = True
    payer: str
    transaction: str
    network: str


class SettleError(_WireModel):
    success: Literal[False] = False
    error_reason: str
    error_reason_details: str | None = None


SettleResponse = Annotated[
    Union[SettleSuccess, SettleError],
    Field(discriminator="success"),
]
