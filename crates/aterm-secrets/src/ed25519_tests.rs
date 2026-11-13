// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_signature_fact_requires_the_exact_key_message_and_signature() {
    let seed = [91; 32];
    let key = astream_aead::signature::public_key(&seed);
    let message = b"synthetic signed log record";
    let signature = astream_aead::signature::sign(&seed, message);
    let memo = Verified::new();
    let checks = std::cell::Cell::new(0);
    let check = |key: &[u8; 32], message: &[u8], signature: &[u8; 64]| {
        memo.check_with(key, message, signature, || {
            checks.set(checks.get() + 1);
            astream_aead::signature::verify(key, message, signature)
        })
    };
    assert!(check(&key, message, &signature));
    assert!(check(&key, message, &signature));
    assert_eq!(checks.get(), 1);
    // Negative controls warm the fact first: omitting any one part of its identity
    // would now accept a tuple the strict verifier refuses.
    assert!(!check(&key, b"changed signed log record", &signature));
    assert!(!check(&[0; 32], message, &signature));
    let mut changed = signature;
    changed[63] ^= 1;
    assert!(!check(&key, message, &changed));
    assert!(!check(&key, message, &changed));
    assert_eq!(checks.get(), 5, "refusals must not be retained");
    assert_eq!(memo.0.lock().unwrap().len(), 1);
}

#[test]
fn signature_facts_are_bounded_even_for_empty_messages() {
    let memo = Verified::new();
    for n in 0..=VERIFIED_ENTRIES {
        let mut seed = [0; 32];
        seed[..size_of::<usize>()].copy_from_slice(&n.to_le_bytes());
        let key = astream_aead::signature::public_key(&seed);
        let signature = astream_aead::signature::sign(&seed, b"");
        assert!(memo.check(&key, b"", &signature));
    }
    assert_eq!(memo.0.lock().unwrap().len(), VERIFIED_ENTRIES);
    let large = vec![42; VERIFIED_BYTES + 1];
    let key = astream_aead::signature::public_key(&[91; 32]);
    let signature = astream_aead::signature::sign(&[91; 32], &large);
    assert!(memo.check(&key, &large, &signature));
    let facts = memo.0.lock().unwrap();
    assert_eq!(facts.len(), VERIFIED_ENTRIES);
    assert!(facts.iter().all(|fact| fact.message.is_empty()));
}

#[test]
fn a_byte_budget_eviction_requires_a_new_strict_check() {
    let memo = Verified::new();
    let seed = [91; 32];
    let key = astream_aead::signature::public_key(&seed);
    let first = vec![41; VERIFIED_BYTES / 2 + 1];
    let second = vec![42; first.len()];
    let a = astream_aead::signature::sign(&seed, &first);
    let b = astream_aead::signature::sign(&seed, &second);
    assert!(memo.check(&key, &first, &a));
    assert!(memo.check(&key, &second, &b));
    assert_eq!(memo.0.lock().unwrap().len(), 1);
    let checked = std::cell::Cell::new(false);
    assert!(memo.check_with(&key, &first, &a, || {
        checked.set(true);
        astream_aead::signature::verify(&key, &first, &a)
    }));
    assert!(checked.get());
}
