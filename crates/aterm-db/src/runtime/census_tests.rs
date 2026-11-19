// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::discovery::{Marks, Verdict};
use super::*;
use crate::replica::{Report, Sweep};

fn actor(n: u16) -> Actor {
    let mut actor = [0xa5; 48];
    actor[..2].copy_from_slice(&n.to_be_bytes());
    actor
}

/// A round this side dialled to `seat` (none where it named no seat) that the scan chose as its
/// endpoint's probe and that rested its endpoint.
fn round(seat: Option<Actor>) -> Verdict {
    Verdict {
        seat,
        marks: Some(Marks {
            chose: Dial::Scan,
            probe: true,
            met_again: false,
            away: false,
        }),
        next: Next::Rest,
    }
}

/// A round's report whose table exchange completed taking `turn` with a gap's ask, where it names
/// a turn, else one that reached no table exchange.
fn took(turn: Option<TableTurn>) -> RepairRound {
    RepairRound {
        application: turn.map(|turn| Report {
            turn: Some(turn),
            asked: Some(Ask::Gap),
            ..Report::default()
        }),
        ..RepairRound::panicked(None)
    }
}

/// How much `event` rose between two of the calling thread's readings.
fn rose(before: &Cadence, after: &Cadence, event: Event) -> u64 {
    after.get(event) - before.get(event)
}

/// The seat ledger counts each seat apart, within its bound: a round that names no seat counts
/// only process-wide; a seat's run is its table rounds since its last regular turn, and its most
/// is kept (`Hinted, Visit, Visit, Regular, Hinted` leave 1 and 3); a round whose table exchange
/// broke off leaves the run alone, whatever turn it chose, and counts the sweep its list trade
/// reported; 300 seats leave [`MAX_SEATS`], the seat written least recently
/// gone first; and every seat's counts are the same words in the same order, each once, so the
/// text form's positions mean the same for every seat: `out`, the `Next` and `Dial` words and six
/// more (`inbound` through `run_max`), and no table turn, ask or serving loop, which only the
/// process-wide ledgers count (`Cadence::ledgers`).
///
/// NEGATIVE CONTROLS (2026-11-15), each planted in this file's subject: with the eviction
/// removed, the ledger kept 301 seats; with the run left unreset by a regular turn, the run read
/// `(4, 4)`; with the process-wide counts made only for a round that names a seat, the seatless
/// round counted no `rest` (`left: 0, right: 1`). NEGATIVE CONTROL (2026-11-16): with a
/// `regular` table-turn cell added to each seat (`SeatCounts::of`), a seat held one word more
/// than `out`, the `Next` and `Dial` words and the six.
#[test]
fn the_seat_ledger_counts_each_seat_apart_within_its_bound() {
    let before = cadence_here();
    let seats = seats_here().len();
    own_round(&round(None), Some(&took(Some(TableTurn::Visit))));
    let after = cadence_here();
    for event in [
        Event::Next(Next::Rest),
        Event::Dial(Dial::Scan),
        Event::Dial(Dial::Probe),
        Event::Turn(TableTurn::Visit),
        Event::Ask(Ask::Gap),
    ] {
        assert_eq!(rose(&before, &after, event), 1, "{event:?}");
    }
    for event in [Event::Dial(Dial::Lead), Event::Turn(TableTurn::Regular)] {
        assert_eq!(rose(&before, &after, event), 0, "{event:?}");
    }
    assert_eq!(seats_here().len(), seats, "a seatless round writes no seat");
    let [(_, next), (_, dials), (_, turns), (_, asks), _] = after.ledgers();
    for (ledger, word) in [
        (&next, "rest"),
        (&dials, "scan"),
        (&turns, "visit"),
        (&asks, "gap"),
    ] {
        assert!(ledger.iter().any(|(seen, _)| *seen == word), "{word}");
    }
    for (key, ledger) in after.ledgers() {
        assert!(
            ledger.iter().all(|(_, count)| *count > 0),
            "{key} is sparse"
        );
    }

    let seat = actor(9_000);
    for turn in [
        TableTurn::Hinted,
        TableTurn::Visit,
        TableTurn::Visit,
        TableTurn::Regular,
        TableTurn::Hinted,
    ] {
        own_round(&round(Some(seat)), Some(&took(Some(turn))));
    }
    // A sweep its list trade reported counts though the exchange then broke off.
    own_round(
        &round(Some(seat)),
        Some(&RepairRound {
            application_interrupted: Some(Report {
                sweep: Some(Sweep::Discarded),
                turn: Some(TableTurn::Regular),
                ..Report::default()
            }),
            ..took(None)
        }),
    );
    inbound(seat);
    let counts = seat_here(seat).expect("the seat is kept");
    assert_eq!((counts["run"], counts["run_max"]), (1, 3));
    assert_eq!(
        [
            counts["out"],
            counts["rest"],
            counts["scan"],
            counts["probe"],
            counts["lead"],
            counts["sweep-discarded"],
            counts["sweep-restarted"],
            counts["inbound"],
        ],
        [6, 6, 6, 6, 0, 1, 0, 1]
    );

    for n in 0..300 {
        own_round(&round(Some(actor(n))), None);
        if n == 200 {
            inbound(actor(1));
        }
    }
    assert_eq!(seats_here().len(), MAX_SEATS);
    assert!(seat_here(actor(1)).is_some(), "seat 1 was written again");
    assert!(seat_here(actor(299)).is_some());
    assert!(
        seat_here(actor(0)).is_none(),
        "the least recently written goes"
    );
    assert!(seat_here(seat).is_none());

    let seats = seats_here();
    let words: Vec<&str> = seats[0].cells.iter().map(|(word, _)| *word).collect();
    assert_eq!(
        words.len(),
        1 + Next::ALL.len() + Dial::ALL.len() + 6,
        "a seat counts no table turn, ask or serving loop: {words:?}"
    );
    let distinct: std::collections::BTreeSet<_> = words.iter().collect();
    assert_eq!(distinct.len(), words.len(), "{words:?}");
    for seat in &seats {
        assert!(
            seat.cells
                .iter()
                .map(|(word, _)| *word)
                .eq(words.iter().copied()),
            "{}",
            seat.label
        );
    }
}

/// A seat is labelled by its actor until a contact with it names its machine, then by its
/// machine; a name for a seat nothing counted writes none, and two seats of one machine keep
/// labels of their own.
#[test]
fn a_seat_is_labelled_by_its_machine_once_a_contact_names_it() {
    let (first, second, unknown) = (actor(1), actor(2), actor(3));
    inbound(first);
    inbound(second);
    let label = |actor: Actor| {
        SEATS_HERE.with(|seats| {
            let ledger = seats.borrow();
            let index = ledger
                .seats
                .keys()
                .position(|key| *key == actor)
                .expect("a kept seat");
            seats_of(&ledger)[index].label.clone()
        })
    };
    assert_eq!(label(first), "actor-0001a5a5a5a5");
    let machine = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    name(first, machine);
    assert_eq!(label(first), "112233445566");
    name(unknown, machine);
    assert_eq!(seat_here(unknown), None, "a name is no write");
    name(second, machine);
    assert_eq!(label(first), "112233445566-actor-0001a5a5a5a5");
    assert_eq!(label(second), "112233445566-actor-0002a5a5a5a5");
}
