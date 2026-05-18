//! Facilitator configuration loaded from environment variables.
//!
//! Recognised variables (all optional, all with defaults):
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `MIDEN_X402_LISTEN_ADDR` | `0.0.0.0:8080` | HTTP bind address |
//! | `MIDEN_X402_RPC_URL` | `https://rpc.testnet.miden.io` | Miden node gRPC URL |
//! | `MIDEN_X402_RPC_TIMEOUT_MS` | `10000` | RPC timeout per call |
//! | `MIDEN_X402_ALLOWED_FAUCETS` | `0x0a7d175ed63ec5200fb2ced86f6aa5` | Comma-separated faucet account IDs accepted as payment assets. Use `*` to allow any. |
//! | `MIDEN_X402_FRESHNESS_BLOCKS` | `24` | Max blocks between note commitment and the current tip. Roughly two minutes at ~5s blocks. |
//! | `MIDEN_X402_GUARDIAN_ENABLED` | `false` | Enable the `/guardian/*` Phase B endpoints |
//! | `MIDEN_X402_REMOTE_PROVER_URL` | _unset_ | gRPC URL of the Miden remote prover. Required when `MIDEN_X402_GUARDIAN_ENABLED=true`. |
//! | `MIDEN_X402_GUARDIAN_CHALLENGE_TTL_SECS` | `120` | TTL for issued `serial_num` challenges |
//! | `MIDEN_X402_GUARDIAN_RESERVATION_TTL_SECS` | `60` | TTL for reserved input nullifiers |
//! | `RUST_LOG` | `info` | Standard `tracing-subscriber` env filter |

use std::env;
use std::net::SocketAddr;

use miden_x402_types::AccountIdHex;
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
}

/// Allowlist of accepted faucet account IDs.
#[derive(Debug, Clone)]
pub enum FaucetAllowlist {
    /// Any faucet is accepted (development convenience).
    Any,
    /// Only faucets in the given list are accepted.
    Only(Vec<AccountIdHex>),
}

impl FaucetAllowlist {
    /// Returns true if `faucet` is permitted by this allowlist.
    pub fn allows(&self, faucet: &AccountIdHex) -> bool {
        match self {
            FaucetAllowlist::Any => true,
            FaucetAllowlist::Only(list) => list.iter().any(|allowed| allowed == faucet),
        }
    }
}

/// Configuration the facilitator binary reads at startup.
#[derive(Debug, Clone)]
pub struct FacilitatorConfig {
    /// HTTP socket the facilitator listens on.
    pub listen_addr: SocketAddr,
    /// Miden node gRPC URL.
    pub rpc_url: String,
    /// RPC timeout in milliseconds.
    pub rpc_timeout_ms: u64,
    /// Allowlist of accepted faucet account IDs.
    pub allowed_faucets: FaucetAllowlist,
    /// Maximum age, in blocks, between the note's commit block and the
    /// current chain tip.
    pub freshness_blocks: u32,
    /// Guardian (Phase B) configuration. Off by default; the `/guardian/*`
    /// endpoints return `501 Not Implemented` when [`GuardianConfig::enabled`]
    /// is `false` and the rest of the facilitator behaves byte-for-byte
    /// like Phase A.
    pub guardian: GuardianConfig,
}

/// Phase B Guardian configuration.
#[derive(Debug, Clone)]
pub struct GuardianConfig {
    /// Whether the `/guardian/*` HTTP endpoints are wired into the router.
    pub enabled: bool,
    /// gRPC URL of the remote prover. Required when `enabled = true`;
    /// otherwise `/guardian/settle` returns `503 Service Unavailable`.
    pub remote_prover_url: Option<String>,
    /// TTL applied to each issued `serial_num` challenge. Should be ≥ the
    /// `maxTimeoutSeconds` the merchant advertises in its 402.
    pub challenge_ttl_secs: u64,
    /// TTL applied to each reserved input nullifier. Defensive — the
    /// success / failure paths normally release reservations explicitly.
    pub reservation_ttl_secs: u64,
}

impl Default for GuardianConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            remote_prover_url: None,
            challenge_ttl_secs: 120,
            reservation_ttl_secs: 60,
        }
    }
}

impl FacilitatorConfig {
    /// Loads configuration from environment variables, falling back to
    /// sensible defaults documented at the module level.
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

