use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FacilitatorError {
    #[error("agent not registered: {0}")]
    AgentNotRegistered(String),

    #[error("agent already registered: {0}")]
    AgentAlreadyRegistered(String),

    #[error("stale base state: client built on {client}, facilitator pending {server}")]
    StaleBaseState { client: String, server: String },

    #[error("invalid signature: {0}")]
    InvalidSignature(String),

    #[error("mandate violation: {0}")]
    MandateViolation(String),

    #[error("output check failed: {0}")]
    OutputCheckFailed(String),

    #[error("double spend: nullifier already reserved or spent")]
    DoubleSpend,

    #[error("malformed payload: {0}")]
    Malformed(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl FacilitatorError {
    fn code_and_status(&self) -> (&'static str, StatusCode) {
        match self {
            Self::AgentNotRegistered(_) => ("AGENT_NOT_REGISTERED", StatusCode::NOT_FOUND),
            Self::AgentAlreadyRegistered(_) => ("AGENT_ALREADY_REGISTERED", StatusCode::CONFLICT),
            Self::StaleBaseState { .. } => ("STALE_BASE_STATE", StatusCode::CONFLICT),
            Self::InvalidSignature(_) => ("INVALID_SIGNATURE", StatusCode::BAD_REQUEST),
            Self::MandateViolation(_) => ("MANDATE_VIOLATION", StatusCode::FORBIDDEN),
            Self::OutputCheckFailed(_) => ("OUTPUT_CHECK_FAILED", StatusCode::BAD_REQUEST),
            Self::DoubleSpend => ("DOUBLE_SPEND", StatusCode::CONFLICT),
            Self::Malformed(_) => ("MALFORMED_PAYLOAD", StatusCode::BAD_REQUEST),
            Self::NotFound(_) => ("NOT_FOUND", StatusCode::NOT_FOUND),
            Self::Internal(_) => ("INTERNAL", StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
}

impl IntoResponse for FacilitatorError {
    fn into_response(self) -> Response {
        let (code, status) = self.code_and_status();
        let body = ErrorBody {
            code,
            message: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, FacilitatorError>;
