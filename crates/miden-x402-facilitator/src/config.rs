//! Guardian-facilitator configuration loaded from environment variables.
//!
//! The binary runs **as** an OZ Guardian server with the x402 module
//! mounted, so the Guardian half of the configuration (database, rate
//! limits, request size, EVM toggle, etc.) is read by `guardian-server`'s
//! own env loaders. This module covers only the x402-specific knobs.
//!
//! Recognised x402 variables (all optional):
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `MIDEN_X402_LISTEN_ADDR` | `0.0.0.0:8080` | HTTP bind address |
//! | `MIDEN_X402_RPC_URL` | `https://rpc.testnet.miden.io` | Miden node gRPC URL (used by the batch worker for submit + the verify-path `check_nullifiers` backstop) |
//! | `MIDEN_X402_REMOTE_PROVER_URL` | _unset_ | gRPC URL of the Miden remote prover. **Required** — the batch worker fails fast on startup if unset. |
//! | `MIDEN_X402_NETWORK` | `miden:testnet` | CAIP-2 network id. `miden:mainnet` for production. |
//! | `MIDEN_X402_STORAGE_ROOT` | `/var/x402/state` | Root directory for filesystem-backed repos (challenges, reservations, batch queue, receipt key) |
//! | `MIDEN_X402_CHALLENGE_TTL_SECS` | `120` | TTL applied to issued `serial_num` challenges. Should be ≥ the `maxTimeoutSeconds` the merchant advertises. |
//! | `MIDEN_X402_RESERVATION_TTL_SECS` | `60` | TTL applied to reserved input nullifiers — defensive, the happy path releases explicitly. |
//! | `MIDEN_X402_BATCH_MAX_SIZE` | `8` | Maximum number of verified txs the worker drains in one cycle. |
//! | `MIDEN_X402_BATCH_MAX_AGE_MS` | `750` | The oldest verified tx triggers a drain after this many milliseconds, even if `BATCH_MAX_SIZE` is not reached. |
//! | `MIDEN_X402_BATCH_TICK_MS` | `100` | How often the batch worker wakes to evaluate the drain conditions. |
//! | `MIDEN_X402_MANDATE_POLICY` | `allow-all` | Selector loaded by the binary. Only `allow-all` is built in today; concrete policies are out of scope (see `docs/mandate.md`). |

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Errors returned by [`FacilitatorConfig::from_env`].
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid {var}: {source}")]
    Parse {
        var: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("invalid {var}: {message}")]
    Invalid { var: &'static str, message: String },
    #[error("missing required {var}")]
    Missing { var: &'static str },
}

/// Top-level configuration the binary reads at startup.
#[derive(Debug, Clone)]
pub struct FacilitatorConfig {
    pub listen_addr: SocketAddr,
    pub rpc_url: String,
    pub remote_prover_url: String,
    /// CAIP-2 network id (e.g. `"miden:testnet"`), echoed into
    /// `SettleResponse::Success { network }`.
    pub network: String,
    pub storage_root: PathBuf,
    pub challenge_ttl: Duration,
    pub reservation_ttl: Duration,
    pub batch: BatchSettleConfig,
    pub mandate: MandatePolicyConfig,
}

/// Batch-settle worker knobs.
#[derive(Debug, Clone)]
pub struct BatchSettleConfig {
    pub max_batch_size: usize,
    pub max_batch_age: Duration,
    pub tick_interval: Duration,
}

/// Mandate policy selection.
#[derive(Debug, Clone)]
pub enum MandatePolicyConfig {
    AllowAll,
}

impl FacilitatorConfig {
    /// Loads configuration from environment variables. Fails fast if any
    /// required variable is missing or malformed.
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen_addr = env::var("MIDEN_X402_LISTEN_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
            .parse::<SocketAddr>()
            .map_err(|e| ConfigError::Parse {
                var: "MIDEN_X402_LISTEN_ADDR",
                source: Box::new(e),
            })?;

        let rpc_url = env::var("MIDEN_X402_RPC_URL")
            .unwrap_or_else(|_| "https://rpc.testnet.miden.io".to_owned());

        let remote_prover_url = env::var("MIDEN_X402_REMOTE_PROVER_URL").map_err(|_| {
            ConfigError::Missing { var: "MIDEN_X402_REMOTE_PROVER_URL" }
        })?;
        if remote_prover_url.trim().is_empty() {
            return Err(ConfigError::Invalid {
                var: "MIDEN_X402_REMOTE_PROVER_URL",
                message: "value is empty".to_owned(),
            });
        }

        let network = env::var("MIDEN_X402_NETWORK")
            .unwrap_or_else(|_| "miden:testnet".to_owned());
        if !network.starts_with("miden:") {
            return Err(ConfigError::Invalid {
                var: "MIDEN_X402_NETWORK",
                message: format!("not a Miden CAIP-2 id: {network}"),
            });
        }

        let storage_root = PathBuf::from(
            env::var("MIDEN_X402_STORAGE_ROOT")
                .unwrap_or_else(|_| "/var/x402/state".to_owned()),
        );

        let challenge_ttl =
            parse_secs("MIDEN_X402_CHALLENGE_TTL_SECS", 120)?;
        let reservation_ttl =
            parse_secs("MIDEN_X402_RESERVATION_TTL_SECS", 60)?;

        let batch = BatchSettleConfig {
            max_batch_size: parse_usize("MIDEN_X402_BATCH_MAX_SIZE", 8)?,
            max_batch_age: parse_ms("MIDEN_X402_BATCH_MAX_AGE_MS", 750)?,
            tick_interval: parse_ms("MIDEN_X402_BATCH_TICK_MS", 100)?,
        };

        let mandate = match env::var("MIDEN_X402_MANDATE_POLICY")
            .unwrap_or_else(|_| "allow-all".to_owned())
            .as_str()
        {
            "allow-all" => MandatePolicyConfig::AllowAll,
            other => {
                return Err(ConfigError::Invalid {
                    var: "MIDEN_X402_MANDATE_POLICY",
                    message: format!("unknown policy: {other}"),
                });
            }
        };

        Ok(Self {
            listen_addr,
            rpc_url,
            remote_prover_url,
            network,
            storage_root,
            challenge_ttl,
            reservation_ttl,
            batch,
            mandate,
        })
    }
}

fn parse_secs(var: &'static str, default: u64) -> Result<Duration, ConfigError> {
    let raw = match env::var(var) {
        Ok(v) => v,
        Err(_) => return Ok(Duration::from_secs(default)),
    };
    let secs: u64 = raw.parse().map_err(|e: std::num::ParseIntError| {
        ConfigError::Parse { var, source: Box::new(e) }
    })?;
    Ok(Duration::from_secs(secs))
}

fn parse_ms(var: &'static str, default: u64) -> Result<Duration, ConfigError> {
    let raw = match env::var(var) {
        Ok(v) => v,
        Err(_) => return Ok(Duration::from_millis(default)),
    };
    let ms: u64 = raw.parse().map_err(|e: std::num::ParseIntError| {
        ConfigError::Parse { var, source: Box::new(e) }
    })?;
    Ok(Duration::from_millis(ms))
}

fn parse_usize(var: &'static str, default: usize) -> Result<usize, ConfigError> {
    let raw = match env::var(var) {
        Ok(v) => v,
        Err(_) => return Ok(default),
    };
    raw.parse::<usize>().map_err(|e| ConfigError::Parse { var, source: Box::new(e) })
}
