//! Real Miden integration: a `MidenIntegration` owns a `miden-client::Client`
//! backed by SQLite, the agent's Miden account, the agent's Falcon
//! `SecretKey`, and the Miden RPC endpoint. It exposes:
//!
//! - `MidenIntegration::create_agent_account(...)` to mint a fresh Miden
//!   wallet account that uses the agent's hot Falcon key for auth.
//! - `MidenIntegration::execute_for_summary(p2id_request)` to run a P2ID
//!   transaction "for summary" — i.e. execute it locally but stop before
//!   proving, so the agent can sign and ship the unproven summary to the
//!   facilitator.
//! - `MidenIntegration::current_account_commitment()` for state tracking.

use std::path::PathBuf;
use std::sync::Arc;

use miden_client::account::component::BasicWallet;
use miden_client::account::{Account, AccountBuilder, AccountStorageMode, AccountType};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::asset::FungibleAsset;
use miden_client::note::{NoteAttachment, NoteType, P2idNote};
use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
use miden_client::{Client as MidenSdkClient, ClientError};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use rand::RngCore;

use crate::error::{AgenticError, Result};

/// Long-lived Miden integration owned by an `AgenticClient`.
pub struct MidenIntegration {
    client: tokio::sync::Mutex<MidenSdkClient<FilesystemKeyStore>>,
    keystore: Arc<FilesystemKeyStore>,
    account_id: tokio::sync::RwLock<Option<AccountId>>,
}

