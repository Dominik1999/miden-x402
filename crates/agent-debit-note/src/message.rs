use miden_protocol::{Hasher, Word};
use miden_protocol::Felt;
use miden_protocol::account::AccountId;

/// Compute the message the agent signs to authorize a debit.
///
/// MESSAGE = poseidon2::merge(SERIAL_NUM, [merchant_suffix, merchant_prefix, amount, 0])
pub fn debit_message(
    note_serial_num: Word,
    merchant_account_id: AccountId,
    amount: u64,
) -> Word {
    let debit_word = Word::from([
        merchant_account_id.suffix(),
        merchant_account_id.prefix().as_felt(),
        Felt::new(amount),
        Felt::ZERO,
    ]);
    merge_words(note_serial_num, debit_word)
}

/// Compute the message the agent signs to authorize a reclaim.
///
/// MESSAGE = poseidon2::merge(SERIAL_NUM, [user_suffix, user_prefix, 0, 0])
pub fn reclaim_message(
    note_serial_num: Word,
    user_account_id: AccountId,
) -> Word {
    let reclaim_word = Word::from([
        user_account_id.suffix(),
        user_account_id.prefix().as_felt(),
        Felt::ZERO,
        Felt::ZERO,
    ]);
    merge_words(note_serial_num, reclaim_word)
}

fn merge_words(a: Word, b: Word) -> Word {
    Hasher::merge(&[a, b])
}
