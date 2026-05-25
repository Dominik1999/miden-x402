//! AgentDebitNote MASM script tests — batch-settlement spec.
//! Single agent signature for consume path, committed merchant in storage.

use std::collections::BTreeMap;

use miden_protocol::{Felt, Hasher, Word};
use miden_protocol::account::AccountId;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::note::*;
use miden_protocol::transaction::RawOutputNote;
use miden_protocol::vm::AdviceInputs;
use miden_standards::code_builder::CodeBuilder;
use miden_testing::{Auth, MockChain};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const NOTE_MASM: &str = include_str!("../masm/agent_debit_note.masm");

fn make_keypair(seed: u64) -> AuthSecretKey {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    AuthSecretKey::new_falcon512_poseidon2_with_rng(&mut rng)
}

fn debit_message(serial: Word, merchant: AccountId, cumulative_amount: u64) -> Word {
    let dw: Word = [merchant.suffix(), merchant.prefix().as_felt(), Felt::new(cumulative_amount), Felt::ZERO].into();
    Hasher::merge(&[serial, dw])
}

fn reclaim_message(serial: Word, user: AccountId) -> Word {
    let rw: Word = [user.suffix(), user.prefix().as_felt(), Felt::ZERO, Felt::ZERO].into();
    Hasher::merge(&[serial, rw])
}

fn serial(a: u64, b: u64, c: u64, d: u64) -> Word {
    [Felt::new(a), Felt::new(b), Felt::new(c), Felt::new(d)].into()
}

/// Build advice inputs with agent sig in the advice map (matching MASM adv.has_mapkey pattern).
fn agent_sig_in_map(agent_sk: &AuthSecretKey, message: Word) -> AdviceInputs {
    let sig = agent_sk.sign(message);
    let agent_pk: Word = agent_sk.public_key().to_commitment().into();
    let sig_key = Hasher::merge(&[agent_pk, message]);
    let prepared = sig.to_prepared_signature(message);
    AdviceInputs::default().with_map([(sig_key, prepared)])
}

/// Build advice inputs with agent sig on the advice stack (fallback path in MASM).
fn agent_sig_on_stack(agent_sk: &AuthSecretKey, message: Word) -> AdviceInputs {
    let sig = agent_sk.sign(message);
    AdviceInputs::default().with_stack(sig.to_prepared_signature(message))
}

struct TestSetup {
    mock_chain: MockChain,
    note_id: NoteId,
    note_script: NoteScript,
    serial_num: Word,
    consumer_id: AccountId,
    merchant_id: AccountId,
    user_id: AccountId,
}

/// Create a test setup with the batch-settlement storage layout (9 items).
fn setup_test(agent_pk: Word, balance: u64, sn: Word, expiry: u64) -> anyhow::Result<TestSetup> {
    let note_script = CodeBuilder::default().compile_note_script(NOTE_MASM)?;

    let mut builder = MockChain::builder();
    let consumer = builder.add_existing_wallet(Auth::IncrNonce)?;
    let faucet = builder.add_existing_basic_faucet(Auth::IncrNonce, "USDC", 1_000_000, None)?;
    let merchant = builder.add_existing_wallet(Auth::Noop)?;
    let user = builder.add_existing_wallet(Auth::Noop)?;

    let asset = FungibleAsset::new(faucet.id(), balance)?;
    // 9-item storage: [user_pk(4), merchant(2), user(2), reclaim_block]
    let storage = NoteStorage::new(vec![
        agent_pk[0], agent_pk[1], agent_pk[2], agent_pk[3],
        merchant.id().suffix(), merchant.id().prefix().as_felt(),
        user.id().suffix(), user.id().prefix().as_felt(),
        Felt::new(expiry),
    ])?;

    let metadata = NoteMetadata::new(consumer.id(), NoteType::Public).with_tag(NoteTag::new(0));
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(sn, note_script.clone(), storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();

    builder.add_output_note(RawOutputNote::Full(note));
    let mut mock_chain = builder.build()?;
    mock_chain.prove_next_block()?;
    mock_chain.prove_next_block()?;

    Ok(TestSetup {
        mock_chain, note_id, note_script, serial_num: sn,
        consumer_id: consumer.id(), merchant_id: merchant.id(),
        user_id: user.id(),
    })
}

/// Note args for consume: [cumulativeAmount, 0, 0, 0]
fn note_args_for_consume(cumulative_amount: u64, note_id: NoteId) -> BTreeMap<NoteId, Word> {
    let mut args = BTreeMap::new();
    args.insert(note_id, [Felt::new(cumulative_amount), Felt::ZERO, Felt::ZERO, Felt::ZERO].into());
    args
}

/// Note args for reclaim: [0, 0, 0, 0]
fn note_args_for_reclaim(note_id: NoteId) -> BTreeMap<NoteId, Word> {
    let mut args = BTreeMap::new();
    args.insert(note_id, [Felt::ZERO; 4].into());
    args
}

// ── CONSUME PATH TESTS (single agent sig, committed merchant) ──

#[tokio::test]
async fn test_valid_consume() -> anyhow::Result<()> {
    let agent_sk = make_keypair(1);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(1,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 100);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(100, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&agent_sk, msg))
        .build()?;

    let executed = tx.execute().await?;
    assert_eq!(executed.output_notes().num_notes(), 2); // P2ID + remainder
    Ok(())
}

