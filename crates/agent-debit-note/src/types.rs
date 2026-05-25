use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::note::NoteStorage;

/// Storage layout for the AgentDebitNote (batch-settlement spec).
///
/// 9 storage items:
///   [0-3]  user_pub_key_commitment (Word) — agent's Falcon public key
///   [4]    merchant_account_id_suffix — committed payee
///   [5]    merchant_account_id_prefix
///   [6]    user_account_id_suffix — for reclaim path
///   [7]    user_account_id_prefix
///   [8]    reclaim_block_height
pub struct AgentDebitNoteStorage {
    pub user_pubkey_commitment: Word,
    pub merchant_account_id: AccountId,
    pub user_account_id: AccountId,
    pub reclaim_block_height: u64,
}

impl AgentDebitNoteStorage {
    pub fn new(
        user_pubkey_commitment: Word,
        merchant_account_id: AccountId,
        user_account_id: AccountId,
        reclaim_block_height: u64,
    ) -> Self {
        Self {
            user_pubkey_commitment,
            merchant_account_id,
            user_account_id,
            reclaim_block_height,
        }
    }
}

impl From<AgentDebitNoteStorage> for NoteStorage {
    fn from(s: AgentDebitNoteStorage) -> Self {
        use miden_protocol::Felt;
        NoteStorage::new(vec![
            s.user_pubkey_commitment[0],
            s.user_pubkey_commitment[1],
            s.user_pubkey_commitment[2],
            s.user_pubkey_commitment[3],
            s.merchant_account_id.suffix(),
            s.merchant_account_id.prefix().as_felt(),
            s.user_account_id.suffix(),
            s.user_account_id.prefix().as_felt(),
            Felt::new(s.reclaim_block_height),
        ])
        .expect("9 storage items should not exceed max")
    }
}
