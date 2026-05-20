//! Messages the [`super::MidenRuntimeHandle`] sends into the
//! `!Send + !Sync` runtime thread.
//!
//! Mirrors the [`inicio-labs/MultiSig` `multisig_client_runtime/msg.rs`](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/crates/coordinator/engine/src/multisig_client_runtime/msg.rs)
//! pattern: each variant carries a `oneshot::Sender` to reply to.

use tokio::sync::oneshot;

use crate::error::AgenticError;

/// Replies the runtime sends back.
#[derive(Debug)]
pub enum RuntimeResponse<T> {
    Ok(T),
    Err(AgenticError),
}

impl<T> RuntimeResponse<T> {
    pub fn into_result(self) -> Result<T, AgenticError> {
        match self {
            Self::Ok(t) => Ok(t),
            Self::Err(e) => Err(e),
        }
    }
}

/// All messages the runtime accepts.
///
/// Each variant pairs a request payload with the oneshot sender for
/// the reply. Payloads are kept opaque on the wire (base64 of canonical
/// Miden serializations) — the runtime thread parses + serialises on
/// behalf of the axum thread.
#[derive(Debug)]
pub enum MidenRuntimeMsg {
    /// Wait until the local store has the genesis block — called at
    /// boot before anything else.
    EnsureGenesis { reply: oneshot::Sender<RuntimeResponse<()>> },

    /// `client.sync_state()`.
    SyncState { reply: oneshot::Sender<RuntimeResponse<()>> },

    /// Dry-run a transaction request; return the resulting
    /// `TransactionSummary` as base64 bytes. Mirrors
    /// `MultisigClient::propose_multisig_transaction`.
    ProposeTx {
        agent_account_id: String,
        /// Base64 of `TransactionRequest`.
        tx_request_b64: String,
        reply: oneshot::Sender<RuntimeResponse<ProposeTxResult>>,
    },

    /// Execute a transaction (prove). Mirrors
    /// `MultisigClient::execute_multisig_transaction`. Returns the
    /// proven tx bytes + canonical id hex.
    ExecuteTx {
        /// Base64 of `Account`.
        account_b64: String,
        /// Base64 of `TransactionRequest`.
        tx_request_b64: String,
        /// Base64 of `TransactionSummary`.
        tx_summary_b64: String,
        /// Per-approver-slot prepared signatures (base64 of each
        /// `Vec<Felt>` encoded). `None` for slots without sigs.
        signatures_b64: Vec<Option<String>>,
        reply: oneshot::Sender<RuntimeResponse<ExecuteTxResult>>,
    },

    /// Submit a `TransactionBatch` (collection of `ProvenTransaction`s)
    /// to the node via `SubmitProvenBatch`.
    SubmitProvenBatch {
        /// Each entry is base64 of one `ProvenTransaction`.
        proven_txs_b64: Vec<String>,
        reply: oneshot::Sender<RuntimeResponse<SubmitBatchResult>>,
    },

    /// Graceful exit.
    Shutdown,
}

impl MidenRuntimeMsg {
    /// In skeleton mode the runtime replies "not implemented" to every
    /// non-shutdown message. This helper sends the right `RuntimeResponse`
    /// variant down whichever oneshot is in the message.
    pub fn send_not_implemented(self) {
        let err = || AgenticError::MidenRuntime("runtime skeleton — message not yet wired".into());
        match self {
            Self::EnsureGenesis { reply } => {
                let _ = reply.send(RuntimeResponse::Err(err()));
            }
            Self::SyncState { reply } => {
                let _ = reply.send(RuntimeResponse::Err(err()));
            }
            Self::ProposeTx { reply, .. } => {
                let _ = reply.send(RuntimeResponse::Err(err()));
            }
            Self::ExecuteTx { reply, .. } => {
                let _ = reply.send(RuntimeResponse::Err(err()));
            }
            Self::SubmitProvenBatch { reply, .. } => {
                let _ = reply.send(RuntimeResponse::Err(err()));
            }
            Self::Shutdown => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProposeTxResult {
    /// Base64 of canonical `TransactionSummary`.
    pub tx_summary_b64: String,
}

#[derive(Debug, Clone)]
pub struct ExecuteTxResult {
    /// Base64 of canonical `ProvenTransaction`.
    pub proven_tx_b64: String,
    /// Hex of the post-prove `ProvenTransaction.id()`.
    pub proven_tx_id_hex: String,
}

#[derive(Debug, Clone)]
pub struct SubmitBatchResult {
    /// Block number the batch landed in.
    pub block_num: u64,
}
