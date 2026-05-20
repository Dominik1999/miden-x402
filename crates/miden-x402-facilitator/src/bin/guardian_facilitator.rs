//! `guardian-facilitator` — Guardian-as-x402-facilitator binary.
//!
//! Hosts both OZ Guardian's routes and the new `/x402/*` routes on the
//! same axum router. Constructs Guardian's `AppState` directly from its
//! pub building blocks so we can compose a single router (`Router::merge`)
//! instead of running two listeners; this works around the fact that
//! `server::api::build_router` does not exist as a `pub fn` upstream yet
//! (see [`docs/UPSTREAM_WISHLIST.md`]).
//!
//! Background tasks spawned at startup:
//! - Guardian's canonicalization worker (`server::services::start_canonicalization_worker`)
//! - x402 batch settle worker
//! - x402 challenge + reservation TTL sweepers
//!
//! Env vars: see [`miden_x402_facilitator::config`] and OZ Guardian's
//! [README](https://github.com/OpenZeppelin/guardian).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::routing::{get, post};
use axum::Router;
use miden_client::note::Nullifier;
use miden_protocol::Word;
use miden_protocol::transaction::TransactionInputs;
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use miden_x402_facilitator::{
    balance::{BalanceError, BalanceLookup},
    batch::{
        BatchSettleQueue, BatchSettleWorker, ProvenTx, ProvenTxProver, ProvenTxSubmitter,
        ProverError, SubmitError,
    },
    buyer_auth::{BuyerAuthError, BuyerAuthLookup},
    config::{FacilitatorConfig, MandatePolicyConfig},
    handlers::{X402AppState, x402_router},
    mandate::AllowAll,
    receipt::ReceiptSigner,
    storage::{
        BatchQueueRepo, ChallengeRepo, FacilitatorKeyStore, ReservationRepo,
        filesystem::{
            FilesystemBatchQueueRepo, FilesystemChallengeRepo, FilesystemKeyStore,
            FilesystemReservationRepo,
        },
        unix_now,
    },
    verify::{NullifierBackstop, NullifierCheckError},
};
use miden_x402_types::AccountIdHex;

