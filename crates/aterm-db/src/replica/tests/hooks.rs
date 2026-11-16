// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a dial scheduler asks of a peer's table rounds, across real rounds: a led table
//! (`PeerRepair::lead`), an ask and an audit (`PeerRepair::ask`, `PeerRepair::audit`), and the
//! scan's regular turns (`Report::regular`, `PeerRepair::cycled`). Each is an order among tables
//! the round's own lists and inventories still decide; none grants or skips anything.

use super::{
    witness::{endpoint, held, selected, tables, write},
    *,
};
use std::{cell::Cell, thread::LocalKey};

/// `body`'s result with `mutant` set as `on` on this thread, where a round this thread initiates
/// reads it.
fn with_mutant<R>(mutant: &'static LocalKey<Cell<bool>>, on: bool, body: impl FnOnce() -> R) -> R {
    mutant.with(|flag| flag.set(on));
    let result = body();
    mutant.with(|flag| flag.set(false));
    result
}

/// What a round's report says: its table, whether it asked which tables differ, and whether the
/// table was the scan's regular turn.
type Turn = (usize, bool, bool);
/// What a scheduler asks of a peer's state before a dial.
type Hook = fn(&mut PeerRepair);

/// One round Mac 0 initiates with Mac 1.
fn round(
    [from, to]: [&Endpoint; 2],
    tables: &[(Scope, [PathBuf; 2])],
    ours: &mut PeerRepair,
    theirs: &mut PeerRepair,
) -> Turn {
    exchange_round(from, to, ours, theirs);
    let report = ours.application().unwrap();
    (
        selected(ours, tables),
        report.witnesses_asked,
        report.regular,
    )
}

/// An endpoint keeps each peer's hints and hands them to a round once its peer authenticates, in
/// place of what the round's caller held. What the scheduler asked of that round before the dial
/// is the caller's and reaches those hints: the round after a lead repairs the led table.
/// MUTANT (`REQUESTS_LEFT`): the kept hints replace the request with the rest, and the round
/// takes the scan's turn.
#[test]
fn what_is_asked_before_a_dial_reaches_the_hints_the_endpoint_kept() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 4);
        let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        assert_eq!(
            round(endpoints.each_ref(), &tables, &mut ours, &mut theirs),
            (0, true, true)
        );
        assert!(ours.lead(tables[3].0.digest()));
        ours.restart_cycle();
        assert!(ours.pending() && !ours.cycled());
        let led = with_mutant(&crate::control::REQUESTS_LEFT, mutant, || {
            round(endpoints.each_ref(), &tables, &mut ours, &mut theirs)
        });
        if mutant {
            assert_eq!(led, (1, false, true), "MUTANT: the scan's turn");
        } else {
            assert_eq!(led, (3, false, false), "the led table");
        }
        assert!(!ours.pending());
    }
}

/// A peer whose endpoint has no tables completes its rounds with no table exchange, so a lead
/// asked of it finds no round to be repaired in. Nothing is pending after such a round, or a
/// scheduler that follows pending work up would dial that peer again for ever; the lead stays in
/// the peer's hints, and a round that does exchange tables takes it.
/// MUTANT (`PENDING_UNEXCHANGED`): the hints' lead counts as pending after every such round.
#[test]
fn a_round_with_no_table_exchange_leaves_nothing_pending() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 3);
    let from = endpoint(&lab, &tables, 0);
    let bare = Endpoint::from_bootstrap(&lab.bootstrap(1))
        .unwrap()
        .unwrap();
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    assert!(ours.lead(tables[2].0.digest()));
    assert!(ours.pending());
    for _ in 0..2 {
        exchange_round(&from, &bare, &mut ours, &mut theirs);
        assert!(ours.actor().is_some() && ours.application().is_none());
        assert!(!ours.pending());
        assert!(
            with_mutant(&crate::control::PENDING_UNEXCHANGED, true, || ours
                .pending()),
            "MUTANT: the lead the hints keep is pending though no round can take it"
        );
        assert!(!ours.cycled(), "no table was listed");
    }
    let holding = endpoint(&lab, &tables, 1);
    assert_eq!(
        round([&from, &holding], &tables, &mut ours, &mut theirs),
        (2, true, false),
        "the first round that exchanges tables takes the lead"
    );
    assert!(!ours.pending());
}

