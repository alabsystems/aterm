// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::Ordering;

const SIGNER: [u8; 32] = [61; 32];
const PREVIOUS: [u8; 32] = [33; 32];
const OTHER: [u8; 32] = [47; 32];

fn signed(previous: &[u8; 32], sequence: u64) -> Vec<u8> {
    let signer = envelope::signer_public(&SIGNER).unwrap();
    envelope::sign(
        &envelope::Unsigned {
            name: "chain-cache".into(),
            seq: sequence,
            time: 1_792_530_000,
            handoff: vec![(
                envelope::signer_public(previous).unwrap(),
                envelope::endorse(previous, "chain-cache", &signer).unwrap(),
            )],
            parents: Vec::new(),
            lockers: vec![envelope::Locker {
                label: "admin".into(),
                armored: envelope::test_armor(b"inert encrypted fixture"),
            }],
        },
        &SIGNER,
    )
    .unwrap()
}

fn secret(bytes: &[u8]) -> SecretBytes {
    SecretBytes::from(bytes)
}

/// NEGATIVE CONTROL (2026-11-05): with `same_bytes` finding nothing, equal bytes parse again
/// (`left: 2, right: 1`).
#[test]
fn equal_bytes_reuse_one_parse_and_their_verified_public_chain() {
    let cache = Cache::new();
    let bytes = signed(&PREVIOUS, 11);
    let pin = envelope::signer_public(&SIGNER).unwrap();
    let expected = vec![envelope::signer_public(&PREVIOUS).unwrap()];
    assert_eq!(cache.parsed(secret(&bytes), None).chain(&pin), expected);
    assert_eq!(cache.parses.load(Ordering::Relaxed), 1);
    assert_eq!(cache.parsed(secret(&bytes), None).chain(&pin), expected);
    assert_eq!(cache.parses.load(Ordering::Relaxed), 1);
}

/// NEGATIVE CONTROL (2026-11-05): with `signature`'s `OnceLock` bypassed, so that each call
/// counts and verifies, the entry verifies once per ask (`left: 6, right: 1`).
#[test]
fn one_entry_verifies_its_signature_once_whichever_reader_asks() {
    let cache = Cache::new();
    let bytes = signed(&PREVIOUS, 11);
    let forged = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("seq: 11\n", "seq: 12\n")
        .into_bytes();
    let pin = envelope::signer_public(&SIGNER).unwrap();
    for _ in 0..3 {
        let genuine = cache.parsed(secret(&bytes), None);
        assert!(!genuine.chain(&pin).is_empty());
        genuine
            .verify_for_reader("chain-cache", &pin, &[], None)
            .unwrap();
        let refused = cache.parsed(secret(&forged), None);
        assert!(refused.chain(&pin).is_empty());
        assert!(
            refused
                .verify_for_reader("chain-cache", &pin, &[], None)
                .is_err()
        );
    }
    assert_eq!(cache.parses.load(Ordering::Relaxed), 2);
    for input in [&bytes, &forged] {
        let entry = cache.parsed(secret(input), None);
        assert_eq!(entry.signatures.load(Ordering::Relaxed), 1);
    }
}

/// NEGATIVE CONTROLS (2026-11-05): with `same_bytes` comparing lengths, the second envelope
/// answers with the first's chain (`left` and `right` differ); with `signature` always `Ok`, the
/// damaged envelope answers with a chain (`assertion failed`, its `is_empty`).
#[test]
fn a_warm_entry_answers_only_for_its_exact_bytes_and_the_supplied_pin() {
    let cache = Cache::new();
    let first = signed(&PREVIOUS, 11);
    let pin = envelope::signer_public(&SIGNER).unwrap();
    let expected = vec![envelope::signer_public(&PREVIOUS).unwrap()];
    assert_eq!(cache.parsed(secret(&first), None).chain(&pin), expected);
    assert!(
        cache
            .parsed(secret(&first), None)
            .chain(&envelope::signer_public(&OTHER).unwrap())
            .is_empty()
    );

    let second = signed(&OTHER, 12);
    assert_eq!(
        cache.parsed(secret(&second), None).chain(&pin),
        vec![envelope::signer_public(&OTHER).unwrap()]
    );
    let damaged = String::from_utf8(second)
        .unwrap()
        .replace("seq: 12\n", "seq: 13\n");
    assert!(
        cache
            .parsed(secret(damaged.as_bytes()), None)
            .chain(&pin)
            .is_empty()
    );
    assert!(
        cache
            .parsed(secret(b"not an envelope"), None)
            .chain(&pin)
            .is_empty()
    );
    assert_eq!(cache.parsed(secret(&first), None).chain(&pin), expected);
}