impl MidenIntegration {
    /// Connect to the Miden network at `rpc_endpoint`, opening a fresh
    /// SQLite store under `data_dir` and a fresh keystore.
    pub async fn connect(
        rpc_endpoint: &str,
        data_dir: PathBuf,
        timeout_ms: u64,
    ) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| AgenticError::Config(format!("data dir: {e}")))?;
        let endpoint = Endpoint::try_from(rpc_endpoint)
            .map_err(|e| AgenticError::Config(format!("endpoint {rpc_endpoint}: {e}")))?;
        let rpc_client = Arc::new(GrpcClient::new(&endpoint, timeout_ms));
        let keystore = Arc::new(
            FilesystemKeyStore::new(data_dir.join("keystore"))
                .map_err(|e| AgenticError::Keystore(format!("keystore: {e}")))?,
        );
        let store_path = data_dir.join("store.sqlite3");
        let client = ClientBuilder::new()
            .rpc(rpc_client)
            .sqlite_store(store_path)
            .authenticator(keystore.clone())
            .in_debug_mode(false.into())
            .build()
            .await
            .map_err(|e| AgenticError::Config(format!("client build: {e}")))?;
        Ok(Self {
            client: tokio::sync::Mutex::new(client),
            keystore,
            account_id: tokio::sync::RwLock::new(None),
        })
    }

    /// Sync the local state with the network. Returns the current block number.
    pub async fn sync(&self) -> Result<u32> {
        let mut client = self.client.lock().await;
        let summary = client
            .sync_state()
            .await
            .map_err(|e: ClientError| AgenticError::Config(format!("sync: {e}")))?;
        Ok(summary.block_num.as_u32())
    }

    /// Create a fresh Miden wallet account whose auth is bound to the
    /// given Falcon `SecretKey`. The same `SecretKey` will be reused
    /// for the agent's hot-key signing on the facilitator side.
    pub async fn create_agent_account(&self, secret_key: SecretKey) -> Result<AccountId> {
        let mut client = self.client.lock().await;
        let mut init_seed = [0u8; 32];
        client.rng().fill_bytes(&mut init_seed);

        let pubkey_commitment = secret_key.public_key().to_commitment();
        let auth_secret = AuthSecretKey::Falcon512Poseidon2(secret_key);

        let account = AccountBuilder::new(init_seed)
            .account_type(AccountType::RegularAccountUpdatableCode)
            .storage_mode(AccountStorageMode::Public)
            .with_auth_component(AuthSingleSig::new(
                pubkey_commitment.into(),
                AuthSchemeId::Falcon512Poseidon2,
            ))
            .with_component(BasicWallet)
            .build()
            .map_err(|e| AgenticError::Config(format!("build account: {e}")))?;

        let id = account.id();
        client
            .add_account(&account, false)
            .await
            .map_err(|e| AgenticError::Config(format!("add account: {e}")))?;
        self.keystore
            .add_key(&auth_secret, id)
            .await
            .map_err(|e| AgenticError::Keystore(format!("keystore add_key: {e}")))?;

        *self.account_id.write().await = Some(id);
        Ok(id)
    }

    /// Load an existing account by ID (already known to this client's store).
    pub async fn use_existing_account(&self, account_id: AccountId) -> Result<()> {
        *self.account_id.write().await = Some(account_id);
        Ok(())
    }

    /// Import a serialized account into this integration's store WITHOUT
    /// adding the secret key. The integration's `execute_for_summary`
    /// path expects auth to fail with `Unauthorized(summary)` — which
    /// only happens when the keystore lacks the key. Use this to wire
    /// in a snapshot produced by the `setup-testnet` binary.
    pub async fn import_account_snapshot(&self, account_bytes: &[u8]) -> Result<AccountId> {
        let account = Account::read_from_bytes(account_bytes)
            .map_err(|e| AgenticError::Config(format!("Account decode: {e}")))?;
        let id = account.id();
        let mut client = self.client.lock().await;
        client
            .add_account(&account, false)
            .await
            .map_err(|e| AgenticError::Config(format!("add_account: {e}")))?;
        *self.account_id.write().await = Some(id);
        Ok(id)
    }

    pub async fn account_id(&self) -> Option<AccountId> {
        *self.account_id.read().await
    }

    /// Fetch the current account commitment from the local store.
    pub async fn current_account_commitment(&self) -> Result<Word> {
        let id = self
            .account_id()
            .await
            .ok_or_else(|| AgenticError::Config("no agent account".into()))?;
        let client = self.client.lock().await;
        let account = client
            .get_account(id)
            .await
            .map_err(|e| AgenticError::Config(format!("get_account: {e}")))?
            .ok_or_else(|| AgenticError::Config(format!("account {id} not in store")))?;
        Ok(account.to_commitment())
    }

    /// Build a P2ID `TransactionRequest` that transfers `amount` units of
    /// the asset minted by `faucet_id` from the agent's vault to `recipient`.
    pub async fn build_p2id_request(
        &self,
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
    ) -> Result<TransactionRequest> {
        let sender_id = self
            .account_id()
            .await
            .ok_or_else(|| AgenticError::Config("no agent account".into()))?;
        let asset = FungibleAsset::new(faucet_id, amount)
            .map_err(|e| AgenticError::Config(format!("FungibleAsset::new: {e}")))?;
        let mut client = self.client.lock().await;
        let p2id = P2idNote::create(
            sender_id,
            recipient,
            vec![asset.into()],
            NoteType::Public,
            NoteAttachment::default(),
            client.rng(),
        )
        .map_err(|e| AgenticError::Config(format!("P2idNote::create: {e}")))?;
        TransactionRequestBuilder::new()
            .own_output_notes(vec![p2id])
            .build()
            .map_err(|e| AgenticError::Config(format!("tx_request build: {e}")))
    }

    /// Execute a transaction "for summary": run the auth flow which
    /// surfaces the `TransactionSummary` via the `Unauthorized` error
    /// variant (because no signature has been provided yet). Returns
    /// the unproven `TransactionSummary` ready for the agent to sign.
    pub async fn execute_for_summary(
        &self,
        request: TransactionRequest,
    ) -> Result<TransactionSummary> {
        use miden_client::transaction::TransactionExecutorError;
        let id = self
            .account_id()
            .await
            .ok_or_else(|| AgenticError::Config("no agent account".into()))?;
        let mut client = self.client.lock().await;
        match client.execute_transaction(id, request).await {
            Ok(_) => Err(AgenticError::Config(
                "expected Unauthorized error carrying summary".into(),
            )),
            Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
                summary,
            ))) => Ok(*summary),
            Err(other) => Err(AgenticError::Config(format!(
                "execute: {other} | debug: {other:?}"
            ))),
        }
    }
}

/// Serialize a `TransactionSummary` for transport to the facilitator.
pub fn summary_to_base64(summary: &TransactionSummary) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(summary.to_bytes())
}

/// Serialize a `TransactionRequest` for transport to the facilitator.
/// The facilitator deserializes it, injects the hot-key signature
/// into its `advice_map`, and submits.
pub fn request_to_base64(request: &TransactionRequest) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(request.to_bytes())
}

/// Pull the input-note nullifiers out of a `TransactionSummary` in hex form.
pub fn extract_input_nullifiers_hex(summary: &TransactionSummary) -> Vec<String> {
    use miden_protocol::transaction::ToInputNoteCommitments;
    summary
        .input_notes()
        .iter()
        .map(|n| format!("0x{}", hex::encode(n.nullifier().as_bytes())))
        .collect()
}
