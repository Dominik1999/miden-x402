//! Facilitator-level error type and HTTP mapping.
//!
//! The facilitator turns every failure into an x402-flavoured JSON body so
//! that downstream merchant middleware can react in a uniform way. Status
//! codes mirror the x402-rs facilitator: `400` for client-side errors, `412`
//! when a precondition is missing, `500` for unexpected node failures.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;
use x402_types::proto::ErrorReason;

/// Internal error type returned by handlers.
#[derive(Debug, Error)]
pub enum FacilitatorError {
    /// The request body was malformed JSON or did not match the schema.
    #[error("malformed request: {0}")]
    BadRequest(String),

    /// The request named a scheme this facilitator does not implement.
    #[error("unsupported scheme")]
    UnsupportedScheme,

    /// The request named a network this facilitator does not implement.
    #[error("unsupported network")]
    UnsupportedNetwork,

    /// The faucet (asset) account ID is not on the configured allowlist.
    #[error("asset not allowed: {0}")]
    AssetNotAllowed(String),

    /// The on-chain note's recipient does not match the requested `payTo`.
    #[error("recipient mismatch")]
    RecipientMismatch,

    /// The on-chain note's asset/amount does not match the requirements.
    #[error("asset or amount mismatch")]
    AssetMismatch,

    /// The note was not found on chain (private note, or never created).
    #[error("note not committed: {0}")]
    NoteNotFound(String),

    /// The note has already been consumed (double-spend / replay).
    #[error("note already consumed")]
    AlreadyConsumed,

    /// The note is older than the freshness window.
    #[error("payment expired (block_num={block_num}, current={current})")]
    Expired { block_num: u32, current: u32 },

    /// The payload claims a sender that disagrees with the on-chain note.
    #[error("sender mismatch")]
    SenderMismatch,

    /// Private-note payments are declared in the wire format but not yet
    /// supported. Returned for M2; will be implemented in Phase 2.
    #[error("private notes are not yet supported")]
    PrivateNotSupported,

    /// A failure inside the Miden node RPC.
    #[error("node RPC error: {0}")]
    NodeRpc(String),

    /// An unclassified internal error.
    #[error("internal error: {0}")]
    Internal(String),
}

impl FacilitatorError {
    /// Maps each error to the canonical x402 [`ErrorReason`].
    pub fn reason(&self) -> ErrorReason {
        match self {
            FacilitatorError::BadRequest(_) => ErrorReason::InvalidFormat,
            FacilitatorError::UnsupportedScheme => ErrorReason::UnsupportedScheme,
            FacilitatorError::UnsupportedNetwork => ErrorReason::UnsupportedChain,
            FacilitatorError::AssetNotAllowed(_) => ErrorReason::AssetMismatch,
            FacilitatorError::RecipientMismatch => ErrorReason::RecipientMismatch,
            FacilitatorError::AssetMismatch => ErrorReason::AssetMismatch,
            FacilitatorError::NoteNotFound(_) => ErrorReason::TransactionSimulation,
            FacilitatorError::AlreadyConsumed => ErrorReason::TransactionSimulation,
            FacilitatorError::Expired { .. } => ErrorReason::InvalidPaymentExpired,
            FacilitatorError::SenderMismatch => ErrorReason::InvalidSignature,
            FacilitatorError::PrivateNotSupported => ErrorReason::UnsupportedScheme,
            FacilitatorError::NodeRpc(_) | FacilitatorError::Internal(_) => {
                ErrorReason::UnexpectedError
            }
        }
    }

    /// Returns the HTTP status code this error maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            FacilitatorError::BadRequest(_)
            | FacilitatorError::UnsupportedScheme
            | FacilitatorError::UnsupportedNetwork
            | FacilitatorError::AssetNotAllowed(_)
            | FacilitatorError::RecipientMismatch
            | FacilitatorError::AssetMismatch
            | FacilitatorError::NoteNotFound(_)
            | FacilitatorError::AlreadyConsumed
            | FacilitatorError::Expired { .. }
            | FacilitatorError::SenderMismatch
            | FacilitatorError::PrivateNotSupported => StatusCode::BAD_REQUEST,
            FacilitatorError::NodeRpc(_) | FacilitatorError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

/// JSON body returned by `/verify` and `/settle` when verification fails.
///
/// Field names match the x402 `VerifyResponse::Invalid` / `SettleResponse`
/// error shape so that x402 client libraries can interpret them uniformly.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody<'a> {
    pub is_valid: bool,
    pub invalid_reason: ErrorReason,
    pub invalid_reason_details: &'a str,
}

impl IntoResponse for FacilitatorError {
    fn into_response(self) -> Response {
        let status = self.status();
        let details = self.to_string();
        let body = ErrorBody {
            is_valid: false,
            invalid_reason: self.reason(),
            invalid_reason_details: &details,
        };
        (status, Json(body)).into_response()
    }
}
