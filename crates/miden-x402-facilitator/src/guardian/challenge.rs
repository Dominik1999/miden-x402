//! Server-generated `serial_num` lifecycle for the Guardian flow.
//!
//! Per GUARDIAN.md, the Guardian generates the P2ID note's `serial_num` at
//! 402-time and includes it in the merchant's `PaymentRequirements.extra`.
//! The buyer must use that exact value when constructing the note, which
//! lets the Guardian:
//!
//! - Know the future note's nullifier the moment it issues the 402 (the
//!   nullifier is a function of `serial_num + script_root + storage +
//!   asset_commitment`, all of which the Guardian can compute up-front).
//! - Bind the challenge to a single merchant offer — the buyer can't reuse
//!   one signed unproven tx against multiple paywalls.
//!
//! Identity is by the `serial_num` itself (a 32-byte `Word`). One issue ⇒
//! one consume; replays fail at the lookup step.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use miden_protocol::Word;
use miden_x402_types::{MidenPaymentRequirements, NoteIdHex};
use rand::RngCore;

/// A challenge issued to a merchant at 402-time.
#[derive(Debug, Clone)]
pub struct IssuedChallenge {
    /// The server-generated `serial_num` as a Word. Also serves as the
    /// challenge id.
    pub serial_num: Word,
    /// Same value as a hex string so it can be echoed onto the wire.
    pub serial_num_hex: NoteIdHex,
    /// Snapshot of the merchant's `PaymentRequirements` at issue time.
    /// The verifier uses these to reconstruct the P2ID note's expected
    /// recipient + asset shape, which together with `serial_num` give the
    /// expected nullifier.
    pub requirements: MidenPaymentRequirements,
    /// When the challenge was issued.
    pub issued_at: Instant,
    /// When the challenge should be considered expired and dropped.
    pub expires_at: Instant,
}

/// In-memory store of issued challenges keyed by serial_num hex.
#[derive(Debug, Clone)]
pub struct ChallengeStore {
    inner: Arc<Mutex<HashMap<String, IssuedChallenge>>>,
    ttl: Duration,
}

impl ChallengeStore {
    /// Constructs a new empty store with the given TTL applied to each
    /// challenge.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Returns the TTL applied to each challenge.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Issues a new challenge for the given requirements. Returns the
    /// generated `serial_num` (as both Word and hex) and the issued record.
    /// `rng` must produce cryptographically random bytes.
    pub fn issue<R: RngCore>(
        &self,
        requirements: &MidenPaymentRequirements,
        rng: &mut R,
    ) -> IssuedChallenge {
        let serial_num = random_word(rng);
        let serial_num_hex: NoteIdHex = serial_num
            .to_hex()
            .parse()
            .expect("Word::to_hex always produces 0x + 64 hex chars");
        let now = Instant::now();
        let issued = IssuedChallenge {
            serial_num,
            serial_num_hex: serial_num_hex.clone(),
            requirements: requirements.clone(),
            issued_at: now,
            expires_at: now + self.ttl,
        };
        let mut g = self.inner.lock().expect("challenge mutex poisoned");
        sweep_locked(&mut g);
        g.insert(serial_num_hex.as_str().to_owned(), issued.clone());
        issued
    }

    /// Looks up a challenge by `serial_num_hex` without consuming it.
    /// Returns `None` if not found or expired (in which case the entry is
    /// also evicted as a side-effect).
    pub fn peek(&self, serial_num_hex: &str) -> Option<IssuedChallenge> {
        let mut g = self.inner.lock().expect("challenge mutex poisoned");
        sweep_locked(&mut g);
        g.get(serial_num_hex).cloned()
    }

    /// Looks up and **removes** a challenge in one atomic step. The
    /// verifier calls this so a challenge cannot be re-used.
    pub fn consume(&self, serial_num_hex: &str) -> Option<IssuedChallenge> {
        let mut g = self.inner.lock().expect("challenge mutex poisoned");
        sweep_locked(&mut g);
        g.remove(serial_num_hex)
    }

