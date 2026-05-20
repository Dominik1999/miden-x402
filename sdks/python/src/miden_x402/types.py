"""Pydantic models for the `miden-p2id-private` wire format.

JSON field names are camelCase to match ``docs/protocol.md``; Python
attribute names are snake_case for ergonomics. Pydantic's
``populate_by_name`` + ``alias_generator`` mode lets both styles co-exist.

The shapes mirror :mod:`miden_x402_types` in the Rust workspace.
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field
from pydantic.alias_generators import to_camel

MIDEN_TESTNET = "miden:testnet"
MIDEN_MAINNET = "miden:mainnet"

MIDEN_P2ID_PRIVATE_SCHEME = "miden-p2id-private"


class _WireModel(BaseModel):
    """Common base: camelCase aliases, allow population by Python names."""

    model_config = ConfigDict(
        alias_generator=to_camel,
        populate_by_name=True,
        extra="allow",
    )


class MidenP2idPrivateExtra(_WireModel):
    """`extra` field on the 402 response."""

    note_tag: str
    serial_num: str | None = None


class MidenPaymentRequirements(_WireModel):
    scheme: Literal["miden-p2id-private"] = MIDEN_P2ID_PRIVATE_SCHEME
    network: str
    amount: str
    asset: str
    pay_to: str
    max_timeout_seconds: int = 120
    extra: MidenP2idPrivateExtra


class MidenP2idPrivatePayload(_WireModel):
    """Signed-but-unproven payload — the only wire variant."""

    note_type: Literal["miden-p2id-private"] = MIDEN_P2ID_PRIVATE_SCHEME
    tx_inputs: str
    signature: str
    signed_summary: str
    expected_note_blob: str
    serial_num: str
    sender: str
    asset: str
    amount: str


# Type alias kept for ergonomics; single-variant for now.
MidenWirePayload = MidenP2idPrivatePayload


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
    payload: MidenWirePayload
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


VerifyResponse = VerifyValid | VerifyInvalid


class SettleSuccess(_WireModel):
    """Successful settle response from `POST /x402/settle`."""

    success: Literal[True] = True
    payer: str
    transaction: str
    network: str
    receipt_sig: str
    receipt_pubkey_commitment: str


class SettleError(_WireModel):
    success: Literal[False] = False
    error_reason: str
    error_reason_details: str | None = None


SettleResponse = SettleSuccess | SettleError


# ---------- Challenge endpoint ----------


class ChallengeRequest(_WireModel):
    payment_requirements: MidenPaymentRequirements


class ChallengeResponse(_WireModel):
    serial_num: str
    expires_in_seconds: int


# ---------- Pubkey endpoint ----------


class FacilitatorPubkey(_WireModel):
    commitment: str
    pubkey_b64: str = Field(alias="pubkeyB64")
