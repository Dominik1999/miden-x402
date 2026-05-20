//! One-time helper: builds an agent's on-chain account.
//!
//! Per NEW_DESIGN line 6: "No new account types." We use Miden's
//! existing Falcon-512 multisig component with three cosigners (`hot`,
//! `cold`, `guardian`) and **threshold = 1**. The hot/cold gating is
//! enforced off-chain by the agentic-guardian's mandate policy.
//!
//! **Skeleton**: the real implementation uses
//! `miden-standards::account::auth::AuthFalcon512RpoMultisig` to set
//! up the auth component. Currently returns `NotImplemented`.

use miden_x402_types::AccountIdHex;

use crate::{AgenticClientError, AgenticClientResult};

#[derive(Debug, Clone)]
pub struct AgentAccountInputs {
    pub hot_pubkey_commitment_hex: String,
    pub cold_pubkey_commitment_hex: String,
    pub guardian_pubkey_commitment_hex: String,
}

/// Creates + submits the agent's on-chain Miden account; returns its
/// id once committed.
pub async fn create_agent_account(
    _inputs: AgentAccountInputs,
) -> AgenticClientResult<AccountIdHex> {
    Err(AgenticClientError::NotImplemented(
        "create_agent_account: uses miden-standards::account::auth::AuthFalcon512RpoMultisig \
         with threshold=1 over {hot, cold, guardian} cosigners. Wire to miden-client \
         in a follow-up; see the multisig client's setup_account for pattern.",
    ))
}