    /// Number of currently outstanding challenges (post-sweep).
    pub fn len(&self) -> usize {
        let mut g = self.inner.lock().expect("challenge mutex poisoned");
        sweep_locked(&mut g);
        g.len()
    }

    /// `true` if no challenges are outstanding.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops expired entries. Called eagerly on each op; the background
    /// sweeper task also calls this periodically.
    pub fn sweep(&self) {
        let mut g = self.inner.lock().expect("challenge mutex poisoned");
        sweep_locked(&mut g);
    }
}

fn sweep_locked(g: &mut HashMap<String, IssuedChallenge>) {
    let now = Instant::now();
    g.retain(|_, c| c.expires_at > now);
}

fn random_word<R: RngCore>(rng: &mut R) -> Word {
    use miden_protocol::Felt;
    // Sample 4 field elements; reduce each into the Goldilocks field by
    // taking the low 64 bits and letting `Felt::new` apply the canonical
    // reduction.
    let f = |rng: &mut R| {
        let mut buf = [0u8; 8];
        rng.fill_bytes(&mut buf);
        Felt::new(u64::from_le_bytes(buf))
    };
    Word::new([f(rng), f(rng), f(rng), f(rng)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_x402_types::{
        AssetTransferMethodTag, ExactScheme, MidenExactExtra, NoteKind, SettlementKind,
        miden_testnet,
    };
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn sample_requirements() -> MidenPaymentRequirements {
        MidenPaymentRequirements {
            scheme: ExactScheme,
            network: miden_testnet(),
            amount: "1000".to_owned(),
            pay_to: "0x103f8a1ad4b983104aec0412ab0b0d".parse().unwrap(),
            max_timeout_seconds: 120,
            asset: "0x0a7d175ed63ec5200fb2ced86f6aa5".parse().unwrap(),
            extra: MidenExactExtra {
                asset_transfer_method: AssetTransferMethodTag,
                token_symbol: "USDC".to_owned(),
                decimals: 6,
                note_type: NoteKind::Private,
                settlement: SettlementKind::GuardianFast,
                guardian_url: Some("https://facilitator.miden.io".to_owned()),
                serial_num: None,
                agentic_guardian_url: None,
                mandate_id: None,
                note_tag: None,
            },
        }
    }

    #[test]
    fn issue_then_consume_succeeds_once() {
        let store = ChallengeStore::new(Duration::from_secs(60));
        let mut rng = StdRng::seed_from_u64(42);
        let issued = store.issue(&sample_requirements(), &mut rng);
        let hex = issued.serial_num_hex.as_str().to_owned();

        let consumed = store.consume(&hex).expect("first consume");
        assert_eq!(consumed.serial_num, issued.serial_num);

        // Second consume must fail.
        assert!(store.consume(&hex).is_none());
    }

    #[test]
    fn peek_does_not_consume() {
        let store = ChallengeStore::new(Duration::from_secs(60));
        let mut rng = StdRng::seed_from_u64(7);
        let issued = store.issue(&sample_requirements(), &mut rng);
        let hex = issued.serial_num_hex.as_str().to_owned();

        assert!(store.peek(&hex).is_some());
        assert!(store.peek(&hex).is_some());
        assert!(store.consume(&hex).is_some());
        assert!(store.peek(&hex).is_none());
    }

    #[test]
    fn issue_produces_distinct_serial_nums() {
        let store = ChallengeStore::new(Duration::from_secs(60));
        let mut rng = StdRng::seed_from_u64(123);
        let a = store.issue(&sample_requirements(), &mut rng);
        let b = store.issue(&sample_requirements(), &mut rng);
        assert_ne!(a.serial_num_hex.as_str(), b.serial_num_hex.as_str());
    }

    #[test]
    fn expired_challenges_are_swept() {
        let store = ChallengeStore::new(Duration::from_millis(1));
        let mut rng = StdRng::seed_from_u64(0);
        store.issue(&sample_requirements(), &mut rng);
        assert_eq!(store.len(), 1);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(store.len(), 0);
    }
}