// --- Guardian-side constructors -----------------------------------------
use server::{
    ack::AckRegistry,
    builder::clock::SystemClock,
    dashboard::DashboardState,
    metadata::{Auth, MetadataStore, filesystem::FilesystemMetadataStore},
    network::{NetworkType, miden::MidenNetworkClient},
    state::AppState as GuardianAppState,
    state_object::StateObject,
    storage::{StorageBackend, filesystem::FilesystemService},
    services::start_canonicalization_worker,
};
use server::api::http::{
    configure, get_delta, get_delta_proposal, get_delta_proposals, get_delta_since,
    get_pubkey, get_state, lookup, push_delta, push_delta_proposal, sign_delta_proposal,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ----- Logging --------------------------------------------------------
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // ----- Config ---------------------------------------------------------
    let cfg = FacilitatorConfig::from_env().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let cfg = Arc::new(cfg);
    info!(listen_addr = %cfg.listen_addr, "loaded facilitator config");

    // ----- Guardian AppState ---------------------------------------------
    let guardian_root: PathBuf = cfg.storage_root.join("guardian");
    let storage: Arc<dyn StorageBackend> = Arc::new(
        FilesystemService::new(guardian_root.clone())
            .await
            .map_err(anyhow::Error::msg)?,
    );
    let metadata: Arc<dyn MetadataStore> = Arc::new(
        FilesystemMetadataStore::new(guardian_root.clone())
            .await
            .map_err(anyhow::Error::msg)?,
    );
    let keystore_path = guardian_root.join("keystore");
    let ack = AckRegistry::new(keystore_path).await.map_err(anyhow::Error::msg)?;
    let dashboard = Arc::new(DashboardState::default());
    let clock = Arc::new(SystemClock);
    let network_client = Arc::new(TokioMutex::new(
        MidenNetworkClient::from_network(NetworkType::MidenTestnet)
            .await
            .map_err(anyhow::Error::msg)?,
    ));
    let guardian_state = GuardianAppState {
        storage: storage.clone(),
        metadata: metadata.clone(),
        network_client: network_client.clone(),
        ack,
        canonicalization: None, // canonicalization worker spawned only when configured
        clock,
        dashboard,
    };

    // ----- x402 storage repos (filesystem) -------------------------------
    let x402_root = cfg.storage_root.join("x402");
    tokio::fs::create_dir_all(&x402_root).await.ok();
    let challenges: Arc<dyn ChallengeRepo> = Arc::new(FilesystemChallengeRepo::new(&x402_root));
    let reservations: Arc<dyn ReservationRepo> = Arc::new(FilesystemReservationRepo::new(&x402_root));
    let batch_repo: Arc<dyn BatchQueueRepo> = Arc::new(FilesystemBatchQueueRepo::new(&x402_root));
    let key_store: Arc<dyn FacilitatorKeyStore> = Arc::new(FilesystemKeyStore::new(&x402_root));

    // ----- Receipt signer (load or generate) -----------------------------
    let signer = ReceiptSigner::load_or_generate(key_store.as_ref()).await?;
    info!(commitment = %signer.pubkey_commitment_hex(), "facilitator receipt key ready");

    // ----- Mandate policy ------------------------------------------------
    let mandate: Arc<dyn miden_x402_facilitator::MandatePolicy> = match cfg.mandate {
        MandatePolicyConfig::AllowAll => Arc::new(AllowAll),
    };

    // ----- Buyer auth + balance lookups (wrapping Guardian metadata/storage) ---
    let buyer_auth: Arc<dyn BuyerAuthLookup> = Arc::new(GuardianBuyerAuthLookup {
        metadata: metadata.clone(),
    });
    let balance: Arc<dyn BalanceLookup> = Arc::new(GuardianBalanceLookup {
        storage: storage.clone(),
    });

    // ----- Nullifier backstop, prover, submitter -------------------------
    let nullifier_backstop: Arc<dyn NullifierBackstop> = Arc::new(NoopNullifierBackstop);
    let prover: Arc<dyn ProvenTxProver> = Arc::new(RemoteProver::new(cfg.remote_prover_url.clone()));
    let submitter: Arc<dyn ProvenTxSubmitter> = Arc::new(NodeSubmitter::new(cfg.rpc_url.clone()));

    // ----- Build x402 app state + batch worker ---------------------------
    let queue = BatchSettleQueue::new(batch_repo.clone());
    let x402_state = X402AppState {
        config: cfg.clone(),
        challenges: challenges.clone(),
        reservations: reservations.clone(),
        queue: queue.clone(),
        mandate: mandate.clone(),
        signer: signer.clone(),
        buyer_auth: buyer_auth.clone(),
        balance: balance.clone(),
        nullifier_backstop: nullifier_backstop.clone(),
    };

    let worker = Arc::new(BatchSettleWorker::new(
        queue.clone(),
        reservations.clone(),
        prover,
        submitter,
        cfg.batch.clone(),
    ));
    worker.clone().spawn();

    // ----- Sweepers ------------------------------------------------------
    let challenges_for_sweep = challenges.clone();
    let reservations_for_sweep = reservations.clone();
    let sweep_interval = Duration::from_secs(30);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(sweep_interval).await;
            let now = unix_now();
            let _ = challenges_for_sweep.sweep(now).await;
            let _ = reservations_for_sweep.sweep(now).await;
        }
    });

    // ----- Canonicalization worker (only if Guardian was built with one) ---
    if guardian_state.canonicalization.is_some() {
        start_canonicalization_worker(guardian_state.clone());
    }

    // ----- Build merged router ------------------------------------------
    let guardian_router = Router::new()
        .route("/configure", post(configure))
        .route("/delta", post(push_delta).get(get_delta))
        .route("/delta/since", get(get_delta_since))
        .route("/delta/proposal", post(push_delta_proposal).get(get_delta_proposals).put(sign_delta_proposal))
        .route("/delta/proposal/single", get(get_delta_proposal))
        .route("/state", get(get_state))
        .route("/state/lookup", get(lookup))
        .route("/pubkey", get(get_pubkey))
        .with_state(guardian_state);
    let app = Router::new()
        .merge(guardian_router)
        .merge(x402_router(x402_state));

    let listener = tokio::net::TcpListener::bind(cfg.listen_addr).await?;
    info!(addr = %listener.local_addr()?, "guardian-facilitator listening");
    axum::serve(listener, app).await?;
    Ok(())
}

// =====================================================================
//  Guardian-backed buyer auth + balance lookups
// =====================================================================

/// Reads `cosigner_commitments` out of Guardian's account metadata.
struct GuardianBuyerAuthLookup {
    metadata: Arc<dyn MetadataStore>,
}

