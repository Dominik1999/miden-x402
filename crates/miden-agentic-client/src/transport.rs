//! Thin reqwest wrapper for the x402 facilitator HTTP API.

use serde::{Serialize, de::DeserializeOwned};

use crate::error::{AgenticError, Result};
use crate::types::*;

#[derive(Debug, serde::Deserialize)]
struct FacilitatorError {
    code: String,
    message: String,
}

#[derive(Debug, Clone)]
pub struct FacilitatorClient {
    base_url: String,
    http: reqwest::Client,
}

impl FacilitatorClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            base_url: base,
            http: reqwest::Client::builder()
                .user_agent("miden-agentic-client/0.1")
                .build()
                .expect("reqwest client"),
        }
    }

    pub async fn register_agent(
        &self,
        req: &RegisterAgentRequest,
    ) -> Result<RegisterAgentResponse> {
        self.post("/agents", req).await
    }

    pub async fn get_state(&self, agent_id: &str) -> Result<AgentStateResponse> {
        self.get(&format!("/agents/{agent_id}/state")).await
    }

    pub async fn post_payment(
        &self,
        agent_id: &str,
        payload: &AgenticPayload,
    ) -> Result<AckResponse> {
        self.post(&format!("/agents/{agent_id}/payments"), payload).await
    }

    pub async fn get_payment_status(
        &self,
        agent_id: &str,
        nullifier: &str,
    ) -> Result<PaymentStatusResponse> {
        self.get(&format!("/agents/{agent_id}/payments/{nullifier}"))
            .await
    }

    async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        let res = self
            .http
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await?;
        self.parse(res).await
    }

    async fn post<B: Serialize, R: DeserializeOwned>(&self, path: &str, body: &B) -> Result<R> {
        let res = self
            .http
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .await?;
        self.parse(res).await
    }

    async fn parse<R: DeserializeOwned>(&self, res: reqwest::Response) -> Result<R> {
        let status = res.status();
        if status.is_success() {
            Ok(res.json::<R>().await?)
        } else {
            let status_code = status.as_u16();
            let err: FacilitatorError = res.json().await.unwrap_or(FacilitatorError {
                code: "UNKNOWN".into(),
                message: format!("non-success status {status_code}"),
            });
            Err(AgenticError::Facilitator {
                status: status_code,
                code: err.code,
                message: err.message,
            })
        }
    }
}