/// Every answer of a warm entry equals [`envelope::verify_for_reader`] of the same bytes,
/// errors included, in whichever order the questions come.
///
/// NEGATIVE CONTROLS (2026-11-05): with `signature` always `Ok`, the forged input is accepted
/// (`left: Ok(…), right: Err(… the signature does not verify …)`); with `same_bytes` finding
/// nothing, each question parses (`left: 180, right: 3`).
#[test]
fn a_warm_entry_answers_exactly_as_the_reader_verification_does() {
    let cache = Cache::new();
    let bytes = signed(&PREVIOUS, 11);
    let digest = envelope::parse(&bytes).unwrap().digest();
    let forged = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("seq: 11\n", "seq: 12\n")
        .into_bytes();
    let signer = envelope::signer_public(&SIGNER).unwrap();
    let previous = envelope::signer_public(&PREVIOUS).unwrap();
    let other = envelope::signer_public(&OTHER).unwrap();
    let floors = [
        None,
        Some(Floor::version(11, digest)),
        Some(Floor::version(11, [7; 32])),
        Some(Floor::version(10, [7; 32])),
        Some(Floor::version(12, [7; 32])),
    ];
    let mut outcomes = std::collections::BTreeSet::new();
    for input in [&bytes[..], &forged[..], b"not an envelope".as_slice()] {
        for name in ["chain-cache", "another-vault"] {
            for pin in [signer, previous, other] {
                for followed in [&[][..], &[previous][..]] {
                    for floor in &floors {
                        let direct = envelope::verify_for_reader(
                            input,
                            name,
                            &pin,
                            followed,
                            floor.as_ref(),
                        )
                        .map(|(envelope, pin, verdict)| (envelope.digest(), pin, verdict));
                        let warm = cache.parsed(secret(input), None);
                        let held = warm
                            .verify_for_reader(name, &pin, followed, floor.as_ref())
                            .map(|(envelope, pin, verdict)| (envelope.digest(), pin, verdict));
                        assert_eq!(held, direct);
                        outcomes.insert(format!("{direct:?}").chars().take(40).collect::<String>());
                    }
                }
            }
        }
    }
    assert!(
        outcomes.len() > 5,
        "the table reaches several outcomes: {outcomes:?}"
    );
    assert_eq!(cache.parses.load(Ordering::Relaxed), 3);
}

fn charged(cache: &Cache) -> usize {
    cache
        .entries
        .lock()
        .unwrap()
        .iter()
        .map(|entry| entry.parsed.charge())
        .sum()
}

