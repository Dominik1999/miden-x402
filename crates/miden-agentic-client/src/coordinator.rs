//! HTTP client to the agentic-guardian (the "coordinator" in
//! NEW_DESIGN's nomenclature; the [`inicio-labs` PoC](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/crates/coordinator)
//! uses the same word).

use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};

use miden_x402_types::{AccountIdHex, AgenticPayload, Ap2SignedMandate, MidenPaymentRequirements};

use crate::{AgenticClientError, AgenticClientResult};

#[derive(Clone)]
pub struct CoordinatorClient {
    base_url: String,
    http: HttpClient,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub agent_account_id: AccountIdHex,
    pub hot_pubkey_commitment_hex: String,
    pub cold_pubkey_commitment_hex: String,
    pub signed_mandate: Ap2SignedMandate,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub agent_account_id: AccountIdHex,
    pub mandate_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    pub payment_requirements: MidenPaymentRequirements,
    pub payload: AgenticPayload,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitAck {
    pub queued_id: String,
    pub new_pending_state_commitment: String,
}

impl CoordinatorClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            http: HttpClient::new(),
        }
    }

    pub async fn register(&self, req: &RegisterRequest) -> AgenticClientResult<RegisterResponse> {
        let r = self
            .http
            .post(format!("{}/agentic/register", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| AgenticClientError::Http(e.to_string()))?
            .error_for_status()
            .map_err(|e| AgenticClientError::Http(e.to_string()))?;
        r.json::<RegisterResponse>()
            .await
            .map_err(|e| AgenticClientError::BadResponse(e.to_string()))
    }

    pub async fn submit(&self, req: &SubmitRequest) -> AgenticClientResult<SubmitAck> {
        let r = self
            .http
            .post(format!("{}/agentic/submit", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| AgenticClientError::Http(e.to_string()))?
            .error_for_status()
            .map_err(|e| AgenticClientError::Http(e.to_string()))?;
        r.json::<SubmitAck>()
            .await
            .map_err(|e| AgenticClientError::BadResponse(e.to_string()))
    }

    pub async fn status(&self, queued_id: &str) -> AgenticClientResult<serde_json::Value> {
        let r = self
            .http
            .get(format!("{}/agentic/status/{queued_id}", self.base_url))
            .send()
            .await
            .map_err(|e| AgenticClientError::Http(e.to_string()))?
            .error_for_status()
            .map_err(|e| AgenticClientError::Http(e.to_string()))?;
        r.json::<serde_json::Value>()
            .await
            .map_err(|e| AgenticClientError::BadResponse(e.to_string()))
    }
}
