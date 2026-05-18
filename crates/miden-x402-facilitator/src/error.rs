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

    /// The private-note `noteBlob` failed base64 / `NoteFile` decoding.
    #[error("note blob decode failed: {0}")]
    NoteBlobDecode(String),

    /// The decoded private note's recipient script is not the canonical P2ID
    /// script.
    #[error("note blob script does not match P2ID")]
    NoteBlobScriptMismatch,

    /// The `NoteFile` variant carried in the blob does not contain enough
    /// information to verify (e.g. a bare `NoteId`).
    #[error("note blob does not carry note details")]
    NoteBlobUnsupportedVariant,

    /// A failure inside the Miden node RPC.
    #[error("node RPC error: {0}")]
    NodeRpc(String),

    // ----- Guardian-flow errors (Phase B) -----

    /// The Guardian endpoints are disabled on this facilitator instance
    /// (`MIDEN_X402_GUARDIAN_ENABLED` is unset or false).
    #[error("guardian endpoints are disabled on this facilitator")]
    GuardianDisabled,

    /// No `MIDEN_X402_REMOTE_PROVER_URL` configured — Guardian cannot prove
    /// + submit the transaction even though verification succeeded.
    #[error("remote prover is not configured: {0}")]
    RemoteProverUnavailable(String),

    /// The challenge id (= serial_num) referenced in the payload is not in
    /// the in-memory store. Either it was never issued, was already
    /// consumed, or expired.
    #[error("challenge not found or already consumed")]
    ChallengeNotFound,

    /// The challenge was issued but has expired before the buyer submitted.
    #[error("challenge expired")]
    ChallengeExpired,

    /// The off-chain `TransactionInputs` did not deserialise.
    #[error("transaction inputs decode failed: {0}")]
    TxInputsDecode(String),

    /// The buyer's account does not use a Falcon-512 / Poseidon2 single-sig
    /// authentication scheme. Phase B only supports that scheme.
    #[error("unsupported auth scheme on buyer account: {0}")]
    UnsupportedAuthScheme(String),

    /// `tx_args.advice_inputs.map` did not contain the expected signature
    /// entry keyed by `Hasher::merge([pub_key, message])`.
    #[error("signature missing from advice inputs")]
    MissingSignature,

    /// The buyer's signature failed Falcon verification against their
    /// publicly-known account key.
    #[error("invalid Falcon signature")]
    BadSignature,

    /// The off-chain `expectedNoteBlob` recomputes to a `note_id` that is
    /// not in the transaction's `output_notes`.
    #[error("output note in tx does not match expectedNoteBlob")]
    OutputNoteMismatch,

    /// The same input nullifier is already reserved by another in-flight
    /// Guardian verification — pending double-spend.
    #[error("input nullifier already reserved in pending window")]
    AlreadyReserved,

    /// The remote prover returned an error (gRPC / proof failure).
    #[error("remote prover failed: {0}")]
    RemoteProverError(String),

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
            FacilitatorError::NoteBlobDecode(_)
            | FacilitatorError::NoteBlobScriptMismatch
            | FacilitatorError::NoteBlobUnsupportedVariant => ErrorReason::InvalidFormat,
            FacilitatorError::GuardianDisabled => ErrorReason::UnsupportedScheme,
            FacilitatorError::ChallengeNotFound
            | FacilitatorError::ChallengeExpired
            | FacilitatorError::TxInputsDecode(_)
            | FacilitatorError::OutputNoteMismatch => ErrorReason::InvalidFormat,
            FacilitatorError::UnsupportedAuthScheme(_) => ErrorReason::UnsupportedScheme,
            FacilitatorError::MissingSignature | FacilitatorError::BadSignature => {
                ErrorReason::InvalidSignature
            }
            FacilitatorError::AlreadyReserved => ErrorReason::TransactionSimulation,
            FacilitatorError::RemoteProverUnavailable(_)
            | FacilitatorError::RemoteProverError(_) => ErrorReason::UnexpectedError,
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
            | FacilitatorError::NoteBlobDecode(_)
            | FacilitatorError::NoteBlobScriptMismatch
            | FacilitatorError::NoteBlobUnsupportedVariant
            | FacilitatorError::ChallengeNotFound
            | FacilitatorError::ChallengeExpired
            | FacilitatorError::TxInputsDecode(_)
            | FacilitatorError::UnsupportedAuthScheme(_)
            | FacilitatorError::MissingSignature
            | FacilitatorError::BadSignature
            | FacilitatorError::OutputNoteMismatch
            | FacilitatorError::AlreadyReserved => StatusCode::BAD_REQUEST,
            FacilitatorError::GuardianDisabled => StatusCode::NOT_IMPLEMENTED,
            FacilitatorError::RemoteProverUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            FacilitatorError::RemoteProverError(_)
            | FacilitatorError::NodeRpc(_)
            | FacilitatorError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
