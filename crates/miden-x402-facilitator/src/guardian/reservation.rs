//! In-memory reserved-nullifier set with TTL.
//!
//! When the Guardian verifies a signed-but-unproven transaction it reserves
//! the input nullifiers so a concurrent verify of a tx that consumes the
//! same note is rejected as a pending double-spend. Reservations are held
//! until one of three things happens:
//!
//! 1. **Settle succeeds.** The reservation is promoted to "consumed" and
//!    kept around until the on-chain tx is included; from that point the
//!    on-chain nullifier check is authoritative.
//! 2. **Settle fails.** The reservation is released immediately so the
//!    note becomes available again.
//! 3. **TTL expires.** A background sweeper releases stale reservations.
//!    Defensive — the success / failure path should normally release.
//!
//! Structurally the same mechanism EIP-3009 (the basis for x402) uses with
//! nonces, just enforced at the Guardian rather than the on-chain contract
//! — see `ideas/GUARDIAN.md` §"Open question 1".

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Hex string of a Miden `Nullifier::to_hex()` value.
pub type NullifierKey = String;

/// State of a single reserved nullifier.
#[derive(Debug, Clone)]
pub struct Reservation {
    /// When the reservation was first made.
    pub reserved_at: Instant,
    /// When the reservation should be auto-released if not explicitly
    /// promoted or released by then.
    pub expires_at: Instant,
    /// `true` once `promote_to_consumed` is called, meaning the Guardian's
    /// settle path has successfully submitted the tx and is waiting for it
    /// to appear on chain.
    pub promoted: bool,
}

/// In-memory set of nullifiers currently reserved by the Guardian.
#[derive(Debug, Clone)]
pub struct ReservedNullifierSet {
    inner: Arc<Mutex<HashMap<NullifierKey, Reservation>>>,
    ttl: Duration,
}

impl ReservedNullifierSet {
    /// Constructs a new empty set with the given TTL applied to each
    /// reservation.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Returns the per-reservation TTL.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Atomically reserves a single nullifier. Returns `Err(AlreadyReserved)`
    /// if the nullifier is currently reserved (regardless of `promoted`).
    pub fn try_reserve(&self, nullifier: &str) -> Result<(), AlreadyReserved> {
        let mut g = self.inner.lock().expect("reservation mutex poisoned");
        self.sweep_locked(&mut g);
        if g.contains_key(nullifier) {
            return Err(AlreadyReserved);
        }
        let now = Instant::now();
        g.insert(
            nullifier.to_owned(),
            Reservation {
                reserved_at: now,
                expires_at: now + self.ttl,
                promoted: false,
            },
        );
        Ok(())
    }

    /// Atomically reserves a batch of nullifiers. Either all are reserved
    /// or none are (rolls back any partial reservations on failure).
    pub fn try_reserve_all(&self, nullifiers: &[String]) -> Result<(), AlreadyReserved> {
        let mut g = self.inner.lock().expect("reservation mutex poisoned");
        self.sweep_locked(&mut g);
        for n in nullifiers {
            if g.contains_key(n) {
                return Err(AlreadyReserved);
            }
        }
        let now = Instant::now();
        for n in nullifiers {
            g.insert(
                n.clone(),
                Reservation {
                    reserved_at: now,
                    expires_at: now + self.ttl,
                    promoted: false,
                },
            );
        }
        Ok(())
    }

    /// Marks each nullifier as "promoted to consumed" — the Guardian has
    /// successfully submitted the tx to the network. The entry is retained
    /// (so an immediate re-verify of the same note still fails) but its
    /// `promoted` flag becomes `true`.
    pub fn promote_to_consumed(&self, nullifiers: &[String]) {
        let mut g = self.inner.lock().expect("reservation mutex poisoned");
        for n in nullifiers {
            if let Some(r) = g.get_mut(n) {
                r.promoted = true;
            }
        }
    }

    /// Releases each reservation in the batch. Called on settle failure.
    pub fn release(&self, nullifiers: &[String]) {
        let mut g = self.inner.lock().expect("reservation mutex poisoned");
        for n in nullifiers {
            g.remove(n);
        }
    }

    /// Returns the current state of `nullifier`. `None` if not reserved.
    pub fn get(&self, nullifier: &str) -> Option<Reservation> {
        let g = self.inner.lock().expect("reservation mutex poisoned");
        g.get(nullifier).cloned()
    }

    /// Number of currently reserved nullifiers (post-sweep).
    pub fn len(&self) -> usize {
        let mut g = self.inner.lock().expect("reservation mutex poisoned");
        self.sweep_locked(&mut g);
        g.len()
    }

    /// `true` if no nullifiers are reserved.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes expired entries. Called eagerly on each operation, and also
    /// from the background sweeper task as a defensive guard against
    /// long-lived reservations that nothing came back to release.
    pub fn sweep(&self) {
        let mut g = self.inner.lock().expect("reservation mutex poisoned");
        self.sweep_locked(&mut g);
    }

    fn sweep_locked(&self, g: &mut HashMap<NullifierKey, Reservation>) {
        let now = Instant::now();
        g.retain(|_, r| r.expires_at > now);
    }
}

/// Returned by `try_reserve` / `try_reserve_all` when one of the requested
/// nullifiers is already in the set.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("nullifier already reserved")]
pub struct AlreadyReserved;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_reserve_single_succeeds_then_blocks_duplicate() {
        let set = ReservedNullifierSet::new(Duration::from_secs(60));
        assert!(set.try_reserve("0xaaaa").is_ok());
        assert_eq!(set.try_reserve("0xaaaa"), Err(AlreadyReserved));
    }

    #[test]
    fn release_makes_nullifier_available_again() {
        let set = ReservedNullifierSet::new(Duration::from_secs(60));
        set.try_reserve("0xaaaa").unwrap();
        set.release(&["0xaaaa".to_owned()]);
        assert!(set.try_reserve("0xaaaa").is_ok());
    }

    #[test]
    fn promote_keeps_entry_reserved() {
        let set = ReservedNullifierSet::new(Duration::from_secs(60));
        set.try_reserve("0xaaaa").unwrap();
        set.promote_to_consumed(&["0xaaaa".to_owned()]);
        // Still reserved after promotion — a parallel verify must not slip
        // through while the promoted tx is in the mempool.
        assert_eq!(set.try_reserve("0xaaaa"), Err(AlreadyReserved));
        let r = set.get("0xaaaa").unwrap();
        assert!(r.promoted);
    }

    #[test]
    fn try_reserve_all_rolls_back_on_conflict() {
        let set = ReservedNullifierSet::new(Duration::from_secs(60));
        set.try_reserve("0xbbbb").unwrap();
        // Try reserving [a, b]: must fail and NOT leave `a` reserved.
        let res = set.try_reserve_all(&["0xaaaa".to_owned(), "0xbbbb".to_owned()]);
        assert_eq!(res, Err(AlreadyReserved));
        assert!(set.get("0xaaaa").is_none());
    }

    #[test]
    fn sweep_drops_expired_entries() {
        let set = ReservedNullifierSet::new(Duration::from_millis(1));
        set.try_reserve("0xaaaa").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        // Expired entries are dropped on the next op.
        assert!(set.try_reserve("0xaaaa").is_ok());
    }
}