        let rpc_timeout_ms = env::var("MIDEN_X402_RPC_TIMEOUT_MS")
            .ok()
            .map(|raw| {
                raw.parse::<u64>().map_err(|e| ConfigError::Parse {
                    var: "MIDEN_X402_RPC_TIMEOUT_MS",
                    source: Box::new(e),
                })
            })
            .transpose()?
            .unwrap_or(10_000);

        let allowed_faucets = parse_allowlist(
            &env::var("MIDEN_X402_ALLOWED_FAUCETS")
                .unwrap_or_else(|_| "0x0a7d175ed63ec5200fb2ced86f6aa5".to_owned()),
        )?;

        let freshness_blocks = env::var("MIDEN_X402_FRESHNESS_BLOCKS")
            .ok()
            .map(|raw| {
                raw.parse::<u32>().map_err(|e| ConfigError::Parse {
                    var: "MIDEN_X402_FRESHNESS_BLOCKS",
                    source: Box::new(e),
                })
            })
            .transpose()?
            .unwrap_or(24);

        let guardian = parse_guardian_config()?;

        Ok(Self {
            listen_addr,
            rpc_url,
            rpc_timeout_ms,
            allowed_faucets,
            freshness_blocks,
            guardian,
        })
    }
}

fn parse_guardian_config() -> Result<GuardianConfig, ConfigError> {
    let enabled = env::var("MIDEN_X402_GUARDIAN_ENABLED")
        .ok()
        .map(|raw| matches!(raw.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);

    let remote_prover_url = env::var("MIDEN_X402_REMOTE_PROVER_URL").ok().and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
    });

    let challenge_ttl_secs = env::var("MIDEN_X402_GUARDIAN_CHALLENGE_TTL_SECS")
        .ok()
        .map(|raw| {
            raw.parse::<u64>().map_err(|e| ConfigError::Parse {
                var: "MIDEN_X402_GUARDIAN_CHALLENGE_TTL_SECS",
                source: Box::new(e),
            })
        })
        .transpose()?
        .unwrap_or(120);

    let reservation_ttl_secs = env::var("MIDEN_X402_GUARDIAN_RESERVATION_TTL_SECS")
        .ok()
        .map(|raw| {
            raw.parse::<u64>().map_err(|e| ConfigError::Parse {
                var: "MIDEN_X402_GUARDIAN_RESERVATION_TTL_SECS",
                source: Box::new(e),
            })
        })
        .transpose()?
        .unwrap_or(60);

    Ok(GuardianConfig {
        enabled,
        remote_prover_url,
        challenge_ttl_secs,
        reservation_ttl_secs,
    })
}

fn parse_allowlist(raw: &str) -> Result<FaucetAllowlist, ConfigError> {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return Ok(FaucetAllowlist::Any);
    }
    let mut ids = Vec::new();
    for entry in trimmed.split(',') {
        let token = entry.trim();
        if token.is_empty() {
            continue;
        }
        let id: AccountIdHex = token.parse().map_err(|_| ConfigError::Invalid {
            var: "MIDEN_X402_ALLOWED_FAUCETS",
            message: format!("not a valid Miden account id: {token}"),
        })?;
        ids.push(id);
    }
    if ids.is_empty() {
        return Err(ConfigError::Invalid {
            var: "MIDEN_X402_ALLOWED_FAUCETS",
            message: "no entries provided".to_owned(),
        });
    }
    Ok(FaucetAllowlist::Only(ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_any_accepts_everything() {
        let list = parse_allowlist("*").unwrap();
        let id: AccountIdHex = "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap();
        assert!(list.allows(&id));
    }

    #[test]
    fn allowlist_filters_unknown_faucets() {
        let list = parse_allowlist("0x0a7d175ed63ec5200fb2ced86f6aa5").unwrap();
        let allowed: AccountIdHex = "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap();
        let other: AccountIdHex = "0xdeadbeefdeadbeefdeadbeefdeadbe".parse().unwrap();
        assert!(list.allows(&allowed));
        assert!(!list.allows(&other));
    }

    #[test]
    fn allowlist_rejects_garbage_entry() {
        assert!(parse_allowlist("not-an-id").is_err());
    }

    #[test]
    fn allowlist_rejects_empty() {
        assert!(parse_allowlist("   ").is_err());
    }
}
