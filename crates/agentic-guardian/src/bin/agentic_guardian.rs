//! `agentic-guardian` binary entry point.
//!
//! Wires the storage layer, runtime thread, mandate evaluator,
//! pending-state tracker, batch worker, and axum router into one
//! process per [`ideas/NEW_DESIGN.md`].

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use agentic_guardian::{
    api,
    batch::{BatchSettleQueue, BatchSettleWorker},
    config::Config,
    mandate::Ap2Policy,
    recovery,
    runtime::{MidenRuntimeHandle, RuntimeConfig, spawn_runtime},
    state::pending::PendingStateTracker,
    storage::{
        AgentRegistryRepo, BatchQueueRepo, ChallengeRepo, MandateCounterRepo, MandateRepo,
        PendingStateRepo, ReservationRepo, postgres::PostgresStorage,
    },
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config_path = std::env::var("MIDENX402_CONFIG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("base_config.ron"));
    let config = Arc::new(Config::load(&config_path).map_err(anyhow::Error::msg)?);
    info!(listen = %config.app.listen, "loaded agentic-guardian config");

    // ----- Storage (currently delegates to in-memory) -----
    let storage = PostgresStorage::in_memory_fallback();
    let agents: Arc<dyn AgentRegistryRepo> = Arc::new(storage.clone());
    let mandates: Arc<dyn MandateRepo> = Arc::new(storage.clone());
    let pending_repo: Arc<dyn PendingStateRepo> = Arc::new(storage.clone());
    let reservations: Arc<dyn ReservationRepo> = Arc::new(storage.clone());
    let queue_repo: Arc<dyn BatchQueueRepo> = Arc::new(storage.clone());
    let challenges: Arc<dyn ChallengeRepo> = Arc::new(storage.clone());
    let counters: Arc<dyn MandateCounterRepo> = Arc::new(storage.clone());

    // ----- Crash-recovery replay at boot -----
    let report = recovery::replay_on_boot(&reservations, &queue_repo).await?;
    info!(?report, "WAL recovery complete");

    // ----- Spawn the !Send + !Sync Miden client runtime -----
    let (rt_tx, rt_rx) = mpsc::unbounded_channel();
    let runtime_handle = MidenRuntimeHandle::new(rt_tx);
    let _runtime_thread = spawn_runtime(
        rt_rx,
        RuntimeConfig {
            node_url: config.miden.node_url.clone(),
            store_path: config.miden.store_path.clone(),
            keystore_path: config.miden.keystore_path.clone(),
            timeout: config.miden.timeout,
        },
    );

    // ----- AP2 policy + pending-state tracker -----
    let policy = Ap2Policy::new(counters);
    let pending_state = PendingStateTracker::new(pending_repo);

    // ----- Batch worker -----
    let queue = BatchSettleQueue::new(queue_repo);
    let worker = Arc::new(BatchSettleWorker::new(
        queue.clone(),
        reservations.clone(),
        runtime_handle.clone(),
        config.batch.clone(),
    ));
    worker.clone().spawn();

    // ----- TTL sweepers -----
    let res_for_sweep = reservations.clone();
    let chl_for_sweep = challenges.clone();
    let sweep_interval = std::time::Duration::from_secs(30);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(sweep_interval).await;
            let now = agentic_guardian::storage::memory::unix_now();
            let _ = res_for_sweep.sweep(now).await;
            let _ = chl_for_sweep.sweep(now).await;
        }
    });

    // ----- Build router + serve -----
    let state = api::AppState {
        config: config.clone(),
        agents,
        mandates,
        pending_state,
        reservations,
        queue,
        challenges,
        policy,
        runtime: runtime_handle,
    };
    let app = api::build_router(state);

    let listener = tokio::net::TcpListener::bind(&config.app.listen).await?;
    info!(addr = %listener.local_addr()?, "agentic-guardian listening");
    axum::serve(listener, app).await?;
    Ok(())
}
