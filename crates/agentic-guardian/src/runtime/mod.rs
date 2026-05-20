//! `MidenAgenticClientRuntime` — dedicated thread for the `!Send + !Sync`
//! Miden client.
//!
//! **Critical infrastructure copied from** [`inicio-labs/MultiSig` ::
//! `coordinator/engine/multisig_client_runtime.rs`](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/crates/coordinator/engine/src/multisig_client_runtime.rs).
//! The pattern: spawn a dedicated OS thread, run a `tokio::task::LocalSet`
//! on it, and have external (axum) threads talk to the client via
//! `mpsc::UnboundedSender<Msg>` + `oneshot::Sender<Result>` replies.
//!
//! ```text
//!  Axum thread                              Runtime thread (LocalSet)
//! ┌───────────────────────┐                ┌───────────────────────────────┐
//! │ Handler               │                │ MultisigClient (!Send + !Sync)│
//! │                       │                │                               │
//! │ mpsc::UnboundedSender ┼────────────────│──> mpsc::UnboundedReceiver    │
//! │                       │                │                               │
//! │ oneshot::Receiver <───┼────────────────┤─── oneshot::Sender            │
//! └───────────────────────┘                └───────────────────────────────┘
//! ```
//!
//! Why a dedicated thread: `miden-client::Client` (and the
//! `MultisigClient` wrapper from `inicio-labs/MultiSig`) embeds
//! `!Send + !Sync` state. Tokio's multi-threaded runtime cannot move
//! tasks holding such state across threads. The LocalSet pattern pins
//! the client to one thread; cross-thread access is mediated by
//! message-passing.
//!
//! Messages this runtime handles (defined in [`msg`]):
//!
//! - `EnsureGenesis` — called at startup to make sure the local store
//!   has the chain's genesis block.
//! - `SyncState` — calls `client.sync_state()` before any operation
//!   that needs fresh on-chain data.
//! - `ProposeTx { account_id, tx_request }` → returns `TransactionSummary`
//!   (dry-run; matches `MultisigClient::propose_multisig_transaction`).
//! - `ExecuteTx { account, tx_request, tx_summary, signatures }` →
//!   returns a proven `TransactionResult` (matches
//!   `MultisigClient::execute_multisig_transaction`).
//! - `SubmitProvenBatch { proven_txs }` → submits a `TransactionBatch`
//!   to the node via `SubmitProvenBatch`.
//! - `Shutdown` — graceful exit.

pub mod msg;

use std::thread::JoinHandle;

use tokio::sync::mpsc;

use crate::error::AgenticError;

pub use msg::{MidenRuntimeMsg, RuntimeResponse};

/// Handle to the dedicated Miden-client thread. Cloneable across axum
/// handlers; sends messages via the embedded `mpsc::UnboundedSender`.
#[derive(Clone)]
pub struct MidenRuntimeHandle {
    sender: mpsc::UnboundedSender<MidenRuntimeMsg>,
}

impl MidenRuntimeHandle {
    pub fn new(sender: mpsc::UnboundedSender<MidenRuntimeMsg>) -> Self { Self { sender } }
    /// Sends a message into the runtime. Returns `Err` only if the
    /// runtime thread has shut down.
    pub fn send(&self, msg: MidenRuntimeMsg) -> Result<(), AgenticError> {
        self.sender
            .send(msg)
            .map_err(|_| AgenticError::MidenRuntime("runtime thread closed".into()))
    }
}

/// Configuration for the runtime thread.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub node_url: url::Url,
    pub store_path: std::path::PathBuf,
    pub keystore_path: std::path::PathBuf,
    pub timeout: std::time::Duration,
}

/// Spawns the dedicated runtime thread.
///
/// **Currently a skeleton**: this thread runs a no-op message loop so
/// the binary compiles and can be wired up end-to-end. The real
/// implementation initializes a `miden_client::Client` + the inicio-labs
/// `MultisigClient`, runs `ensure_genesis_in_place` and `sync_state`,
/// then dispatches messages — see the copied pattern in
/// [`/.reference/inicio-multisig/engine/multisig_client_runtime.rs`].
pub fn spawn_runtime(
    mut receiver: mpsc::UnboundedReceiver<MidenRuntimeMsg>,
    _config: RuntimeConfig,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // Build a single-threaded tokio runtime so the LocalSet works.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread tokio runtime");
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async move {
            // TODO: construct miden_client::Client::builder() ...
            // TODO: wrap in MultisigClient::new(...)
            // TODO: ensure_genesis_in_place + sync_state
            tracing::info!("MidenAgenticClientRuntime started (skeleton — no real client yet)");
            while let Some(msg) = receiver.recv().await {
                match msg {
                    MidenRuntimeMsg::Shutdown => {
                        tracing::info!("runtime shutdown requested");
                        break;
                    }
                    other => {
                        tracing::warn!(
                            "runtime received message but is in skeleton mode; replying NotImplemented"
                        );
                        other.send_not_implemented();
                    }
                }
            }
        }));
    })
}