#[async_trait]
impl BuyerAuthLookup for GuardianBuyerAuthLookup {
    async fn cosigner_commitments(
        &self,
        buyer: &AccountIdHex,
    ) -> Result<Vec<Word>, BuyerAuthError> {
        let m = self
            .metadata
            .get(buyer.as_str())
            .await
            .map_err(BuyerAuthError::Backend)?;
        let m = m.ok_or(BuyerAuthError::NotConfigured)?;
        let commitments_hex: &[String] = match &m.auth {
            Auth::MidenFalconRpo { cosigner_commitments } => cosigner_commitments.as_slice(),
            _ => return Err(BuyerAuthError::UnsupportedScheme),
        };
        let mut out = Vec::with_capacity(commitments_hex.len());
        for s in commitments_hex {
            let w = Word::try_from(s.as_str())
                .map_err(|e| BuyerAuthError::InvalidCommitment(format!("{s}: {e}")))?;
            out.push(w);
        }
        Ok(out)
    }
}

/// Parses Guardian's stored `state_json` for the buyer's vault balance.
/// Graceful degrade: unrecognized shape → allow (matches the documented
/// fallback in `crate::balance`).
struct GuardianBalanceLookup {
    storage: Arc<dyn StorageBackend>,
}

#[async_trait]
impl BalanceLookup for GuardianBalanceLookup {
    async fn check_sufficient(
        &self,
        buyer: &AccountIdHex,
        faucet: &AccountIdHex,
        required_amount: u128,
    ) -> Result<(), BalanceError> {
        let state: StateObject = self
            .storage
            .pull_state(buyer.as_str())
            .await
            .map_err(|e| BalanceError::Backend(e.to_string()))?;
        match miden_x402_facilitator::balance::check_balance_against_state_json(
            &state.state_json,
            faucet.as_str(),
            required_amount,
        ) {
            None => Ok(()), // unrecognised shape; degrade
            Some(true) => Ok(()),
            Some(false) => Err(BalanceError::Insufficient { have: 0, need: required_amount }),
        }
    }
}

// =====================================================================
//  Prover + submitter + nullifier backstop wiring
// =====================================================================

/// Submits a `TransactionInputs` to the configured remote prover and
/// returns the resulting `ProvenTransaction` bytes + id.
struct RemoteProver {
    url: String,
}

impl RemoteProver {
    fn new(url: String) -> Self { Self { url } }
}

#[async_trait]
impl ProvenTxProver for RemoteProver {
    async fn prove(&self, tx_inputs: TransactionInputs) -> Result<ProvenTx, ProverError> {
        use miden_crypto::utils::Serializable;
        use miden_remote_prover_client::RemoteTransactionProver;
        let prover = RemoteTransactionProver::new(&self.url);
        let proven = prover
            .prove(&tx_inputs)
            .await
            .map_err(|e| ProverError::Backend(e.to_string()))?;
        Ok(ProvenTx {
            id_hex: proven.id().to_hex(),
            bytes: proven.to_bytes(),
        })
    }
}

/// Submits a `ProvenTransaction` blob to the Miden node via the OZ-Guardian
/// `miden-rpc-client` crate.
struct NodeSubmitter {
    url: String,
}

impl NodeSubmitter {
    fn new(url: String) -> Self { Self { url } }
}

#[async_trait]
impl ProvenTxSubmitter for NodeSubmitter {
    async fn submit(&self, proven: ProvenTx) -> Result<(), SubmitError> {
        let mut client = miden_rpc_client::MidenRpcClient::connect(&self.url)
            .await
            .map_err(|e| SubmitError::Backend(format!("connect: {e}")))?;
        client
            .submit_transaction(proven.bytes)
            .await
            .map_err(|e| SubmitError::Backend(format!("submit: {e}")))?;
        Ok(())
    }
}

/// Placeholder nullifier backstop — until the `check_nullifiers` RPC is
/// wired to `miden-rpc-client`, we return `Ok(())` so the verify path
/// proceeds. The reservation set still protects against in-flight
/// double-spends. The Miden team should swap this for a real RPC call
/// before going to production (tracked in
/// [`docs/UPSTREAM_WISHLIST.md`]).
struct NoopNullifierBackstop;

#[async_trait]
impl NullifierBackstop for NoopNullifierBackstop {
    async fn assert_unspent(&self, _: &[Nullifier]) -> Result<(), NullifierCheckError> {
        Ok(())
    }
}
