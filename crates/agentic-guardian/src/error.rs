//! Top-level error type for the agentic-guardian server.
//!
//! Maps to HTTP status codes + canonical x402 `ErrorReason` values so
//! merchants get a uniform error surface from `POST /x402/{verify,settle}`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;
use x402_types::proto::ErrorReason;

use miden_x402_types::MandateSchemaError;

/// Errors returned by the agentic-guardian.
#[derive(Debug, Error)]
pub enum AgenticError {
    // ---------- input shape ----------
    #[error("malformed request: {0}")]
    BadRequest(String),

    #[error("unsupported scheme")]
    UnsupportedScheme,

    #[error("unsupported network")]
    UnsupportedNetwork,

    // ---------- agent / mandate ----------
    #[error("agent not registered")]
    AgentNotRegistered,

    #[error("mandate not found: {0}")]
    MandateNotFound(String),

    #[error("mandate signature verification failed")]
    InvalidMandateSignature,

    #[error("mandate evaluation: {0}")]
    MandateRejected(#[from] MandateSchemaError),

    // ---------- per-tx ----------
    #[error("hot-key signature verification failed")]
    InvalidHotKeySignature,

    #[error("pending state mismatch: agent built on {claimed}, current is {actual}")]
    PendingStateMismatch { claimed: String, actual: String },

    #[error("amount/asset mismatch with 402 requirements")]
    AssetMismatch,

    #[error("recipient mismatch with 402 requirements")]
    RecipientMismatch,

    #[error("sender mismatch with tx_inputs.account.id")]
    SenderMismatch,

    #[error("note blob decode failed: {0}")]
    NoteBlobDecode(String),

    #[error("note blob script is not canonical P2ID")]
    NoteBlobScriptMismatch,

    #[error("output note does not match expected_note_blob commitment")]
    OutputNoteMismatch,

    #[error("input notes commitment mismatch (tx_inputs vs signed_summary)")]
    InputNotesMismatch,

    #[error("transaction inputs decode failed: {0}")]
    TxInputsDecode(String),

    #[error("input or output nullifier already reserved")]
    AlreadyReserved,

    #[error("input nullifier already consumed on chain")]
    AlreadyConsumed,

    // ---------- batch / settle ----------
    #[error("queued tx not found: {0}")]
    QueuedTxNotFound(String),

    #[error("remote prover failed: {0}")]
    RemoteProverError(String),

    #[error("node submit failed: {0}")]
    NodeSubmitError(String),

    // ---------- infra ----------
    #[error("storage error: {0}")]
    Storage(String),

    #[error("node rpc error: {0}")]
    NodeRpc(String),

    #[error("miden client runtime error: {0}")]
    MidenRuntime(String),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type AgenticResult<T> = Result<T, AgenticError>;

impl AgenticError {
    /// Maps to canonical x402 `ErrorReason`.
    pub fn reason(&self) -> ErrorReason {
        use AgenticError::*;
        match self {
            BadRequest(_)
            | TxInputsDecode(_)
            | NoteBlobDecode(_)
            | NoteBlobScriptMismatch
            | OutputNoteMismatch
            | InputNotesMismatch => ErrorReason::InvalidFormat,
            UnsupportedScheme => ErrorReason::UnsupportedScheme,
            UnsupportedNetwork => ErrorReason::UnsupportedChain,
            RecipientMismatch => ErrorReason::RecipientMismatch,
            AssetMismatch => ErrorReason::AssetMismatch,
            InvalidHotKeySignature | SenderMismatch | InvalidMandateSignature => {
                ErrorReason::InvalidSignature
            }
            AgentNotRegistered | MandateNotFound(_) | PendingStateMismatch { .. } => {
                ErrorReason::InvalidPaymentAmount
            }
            MandateRejected(_) => ErrorReason::InvalidPaymentAmount,
            AlreadyReserved | AlreadyConsumed => ErrorReason::TransactionSimulation,
            QueuedTxNotFound(_) => ErrorReason::InvalidFormat,
            RemoteProverError(_)
            | NodeSubmitError(_)
            | NodeRpc(_)
            | MidenRuntime(_)
            | Storage(_)
            | Internal(_) => ErrorReason::UnexpectedError,
        }
    }

    /// HTTP status code.
    pub fn status(&self) -> StatusCode {
        use AgenticError::*;
        match self {
            BadRequest(_)
            | UnsupportedScheme
            | UnsupportedNetwork
            | RecipientMismatch
            | AssetMismatch
            | InvalidHotKeySignature
            | SenderMismatch
            | InvalidMandateSignature
            | NoteBlobDecode(_)
            | NoteBlobScriptMismatch
            | OutputNoteMismatch
            | InputNotesMismatch
            | TxInputsDecode(_)
            | MandateRejected(_)
            | QueuedTxNotFound(_) => StatusCode::BAD_REQUEST,
            AgentNotRegistered | MandateNotFound(_) => StatusCode::NOT_FOUND,
            PendingStateMismatch { .. } | AlreadyReserved | AlreadyConsumed => {
                StatusCode::CONFLICT
            }
            NodeRpc(_) | RemoteProverError(_) | NodeSubmitError(_) | MidenRuntime(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Storage(_) | Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// JSON error body — x402-compatible.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody<'a> {
    pub is_valid: bool,
    pub invalid_reason: ErrorReason,
    pub invalid_reason_details: &'a str,
}

impl IntoResponse for AgenticError {
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
