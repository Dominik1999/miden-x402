//! agentic-guardian configuration.
//!
//! Loaded from a RON file (default `base_config.ron`) plus environment
//! variable overrides with the `MIDENX402_` prefix. Mirrors the
//! [inicio-labs coordinator-server config pattern](https://github.com/inicio-labs/MultiSig/blob/hubcycle-ui-flow-md/bin/coordinator-server/README.md).
//!
//! ```text
//! Config(
//!     app: AppConfig(...),
//!     db:  DbConfig(...),
//!     miden: MidenConfig(...),
//!     batch: BatchConfig(...),
//!     mandate: MandateConfig(...),
//! )
//! ```
//!
//! Override examples:
//!
//! ```bash
//! export MIDENX402_APP__LISTEN="0.0.0.0:8080"
//! export MIDENX402_DB__DB_URL="postgres://..."
//! export MIDENX402_MIDEN__NODE_URL="https://rpc.testnet.miden.io:443"
//! export MIDENX402_BATCH__MAX_BATCH_SIZE="16"
//! ```

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config build error: {0}")]
    Build(#[from] config::ConfigError),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub app: AppConfig,
    pub db: DbConfig,
    pub miden: MidenConfig,
    pub batch: BatchConfig,
    pub mandate: MandateConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub listen: String,
    pub network_id: String,
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DbConfig {
    pub db_url: String,
    pub max_conn: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MidenConfig {
    pub node_url: Url,
    pub remote_prover_url: Url,
    pub store_path: PathBuf,
    pub keystore_path: PathBuf,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BatchConfig {
    pub max_batch_size: usize,
    #[serde(with = "humantime_serde")]
    pub max_batch_age: Duration,
    #[serde(with = "humantime_serde")]
    pub tick_interval: Duration,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MandateConfig {
    /// Per-mandate challenge TTL (the agentic-guardian issues a
    /// `serial_num` per 402; this is how long it stays valid).
    #[serde(with = "humantime_serde")]
    pub challenge_ttl: Duration,
    /// Defensive ceiling on a reserved nullifier's lifetime. Happy path
    /// is release-on-commit / release-on-failure; this is the fallback
    /// for a process that crashed without observing the reconciler.
    #[serde(with = "humantime_serde")]
    pub reservation_ttl: Duration,
}

impl Config {
    /// Loads from `path` (RON) with `MIDENX402_*` env overrides.
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let cfg = config::Config::builder()
            .add_source(config::File::from(path))
            .add_source(
                config::Environment::with_prefix("MIDENX402")
                    .prefix_separator("_")
                    .separator("__"),
            )
            .build()?;
        Ok(cfg.try_deserialize()?)
    }
}