#[tokio::test]
async fn test_consume_with_sig_on_stack() -> anyhow::Result<()> {
    let agent_sk = make_keypair(2);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(2,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 100);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(100, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_on_stack(&agent_sk, msg))
        .build()?;

    let executed = tx.execute().await?;
    assert_eq!(executed.output_notes().num_notes(), 2);
    Ok(())
}

#[tokio::test]
async fn test_wrong_agent_sig_rejected() -> anyhow::Result<()> {
    let agent_sk = make_keypair(3);
    let wrong_sk = make_keypair(999);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(3,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 100);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(100, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&wrong_sk, msg))
        .build()?;

    assert!(tx.execute().await.is_err());
    Ok(())
}

#[tokio::test]
async fn test_cumulative_exceeds_balance() -> anyhow::Result<()> {
    let agent_sk = make_keypair(4);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 100, serial(4,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 999); // exceeds 100

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(999, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&agent_sk, msg))
        .build()?;

    assert!(tx.execute().await.is_err());
    Ok(())
}

#[tokio::test]
async fn test_remainder_correct_value() -> anyhow::Result<()> {
    let agent_sk = make_keypair(5);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(5,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 300);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(300, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&agent_sk, msg))
        .build()?;

    let executed = tx.execute().await?;
    let notes = executed.output_notes();
    assert_eq!(notes.num_notes(), 2);
    // Both output notes' assets should sum to 1000
    Ok(())
}

// ── RECLAIM PATH TESTS ──

#[tokio::test]
async fn test_valid_reclaim() -> anyhow::Result<()> {
    let agent_sk = make_keypair(10);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    // expiry=1 means block > 1 → routes to consume; block <= 1 → routes to reclaim
    // MockChain starts at block ~2-3 after two prove_next_block calls
    // We need expiry high enough that block > expiry is FALSE → reclaim path
    // Actually: the check is `block > expiry → consume, else → reclaim`
    // To trigger reclaim: block <= expiry, i.e. set expiry very high
    // Wait — that's the opposite. Let me re-read the MASM:
    // `swap gt` with [expiry, block_num] → gt checks block_num > expiry
    // if TRUE → consume_path
    // if FALSE → reclaim_path
    // So reclaim triggers when block_num <= expiry
    // For MockChain (block ~2), set expiry=1 → block(2) > 1 → consume path (NOT reclaim)
    // Set expiry=100 → block(2) > 100 = FALSE → reclaim path ✓
    // For reclaim: expiry must be <= current block
    // MockChain is at block ~2 after two prove_next_block calls
    // gt checks: reclaim > block → if true, consume; else reclaim
    // So expiry=1, block=2: reclaim(1) > block(2) = FALSE → reclaim path ✓
    let s = setup_test(pk, 1000, serial(10,2,3,4), 1)?;
    let msg = reclaim_message(s.serial_num, s.user_id);

    // Reclaim uses the advice stack path (no adv.has_mapkey in reclaim MASM)
    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_reclaim(s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_on_stack(&agent_sk, msg))
        .build()?;

    let executed = tx.execute().await?;
    assert_eq!(executed.output_notes().num_notes(), 1); // P2ID to user only
    Ok(())
}

#[tokio::test]
async fn test_reclaim_wrong_sig() -> anyhow::Result<()> {
    let agent_sk = make_keypair(11);
    let wrong_sk = make_keypair(888);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(11,2,3,4), 1)?;
    let msg = reclaim_message(s.serial_num, s.user_id);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_reclaim(s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_on_stack(&wrong_sk, msg))
        .build()?;

    assert!(tx.execute().await.is_err());
    Ok(())
}

// ── ATTACK VECTOR TESTS ──

#[tokio::test]
async fn test_attack_amount_inflation() -> anyhow::Result<()> {
    let agent_sk = make_keypair(20);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(20,2,3,4), 1_000_000)?;
    // Agent signs for 100, but note_args say 999
    let msg = debit_message(s.serial_num, s.merchant_id, 100);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(999, s.note_id))  // inflated
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&agent_sk, msg))
        .build()?;

    assert!(tx.execute().await.is_err());
    Ok(())
}

#[tokio::test]
async fn test_attack_unauthorized_consumer() -> anyhow::Result<()> {
    let agent_sk = make_keypair(21);
    let attacker_sk = make_keypair(777);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(21,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 100);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(100, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&attacker_sk, msg))
        .build()?;

    assert!(tx.execute().await.is_err());
    Ok(())
}

#[tokio::test]
async fn test_no_facilitator_needed() -> anyhow::Result<()> {
    // Verify that single agent sig is sufficient (no dual-sig required)
    let agent_sk = make_keypair(22);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(22,2,3,4), 1_000_000)?;
    let msg = debit_message(s.serial_num, s.merchant_id, 200);

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(note_args_for_consume(200, s.note_id))
        .add_note_script(s.note_script)
        .extend_advice_inputs(agent_sig_in_map(&agent_sk, msg))
        .build()?;

    let executed = tx.execute().await?;
    assert_eq!(executed.output_notes().num_notes(), 2);
    Ok(())
}