/// A led table is repaired in the next round though that round asks which tables differ and the
/// peer's answer names another: the answer's table follows, and then the scan. Neither is a
/// regular turn.
/// MUTANT (`LEAD_IN_DIFFERING`): the lead is kept among the tables whose witnesses differed,
/// and the answer, which replaces them, erases it.
#[test]
fn a_lead_survives_the_witness_answer() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 5);
        write(&lab, &tables, 1, 1);
        let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        assert!(!ours.pending());
        assert!(ours.lead(tables[3].0.digest()));
        assert!(ours.pending(), "a lead is work for the next round");
        let mut rounds = Vec::new();
        let mut pending = Vec::new();
        for _ in 0..3 {
            rounds.push(with_mutant(&LEAD_IN_DIFFERING, mutant, || {
                round(endpoints.each_ref(), &tables, &mut ours, &mut theirs)
            }));
            pending.push(ours.pending());
        }
        if mutant {
            assert_eq!(
                rounds,
                [(1, true, false), (2, false, true), (3, false, true)],
                "MUTANT: the answer erased the lead, and its table waits for its turn"
            );
        } else {
            assert_eq!(
                rounds,
                [(3, true, false), (1, false, false), (2, false, true)],
                "the lead, the table the answer named, then the scan"
            );
            assert_eq!(pending, [true, false, false]);
        }
        assert_eq!(held(&lab, &tables, 1, 0), 1);
    }
}

/// A led table the peer does not list is no table of that round, which takes the scan's; the
/// lead waits one round, and once the peer lists the table (a ring table it took up from this
/// side's list, here a table its next endpoint is given) the next round repairs it. A peer that
/// still lacks it then ends the lead, so none is owed for ever. The table the peer takes up is
/// one the lists newly share, which the round leads for that alone
/// (`witness::a_table_the_lists_newly_share_is_the_next_rounds_before_the_scans_turn`): each round
/// here leaves that unled (`TAKEN_UP_UNLED`), so what takes the table is the lead that waited.
/// MUTANT (`LEAD_NEEDS_COMMON`): the lead is dropped in the round that finds it unshared.
#[test]
fn a_led_table_the_peer_lacks_is_taken_in_the_next_round() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 3);
        write(&lab, &tables, 2, 0);
        // Mac 0's endpoint, and so its hints of the peer, stays.
        let from = endpoint(&lab, &tables, 0);
        let lacking = [&from, &endpoint(&lab, &tables[..2], 1)];
        let holding = [&from, &endpoint(&lab, &tables, 1)];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        assert!(ours.lead(tables[2].0.digest()));
        let mut rounds = Vec::new();
        let mut pending = Vec::new();
        for endpoints in [lacking, holding, holding] {
            rounds.push(with_mutant(&LEAD_NEEDS_COMMON, mutant, || {
                with_mutant(&TAKEN_UP_UNLED, true, || {
                    round(endpoints, &tables, &mut ours, &mut theirs)
                })
            }));
            pending.push(ours.pending());
        }
        if mutant {
            assert_eq!(
                rounds,
                [(0, true, true), (1, false, true), (2, false, true)],
                "MUTANT: the lead went with the round that lacked it"
            );
            continue;
        }
        assert_eq!(
            rounds,
            [(0, true, true), (2, false, false), (1, false, true)],
            "the scan while the peer lacks the table, then the lead, then the scan again"
        );
        assert_eq!(pending, [true, false, false]);
        assert_eq!(held(&lab, &tables, 2, 1), 1);

        // A peer that never takes the table up: the second round that lacks it ends the lead.
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        assert!(ours.lead(tables[2].0.digest()));
        let mut pending = Vec::new();
        for _ in 0..2 {
            round(lacking, &tables, &mut ours, &mut theirs);
            pending.push(ours.pending());
        }
        assert_eq!(pending, [true, false]);
    }
}

/// With the wall clock set back an hour after a round, the next round asks nothing, and no later
/// one would until the clock passed the old reading again; after `audit` or `ask` it asks, once.
/// MUTANT (`ASK_BY_CLOCK`): the hook moves the last list's time back by the gap, which a clock
/// set back further never reaches.
#[test]
fn an_audit_asks_with_the_wall_clock_set_back() {
    let hooks: [(&str, Option<Hook>); 3] = [
        ("neither", None),
        ("audit", Some(PeerRepair::audit)),
        ("ask", Some(PeerRepair::ask)),
    ];
    let start = 1_800_000_000;
    for mutant in [false, true] {
        for (name, hook) in hooks {
            let lab = Lab::new(&[Role::Writer; 2]);
            let tables = tables(&lab, 3);
            // One endpoint each for the pair's life, so each keeps its hints of the other; a
            // later round sets its clock back.
            let at = |endpoints: [Endpoint; 2], secs: u64| {
                endpoints.map(|endpoint| endpoint.with_ring_clock(secs))
            };
            let first = at([0, 1].map(|node| endpoint(&lab, &tables, node)), start);
            let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
            let (_, asked, _) = round(first.each_ref(), &tables, &mut ours, &mut theirs);
            assert!(asked, "a first round asks");
            if let Some(hook) = hook {
                hook(&mut ours);
            }
            let back = at(first, start - 3600);
            let (_, asked, _) = with_mutant(&ASK_BY_CLOCK, mutant, || {
                round(back.each_ref(), &tables, &mut ours, &mut theirs)
            });
            assert_eq!(
                asked,
                hook.is_some() && !mutant,
                "{name} (mutant {mutant}): the round after the clock went back"
            );
            let (_, asked, _) = round(back.each_ref(), &tables, &mut ours, &mut theirs);
            assert!(!asked, "{name}: a hook asks once");
        }
    }
}

