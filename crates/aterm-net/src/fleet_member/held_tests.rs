// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kept verdicts are the check's own: each law counts the checks this thread made
//! ([`signature_checks_here`]) and compares every verdict with [`Peer::signature_holds`].

use super::super::{NodeIdentity, signature_checks_here};
use super::*;

fn identity(n: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{n:032x}"), &[n; 32]).unwrap()
}

/// A book of `count` nodes' own entries, each signed.
fn book(count: u8) -> Vec<Peer> {
    (0..count)
        .map(|n| Peer::own(&identity(n), ("member.invalid", 7100, "1.0.0"), 7))
        .collect()
}

/// `judge` of `book`, with how many checks it made.
fn judged(held: &mut HeldSignatures, book: &[Peer]) -> (Vec<bool>, u64) {
    let before = signature_checks_here();
    let verdicts = held.judge(book);
    (verdicts, signature_checks_here() - before)
}

/// Two reads of an unchanged 85-entry book: the first checks each entry once, the second none,
/// and both give the check's own verdicts.
///
/// NEGATIVE CONTROL (2026-11-12): with [`HeldSignatures::judge`] never looking a kept verdict
/// up, the second read made 85 checks (`an unchanged book is not checked again`).
#[test]
fn an_unchanged_book_is_checked_once() {
    let book = book(85);
    let mut held = HeldSignatures::default();
    let (first, checks) = judged(&mut held, &book);
    assert_eq!(checks, 85, "a book not judged before is checked whole");
    assert_eq!(first, vec![true; 85]);
    let (second, checks) = judged(&mut held, &book);
    assert_eq!(checks, 0, "an unchanged book is not checked again");
    assert_eq!(second, first);
}

/// Every field the check reads is part of what a verdict is kept under: an entry that differs
/// from a judged one in any of them is checked again and takes the check's own verdict, which
/// for a field the signature covers is a refusal. A failed verdict is kept as a failure.
///
/// NEGATIVE CONTROL (2026-11-12): with a verdict kept under the node id and key alone, the
/// entry whose version changed took the verdict of the entry it was (`host`: `left: [true]`,
/// `right: [false]`, the first field the loop changes).
#[test]
fn an_entry_changed_in_any_signed_field_is_checked_again() {
    let me = identity(7);
    let signed =
        Peer::own(&me, ("member.invalid", 7100, "1.0.0"), 7).with_primary_name(&me, Some("alpha"));
    assert!(signed.signature_holds());
    let other = identity(8);
    let changed: [(&str, Peer); 8] = [
        ("host", {
            let mut peer = signed.clone();
            peer.host = "moved.invalid".into();
            peer
        }),
        ("port", {
            let mut peer = signed.clone();
            peer.port = 7101;
            peer
        }),
        ("version", {
            let mut peer = signed.clone();
            peer.version = Some("2.0.0".into());
            peer
        }),
        ("key", {
            let mut peer = signed.clone();
            peer.key = Some(other.key());
            peer
        }),
        ("node", {
            let mut peer = signed.clone();
            peer.node = Some(other.node().to_owned());
            peer
        }),
        ("self-seq", {
            let mut peer = signed.clone();
            peer.signed.as_mut().unwrap().seq = 8;
            peer
        }),
        ("signature", {
            let mut peer = signed.clone();
            peer.signed.as_mut().unwrap().sig[0] ^= 1;
            peer
        }),
        ("name", {
            // Another name under the first name's signature.
            let mut peer = signed.clone();
            let other =
                Signed::named_by(&me, ("member.invalid", 7100, "1.0.0"), 7, Some("beta")).unwrap();
            let mut name = other.name.unwrap();
            name.sig = signed.signed.unwrap().name.unwrap().sig;
            peer.signed.as_mut().unwrap().name = Some(name);
            peer
        }),
    ];
    for (field, peer) in changed {
        let mut held = HeldSignatures::default();
        assert_eq!(
            judged(&mut held, std::slice::from_ref(&signed)),
            (vec![true], 1)
        );
        let expected = peer.signature_holds();
        assert!(!expected, "{field}: the signature covers it");
        let (verdict, checks) = judged(&mut held, std::slice::from_ref(&peer));
        assert_eq!(verdict, [expected], "{field}");
        assert_eq!(checks, 1, "{field}: a changed entry is checked");
        let (verdict, checks) = judged(&mut held, std::slice::from_ref(&peer));
        assert_eq!(verdict, [false], "{field}: a failure is kept as a failure");
        assert_eq!(checks, 0, "{field}: and not checked again");
    }
}

/// What the check does not read changes no verdict and costs no check (the address and time a
/// book notes of an entry), and an entry with no signature holds nothing without one.
#[test]
fn what_the_check_does_not_read_keeps_its_verdict() {
    let signed = Peer::own(&identity(7), ("member.invalid", 7100, "1.0.0"), 7);
    let mut held = HeldSignatures::default();
    assert_eq!(
        judged(&mut held, std::slice::from_ref(&signed)),
        (vec![true], 1)
    );
    let mut noted = signed.clone();
    noted.addr = Some(std::net::IpAddr::from([192, 0, 2, 17]));
    noted.seen_ms = Some(0);
    assert!(noted.signature_holds());
    let mut unsigned = signed.clone();
    unsigned.signed = None;
    assert!(!unsigned.signature_holds());
    let before = signature_checks_here();
    assert_eq!(held.judge(&[noted, unsigned]), [true, false]);
    assert_eq!(signature_checks_here() - before, 0);
}

/// The verdicts kept are the last book's alone: an entry that left the book is checked when it
/// comes back, and one the book names twice is checked once.
#[test]
fn only_the_last_books_verdicts_are_kept() {
    let book = book(3);
    let mut held = HeldSignatures::default();
    assert_eq!(judged(&mut held, &book), (vec![true; 3], 3));
    assert_eq!(judged(&mut held, &book[..2]), (vec![true; 2], 0));
    assert_eq!(
        judged(&mut held, &book),
        (vec![true; 3], 1),
        "the entry that left is checked again"
    );
    let twice = [book[0].clone(), book[0].clone()];
    let mut held = HeldSignatures::default();
    assert_eq!(judged(&mut held, &twice), (vec![true; 2], 1));
}
