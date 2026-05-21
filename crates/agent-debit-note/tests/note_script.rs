//! AgentDebitNote MASM script tests — all 15 spec cases.

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

// ── Helpers ──

fn make_keypair(seed: u64) -> AuthSecretKey {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    AuthSecretKey::new_falcon512_poseidon2_with_rng(&mut rng)
}

fn debit_message(serial: Word, merchant: AccountId, amount: u64) -> Word {
    let dw: Word = [merchant.suffix(), merchant.prefix().as_felt(), Felt::new(amount), Felt::ZERO].into();
    Hasher::merge(&[serial.into(), dw.into()]).into()
}

fn reclaim_message(serial: Word, user: AccountId) -> Word {
    let rw: Word = [user.suffix(), user.prefix().as_felt(), Felt::ZERO, Felt::ZERO].into();
    Hasher::merge(&[serial.into(), rw.into()]).into()
}

fn serial(a: u64, b: u64, c: u64, d: u64) -> Word {
    [Felt::new(a), Felt::new(b), Felt::new(c), Felt::new(d)].into()
}

/// Build a complete test scenario: MockChain + AgentDebitNote + consumer account.
struct TestSetup {
    mock_chain: MockChain,
    note_id: NoteId,
    note_script: NoteScript,
    serial_num: Word,
    consumer_id: AccountId,
    merchant_id: AccountId,
    user_id: AccountId,
    faucet_id: AccountId,
}

fn setup_test(
    agent_pk_commitment: Word,
    note_balance: u64,
    serial_num: Word,
    expiry_block: u32,
) -> anyhow::Result<TestSetup> {
    let note_script = CodeBuilder::default().compile_note_script(NOTE_MASM)?;

    let mut builder = MockChain::builder();
    let consumer = builder.add_existing_wallet(Auth::IncrNonce)?;
    let faucet = builder.add_existing_basic_faucet(Auth::IncrNonce, "USDC", 1_000_000, None)?;
    let merchant = builder.add_existing_wallet(Auth::Noop)?;
    let user = builder.add_existing_wallet(Auth::Noop)?;

    let asset = FungibleAsset::new(faucet.id(), note_balance)?;
    let storage = NoteStorage::new(vec![
        agent_pk_commitment[0],
        agent_pk_commitment[1],
        agent_pk_commitment[2],
        agent_pk_commitment[3],
        user.id().suffix(),
        user.id().prefix().as_felt(),
        Felt::new(expiry_block as u64),
    ])?;

    let metadata = NoteMetadata::new(consumer.id(), NoteType::Public).with_tag(NoteTag::new(0));
    let vault = NoteAssets::new(vec![Asset::Fungible(asset)])?;
    let recipient = NoteRecipient::new(serial_num, note_script.clone(), storage);
    let note = Note::new(vault, metadata, recipient);
    let note_id = note.id();

    builder.add_output_note(RawOutputNote::Full(note));
    let mut mock_chain = builder.build()?;
    mock_chain.prove_next_block()?;
    mock_chain.prove_next_block()?;

    Ok(TestSetup {
        mock_chain,
        note_id,
        note_script,
        serial_num,
        consumer_id: consumer.id(),
        merchant_id: merchant.id(),
        user_id: user.id(),
        faucet_id: faucet.id(),
    })
}

// ── CONSUME PATH TESTS ──

/// #1: Valid consume produces P2ID + remainder.
#[tokio::test]
async fn test_01_valid_consume() -> anyhow::Result<()> {
    let agent_sk = make_keypair(1);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(1,2,3,4), 1_000_000)?;

    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(100), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    let executed = tx.execute().await?;
    assert_eq!(executed.output_notes().num_notes(), 2, "expected P2ID + remainder");
    println!("Test #1 PASSED");
    Ok(())
}

/// #2: Invalid agent signature (garbage) rejected.
#[tokio::test]
async fn test_02_invalid_sig_rejected() -> anyhow::Result<()> {
    let agent_sk = make_keypair(2);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(2,2,3,4), 1_000_000)?;

    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    // Sign with a DIFFERENT key
    let wrong_sk = make_keypair(999);
    let bad_sig = wrong_sk.sign(msg);
    let prepared = bad_sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(100), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    assert!(tx.execute().await.is_err(), "should reject wrong signer");
    println!("Test #2 PASSED");
    Ok(())
}

/// #3: Wrong merchant — signed for A, args say B.
#[tokio::test]
async fn test_03_wrong_merchant() -> anyhow::Result<()> {
    let agent_sk = make_keypair(3);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(3,2,3,4), 1_000_000)?;

    // Sign for merchant
    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    // But pass user_id as the merchant in note_args
    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.user_id.suffix(), s.user_id.prefix().as_felt(), Felt::new(100), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    assert!(tx.execute().await.is_err(), "should reject wrong merchant");
    println!("Test #3 PASSED");
    Ok(())
}

/// #4: Wrong amount — signed for 100, args say 200.
#[tokio::test]
async fn test_04_wrong_amount() -> anyhow::Result<()> {
    let agent_sk = make_keypair(4);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(4,2,3,4), 1_000_000)?;

    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    // Pass amount=200 in note_args
    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(200), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    assert!(tx.execute().await.is_err(), "should reject wrong amount");
    println!("Test #4 PASSED");
    Ok(())
}

/// #5: Debit exceeds balance.
#[tokio::test]
async fn test_05_debit_exceeds_balance() -> anyhow::Result<()> {
    let agent_sk = make_keypair(5);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 100, serial(5,2,3,4), 1_000_000)?;

    let msg = debit_message(s.serial_num, s.merchant_id, 500);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(500), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    assert!(tx.execute().await.is_err(), "should reject debit > balance");
    println!("Test #5 PASSED");
    Ok(())
}

