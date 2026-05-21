//! HTTP transport to the facilitator's ADN endpoints.

use crate::types::{PayAck, SignedDebit};

#[derive(Debug, Clone)]
pub struct FacilitatorTransport {
    base_url: String,
    http: reqwest::Client,
}

impl FacilitatorTransport {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .user_agent("adn-client/0.1")
                .build()
                .expect("build reqwest client"),
        }
    }

    /// POST /adn/pay — send a signed debit, receive facilitator ack.
    pub async fn pay(&self, debit: &SignedDebit) -> Result<PayAck, AdnTransportError> {
        let url = format!("{}/adn/pay", self.base_url.trim_end_matches('/'));
        let res = self.http
            .post(&url)
            .json(debit)
            .send()
            .await
            .map_err(|e| AdnTransportError::Http(format!("{e}")))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(AdnTransportError::Facilitator(format!("{status}: {body}")));
        }

        res.json().await.map_err(|e| AdnTransportError::Deserialize(format!("{e}")))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdnTransportError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("facilitator error: {0}")]
    Facilitator(String),
    #[error("deserialize error: {0}")]
    Deserialize(String),
}
