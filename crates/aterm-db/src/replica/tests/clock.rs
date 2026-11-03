// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The clock mark across a repair round (docs/DESIGN-fluid-swarm-2026-10-28.md F22): what each
//! side reads of the other's clock, and a peer from before F22, which reads a mark as a table it
//! does not hold and sends none.

use super::*;
use crate::{
    accounts::claims::{Caps, EPOCH_S, write_at},
    catalog::ring::CLAIMS,
    clock::{self, Class, Source, Standing, T_MS},
    table::tests::clock_round,
};
use aterm_json::json;

const DAY: u64 = EPOCH_S;
/// Day 20,000 since the Unix epoch.
const E: u64 = 20_000;

/// Noon of day `day`, in seconds.
const fn noon(day: u64) -> u64 {
    day * DAY + DAY / 2
}

/// Write Mac `n`'s claims row at `now` (seconds), its `counter`-th.
fn claim(lab: &Lab, n: usize, counter: u64, now: u64) {
    let body = json!({"counter": counter, "until": now + DAY, "claims": []});
    write_at(&lab.paths[n], &body, now, Caps::default()).unwrap();
}

/// The claims day tables Mac `n` holds storage for among `days`.
fn held(lab: &Lab, n: usize, days: std::ops::RangeInclusive<u64>) -> Vec<u64> {
    let bootstrap = lab.bootstrap(n);
    let directory = crate::catalog::Directory::new(&lab.paths[n], bootstrap.trust());
    days.filter(|day| {
        let scope = CLAIMS.scope(bootstrap.trust(), *day).unwrap();
        directory.path(scope).unwrap().exists()
    })
    .collect()
}

/// Each Mac's Commons machine and recipient, and Mac `n`'s own machine.
fn members(lab: &Lab, n: usize) -> (BTreeMap<[u8; 16], String>, [u8; 16]) {
    let bindings: Vec<_> = (0..lab.paths.len())
        .map(|m| lab.bootstrap(m).request().unwrap().binding().clone())
        .collect();
    (clock::members(bindings.iter()), *bindings[n].machine())
}

fn machine(lab: &Lab, n: usize) -> [u8; 16] {
    *lab.bootstrap(n).request().unwrap().binding().machine()
}

/// Two endpoints two days apart: one round hands each side a reading of the other's mark, the
/// one ahead reading itself ahead by more than the tolerance and the one behind behind by more
/// than it; on one clock both agree.
///
/// Negative control: with the mark left out of the list (`with_mark` adding none), neither side
/// read anything (`left: None`, the `expect("Mac 0 read Mac 1's mark")` failing).
#[test]
fn one_round_hands_both_sides_a_reading() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let ahead = noon(E + 2);
    let [a, b] = clock_round(&lab, (0, 1), [ahead, noon(E)], [true, true]);
    let a = a.clock().expect("Mac 0 read Mac 1's mark");
    let b = b.clock().expect("Mac 1 read Mac 0's mark");
    assert_eq!((a.source, b.source), (Source::Mark, Source::Mark));
    assert!(a.lo_ms > T_MS, "{a:?}");
    assert!(b.hi_ms < -T_MS, "{b:?}");
    assert_eq!(a.class(Standing::Unknown), Class::Ahead);
    assert_eq!(b.class(Standing::Unknown), Class::Behind);
    let [a, b] = clock_round(&lab, (0, 1), [noon(E); 2], [true, true]);
    for read in [a.clock(), b.clock()] {
        assert_eq!(
            read.map(|read| read.class(Standing::Unknown)),
            Some(Class::Agree)
        );
    }
}

/// A peer from before F22 (`with_marks(false)`: it sends no mark and reads the list whole) takes
/// the other side's mark as a table it does not hold: every round completes both ways, each side
/// repairing the one table both chose, the claims table reaches the old side, and the old side
/// takes up no table for the mark, holding exactly the claims day it would.
///
/// Those assertions hold the old side's tolerance of a mark, which is pre-F22 code (it chooses
/// only from the lists' intersection and adopts only ring tables in its window, and no mark is
/// either): no F22 change reaches them but the mark beside a full list, which is
/// `the_mark_waits_for_room_below_the_bound`'s mutant. They stand against a later change that
/// would let a listed ID it does not hold into its selection or adoption. With the window filter
/// in front of adoption left out, this test still passed: `adopt_ring_tables` walks each ring's
/// window and looks its tables up in the offer, and a mark is none of them, so that filter is not
/// what holds the mark out.
///
/// Negative controls: with no reading taken from a peer that sent no mark (`Heard::Nothing` giving
/// none in place of `offered_reading`), the new side read nothing of the old one (`a reading from
/// the offer` failed); with a reading taken whether or not this side sends marks (the `marks`
/// gate in front of the reading left out), the old side read the new side's offer (`the old side
/// reads no clock`, `left: Some(Reading { lo_ms: -129600000, hi_ms: 129600000, at_ms:
/// 1728043200000, source: Ring })`, `right: None`).
#[test]
fn marks_cross_a_pre_mark_peer_unharmed() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let now = noon(E);
    claim(&lab, 0, 1, now);
    for round in 0..6 {
        let (pair, marks) = if round % 2 == 0 {
            ((0, 1), [true, false])
        } else {
            ((1, 0), [false, true])
        };
        let [a, b] = clock_round(&lab, pair, [now; 2], marks);
        let (a, b) = (
            a.application().expect("the initiator completed"),
            b.application().expect("the responder completed"),
        );
        assert_eq!(a.table, b.table, "round {round}: one table both chose");
    }
    assert_eq!(held(&lab, 1, E - 3..=E + 3), vec![E]);
    let [old, new] = clock_round(&lab, (1, 0), [now; 2], [false, true]);
    assert_eq!(old.clock(), None, "the old side reads no clock");
    // The new side reads the old one by the claims table it offers: day-coarse, undecided.
    let read = new.clock().expect("a reading from the offer");
    assert_eq!(read.source, Source::Ring);
    assert_eq!(read.class(Standing::Unknown), Class::Undecided);
}

