//! CAIP-2 identifiers for the Miden network.
//!
//! Miden does not yet have an officially registered CAIP-2 namespace.
//! We use the provisional namespace `miden` with references `testnet` and
//! `mainnet`. The string forms are `"miden:testnet"` and `"miden:mainnet"`.

use x402_types::chain::ChainId;

/// The provisional CAIP-2 namespace for Miden.
pub const MIDEN_NAMESPACE: &str = "miden";

/// The CAIP-2 reference for the Miden public testnet.
pub const TESTNET_REFERENCE: &str = "testnet";

/// The CAIP-2 reference for the Miden mainnet (reserved; not yet live).
pub const MAINNET_REFERENCE: &str = "mainnet";

/// Returns the [`ChainId`] for the Miden testnet.
pub fn miden_testnet() -> ChainId {
    ChainId::new(MIDEN_NAMESPACE, TESTNET_REFERENCE)
}

/// Returns the [`ChainId`] for the Miden mainnet.
///
/// Reserved for when Miden mainnet goes live.
pub fn miden_mainnet() -> ChainId {
    ChainId::new(MIDEN_NAMESPACE, MAINNET_REFERENCE)
}

/// Returns `true` if the given chain identifier is in the Miden namespace.
pub fn is_miden(chain_id: &ChainId) -> bool {
    chain_id.namespace() == MIDEN_NAMESPACE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn testnet_serialises_correctly() {
        assert_eq!(miden_testnet().to_string(), "miden:testnet");
    }

    #[test]
    fn mainnet_serialises_correctly() {
        assert_eq!(miden_mainnet().to_string(), "miden:mainnet");
    }

    #[test]
    fn testnet_round_trips_through_chain_id_fromstr() {
        let parsed = ChainId::from_str("miden:testnet").expect("parse");
        assert_eq!(parsed, miden_testnet());
    }

    #[test]
    fn is_miden_recognises_namespace() {
        assert!(is_miden(&miden_testnet()));
        assert!(is_miden(&miden_mainnet()));
        assert!(!is_miden(&ChainId::new("eip155", "1")));
    }
}
