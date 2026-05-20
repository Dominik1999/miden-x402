//! x402 v2 wire-format types for the Miden network.
//!
//! This crate plugs Miden-specific identifiers and a Miden-flavoured
//! `exact` payment scheme into the network- and scheme-agnostic types from
//! [`x402_types`]. See the [project README](https://github.com/ermvrs/miden-x402)
//! for the overall protocol design.
//!
//! # Quick tour
//!
//! - [`network`] — CAIP-2 identifiers for Miden (`miden:testnet`, `miden:mainnet`).
//! - [`ids`] — validated hex newtypes for `AccountId`, `NoteId`, `TransactionId`.
//! - [`scheme`] — the `exact` scheme tag, `MidenExactExtra`, `MidenExactPayload`.
//! - [`aliases`] — composed `MidenPaymentRequirements` / `MidenPaymentPayload` /
//!   `MidenPaymentRequired` / `MidenVerifyRequest` / `MidenSettleRequest`.
//! - [`header`] — base64 helpers for the `PAYMENT-SIGNATURE` and
//!   `PAYMENT-RESPONSE` HTTP headers.

#![forbid(unsafe_code)]

pub mod aliases;
pub mod header;
pub mod ids;
pub mod mandate;
pub mod network;
pub mod scheme;

pub use aliases::{
    MidenPaymentPayload, MidenPaymentRequired, MidenPaymentRequirements, MidenSettleRequest,
    MidenVerifyRequest,
};
pub use header::{
    HeaderError, PAYMENT_REQUIRED_HEADER, PAYMENT_RESPONSE_HEADER, PAYMENT_SIGNATURE_HEADER,
    decode_payment_header, decode_payment_required_header, decode_payment_response_header,
    decode_payment_signature_header, encode_payment_header, encode_payment_required_header,
    encode_payment_response_header, encode_payment_signature_header,
};
pub use ids::{AccountIdHex, IdError, NoteIdHex, TransactionIdHex};
pub use mandate::{Ap2Mandate, Ap2SignedMandate, MandateSchemaError};
pub use network::{
    MAINNET_REFERENCE, MIDEN_NAMESPACE, TESTNET_REFERENCE, miden_mainnet, miden_testnet,
};
pub use scheme::{
    ASSET_TRANSFER_METHOD_P2ID, AgenticPayload, AssetTransferMethodTag, ExactScheme,
    GuardianFastPayload, MidenExactExtra, MidenExactPayload, NoteKind, PrivateP2idPayload,
    PublicP2idPayload, SettlementKind,
};

// Re-export the upstream x402 v2 types we share with the rest of the
// ecosystem, so callers can take a single dep on `miden-x402-types`.
pub use x402_types::chain::ChainId;
pub use x402_types::proto::v1::{SettleResponse, VerifyResponse};
pub use x402_types::proto::v2::{ResourceInfo, X402Version2};
pub use x402_types::proto::{ErrorReason, PaymentVerificationError};