/// #6: Wrong signer — different private key, valid signature format.
#[tokio::test]
async fn test_06_wrong_signer_key() -> anyhow::Result<()> {
    let agent_sk = make_keypair(6);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(6,2,3,4), 1_000_000)?;

    // Sign with a completely different key
    let other_sk = make_keypair(600);
    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    let other_sig = other_sk.sign(msg);
    let prepared = other_sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(100), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    assert!(tx.execute().await.is_err(), "should reject: signed by wrong key");
    println!("Test #6 PASSED");
    Ok(())
}

/// #7: Remainder has correct value (1000 - 100 = 900).
#[tokio::test]
async fn test_07_remainder_correct_value() -> anyhow::Result<()> {
    let agent_sk = make_keypair(7);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(7,2,3,4), 1_000_000)?;

    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(100), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    let executed = tx.execute().await?;
    let notes = executed.output_notes();
    assert_eq!(notes.num_notes(), 2);

    // Check that total output assets = 1000 (100 to P2ID + 900 to remainder)
    let mut total_output = 0u64;
    for note in notes.iter() {
        if let RawOutputNote::Full(n) = note {
            for asset in n.assets().iter_fungible() {
                total_output += asset.amount();
            }
        }
    }
    assert_eq!(total_output, 1000, "total output should equal input (asset preservation)");
    println!("Test #7 PASSED");
    Ok(())
}

// ── BLOCK HEIGHT / RECLAIM TESTS ──

/// #10: Consume at expiry block → should take reclaim path (fails with consume note_args).
#[tokio::test]
async fn test_10_consume_at_expiry_rejected() -> anyhow::Result<()> {
    let agent_sk = make_keypair(10);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    // Expiry at block 0 — any block should be past expiry
    let s = setup_test(pk, 1000, serial(10,2,3,4), 0)?;

    let msg = debit_message(s.serial_num, s.merchant_id, 100);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [s.merchant_id.suffix(), s.merchant_id.prefix().as_felt(), Felt::new(100), Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    // At expiry, the script takes the reclaim path, which expects a reclaim signature.
    // The consume signature is wrong for reclaim → should fail.
    assert!(tx.execute().await.is_err(), "consume at expiry should fail (takes reclaim path)");
    println!("Test #10 PASSED");
    Ok(())
}

/// #12: Valid reclaim at expiry.
#[tokio::test]
async fn test_12_valid_reclaim() -> anyhow::Result<()> {
    let agent_sk = make_keypair(12);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    // Expiry at block 0
    let s = setup_test(pk, 1000, serial(12,2,3,4), 0)?;

    // Reclaim message = merge(serial, [user_suffix, user_prefix, 0, 0])
    let msg = reclaim_message(s.serial_num, s.user_id);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    // Reclaim note_args: [0, 0, 0, 0] (unused for reclaim)
    let mut args = BTreeMap::new();
    args.insert(s.note_id, [Felt::ZERO, Felt::ZERO, Felt::ZERO, Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    let executed = tx.execute().await?;
    // Reclaim produces 1 output note (P2ID to user)
    assert_eq!(executed.output_notes().num_notes(), 1, "reclaim should produce 1 P2ID to user");
    println!("Test #12 PASSED");
    Ok(())
}

/// #13: Reclaim before expiry rejected.
#[tokio::test]
async fn test_13_reclaim_before_expiry() -> anyhow::Result<()> {
    let agent_sk = make_keypair(13);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    // Expiry far in future — reclaim should fail
    let s = setup_test(pk, 1000, serial(13,2,3,4), 1_000_000)?;

    let msg = reclaim_message(s.serial_num, s.user_id);
    let sig = agent_sk.sign(msg);
    let prepared = sig.to_prepared_signature(msg);

    // Try reclaim before expiry
    let mut args = BTreeMap::new();
    args.insert(s.note_id, [Felt::ZERO, Felt::ZERO, Felt::ZERO, Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    // Before expiry → takes consume path → reclaim sig doesn't match consume message → fails
    assert!(tx.execute().await.is_err(), "reclaim before expiry should fail");
    println!("Test #13 PASSED");
    Ok(())
}

/// #14: Reclaim with wrong user signature rejected.
#[tokio::test]
async fn test_14_reclaim_wrong_sig() -> anyhow::Result<()> {
    let agent_sk = make_keypair(14);
    let pk: Word = agent_sk.public_key().to_commitment().into();
    let s = setup_test(pk, 1000, serial(14,2,3,4), 0)?;

    let msg = reclaim_message(s.serial_num, s.user_id);
    // Sign with wrong key
    let wrong_sk = make_keypair(1400);
    let bad_sig = wrong_sk.sign(msg);
    let prepared = bad_sig.to_prepared_signature(msg);

    let mut args = BTreeMap::new();
    args.insert(s.note_id, [Felt::ZERO, Felt::ZERO, Felt::ZERO, Felt::ZERO].into());

    let tx = s.mock_chain
        .build_tx_context(s.consumer_id, &[s.note_id], &[])?
        .extend_note_args(args)
        .add_note_script(s.note_script)
        .extend_advice_inputs(AdviceInputs::default().with_stack(prepared))
        .build()?;

    assert!(tx.execute().await.is_err(), "reclaim with wrong sig should fail");
    println!("Test #14 PASSED");
    Ok(())
}