/// NEGATIVE CONTROL (2026-11-05): with the byte clause dropped from `parsed`'s eviction loop,
/// the second maximum-size input joins the first and the small entries past the budget
/// (`after large input 1`).
#[test]
fn the_retained_bytes_stay_within_their_budget_whatever_arrives() {
    let cache = Cache::new();
    for sequence in 1..40 {
        let _ = cache.parsed(secret(&signed(&PREVIOUS, sequence)), None);
        assert!(cache.entries.lock().unwrap().len() <= MAX_ENTRIES);
        assert!(charged(&cache) <= MAX_CACHED_BYTES);
    }
    // A refused input is charged its own length, so two of the largest a reader admits fill
    // the budget: each one after them pushes out the oldest.
    let large: Vec<Vec<u8>> = (0..4u8)
        .map(|tag| vec![b'a' + tag; envelope::MAX_ENVELOPE])
        .collect();
    for (at, input) in large.iter().enumerate() {
        let _ = cache.parsed(secret(input), None);
        assert!(
            charged(&cache) <= MAX_CACHED_BYTES,
            "after large input {at}"
        );
        let entries = cache.entries.lock().unwrap();
        assert_eq!(entries.back().unwrap().parsed.bytes(), input.as_slice());
        if at > 0 {
            assert_eq!(entries.len(), 2, "after large input {at}");
            assert_eq!(
                entries.front().unwrap().parsed.bytes(),
                large[at - 1].as_slice()
            );
        }
    }
    let parses = cache.parses.load(Ordering::Relaxed);
    let _ = cache.parsed(secret(&large[0]), None);
    assert_eq!(
        cache.parses.load(Ordering::Relaxed),
        parses + 1,
        "the evicted input parses again"
    );
}

/// A held vault that has not changed is read once: each later read opens and stamps it and
/// takes the entry known by that stamp. A vault renamed over it, even of the same bytes, is
/// read again (and takes the entry of those bytes, unparsed again); one of other bytes is
/// those bytes. NEGATIVE CONTROL (2026-11-09): with `held` finding no entry by its stamp,
/// every read reads the file (`left: 4, right: 1`).
#[test]
fn a_held_vault_that_has_not_changed_is_read_once() {
    let cache = Cache::new();
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join("vault");
    assert!(cache.held(&path).unwrap().is_none(), "no file, no vault");
    let first = signed(&PREVIOUS, 11);
    fsx::write_atomic(&path, &first, 0o600).unwrap();
    let read = cache.held(&path).unwrap().unwrap();
    for _ in 0..3 {
        let again = cache.held(&path).unwrap().unwrap();
        assert!(Arc::ptr_eq(&read, &again));
    }
    assert_eq!(cache.reads.load(Ordering::Relaxed), 1);
    assert_eq!(cache.parses.load(Ordering::Relaxed), 1);

    fsx::write_atomic(&path, &first, 0o600).unwrap();
    let renamed = cache.held(&path).unwrap().unwrap();
    assert!(
        Arc::ptr_eq(&read, &renamed),
        "the same bytes keep their entry"
    );
    assert_eq!(cache.reads.load(Ordering::Relaxed), 2);
    assert_eq!(cache.parses.load(Ordering::Relaxed), 1);
    let _ = cache.held(&path).unwrap().unwrap();
    assert_eq!(cache.reads.load(Ordering::Relaxed), 2);

    let second = signed(&PREVIOUS, 12);
    fsx::write_atomic(&path, &second, 0o600).unwrap();
    assert_eq!(
        cache.held(&path).unwrap().unwrap().bytes(),
        second.as_slice()
    );
    assert_eq!(cache.reads.load(Ordering::Relaxed), 3);
}

/// A held vault is admitted at every read, as `fsx::read_private` admits it, before its stamp
/// is looked up: one whose mode lets other users read it is refused with `read_private`'s
/// error, and one gone is none. NEGATIVE CONTROL (2026-11-09): with `held` admitting by
/// `read_owned`'s rule, the widened file reads (`refused`).
#[test]
fn a_held_vault_is_admitted_at_every_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let cache = Cache::new();
    let home = aterm_tempfile::tempdir().unwrap();
    let path = home.path().join("vault");
    fsx::write_atomic(&path, &signed(&PREVIOUS, 11), 0o600).unwrap();
    let _ = cache.held(&path).unwrap().unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = cache.held(&path).err().expect("refused").to_string();
    let expected = fsx::read_private(&path, envelope::MAX_ENVELOPE)
        .unwrap_err()
        .to_string();
    assert_eq!(refused, expected);
    std::fs::remove_file(&path).unwrap();
    assert!(cache.held(&path).unwrap().is_none());
    assert_eq!(cache.reads.load(Ordering::Relaxed), 1);
}
