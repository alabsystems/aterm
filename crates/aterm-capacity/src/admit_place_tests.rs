// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PLACE ([`mod@super`]): an entry at a place is ordered by the place and is as old as its own
//! laying, and a place is one only while a record this process laid at it still stands.

use super::*;
use crate::{Fixed, Proc};

const HOUR: u64 = 3_600_000;
const DAY: u64 = 24 * HOUR;

/// Poll once and expect admission.
#[track_caller]
fn admitted(w: Waiter, m: &Fixed) -> Claim {
    match w.poll(m) {
        Polled::Admitted(c) => c,
        Polled::Waiting(_, short) => panic!("expected admission, waits: {short}"),
        Polled::Gone(e) => panic!("expected admission, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected admission, unqualified: {why}"),
    }
}

/// Poll once and expect a wait: the waiter and what it is short of.
#[track_caller]
fn waits(w: Waiter, m: &Fixed) -> (Waiter, Shortfall) {
    match w.poll(m) {
        Polled::Waiting(w, short) => (w, short),
        Polled::Admitted(c) => panic!("expected a wait, admitted: {:?}", c.record()),
        Polled::Gone(e) => panic!("expected a wait, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected a wait, unqualified: {why}"),
    }
}

/// THE LAW OF A PLACE, in a scratch ledger on a fixed machine (36 GiB, 10 of it available), the
/// stamps chosen and no clock waited on. A piece of work laid its first 2 GiB request a day ago;
/// another process laid a 16 GiB claim an hour after it, which cannot fit beside the work.
/// - An entry at the work's place takes the place's order stamp, so the ledger's readers sort it
///   ahead of the large claim, and it is as old as its own laying: its record says when it was
///   laid, its ask is that laying, and the large claim's wait line counts its age from there,
///   never the day the place is old.
/// - While a record of the work stands at the place, held or waiting, the large claim waits;
///   the work's next request, entered at the place before the claim it holds is released, keeps
///   it waiting across that release.
/// - A place holds nothing: once the last record at it is gone it is no place, and an entry at
///   it is laid at now. So is an entry at a place another ledger or another process minted.
/// - A shared observation judges a need at the place ahead of the large claim, and the same
///   need judged as laid now behind it.
///
/// NEGATIVE CONTROLS. The successor laid at now (the code before): the large claim is admitted
/// alone as soon as the held claim is released, ahead of the work's next request. A laying
/// stamp written earlier than the place reads as none.
#[test]
fn an_entry_at_a_place_orders_by_the_place_and_ages_from_its_laying() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("cap-place-")
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let m = Fixed::new(Reading::of_gib(36, 10, 14));
    let me = std::process::id();
    let t0 = now_ms() - DAY;
    let binary = || Need::new("verify test binary").mem_gib(2);

    // The work's first request, laid a day ago, and the place it mints.
    let first = Waiter::enter_stand_in(&ledger, &m, binary(), None, t0).expect("entered");
    assert_eq!(first.record().laid_ms(), t0, "laid at its own place");
    let place = first.place();
    assert_eq!(place.standing_ms(), Some(t0));
    // Another process's claim, laid an hour after it: 16 GiB where 10 are available.
    let large = Waiter::enter_stand_in(
        &ledger,
        &m,
        Need::new("cold build").mem_gib(16),
        Some(me + 1),
        t0 + HOUR,
    )
    .expect("entered");

    // AN ENTRY AT THE PLACE: ordered by the place, as old as its laying.
    let second = Waiter::enter_at(&ledger, &m, binary(), &place).expect("entered at the place");
    let at_place = second.record().clone();
    assert_eq!(at_place.start_ms, t0, "its order stamp is the place's");
    assert!(
        at_place.laid_ms() > t0 + HOUR,
        "its laying is now, not the place: {at_place:?}"
    );
    assert_eq!(at_place.laid_at, Some(at_place.laid_ms()));
    assert_eq!(second.place().standing_ms(), Some(t0));
    let listed = ledger.list().expect("listed");
    assert_eq!(
        listed.iter().map(Record::order).collect::<Vec<_>>(),
        [
            first.record().order(),
            at_place.order(),
            large.record().order()
        ],
        "the ledger's readers sort both of the work's records ahead of the later claim"
    );
    let read = &listed[1];
    assert_eq!(
        (read.start_ms, read.laid_at),
        (t0, at_place.laid_at),
        "another reader finds the place and the laying in its file"
    );

    // The first is admitted; the large claim waits behind the work's two records, and counts
    // the age of the entry at the place from its laying.
    let claim1 = admitted(first, &m);
    let (large, short) = waits(large, &m);
    assert!(
        matches!(short.shorts[..], [Short::Memory { .. }]),
        "{short}"
    );
    let holder = short
        .holders
        .iter()
        .find(|h| h.order == at_place.order())
        .unwrap_or_else(|| panic!("the entry at the place is ahead of it: {short}"));
    assert!(
        holder.age < Duration::from_millis(DAY - HOUR),
        "its age counts from its laying, not from the day-old place: {:?}",
        holder.age
    );
    // Its ask, which its telemetry's wait counts from, is its laying too.
    let claim2 = admitted(second, &m);
    assert_eq!(claim2.asked_ms(), at_place.laid_ms());
    assert!(claim2.asked_ms() > t0 + HOUR);
    assert_eq!(claim2.record().start_ms, t0);

    // THE PLACE IS HANDED ON: the next request is entered at it while a claim at it is held,
    // and it keeps the large claim waiting once the held claims are released.
    let third = Waiter::enter_at(&ledger, &m, binary(), &claim2.place()).expect("entered");
    assert_eq!(third.record().start_ms, t0);
    drop((claim1, claim2));
    assert_eq!(
        place.standing_ms(),
        Some(t0),
        "the waiting successor keeps it"
    );
    let (large, short) = waits(large, &m);
    assert!(
        short
            .holders
            .iter()
            .any(|h| h.order == third.record().order()),
        "it waits for the work's standing request: {short}"
    );

    // A SHARED OBSERVATION judges a need at the place, or as one laid now.
    let observed = Fixed::new(Reading::of_gib(36, 10, 14));
    observed.set_procs(Some(Procs::of(
        [me, me + 1]
            .into_iter()
            .map(|pid| Proc {
                pid,
                charged: 1,
                footprint: 1,
                ..Proc::default()
            })
            .collect(),
    )));
    let at = Instant::now();
    let probe = ResourceProbe::capture_at(&ledger, &observed, at)
        .expect("the ledger reads")
        .observed()
        .expect("an observation");
    assert!(
        probe
            .rejection_placed(&binary(), at, Some(&place))
            .is_none(),
        "at the place it is ahead of the large claim, and fits"
    );
    assert!(
        matches!(
            probe.rejection_at(&binary(), at),
            Some(ProbeRejection::Waiting(_))
        ),
        "laid now it is behind the large claim, which spares nothing"
    );

    // NEGATIVE CONTROL: the successor laid at now. The large claim, laid before it, is admitted
    // alone the moment the work's held claim is released.
    let claim3 = admitted(third, &m);
    let successor = Waiter::enter(&ledger, &m, binary()).expect("entered at now");
    assert!(successor.record().start_ms > t0 + HOUR);
    let (large, _) = waits(large, &m);
    drop(claim3);
    let large = admitted(large, &m);
    assert!(
        large.alone().is_some(),
        "nothing of the work stood ahead of it"
    );
    let (successor, short) = waits(successor, &m);
    assert!(matches!(short.shorts[..], [Short::Alone { .. }]), "{short}");
    drop((large, successor));

    // A PLACE HOLDS NOTHING. With no record left at it, it is no place.
    assert_eq!(place.standing_ms(), None);
    let late = Waiter::enter_at(&ledger, &m, binary(), &place).expect("entered");
    assert!(
        late.record().start_ms > t0 + HOUR && late.record().laid_at.is_none(),
        "an entry at a place nobody stands at is laid at now: {:?}",
        late.record()
    );
    drop(late);
    // Nor is one another ledger's record minted, or another process's.
    let elsewhere = Ledger::at(dir.path().join("elsewhere"));
    let there = Waiter::enter_stand_in(&elsewhere, &m, binary(), None, t0).expect("entered");
    let here = Waiter::enter_at(&ledger, &m, binary(), &there.place()).expect("entered");
    assert!(here.record().start_ms > t0 + HOUR, "{:?}", here.record());
    let theirs = Waiter::enter_stand_in(&ledger, &m, binary(), Some(me + 1), t0).expect("entered");
    let mine = Waiter::enter_at(&ledger, &m, binary(), &theirs.place()).expect("entered");
    assert!(mine.record().start_ms > t0 + HOUR, "{:?}", mine.record());

    // NEGATIVE CONTROL: a laying before its place is none, as a reader finds it.
    let head = |laid: u64| {
        format!(
            "{{\"v\":1,\"pid\":1,\"start_ms\":10,\"laid_ms\":{laid},\"seq\":1,\"checkout\":\"\",\
             \"what\":\"w\",\"mem\":0,\"cores\":0,\"disk\":0}}\n"
        )
    };
    let decoded = |laid: u64| Record::decode(&head(laid), false).expect("a record");
    assert_eq!(decoded(12).laid_at, Some(12));
    assert_eq!(decoded(12).laid_ms(), 12);
    for laid in [9, 10] {
        assert_eq!(decoded(laid).laid_at, None);
        assert_eq!(decoded(laid).laid_ms(), 10);
    }
}
