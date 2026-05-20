use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgenticError {
    #[error("http transport: {0}")]
    Http(#[from] reqwest::Error),

    #[error("facilitator returned {status} {code}: {message}")]
    Facilitator {
        status: u16,
        code: String,
        message: String,
    },

    #[error("stale base after retry: client {client}, server {server}")]
    StaleBaseAfterRetry { client: String, server: String },

    #[error("keystore: {0}")]
    Keystore(String),

    #[error("serialize: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("config: {0}")]
    Config(String),
}

impl AgenticError {
    pub fn is_stale_base(&self) -> bool {
        matches!(self, Self::Facilitator { code, .. } if code == "STALE_BASE_STATE")
    }
}

pub type Result<T> = std::result::Result<T, AgenticError>;
