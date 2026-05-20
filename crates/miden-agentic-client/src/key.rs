//! Thin wrapper around `miden-keystore` for the agent's hot key.

use std::fs;
use std::path::PathBuf;

use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::error::{AgenticError, Result};

/// Holds the agent's hot Falcon keypair on disk (one keystore dir per
/// agent). Use `load_or_create` to get a stable key across restarts.
#[derive(Clone)]
pub struct HotKey {
    inner: FilesystemKeyStore<ChaCha20Rng>,
    commitment: Word,
}

impl HotKey {
    pub fn load_or_create(keystore_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&keystore_dir)
            .map_err(|e| AgenticError::Keystore(format!("mkdir: {e}")))?;
        let marker = keystore_dir.join("commitment_marker.txt");
        let inner = FilesystemKeyStore::with_rng(keystore_dir.clone(), ChaCha20Rng::from_os_rng())
            .map_err(|e| AgenticError::Keystore(format!("keystore: {e}")))?;
        let commitment = if marker.is_file() {
            let hex = fs::read_to_string(&marker)
                .map_err(|e| AgenticError::Keystore(format!("read marker: {e}")))?;
            parse_word_hex(hex.trim())?
        } else {
            let w = inner
                .generate_key()
                .map_err(|e| AgenticError::Keystore(format!("keygen: {e}")))?;
            fs::write(&marker, word_to_hex(w))
                .map_err(|e| AgenticError::Keystore(format!("write marker: {e}")))?;
            w
        };
        Ok(Self { inner, commitment })
    }

    pub fn commitment(&self) -> Word {
        self.commitment
    }

    pub fn commitment_hex(&self) -> String {
        word_to_hex(self.commitment)
    }

    pub fn sign_word_hex(&self, msg: Word) -> Result<String> {
        use miden_protocol::utils::serde::Serializable;
        let sig = self
            .inner
            .sign(self.commitment, msg)
            .map_err(|e| AgenticError::Keystore(format!("sign: {e}")))?;
        Ok(format!("0x{}", hex::encode(sig.to_bytes())))
    }

    /// Read the underlying Falcon `SecretKey` from the keystore.
    /// Needed when the same key is also bound into a Miden account's
    /// auth component via `miden-client`.
    pub fn secret_key(&self) -> Result<SecretKey> {
        self.inner
            .get_key(self.commitment)
            .map_err(|e| AgenticError::Keystore(format!("get_key: {e}")))
    }

    /// Import a pre-existing Falcon `SecretKey` into a fresh keystore
    /// at `keystore_dir`. Used by the bench to attach the agent's
    /// hot key from `setup-testnet`'s saved `hot_key.bin` snapshot.
    pub fn import_secret_key(keystore_dir: PathBuf, sk: SecretKey) -> Result<Self> {
        fs::create_dir_all(&keystore_dir)
            .map_err(|e| AgenticError::Keystore(format!("mkdir: {e}")))?;
        let inner = FilesystemKeyStore::with_rng(keystore_dir.clone(), ChaCha20Rng::from_os_rng())
            .map_err(|e| AgenticError::Keystore(format!("keystore: {e}")))?;
        let commitment = sk.public_key().to_commitment();
        inner
            .add_key(&sk)
            .map_err(|e| AgenticError::Keystore(format!("add_key: {e}")))?;
        let marker = keystore_dir.join("commitment_marker.txt");
        fs::write(&marker, word_to_hex(commitment))
            .map_err(|e| AgenticError::Keystore(format!("write marker: {e}")))?;
        Ok(Self { inner, commitment })
    }
}

pub fn word_to_hex(w: Word) -> String {
    use miden_protocol::utils::serde::Serializable;
    format!("0x{}", hex::encode(w.to_bytes()))
}

pub fn parse_word_hex(s: &str) -> Result<Word> {
    use miden_protocol::utils::serde::Deserializable;
    let stripped = s.trim_start_matches("0x");
    let bytes =
        hex::decode(stripped).map_err(|e| AgenticError::Keystore(format!("hex Word: {e}")))?;
    Word::read_from_bytes(&bytes).map_err(|e| AgenticError::Keystore(format!("Word read: {e}")))
}
