//! Facilitator-level error type and HTTP mapping.
//!
//! The facilitator turns every failure into an x402-flavoured JSON body so
//! merchant middleware can react uniformly. Status codes:
//!
//! - `400 Bad Request` — verification failed for a client-side reason
//!   (bad signature, mandate rejection, blob mismatch, ...).
//! - `409 Conflict` — input nullifier already reserved.
//! - `412 Precondition Failed` — buyer's stored balance is insufficient.
//! - `500 Internal Server Error` — unexpected internal failure.
//! - `503 Service Unavailable` — remote prover unreachable / Miden node
//!   RPC failed / mandate evaluation backend is down.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;
use x402_types::proto::ErrorReason;

use crate::mandate::MandateError;
use crate::storage::StorageError;

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

    /// The decoded note's recipient does not match the requested `payTo`.
    #[error("recipient mismatch")]
    RecipientMismatch,

    /// The decoded note's asset/amount does not match the requirements.
    #[error("asset or amount mismatch")]
    AssetMismatch,

    /// The payload claims a sender that disagrees with the
    /// authenticated account / tx_inputs account id.
    #[error("sender mismatch")]
    SenderMismatch,

    /// The `expected_note_blob` failed base64 / `NoteFile` decoding.
    #[error("note blob decode failed: {0}")]
    NoteBlobDecode(String),

    /// The decoded note's recipient script is not the canonical P2ID
    /// script root.
    #[error("note blob script does not match P2ID")]
    NoteBlobScriptMismatch,

    /// The `NoteFile` variant carried in the blob does not contain enough
    /// information to verify (e.g. a bare `NoteId`).
    #[error("note blob does not carry note details")]
    NoteBlobUnsupportedVariant,

    /// The challenge id (= serial_num) referenced in the payload is not
    /// known to the facilitator. Either it was never issued, was already
    /// consumed, or expired and was swept.
    #[error("challenge not found or already consumed")]
    ChallengeNotFound,

    /// The challenge was found but its TTL elapsed before the buyer
    /// submitted.
    #[error("challenge expired")]
    ChallengeExpired,

    /// The `TransactionInputs` blob did not deserialise.
    #[error("transaction inputs decode failed: {0}")]
    TxInputsDecode(String),

    /// The buyer's account auth policy is unsupported by the facilitator
    /// (only Falcon-512 Poseidon2 cosigner keys are accepted).
    #[error("unsupported auth scheme on buyer account: {0}")]
    UnsupportedAuthScheme(String),

    /// The buyer's Falcon signature over the tx summary failed verification
    /// against the account's stored cosigner pubkey.
    #[error("invalid Falcon signature")]
    BadSignature,

    /// The recomputed `note_id` from `expected_note_blob` is not in the
    /// `signed_summary.output_notes` set.
    #[error("output note in tx does not match expectedNoteBlob")]
    OutputNoteMismatch,

    /// `signed_summary.input_notes` and `tx_inputs.input_notes` disagree —
    /// signature does not cover the same input note set as the tx.
    #[error("signed summary input-notes commitment does not match tx_inputs")]
    InputNotesMismatch,

    /// One of the input nullifiers is already reserved by another in-flight
    /// verification — pending double-spend.
    #[error("input nullifier already reserved in pending window")]
    AlreadyReserved,

    /// `check_nullifiers` on the Miden node reports one or more nullifiers
    /// as already spent on chain — the verify-before-prove backstop caught
    /// a replay against a settled-and-included tx.
    #[error("input nullifier already consumed on chain")]
    AlreadyConsumed,

    /// The buyer's stored vault balance (from Guardian's persisted state)
    /// is less than the amount requested.
    #[error("buyer has insufficient balance")]
    InsufficientBalance,

    /// The mandate policy rejected the payment.
    #[error("mandate rejected: {0}")]
    MandateRejected(String),

    /// Mandate evaluation could not complete due to a transient backend
    /// error.
    #[error("mandate evaluation failed: {0}")]
    MandateEvaluationFailed(String),

    /// A storage backend failure — repos returned an I/O / serialisation /
    /// backend error.
    #[error("storage error: {0}")]
    Storage(String),

    /// The Miden node RPC returned an error (used by the verify-path
    /// nullifier backstop and by the batch worker on submit).
    #[error("node RPC error: {0}")]
    NodeRpc(String),

    /// The remote prover returned an error (gRPC / proof failure).
    #[error("remote prover failed: {0}")]
    RemoteProverError(String),

    /// Receipt-signing key was unavailable or could not produce a
    /// signature.
    #[error("receipt signing failed: {0}")]
    ReceiptSigning(String),

    /// Generic internal error.
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
            FacilitatorError::RecipientMismatch => ErrorReason::RecipientMismatch,
            FacilitatorError::AssetMismatch => ErrorReason::AssetMismatch,
            FacilitatorError::SenderMismatch => ErrorReason::InvalidSignature,
            FacilitatorError::NoteBlobDecode(_)
            | FacilitatorError::NoteBlobScriptMismatch
            | FacilitatorError::NoteBlobUnsupportedVariant
            | FacilitatorError::ChallengeNotFound
            | FacilitatorError::ChallengeExpired
            | FacilitatorError::TxInputsDecode(_)
            | FacilitatorError::OutputNoteMismatch
            | FacilitatorError::InputNotesMismatch => ErrorReason::InvalidFormat,
            FacilitatorError::UnsupportedAuthScheme(_) => ErrorReason::UnsupportedScheme,
            FacilitatorError::BadSignature => ErrorReason::InvalidSignature,
            FacilitatorError::AlreadyReserved
            | FacilitatorError::AlreadyConsumed => ErrorReason::TransactionSimulation,
            FacilitatorError::InsufficientBalance => ErrorReason::InsufficientFunds,
            FacilitatorError::MandateRejected(_) => ErrorReason::InvalidPaymentAmount,
            FacilitatorError::MandateEvaluationFailed(_)
            | FacilitatorError::Storage(_)
            | FacilitatorError::NodeRpc(_)
            | FacilitatorError::RemoteProverError(_)
            | FacilitatorError::ReceiptSigning(_)
            | FacilitatorError::Internal(_) => ErrorReason::UnexpectedError,
        }
    }

    /// Returns the HTTP status code this error maps to.
    pub fn status(&self) -> StatusCode {
        match self {
            FacilitatorError::BadRequest(_)
            | FacilitatorError::UnsupportedScheme
            | FacilitatorError::UnsupportedNetwork
            | FacilitatorError::RecipientMismatch
            | FacilitatorError::AssetMismatch
            | FacilitatorError::SenderMismatch
            | FacilitatorError::NoteBlobDecode(_)
            | FacilitatorError::NoteBlobScriptMismatch
            | FacilitatorError::NoteBlobUnsupportedVariant
            | FacilitatorError::ChallengeNotFound
            | FacilitatorError::ChallengeExpired
            | FacilitatorError::TxInputsDecode(_)
            | FacilitatorError::UnsupportedAuthScheme(_)
            | FacilitatorError::BadSignature
            | FacilitatorError::OutputNoteMismatch
            | FacilitatorError::InputNotesMismatch
            | FacilitatorError::MandateRejected(_) => StatusCode::BAD_REQUEST,
            FacilitatorError::AlreadyReserved
            | FacilitatorError::AlreadyConsumed => StatusCode::CONFLICT,
            FacilitatorError::InsufficientBalance => StatusCode::PRECONDITION_FAILED,
            FacilitatorError::MandateEvaluationFailed(_)
            | FacilitatorError::NodeRpc(_)
            | FacilitatorError::RemoteProverError(_) => StatusCode::SERVICE_UNAVAILABLE,
            FacilitatorError::Storage(_)
            | FacilitatorError::ReceiptSigning(_)
            | FacilitatorError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl From<StorageError> for FacilitatorError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => FacilitatorError::ChallengeNotFound,
            StorageError::Conflict(s) => {
                if s.starts_with("nullifier already reserved") {
                    FacilitatorError::AlreadyReserved
                } else {
                    FacilitatorError::Storage(s)
                }
            }
            other => FacilitatorError::Storage(other.to_string()),
        }
    }
}

impl From<MandateError> for FacilitatorError {
    fn from(e: MandateError) -> Self {
        match e {
            MandateError::Rejected { reason } => FacilitatorError::MandateRejected(reason),
            MandateError::EvaluationFailed(s) => FacilitatorError::MandateEvaluationFailed(s),
        }
    }
}

/// JSON body returned when verification or settlement fails. Field names
/// match the x402 `VerifyResponse::Invalid` / `SettleResponse::Error` shape
/// so client libraries can interpret them uniformly.
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
