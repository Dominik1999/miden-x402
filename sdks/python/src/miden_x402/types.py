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
    # Settlement model. ``"commit"`` (default) = settled-at-commit;
    # ``"guardian-fast"`` = verify-before-prove via the Guardian endpoints.
    settlement: Literal["commit", "guardian-fast"] = "commit"
    # Only meaningful with settlement == "guardian-fast".
    guardian_url: str | None = None
    # Server-generated 32-byte hex Word. Only meaningful with
    # settlement == "guardian-fast".
    serial_num: str | None = None


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
    """Private-note payment payload.

    Carries the canonical Miden ``NoteFile`` blob (base64-encoded) so the
    facilitator can reconstruct the note off-chain and bind it to the
    on-chain commitment by recomputing the note id. Other fields mirror
    :class:`PublicP2idPayload` so the wire envelope is uniform across both
    note types.
    """

    note_type: Literal["private"] = "private"
    note_blob: str
    transaction_id: str
    sender: str
    block_num: int
    asset: str
    amount: str


class GuardianFastPayload(_WireModel):
    """Guardian-fast payment payload.

    Carries a signed-but-unproven transaction. The facilitator verifies the
    Falcon signature offline, reserves input nullifiers, and proves +
    submits the tx asynchronously. ``transaction_id`` is the pre-prove id.
    There is no ``block_num`` field — the tx is not yet on chain at the
    time the payload is constructed.
    """

    note_type: Literal["guardianFast"] = "guardianFast"
    tx_inputs: str
    # Base64 of miden_protocol::account::auth::Signature.
    signature: str
    # Base64 of miden_protocol::transaction::TransactionSummary.
    signed_summary: str
    expected_note_blob: str
    serial_num: str
    transaction_id: str
    sender: str
    asset: str
    amount: str


MidenExactPayload = Annotated[
    Union[PublicP2idPayload, PrivateP2idPayload, GuardianFastPayload],
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
