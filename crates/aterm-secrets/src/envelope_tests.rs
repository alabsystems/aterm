// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const OLD: [u8; 32] = [7u8; 32];
const NEW: [u8; 32] = [8u8; 32];
const STRANGER: [u8; 32] = [9u8; 32];

fn lockers() -> Vec<Locker> {
    vec![
        Locker {
            label: "admin".to_owned(),
            armored: test_armor(&[1u8; 70]),
        },
        Locker {
            label: "1".to_owned(),
            armored: test_armor(b"everyone"),
        },
    ]
}

fn unsigned(seq: u64) -> Unsigned {
    Unsigned {
        name: "alab".to_owned(),
        seq,
        time: 1_792_530_000,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: lockers(),
    }
}

fn public(seed: &[u8; 32]) -> [u8; 32] {
    signer_public(seed).expect("public key")
}

#[test]
fn a_signed_envelope_renders_exactly_and_parses_back() {
    let bytes = sign(&unsigned(7), &OLD).expect("signed");
    let text = std::str::from_utf8(&bytes).expect("text");
    let signer = b64::encode_padded(&public(&OLD));
    let expected_head = format!(
        "alab-vault/1\nname: alab\nseq: 7\ntime: 1792530000\nsigner: {signer}\nlocker: admin\n{}locker: 1\n{}signature: ",
        test_armor(&[1u8; 70]),
        test_armor(b"everyone")
    );
    assert!(text.starts_with(&expected_head), "{text}");
    assert!(text.ends_with('\n'));
    assert!(!text.ends_with("\n\n"));
    let envelope = parse(&bytes).expect("parsed");
    assert_eq!(envelope.name, "alab");
    assert_eq!(envelope.seq, 7);
    assert_eq!(envelope.time, 1_792_530_000);
    assert_eq!(envelope.signer, public(&OLD));
    assert!(envelope.handoff.is_empty());
    assert_eq!(envelope.lockers, lockers());
    assert_eq!(envelope.raw(), bytes.as_slice());
    assert_eq!(
        envelope.admin_locker().map(|l| l.label.as_str()),
        Some("admin")
    );
    envelope.verify_signature().expect("verifies");
    assert_eq!(envelope.digest(), aterm_digest::Sha256::digest(&bytes));
    assert_eq!(envelope.check_pin(&public(&OLD), &[]), Ok(Pin::Same));
}

#[test]
fn every_single_bit_flip_of_a_signed_envelope_is_refused() {
    let bytes = sign(&unsigned(3), &OLD).expect("signed");
    let pin = public(&OLD);
    verify_for_reader(&bytes, "alab", &pin, &[], None).expect("the original verifies");
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut flipped = bytes.clone();
            flipped[index] ^= 1 << bit;
            assert!(
                verify_for_reader(&flipped, "alab", &pin, &[], None).is_err(),
                "byte {index} bit {bit} flipped was accepted"
            );
        }
    }
}

#[test]
fn a_truncated_or_extended_envelope_is_refused() {
    let bytes = sign(&unsigned(3), &OLD).expect("signed");
    let pin = public(&OLD);
    for len in 0..bytes.len() {
        assert!(
            verify_for_reader(&bytes[..len], "alab", &pin, &[], None).is_err(),
            "{len}"
        );
    }
    let mut longer = bytes.clone();
    longer.push(b'\n');
    assert!(parse(&longer).is_err());
    let mut crlf = bytes.clone();
    crlf.insert(12, b'\r');
    assert!(parse(&crlf).unwrap_err().0.contains("CR"));
}

#[test]
fn the_wrong_pin_or_the_wrong_name_is_refused() {
    let bytes = sign(&unsigned(3), &OLD).expect("signed");
    let error = verify_for_reader(&bytes, "alab", &public(&STRANGER), &[], None).unwrap_err();
    assert!(error.0.contains(&fingerprint(&public(&OLD))), "{error}");
    assert!(
        error.0.contains(&fingerprint(&public(&STRANGER))),
        "{error}"
    );
    let error = verify_for_reader(&bytes, "other", &public(&OLD), &[], None).unwrap_err();
    assert!(error.0.contains("other"), "{error}");
}

