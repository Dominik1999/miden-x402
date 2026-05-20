//! x402 v2 wire-format types for the Miden network.
//!
//! This crate plugs Miden-specific identifiers and the `miden-p2id-private`
//! payment scheme into the network- and scheme-agnostic types from
//! [`x402_types`]. See [`ideas/DESIGN.md`] in the repo root for the design
//! that drives this wire format.
//!
//! # Quick tour
//!
//! - [`network`] — CAIP-2 identifiers for Miden (`miden:testnet`,
//!   `miden:mainnet`). The x402 v2 protocol mandates CAIP-2 for the `network`
//!   field, so the hyphenated form used casually in DESIGN.md
//!   (`miden-mainnet`) becomes `miden:mainnet` on the wire.
//! - [`ids`] — validated hex newtypes for `AccountId`, `NoteId`,
//!   `TransactionId`.
//! - [`scheme`] — the `miden-p2id-private` scheme tag,
//!   `MidenP2idPrivateExtra`, `MidenP2idPrivatePayload`, `MidenWirePayload`.
//! - [`aliases`] — composed `MidenPaymentRequirements` / `MidenPaymentPayload`
//!   / `MidenPaymentRequired` / `MidenVerifyRequest` / `MidenSettleRequest`.
//! - [`header`] — base64 helpers for the `PAYMENT-REQUIRED`,
//!   `PAYMENT-SIGNATURE`, and `PAYMENT-RESPONSE` HTTP headers.

#![forbid(unsafe_code)]

pub mod aliases;
pub mod header;
pub mod ids;
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
pub use network::{
    MAINNET_REFERENCE, MIDEN_NAMESPACE, TESTNET_REFERENCE, is_miden, miden_mainnet, miden_testnet,
};
pub use scheme::{
    MidenP2idPrivateExtra, MidenP2idPrivatePayload, MidenP2idPrivateScheme, MidenWirePayload,
};

// Re-export the upstream x402 v2 types we share with the rest of the
// ecosystem, so callers can take a single dep on `miden-x402-types`.
pub use x402_types::chain::ChainId;
pub use x402_types::proto::v1::{SettleResponse, VerifyResponse};
pub use x402_types::proto::v2::{ResourceInfo, X402Version2};
pub use x402_types::proto::{ErrorReason, PaymentVerificationError};