/// A round that moved a full batch saves its table as the next round's follow-up. `ask` leaves
/// that follow-up its round; `audit` gives the round to the scan's regular turn, as it must
/// where the follow-up is of a table that moves a batch in every round.
/// MUTANT (`AUDIT_FOLLOWS`): an audit asks and takes the follow-up.
#[test]
fn an_audit_takes_the_regular_turn_where_an_ask_keeps_a_follow_up() {
    let hooks: [(Hook, bool, Turn); 3] = [
        (PeerRepair::ask, false, (0, true, false)),
        (PeerRepair::audit, false, (1, true, true)),
        (PeerRepair::audit, true, (0, true, false)),
    ];
    for (hook, mutant, expected) in hooks {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 3);
        for _ in 0..BATCH + 4 {
            write(&lab, &tables, 0, 1);
        }
        // The peer answers no ask, as a build from before CR1: the follow-up alone names the
        // table, not the tables whose witnesses differ.
        let endpoints = [
            endpoint(&lab, &tables, 0).duplex(),
            endpoint(&lab, &tables, 1).duplex().with_witnesses(false),
        ];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        assert_eq!(
            round(endpoints.each_ref(), &tables, &mut ours, &mut theirs),
            (0, true, true)
        );
        assert_eq!(held(&lab, &tables, 0, 0), BATCH);
        assert!(ours.pending(), "a full batch leaves a follow-up");
        hook(&mut ours);
        assert_eq!(
            with_mutant(&AUDIT_FOLLOWS, mutant, || {
                round(endpoints.each_ref(), &tables, &mut ours, &mut theirs)
            }),
            expected,
            "mutant {mutant}"
        );
    }
}

/// How many tables the audited pair holds, as a live window's nine.
const AUDITED: usize = 9;
/// The one of them Mac 1 cannot read: its witness never matches.
const UNREADABLE: usize = 4;

/// One table's witness differs at every ask (the peer cannot read it). Each audit visits that
/// table first and then takes one regular turn, and the turns of nine audits visit all nine
/// tables: the visit to the differing table leaves the scan's place where it was. After them,
/// and not before, the scan has cycled.
/// MUTANT (`DIFFERING_MOVES_CURSOR`, the scan as CR1 first had it): each visit moves the
/// scan's place, so every audit's regular turn is the table after the unreadable one and seven
/// tables are never visited.
#[test]
fn audits_with_one_table_that_always_differs_visit_every_table() {
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, AUDITED);
        std::fs::remove_file(tables[UNREADABLE].1[1].join("table.checkpoint")).unwrap();
        let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        let mut regular = Vec::new();
        for _ in 0..AUDITED {
            assert!(!ours.cycled(), "{regular:?}");
            ours.audit();
            let mut rounds = Vec::new();
            loop {
                rounds.push(with_mutant(&DIFFERING_MOVES_CURSOR, mutant, || {
                    round(endpoints.each_ref(), &tables, &mut ours, &mut theirs)
                }));
                if rounds.last().unwrap().2 {
                    break;
                }
                assert!(
                    rounds.len() < 4,
                    "an audit ends in a regular turn: {rounds:?}"
                );
            }
            assert_eq!(
                rounds[0],
                (UNREADABLE, true, false),
                "an audit asks and visits the table whose witness differs: {rounds:?}"
            );
            regular.push(rounds.last().unwrap().0);
        }
        if mutant {
            assert_eq!(
                regular,
                [UNREADABLE + 1; AUDITED],
                "MUTANT: every audit's turn is the table after the one that differs"
            );
            continue;
        }
        let scan: Vec<_> = (UNREADABLE + 1..AUDITED).chain(0..=UNREADABLE).collect();
        assert_eq!(regular, scan, "one turn of the scan for each audit");
        assert!(ours.cycled());
        ours.restart_cycle();
        assert!(!ours.cycled());
        ours.audit();
        round(endpoints.each_ref(), &tables, &mut ours, &mut theirs);
        assert!(!ours.cycled(), "a restarted cycle counts from none");
    }
}