#[test]
fn an_endorsed_rotation_is_accepted_and_re_pins() {
    let endorsement = endorse(&OLD, "alab", &public(&NEW)).expect("endorsed");
    let mut rotated = unsigned(4);
    rotated.handoff = vec![(public(&OLD), endorsement)];
    let bytes = sign(&rotated, &NEW).expect("signed");
    let (envelope, pin, verdict) = verify_for_reader(
        &bytes,
        "alab",
        &public(&OLD),
        &[],
        Some(&Floor::version(3, [0u8; 32])),
    )
    .expect("accepted");
    assert_eq!(pin, Pin::Rotated(public(&NEW)));
    assert_eq!(verdict, Verdict::New);
    assert_eq!(envelope.handoff, vec![(public(&OLD), endorsement)]);
    let next = sign(&unsigned(5), &NEW).expect("signed");
    let (_, pin, _) = verify_for_reader(
        &next,
        "alab",
        &public(&NEW),
        &[],
        Some(&Floor::version(4, [0u8; 32])),
    )
    .expect("accepted");
    assert_eq!(pin, Pin::Same);
    assert!(verify_for_reader(&next, "alab", &public(&OLD), &[], None).is_err());
}

#[test]
fn an_endorsement_for_another_vault_or_by_another_key_is_refused() {
    let elsewhere = endorse(&OLD, "other", &public(&NEW)).expect("endorsed");
    let mut rotated = unsigned(4);
    rotated.handoff = vec![(public(&OLD), elsewhere)];
    assert!(
        sign(&rotated, &NEW).is_err(),
        "sign refuses a handoff no reader would accept"
    );
    let bytes = sign_unchecked(&rotated, &NEW).expect("rendered");
    let error = verify_for_reader(&bytes, "alab", &public(&OLD), &[], None).unwrap_err();
    assert!(error.0.contains("endorsement"), "{error}");

    let forged = endorse(&STRANGER, "alab", &public(&NEW)).expect("endorsed");
    rotated.handoff = vec![(public(&OLD), forged)];
    let bytes = sign_unchecked(&rotated, &NEW).expect("rendered");
    assert!(verify_for_reader(&bytes, "alab", &public(&OLD), &[], None).is_err());

    let other_key = endorse(&OLD, "alab", &public(&STRANGER)).expect("endorsed");
    rotated.handoff = vec![(public(&OLD), other_key)];
    let bytes = sign_unchecked(&rotated, &NEW).expect("rendered");
    assert!(verify_for_reader(&bytes, "alab", &public(&OLD), &[], None).is_err());
}

/// The seeds of a vault whose key rotated `rotations` times: the first made it, the last
/// signs now.
fn seeds(rotations: usize) -> Vec<[u8; 32]> {
    (0..=rotations)
        .map(|index| [0x40 + u8::try_from(index).expect("small"); 32])
        .collect()
}

fn chain(seeds: &[[u8; 32]]) -> Vec<([u8; 32], [u8; 64])> {
    test_chain("alab", seeds)
}

