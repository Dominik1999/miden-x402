//! Binary entry point for the Miden x402 facilitator.
//!
//! Loads configuration from environment variables, initialises tracing,
//! connects to the configured Miden node, and serves the axum router from
//! [`miden_x402_facilitator::handlers::build_router`].

use std::process::ExitCode;

use miden_x402_facilitator::{AppState, FacilitatorConfig, GrpcMidenNode, build_router};
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = run().await {
        eprintln!("miden-x402-facilitator: fatal: {e}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_tracing();

    let config = FacilitatorConfig::from_env()?;
    info!(
        listen_addr = %config.listen_addr,
        rpc_url = %config.rpc_url,
        freshness_blocks = config.freshness_blocks,
        "loaded facilitator configuration",
    );

    let node = GrpcMidenNode::from_url(&config.rpc_url, config.rpc_timeout_ms)?;
    let listen_addr = config.listen_addr;
    let state = AppState::new(node, config);

    let app = build_router(state).layer(TraceLayer::new_for_http());
    let listener = TcpListener::bind(listen_addr).await?;
    info!(%listen_addr, "miden-x402-facilitator listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
