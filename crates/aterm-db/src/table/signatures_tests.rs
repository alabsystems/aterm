// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Arc, atomic::Ordering};

fn signed(tag: u8, domain: &[u8]) -> ([u8; 32], Vec<u8>) {
    let seed = [17; 32];
    let mut wire = vec![tag; 128];
    let signature = crypto::signed(&seed, domain, &wire).unwrap();
    wire.extend_from_slice(&signature);
    (crypto::public(&seed).unwrap(), wire)
}

#[test]
fn reuse_requires_the_same_signer_and_complete_signed_bytes() {
    let cache = Cache::new(4);
    let (signer, wire) = signed(1, DOMAIN);
    let id = cache.verify(&signer, &wire).unwrap();
    assert_eq!(id, Sha256::digest(&wire));
    assert_eq!(cache.verify(&signer, &wire).unwrap(), id);
    assert_eq!(cache.checks.load(Ordering::Relaxed), 1);

    let mut changed = wire.clone();
    changed[0] ^= 1;
    for _ in 0..2 {
        assert!(cache.verify(&signer, &changed).is_err());
    }
    let mut changed_signature = wire.clone();
    *changed_signature.last_mut().unwrap() ^= 1;
    assert!(cache.verify(&signer, &changed_signature).is_err());
    assert!(
        cache
            .verify(&crypto::public(&[18; 32]).unwrap(), &wire)
            .is_err()
    );
    let (_, other_domain) = signed(1, b"another signature domain");
    assert!(cache.verify(&signer, &other_domain).is_err());
    assert_eq!(cache.checks.load(Ordering::Relaxed), 6);
    assert_eq!(cache.verify(&signer, &wire).unwrap(), id);
    assert_eq!(cache.checks.load(Ordering::Relaxed), 6);
}

#[test]
fn successful_checks_are_bounded_and_eviction_requires_verification_again() {
    let cache = Cache::new(2);
    let (signer, first) = signed(1, DOMAIN);
    let (_, second) = signed(2, DOMAIN);
    let (_, third) = signed(3, DOMAIN);
    for wire in [&first, &second, &first, &third] {
        cache.verify(&signer, wire).unwrap();
    }
    assert_eq!(cache.checks.load(Ordering::Relaxed), 3);
    cache.verify(&signer, &first).unwrap();
    assert_eq!(cache.checks.load(Ordering::Relaxed), 4);
    let entries = cache.entries.lock().unwrap();
    assert_eq!(entries.known.len(), 2);
    assert_eq!(entries.order.len(), 2);
    assert_eq!(
        entries.known,
        entries.order.iter().copied().collect::<BTreeSet<_>>()
    );
}

/// A journal can retain more immutable wires than the global signature cache holds. A complete
/// cyclic replay must keep each wire's successful signature fact across clones and cache eviction.
/// The old FIFO-only check verifies every wire again on every scan of this three-wire, two-slot
/// fixture. Raw bytes newly identified after eviction still need their own signature check.
#[test]
fn retained_identified_wires_keep_signature_facts_across_cyclic_replay() {
    let cache = Cache::new(2);
    let signer = crypto::public(&[17; 32]).unwrap();
    let wires: Vec<_> = (1..=3)
        .map(|tag| Identified::new(signed(tag, DOMAIN).1))
        .collect();
    for _ in 0..3 {
        for wire in &wires {
            let replayed = wire.clone();
            assert_eq!(
                cache.verify_identified(&signer, &replayed).unwrap(),
                wire.id()
            );
        }
    }
    assert_eq!(cache.checks.load(Ordering::Relaxed), 3);
    assert_eq!(cache.entries.lock().unwrap().known.len(), 2);

    let identified_again = Identified::new(wires[0].wire().to_vec());
    cache.verify_identified(&signer, &identified_again).unwrap();
    assert_eq!(cache.checks.load(Ordering::Relaxed), 4);
}

#[test]
fn identified_facts_require_the_exact_signer_and_wire_and_never_keep_failures() {
    let cache = Cache::new(1);
    let (signer, bytes) = signed(1, DOMAIN);
    let wire = Identified::new(bytes.clone());
    cache.verify_identified(&signer, &wire).unwrap();
    let other_signer = crypto::public(&[18; 32]).unwrap();
    for _ in 0..2 {
        assert!(
            cache
                .verify_identified(&other_signer, &wire.clone())
                .is_err()
        );
    }
    let mut changed_payload = bytes.clone();
    changed_payload[0] ^= 1;
    let mut changed_signature = bytes;
    *changed_signature.last_mut().unwrap() ^= 1;
    for bytes in [
        changed_payload,
        changed_signature,
        signed(1, b"other domain").1,
    ] {
        let changed = Identified::new(bytes);
        for _ in 0..2 {
            assert!(cache.verify_identified(&signer, &changed.clone()).is_err());
        }
    }
    assert_eq!(cache.checks.load(Ordering::Relaxed), 9);
    assert_eq!(
        cache.verify_identified(&signer, &wire.clone()).unwrap(),
        wire.id()
    );
    assert_eq!(cache.checks.load(Ordering::Relaxed), 9);
}