/// A list that holds `MAX_TABLES` tables goes without its mark, since every receiver since 0.145
/// refuses a longer one; a list with room carries it.
///
/// Negative control: with the mark sent whatever the list's length (`with_mark` without its room
/// check), a receiver of 0.145's bound refused the list (`the old side reads it: Error("application
/// inventory exceeds its bound")`).
#[test]
fn the_mark_waits_for_room_below_the_bound() {
    let tables = |n: u8| (0..n).map(|i| [i | 0x80; 32]).collect::<BTreeSet<Digest>>();
    for (n, marked) in [(MAX_TABLES - 1, true), (MAX_TABLES, false)] {
        let local = tables(u8::try_from(n).unwrap());
        let listed = with_mark(&local, Some(1_728_000_000_000));
        // The list as it goes on the wire, unbounded here: what an old receiver is sent.
        let mut wire = u32::try_from(listed.len()).unwrap().to_le_bytes().to_vec();
        for id in &listed {
            wire.extend_from_slice(id);
        }
        let received: BTreeSet<Digest> =
            receive_ids(&mut Cursor::new(wire), MAX_TABLES).expect("the old side reads it");
        assert_eq!(received.len() == n + 1, marked, "{n} tables");
        assert_eq!(clock::split(&received).0, local);
    }
}

/// Peers from before F22 send no mark, so a Mac reads each by the claims tables it offers, which
/// lie in its window: a Mac two days ahead among peers offering days E−1 and E reads itself
/// ahead of both and stands Off; a Mac on day E reads a peer offering days E+1 and E+2 as
/// skewed, and an honest peer's mark as agreeing.
///
/// Negative control: with `from_offers` placing the peer's period at its least offer alone (`hi`
/// from `min − 1`), the Mac on day E read the peer offering E+1 and E+2 as undecided and named
/// nobody skewed (`left: []`, `right: [..]`).
#[test]
fn ring_offers_place_a_pre_mark_peer() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for n in [1, 2] {
        claim(&lab, n, 1, (E - 1) * DAY + 60);
        claim(&lab, n, 2, E * DAY + 60);
        assert_eq!(held(&lab, n, E - 3..=E + 3), vec![E - 1, E]);
    }
    let ahead = noon(E + 2);
    let readings: BTreeMap<_, _> = [1, 2]
        .into_iter()
        .map(|n| {
            let [a, _] = clock_round(&lab, (0, n), [ahead, noon(E)], [true, false]);
            (
                machine(&lab, n),
                a.clock().expect("a reading from the offers"),
            )
        })
        .collect();
    for read in readings.values() {
        assert_eq!(read.source, Source::Ring);
        assert_eq!(read.class(Standing::Unknown), Class::Ahead, "{read:?}");
    }
    let (all, me) = members(&lab, 0);
    let judged = clock::standing(&readings, &all, &me, Standing::Unknown, ahead * 1000);
    assert_eq!(judged.standing, Standing::Off { ahead: true });
    assert_eq!((judged.against, judged.of), (2, 2));

    // A pre-F22 Mac two days ahead offers days E+1 and E+2; Mac 0 on day E reads it as skewed.
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    claim(&lab, 1, 1, (E + 1) * DAY + 60);
    claim(&lab, 1, 2, (E + 2) * DAY + 60);
    assert_eq!(held(&lab, 1, E - 3..=E + 3), vec![E + 1, E + 2]);
    let [skewed, _] = clock_round(&lab, (0, 1), [noon(E), ahead], [true, false]);
    let [honest, _] = clock_round(&lab, (0, 2), [noon(E); 2], [true, true]);
    let readings = BTreeMap::from([
        (
            machine(&lab, 1),
            skewed.clock().expect("a reading from the offers"),
        ),
        (
            machine(&lab, 2),
            honest.clock().expect("a reading of the mark"),
        ),
    ]);
    let (all, me) = members(&lab, 0);
    let judged = clock::standing(&readings, &all, &me, Standing::Unknown, noon(E) * 1000);
    assert_eq!(
        judged
            .skewed
            .iter()
            .map(|skewed| skewed.machine)
            .collect::<Vec<_>>(),
        vec![machine(&lab, 1)]
    );
    assert_eq!(judged.standing, Standing::Split);
}