fn hex_digest(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

#[test]
fn no_handoff_and_one_handoff_are_spelled_byte_for_byte_as_before_chains() {
    let none = sign(&unsigned(7), &OLD).expect("signed");
    assert_eq!(
        hex_digest(&none),
        "c576b8917d3fe1ee5f321e2c6e26125bb366995719b52d47d9e93004362dd62b"
    );
    let mut one = unsigned(7);
    let endorsement = endorse(&OLD, "alab", &public(&NEW)).expect("endorsed");
    one.handoff = vec![(public(&OLD), endorsement)];
    let bytes = sign(&one, &NEW).expect("signed");
    let text = std::str::from_utf8(&bytes).expect("text");
    let head = format!(
        "alab-vault/1\nname: alab\nseq: 7\ntime: 1792530000\nsigner: {}\nprevious-signer: {}\nendorsement: {}\nlocker: admin\n",
        b64::encode_padded(&public(&NEW)),
        b64::encode_padded(&public(&OLD)),
        b64::encode_padded(&endorsement)
    );
    assert!(text.starts_with(&head), "{text}");
    assert_eq!(
        hex_digest(&bytes),
        "4b0a33b5f51a0e5402757310f788b88fc15db502a1dbae5788d7e608b8db4f53"
    );
    assert_eq!(parse(&bytes).expect("parsed").handoff, one.handoff);
}

#[test]
fn a_chain_of_rotations_is_followed_from_every_key_the_vault_had() {
    let seeds = seeds(3);
    let mut rotated = unsigned(9);
    rotated.handoff = chain(&seeds);
    let bytes = sign(&rotated, &seeds[3]).expect("signed");
    let text = std::str::from_utf8(&bytes).expect("text");
    let pairs: String = rotated
        .handoff
        .iter()
        .map(|(key, endorsement)| {
            format!(
                "previous-signer: {}\nendorsement: {}\n",
                b64::encode_padded(key),
                b64::encode_padded(endorsement)
            )
        })
        .collect();
    let signer = b64::encode_padded(&public(&seeds[3]));
    assert!(
        text.contains(&format!("\nsigner: {signer}\n{pairs}locker: admin\n")),
        "one pair per rotation, oldest first, after the signer: {text}"
    );
    assert_eq!(parse(&bytes).expect("parsed").handoff, rotated.handoff);
    for seed in &seeds[..3] {
        let (_, pin, _) =
            verify_for_reader(&bytes, "alab", &public(seed), &[], None).expect("followed");
        assert_eq!(pin, Pin::Rotated(public(&seeds[3])));
    }
    let (_, pin, _) =
        verify_for_reader(&bytes, "alab", &public(&seeds[3]), &[], None).expect("the signer");
    assert_eq!(pin, Pin::Same);
    let error = verify_for_reader(&bytes, "alab", &public(&STRANGER), &[], None).unwrap_err();
    assert!(error.0.contains("endorsed no handoff"), "{error}");
}

#[test]
fn a_chain_with_a_broken_link_is_refused_from_every_key_before_the_break() {
    let seeds = seeds(3);
    let mut broken = unsigned(9);
    broken.handoff = chain(&seeds);
    broken.handoff[1].1 = endorse(&seeds[1], "alab", &public(&STRANGER)).expect("endorsed");
    let error = sign(&broken, &seeds[3]).expect_err("sign refuses it");
    assert!(error.0.contains("handoff 2 of 3"), "{error}");
    let bytes = sign_unchecked(&broken, &seeds[3]).expect("rendered");
    for seed in &seeds[..2] {
        let error = verify_for_reader(&bytes, "alab", &public(seed), &[], None).unwrap_err();
        assert!(error.0.contains("endorsement of handoff 2 of 3"), "{error}");
    }
    let (_, pin, _) =
        verify_for_reader(&bytes, "alab", &public(&seeds[2]), &[], None).expect("followed");
    assert_eq!(pin, Pin::Rotated(public(&seeds[3])));

    let mut dropped = unsigned(9);
    dropped.handoff = chain(&seeds);
    dropped.handoff.remove(1);
    assert!(sign(&dropped, &seeds[3]).is_err());
    let bytes = sign_unchecked(&dropped, &seeds[3]).expect("rendered");
    assert!(verify_for_reader(&bytes, "alab", &public(&seeds[0]), &[], None).is_err());
    let mut swapped = unsigned(9);
    swapped.handoff = chain(&seeds);
    swapped.handoff.swap(0, 1);
    assert!(sign(&swapped, &seeds[3]).is_err());
    let bytes = sign_unchecked(&swapped, &seeds[3]).expect("rendered");
    assert!(verify_for_reader(&bytes, "alab", &public(&seeds[0]), &[], None).is_err());
}

#[test]
fn the_handoff_chain_is_capped() {
    let seeds = seeds(MAX_HANDOFF + 1);
    let mut long = unsigned(1);
    long.handoff = chain(&seeds[..=MAX_HANDOFF]);
    let bytes = sign(&long, &seeds[MAX_HANDOFF]).expect("64 handoffs");
    assert_eq!(parse(&bytes).expect("parsed").handoff.len(), MAX_HANDOFF);
    let (_, pin, _) =
        verify_for_reader(&bytes, "alab", &public(&seeds[0]), &[], None).expect("followed");
    assert_eq!(pin, Pin::Rotated(public(&seeds[MAX_HANDOFF])));
    long.handoff = chain(&seeds);
    let error = sign(&long, &seeds[MAX_HANDOFF + 1]).expect_err("65 handoffs");
    assert!(error.0.contains("more than 64 handoffs"), "{error}");

    let pair = format!(
        "previous-signer: {}\nendorsement: {}\n",
        b64::encode_padded(&[1u8; 32]),
        b64::encode_padded(&[2u8; 64])
    );
    let with = |pairs: usize| {
        resigned(|text| {
            text.replacen(
                "locker: admin\n",
                &format!("{}locker: admin\n", pair.repeat(pairs)),
                1,
            )
        })
    };
    assert_eq!(
        parse(&with(MAX_HANDOFF))
            .expect("the control")
            .handoff
            .len(),
        MAX_HANDOFF
    );
    let error = parse(&with(MAX_HANDOFF + 1)).unwrap_err();
    assert!(error.0.contains("more than 64 handoffs"), "{error}");
}

#[test]
fn a_retired_key_signs_nothing_and_a_pin_to_it_follows_the_key_that_replaced_it() {
    let retired = Retired {
        keys: vec![public(&OLD)],
        current: Some(public(&NEW)),
    };
    let verify = |bytes: &[u8], pin: [u8; 32], retired: &Retired| {
        verify_for_reader_with(bytes, "alab", &pin, &[], None, retired).map(|(_, pin, _)| pin)
    };

    let direct = sign(&unsigned(9), &OLD).expect("signed");
    assert_eq!(
        verify(&direct, public(&OLD), &Retired::default()),
        Ok(Pin::Same)
    );
    for pin in [public(&OLD), public(&NEW)] {
        let error = verify(&direct, pin, &retired).unwrap_err();
        assert!(error.0.contains("rotated away from"), "{error}");
    }

    let mut fork = unsigned(9);
    let endorsement = endorse(&OLD, "alab", &public(&STRANGER)).expect("endorsed");
    fork.handoff = vec![(public(&OLD), endorsement)];
    let fork = sign(&fork, &STRANGER).expect("signed");
    assert_eq!(
        verify(&fork, public(&OLD), &Retired::default()),
        Ok(Pin::Rotated(public(&STRANGER)))
    );
    assert!(verify(&fork, public(&OLD), &retired).is_err());

    let mut real = unsigned(9);
    let endorsement = endorse(&OLD, "alab", &public(&NEW)).expect("endorsed");
    real.handoff = vec![(public(&OLD), endorsement)];
    let real = sign(&real, &NEW).expect("signed");
    assert_eq!(
        verify(&real, public(&OLD), &retired),
        Ok(Pin::Replaced(public(&NEW)))
    );
    assert_eq!(verify(&real, public(&NEW), &retired), Ok(Pin::Same));
    let bare = sign(&unsigned(10), &NEW).expect("signed");
    assert_eq!(
        verify(&bare, public(&OLD), &retired),
        Ok(Pin::Replaced(public(&NEW)))
    );
    let unreplaced = Retired {
        keys: vec![public(&OLD)],
        current: None,
    };
    assert!(verify(&real, public(&OLD), &unreplaced).is_err());
}

#[test]
fn a_pin_the_removed_admin_led_a_reader_to_gives_way_and_what_it_held_floors_nothing() {
    let retired = Retired {
        keys: vec![public(&OLD)],
        current: Some(public(&NEW)),
    };
    let mut real = unsigned(10);
    real.handoff = vec![(
        public(&OLD),
        endorse(&OLD, "alab", &public(&NEW)).expect("endorsed"),
    )];
    let real = sign(&real, &NEW).expect("signed");
    let verify = |bytes: &[u8], pin: [u8; 32], followed: &[[u8; 32]], held: &Floor| {
        let take = |retired: &Retired| {
            verify_for_reader_with(bytes, "alab", &pin, followed, Some(held), retired)
                .map(|(_, pin, verdict)| (pin, verdict))
        };
        (take(&Retired::default()), take(&retired))
    };
    let keys = |envelope: &Envelope| -> Vec<[u8; 32]> {
        envelope.handoff.iter().map(|(key, _)| *key).collect()
    };

    let mut fork = unsigned(1000);
    fork.handoff = vec![(
        public(&OLD),
        endorse(&OLD, "alab", &public(&STRANGER)).expect("endorsed"),
    )];
    let fork = sign(&fork, &STRANGER).expect("signed");
    let (taken, pin, _) =
        verify_for_reader_with(&fork, "alab", &public(&OLD), &[], None, &Retired::default())
            .expect("followed before a build knew");
    assert_eq!(pin, Pin::Rotated(public(&STRANGER)));
    let held = Floor::of(&taken);
    let followed = keys(&taken);
    let (unaware, aware) = verify(&real, public(&STRANGER), &followed, &held);
    assert!(
        unaware
            .as_ref()
            .unwrap_err()
            .0
            .contains("endorsed no handoff"),
        "{unaware:?}"
    );
    assert_eq!(aware, Ok((Pin::Replaced(public(&NEW)), Verdict::New)));
    let mut later = unsigned(1001);
    later.handoff = taken.handoff.clone();
    let later = sign(&later, &STRANGER).expect("signed");
    let (unaware, aware) = verify(&later, public(&STRANGER), &followed, &held);
    assert_eq!(unaware, Ok((Pin::Same, Verdict::New)));
    assert!(aware.is_err());
    let (_, aware) = verify(&real, public(&STRANGER), &[], &held);
    assert!(aware.is_err());

    let high = parse(&sign(&unsigned(1 << 62), &OLD).expect("signed")).expect("parsed");
    let (unaware, aware) = verify(&real, public(&OLD), &[], &Floor::of(&high));
    assert!(
        unaware.as_ref().unwrap_err().0.contains("rollback"),
        "{unaware:?}"
    );
    assert_eq!(aware, Ok((Pin::Replaced(public(&NEW)), Verdict::New)));

    let real = parse(&real).expect("parsed");
    let (_, aware) = verify(real.raw(), public(&NEW), &keys(&real), &Floor::of(&real));
    assert_eq!(aware, Ok((Pin::Same, Verdict::Unchanged)));
    let beyond = [10u8; 32];
    let mut rotated = unsigned(11);
    rotated.handoff = real.handoff.clone();
    rotated.handoff.push((
        public(&NEW),
        endorse(&NEW, "alab", &public(&beyond)).expect("endorsed"),
    ));
    let rotated = parse(&sign(&rotated, &beyond).expect("signed")).expect("parsed");
    let mut older = unsigned(9);
    older.handoff = rotated.handoff.clone();
    let older = sign(&older, &beyond).expect("signed");
    let (_, aware) = verify(
        &older,
        public(&beyond),
        &keys(&rotated),
        &Floor::of(&rotated),
    );
    assert!(aware.unwrap_err().0.contains("rollback"));
}

#[test]
fn a_newer_version_carries_on_the_handoff_chain_the_reader_holds() {
    let seeds = seeds(2);
    let key = |index: usize| public(&seeds[index]);
    let full = chain(&seeds);
    let version = |seq: u64, handoff: &[([u8; 32], [u8; 64])], seed: &[u8; 32]| {
        let mut unsigned = unsigned(seq);
        unsigned.handoff = handoff.to_vec();
        sign(&unsigned, seed).expect("signed")
    };
    let read = |bytes: &[u8], pin: [u8; 32], held: &Floor| {
        verify_for_reader(bytes, "alab", &pin, &[], Some(held))
            .map(|(_, pin, verdict)| (pin, verdict))
    };
    let held = Floor::of(&parse(&version(5, &full[..1], &seeds[1])).expect("parsed"));

    let cut = version(6, &full[1..], &seeds[2]);
    let error = read(&cut, key(1), &held).unwrap_err();
    assert!(
        error.0.contains("drops or changes handoff 1 of 1"),
        "{error}"
    );
    assert!(error.0.contains(&fingerprint(&key(0))), "{error}");
    assert_eq!(
        read(&cut, key(1), &Floor::version(held.seq, held.digest)),
        Ok((Pin::Rotated(key(2)), Verdict::New))
    );
    assert_eq!(
        read(&version(6, &full, &seeds[2]), key(1), &held),
        Ok((Pin::Rotated(key(2)), Verdict::New))
    );
    let error = read(&version(6, &[], &seeds[1]), key(1), &held).unwrap_err();
    assert!(error.0.contains("handoff 1 of 1"), "{error}");
    assert_eq!(
        read(&version(6, &full[..1], &seeds[1]), key(1), &held),
        Ok((Pin::Same, Verdict::New))
    );
    let mut changed = full.clone();
    changed[0] = (
        public(&STRANGER),
        endorse(&STRANGER, "alab", &key(1)).expect("endorsed"),
    );
    let error = read(&version(6, &changed, &seeds[2]), key(1), &held).unwrap_err();
    assert!(error.0.contains("handoff 1 of 1"), "{error}");
    let held = Floor::of(&parse(&cut).expect("parsed"));
    assert_eq!(
        read(&version(7, &full, &seeds[2]), key(2), &held),
        Ok((Pin::Same, Verdict::New))
    );
}

#[test]
fn an_admin_puts_back_the_links_a_rotation_by_a_build_before_chains_dropped() {
    let seeds = seeds(3);
    let key = |index: usize| public(&seeds[index]);
    let full = chain(&seeds);
    let held = chain(&seeds[..3]);
    let opened = |handoff: &[([u8; 32], [u8; 64])], seed: &[u8; 32]| {
        let mut unsigned = unsigned(8);
        unsigned.handoff = handoff.to_vec();
        parse(&sign(&unsigned, seed).expect("signed")).expect("parsed")
    };
    let cut = opened(&full[2..], &seeds[3]);
    let restored = cut.chain_carried_on(&key(2), &held).expect("restored");
    assert_eq!(restored, full);
    let mut next = unsigned(9);
    next.handoff = restored;
    let next = sign(&next, &seeds[3]).expect("the restored chain verifies");
    let (_, pin, _) =
        verify_for_reader(&next, "alab", &key(0), &[], None).expect("the first key follows");
    assert_eq!(pin, Pin::Rotated(key(3)));
    assert_eq!(
        opened(&full, &seeds[3]).chain_carried_on(&key(2), &held),
        Ok(full.clone())
    );
    assert_eq!(
        opened(&[], &seeds[2]).chain_carried_on(&key(2), &held),
        Ok(held.clone())
    );
    let mut changed = full[1..].to_vec();
    changed[0] = (
        public(&STRANGER),
        endorse(&STRANGER, "alab", &key(2)).expect("endorsed"),
    );
    let error = opened(&changed, &seeds[3])
        .chain_carried_on(&key(2), &held)
        .unwrap_err();
    assert!(error.0.contains("handoff 1 of 2"), "{error}");
}

#[test]
fn rollback_and_fork_are_refused_and_identical_bytes_are_unchanged() {
    let pin = public(&OLD);
    let seven = sign(&unsigned(7), &OLD).expect("signed");
    let held = Floor::of(&parse(&seven).expect("parsed"));
    let (_, _, verdict) = verify_for_reader(&seven, "alab", &pin, &[], Some(&held)).expect("same");
    assert_eq!(verdict, Verdict::Unchanged);

    let six = sign(&unsigned(6), &OLD).expect("signed");
    let error = verify_for_reader(&six, "alab", &pin, &[], Some(&held)).unwrap_err();
    assert!(error.0.contains("rollback"), "{error}");

    let mut fork = unsigned(7);
    fork.time += 1;
    let fork = sign(&fork, &OLD).expect("signed");
    let error = verify_for_reader(&fork, "alab", &pin, &[], Some(&held)).unwrap_err();
    assert!(error.0.contains("fork"), "{error}");

    let eight = sign(&unsigned(next_seq(held.seq).expect("successor")), &OLD).expect("signed");
    let (_, _, verdict) = verify_for_reader(&eight, "alab", &pin, &[], Some(&held)).expect("newer");
    assert_eq!(verdict, Verdict::New);

    let penultimate = sign(&unsigned(u64::MAX - 1), &OLD).expect("signed");
    let held = Floor::of(&parse(&penultimate).expect("parsed"));
    let last_seq = next_seq(held.seq).expect("last successor");
    assert_eq!(last_seq, u64::MAX);
    let last = sign(&unsigned(last_seq), &OLD).expect("signed");
    let (last, _, verdict) =
        verify_for_reader(&last, "alab", &pin, &[], Some(&held)).expect("last is newer");
    assert_eq!(last.seq, u64::MAX);
    assert_eq!(verdict, Verdict::New);
    assert!(next_seq(last.seq).unwrap_err().0.contains("exhausted"));
}

/// The signed text with one line replaced, re-signed: structure refusals, not signatures.
fn resigned(edit: impl Fn(&str) -> String) -> Vec<u8> {
    let bytes = sign(&unsigned(2), &OLD).expect("signed");
    let text = std::str::from_utf8(&bytes).expect("text");
    let unsigned_text = &text[..text.find("signature: ").expect("signature line")];
    let edited = edit(unsigned_text);
    let pair = key_pair(&OLD).expect("pair");
    let signature = pair.sign(edited.as_bytes());
    format!(
        "{edited}signature: {}\n",
        b64::encode_padded(signature.as_ref())
    )
    .into_bytes()
}

#[test]
fn the_grammar_has_one_spelling() {
    parse(&resigned(str::to_owned)).expect("control");
    let long = format!("{}\n", "A".repeat(65));
    let previous_alone = format!(
        "previous-signer: {}\nlocker: admin\n",
        b64::encode_padded(&[1u8; 32])
    );
    for (what, edit) in [
        ("seq with a leading zero", ("seq: 2\n", "seq: 02\n")),
        ("seq zero", ("seq: 2\n", "seq: 0\n")),
        ("seq with a sign", ("seq: 2\n", "seq: +2\n")),
        ("two spaces", ("seq: 2\n", "seq:  2\n")),
        (
            "time with a leading zero",
            ("time: 1792530000\n", "time: 01792530000\n"),
        ),
        ("uppercase key", ("name: alab\n", "Name: alab\n")),
        ("bad name", ("name: alab\n", "name: Alab\n")),
        ("name with a dot", ("name: alab\n", "name: a.b\n")),
        (
            "headers out of order",
            ("seq: 2\ntime: 1792530000\n", "time: 1792530000\nseq: 2\n"),
        ),
        ("duplicate header", ("seq: 2\n", "seq: 2\nseq: 2\n")),
        ("unknown header", ("seq: 2\n", "seq: 2\ncolour: red\n")),
        ("label with a leading zero", ("locker: 1\n", "locker: 01\n")),
        ("label zero", ("locker: 1\n", "locker: 0\n")),
        ("label word", ("locker: 1\n", "locker: m22\n")),
        ("duplicate label", ("locker: 1\n", "locker: admin\n")),
        ("blank line", ("locker: 1\n", "\nlocker: 1\n")),
        ("armor line too long", ("ZXZlcnlvbmU=\n", long.as_str())),
        (
            "armor line outside the alphabet",
            ("ZXZlcnlvbmU=\n", "ZXZlcnl-bmU=\n"),
        ),
        ("empty armor line", ("ZXZlcnlvbmU=\n", "ZXZlcnlvbmU=\n\n")),
        (
            "bad BEGIN",
            ("locker: 1\n-----BEGIN AGE", "locker: 1\n-----BEGIN  AGE"),
        ),
        (
            "endorsement without previous-signer",
            ("locker: admin\n", "endorsement: AAAA\nlocker: admin\n"),
        ),
        (
            "previous-signer without endorsement",
            ("locker: admin\n", previous_alone.as_str()),
        ),
    ] {
        let bytes = resigned(|text| {
            assert!(text.contains(edit.0), "{what}: fixture lacks {:?}", edit.0);
            text.replacen(edit.0, edit.1, 1)
        });
        assert!(parse(&bytes).is_err(), "{what} was accepted");
    }
    let bytes =
        resigned(|text| text.replacen(&format!("{ARMOR_END}\nlocker: 1\n"), "locker: 1\n", 1));
    assert!(parse(&bytes).is_err());
}

#[test]
fn keys_and_signatures_must_be_canonical_and_sized() {
    let signer = b64::encode_padded(&public(&OLD));
    let short = b64::encode_padded(&[1u8; 31]);
    let bytes = resigned(|text| text.replacen(&signer, &short, 1));
    assert!(parse(&bytes).is_err());
    let bytes = sign(&unsigned(2), &OLD).expect("signed");
    let text = std::str::from_utf8(&bytes).expect("text");
    let unpadded = text.replacen("=\n", "\n", 1);
    assert!(parse(unpadded.as_bytes()).is_err());
}

#[test]
fn sizes_are_bounded() {
    let mut many = unsigned(1);
    many.lockers = (1..=MAX_LOCKERS + 1)
        .map(|index| Locker {
            label: index.to_string(),
            armored: test_armor(b"x"),
        })
        .collect();
    assert!(sign(&many, &OLD).is_err());
    many.lockers.pop();
    let bytes = sign(&many, &OLD).expect("64 lockers");
    assert_eq!(parse(&bytes).expect("parsed").lockers.len(), MAX_LOCKERS);

    let mut big = unsigned(1);
    big.lockers = vec![Locker {
        label: "1".to_owned(),
        armored: test_armor(&vec![0u8; MAX_ENVELOPE]),
    }];
    assert!(sign(&big, &OLD).is_err(), "over 16 MiB");
    assert!(
        parse(&vec![b'a'; MAX_ENVELOPE + 1])
            .unwrap_err()
            .0
            .contains("larger")
    );
}

#[test]
fn sign_refuses_what_parse_would() {
    let mut bad = unsigned(0);
    assert!(sign(&bad, &OLD).is_err(), "seq 0");
    bad.seq = 1;
    bad.name = "../x".to_owned();
    assert!(sign(&bad, &OLD).is_err(), "name");
    let mut bad = unsigned(1);
    bad.lockers[1].label = "admin".to_owned();
    assert!(sign(&bad, &OLD).is_err(), "duplicate label");
    let mut bad = unsigned(1);
    bad.lockers[1].armored.pop();
    assert!(sign(&bad, &OLD).is_err(), "no final LF");
}

#[test]
fn a_fingerprint_is_five_groups_of_hex() {
    let print = fingerprint(&public(&OLD));
    assert_eq!(print.len(), 24);
    let groups: Vec<&str> = print.split('-').collect();
    assert_eq!(groups.len(), 5);
    assert!(groups.iter().all(|g| {
        g.len() == 4
            && g.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }));
    let digest = aterm_digest::Sha256::digest(public(&OLD));
    assert_eq!(&print[..4], format!("{:02x}{:02x}", digest[0], digest[1]));
    assert_eq!(&print[20..], format!("{:02x}{:02x}", digest[8], digest[9]));
    assert_ne!(print, fingerprint(&public(&NEW)));
}

#[test]
fn names_and_labels() {
    for good in ["alab", "a", "0", "my-vault", &"a".repeat(63)] {
        assert!(valid_name(good), "{good}");
    }
    for bad in ["", "-a", "A", "a.b", "a/b", "a_b", "..", &"a".repeat(64)] {
        assert!(!valid_name(bad), "{bad}");
    }
    for good in ["admin", "1", "64", "10"] {
        assert!(valid_label(good), "{good}");
    }
    for bad in ["", "0", "01", "Admin", "1a", "-1"] {
        assert!(!valid_label(bad), "{bad}");
    }
}

#[test]
fn parents_round_trip_are_signed_and_forbid_seq_one_or_duplicates() {
    let p1 = [0x11u8; 32];
    let p2 = [0x22u8; 32];
    let mut u = unsigned(2);
    u.parents = vec![p1, p2];
    let bytes = sign(&u, &OLD).expect("signed with two parents");
    let parsed = parse(&bytes).expect("parsed");
    assert_eq!(parsed.parents, vec![p1, p2]);
    parsed.verify_signature().expect("signature covers parents");

    // Negative control 1: mutating a `parent:` byte invalidates the Ed25519 signature.
    let text = String::from_utf8(bytes).expect("utf-8");
    let tampered = text.replacen(
        &format!("parent: {}\n", b64::encode_padded(&p1)),
        &format!("parent: {}\n", b64::encode_padded(&[0x33u8; 32])),
        1,
    );
    let tampered_env = parse(tampered.as_bytes()).expect("structure still parses");
    assert!(
        tampered_env.verify_signature().is_err(),
        "tampered parent must fail signature verification"
    );

    // Negative control 2: `seq == 1` with parents and duplicate parents are refused.
    let mut seq1 = unsigned(1);
    seq1.parents = vec![p1];
    assert!(sign(&seq1, &OLD).is_err(), "seq 1 cannot carry parents");
    let mut dup = unsigned(2);
    dup.parents = vec![p1, p1];
    assert!(sign(&dup, &OLD).is_err(), "duplicate parents refused");
}