#[test]
fn replay_clones_keep_their_fact_while_the_global_cache_is_busy() {
    let cache = Arc::new(Cache::new(1));
    let (signer, bytes) = signed(1, DOMAIN);
    let wire = Identified::new(bytes);
    cache.verify_identified(&signer, &wire).unwrap();
    let held = cache.entries.lock().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = {
        let cache = cache.clone();
        let replayed = wire.clone();
        std::thread::spawn(move || {
            send.send(cache.verify_identified(&signer, &replayed))
                .unwrap()
        })
    };
    let result = receive.recv_timeout(std::time::Duration::from_secs(60));
    drop(held);
    worker.join().unwrap();
    assert_eq!(
        result
            .expect("a retained fact needs no global cache lock")
            .unwrap(),
        wire.id()
    );
    assert_eq!(cache.checks.load(Ordering::Relaxed), 1);
}

/// Compare the previous identified-wire check with wire-owned facts, below and above the real
/// global cache's capacity. Both arms use already identified, valid signed bytes. Count allocations
/// separately from instructions; this is the signature helper, not a table open or network round.
#[test]
#[ignore = "manual paired signature fact allocation and instruction measurement"]
fn measure_retained_signature_facts() {
    use aterm_bench::measurement_clock::{counters, declare_interactive};
    use std::hint::black_box;
    let _ = declare_interactive();
    let seed = [17; 32];
    let signer = crypto::public(&seed).unwrap();
    for records in [128_usize, MAX_SIGNATURES, MAX_SIGNATURES + 1, 6000] {
        let wires: Vec<_> = (0..records)
            .map(|index| {
                let mut wire = vec![0x55; 128];
                wire[..8].copy_from_slice(&(index as u64).to_le_bytes());
                let signature = crypto::signed(&seed, DOMAIN, &wire).unwrap();
                wire.extend_from_slice(&signature);
                Identified::new(wire)
            })
            .collect();
        let reference = Cache::new(MAX_SIGNATURES);
        let candidate = Cache::new(MAX_SIGNATURES);
        let run = |facts| {
            for wire in &wires {
                let id = if facts {
                    candidate.verify_identified(&signer, black_box(wire))
                } else {
                    reference.verify_as(&signer, black_box(wire.wire()), || wire.id())
                };
                black_box(id.unwrap());
            }
        };
        run(false);
        run(true);
        for sample in 0..5 {
            for facts in if sample % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let (_, allocations) = aterm_heap_count::allocations_in(|| run(facts));
                let before = counters();
                run(facts);
                let after = counters();
                let instructions = before
                    .zip(after)
                    .map(|(before, after)| after.instructions.saturating_sub(before.instructions));
                eprintln!(
                    "SIGNATURE-FACTS records={records} candidate={facts} sample={sample} calls={records} allocations={allocations} instructions={instructions:?}"
                );
            }
        }
    }
}

#[test]
fn contention_and_poison_fall_back_to_fresh_verification() {
    let cache = Arc::new(Cache::new(2));
    let (signer, wire) = signed(1, DOMAIN);
    cache.verify(&signer, &wire).unwrap();
    let held = cache.entries.lock().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = {
        let cache = cache.clone();
        let wire = wire.clone();
        std::thread::spawn(move || send.send(cache.verify(&signer, &wire)).unwrap())
    };
    let result = receive.recv_timeout(std::time::Duration::from_secs(60));
    drop(held);
    worker.join().unwrap();
    assert!(
        result
            .expect("a busy cache cannot block verification")
            .is_ok()
    );
    assert_eq!(cache.checks.load(Ordering::Relaxed), 2);

    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = cache.entries.lock().unwrap();
        panic!("test a poisoned optional cache");
    }));
    assert!(poisoned.is_err());
    assert!(cache.verify(&signer, &wire).is_ok());
    assert_eq!(cache.checks.load(Ordering::Relaxed), 3);
    let mut changed = wire;
    changed[0] ^= 1;
    assert!(cache.verify(&signer, &changed).is_err());
    assert_eq!(cache.checks.load(Ordering::Relaxed), 4);
}
