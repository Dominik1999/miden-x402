//! Cumulative voucher sign/verify tests.

use miden_protocol::{Felt, Word};
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;

use agent_debit_note::voucher::{sign_voucher, verify_voucher};

fn test_merchant() -> AccountId {
    AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap()
}

fn test_serial() -> Word {
    [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)].into()
}

#[test]
fn test_voucher_sign_and_verify() {
    let sk = SecretKey::new();
    let pk = sk.public_key();
    let serial = test_serial();
    let merchant = test_merchant();

    let sig = sign_voucher(&sk, serial, merchant, 5000);
    assert!(verify_voucher(&pk, serial, merchant, 5000, &sig));
}

#[test]
fn test_voucher_wrong_amount_fails() {
    let sk = SecretKey::new();
    let pk = sk.public_key();
    let serial = test_serial();
    let merchant = test_merchant();

    let sig = sign_voucher(&sk, serial, merchant, 5000);
    assert!(!verify_voucher(&pk, serial, merchant, 9999, &sig));
}

#[test]
fn test_voucher_wrong_key_fails() {
    let sk = SecretKey::new();
    let wrong_sk = SecretKey::new();
    let wrong_pk = wrong_sk.public_key();
    let serial = test_serial();
    let merchant = test_merchant();

    let sig = sign_voucher(&sk, serial, merchant, 5000);
    assert!(!verify_voucher(&wrong_pk, serial, merchant, 5000, &sig));
}

#[test]
fn test_voucher_cumulative_amounts() {
    let sk = SecretKey::new();
    let pk = sk.public_key();
    let serial = test_serial();
    let merchant = test_merchant();

    for amount in [1000, 2000, 3000, 4000, 5000] {
        let sig = sign_voucher(&sk, serial, merchant, amount);
        assert!(verify_voucher(&pk, serial, merchant, amount, &sig));
        // Each voucher is independent — can't reuse sig for different amount
        assert!(!verify_voucher(&pk, serial, merchant, amount + 1, &sig));
    }
}

#[test]
fn test_voucher_different_serial_fails() {
    let sk = SecretKey::new();
    let pk = sk.public_key();
    let serial1 = test_serial();
    let serial2: Word = [Felt::new(99), Felt::new(99), Felt::new(99), Felt::new(99)].into();
    let merchant = test_merchant();

    let sig = sign_voucher(&sk, serial1, merchant, 1000);
    assert!(!verify_voucher(&pk, serial2, merchant, 1000, &sig));
}
