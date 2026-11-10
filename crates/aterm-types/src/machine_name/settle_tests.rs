// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `settle`, `choose`, `top` and `check` against `docs/DESIGN-machine-names-2026-11-04.md`
//! §5.3, §6.1, §6.2 and §6.4.

use std::collections::BTreeMap;

use super::{
    Anchor, Blocked, Cap, Choice, Class, Entry, Fleet, Holder, IMPLIED_SINCE, Kept, Labels, NodeId,
    Number, Record, Settled, Standing, Verdict, Was, check, choose, next_stamp, renamed, settle,
    taken, top,
};

const DAY: u64 = 86_400_000;
/// 2026-06-01, well past the floor.
const T0: u64 = 1_780_272_000_000;

fn node(n: u8) -> NodeId {
    NodeId::new(&[n; 16]).expect("16 bytes")
}

fn m(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn rec(num: Option<u32>, class: Class, since: u64) -> Record {
    Record {
        num: num.map(m),
        class,
        since,
        base: None,
        tgen: 0,
        sgen: 0,
        act: since.max(T0),
        anchor: None,
        prev: None,
        waits: false,
        ephemeral: false,
        was: Vec::new(),
        top: 0,
        cap: None,
    }
}

fn auto(num: u32, since: u64) -> Entry {
    Entry::Record(rec(Some(num), Class::Auto, since))
}

fn chosen(num: u32, since: u64) -> Entry {
    Entry::Record(rec(Some(num), Class::Chosen, since))
}

fn on(anchor: u8, entry: Entry) -> Entry {
    match entry {
        Entry::Record(mut record) => {
            record.anchor = Some(Anchor([anchor; 8]));
            Entry::Record(record)
        }
        other => other,
    }
}

fn view(entries: impl IntoIterator<Item = (u8, Entry)>) -> BTreeMap<NodeId, Entry> {
    entries.into_iter().map(|(n, e)| (node(n), e)).collect()
}

fn holder(settled: &Settled, num: u32) -> Option<NodeId> {
    settled.holder(m(num))
}

fn fleet<'a>(
    view: &'a BTreeMap<NodeId, Entry>,
    settled: &'a Settled,
    top: u32,
    kept: &'a [Kept],
    labels: &'a BTreeMap<NodeId, Labels>,
) -> Fleet<'a> {
    Fleet {
        view,
        settled,
        top,
        kept,
        labels,
    }
}

#[test]
fn the_answer_depends_on_the_set_of_records_not_their_order() {
    let entries = [
        (3, auto(5, T0 + 2)),
        (1, auto(5, T0 + 1)),
        (2, chosen(6, T0)),
        (4, on(9, auto(7, T0))),
        (5, on(9, Entry::Record(rec(None, Class::Auto, T0)))),
    ];
    let forward = settle(&view(entries.clone()));
    let backward = settle(&view(entries.into_iter().rev()));
    assert_eq!(forward, backward);
    assert_eq!(holder(&forward, 5), Some(node(1)));
}

#[test]
fn every_number_has_at_most_one_holder() {
    let v = view([
        (1, auto(5, T0)),
        (2, auto(5, T0)),
        (3, auto(5, T0)),
        (4, chosen(5, T0 + 9)),
    ]);
    let s = settle(&v);
    let holding: Vec<_> = s
        .standings
        .values()
        .filter(|st| **st == Standing::Holds(m(5)))
        .collect();
    assert_eq!(holding.len(), 1);
    assert_eq!(
        holder(&s, 5),
        Some(node(4)),
        "a person's claim before automatic ones"
    );
    assert_eq!(
        s.standings.get(&node(1)),
        Some(&Standing::Displaced {
            lost: m(5),
            to: node(4)
        })
    );
}

#[test]
fn claims_rank_by_class_then_since_then_base_then_node() {
    let s = settle(&view([(1, auto(5, T0)), (2, auto(5, T0 - 1))]));
    assert_eq!(holder(&s, 5), Some(node(2)), "the earlier since");
    let s = settle(&view([(2, auto(5, T0)), (1, auto(5, T0))]));
    assert_eq!(
        holder(&s, 5),
        Some(node(1)),
        "equal since: the lower node id"
    );
    let s = settle(&view([
        (1, auto(5, T0 - DAY)),
        (2, Entry::Record(rec(Some(5), Class::Seeded, 3))),
    ]));
    assert_eq!(holder(&s, 5), Some(node(2)), "seeded before auto");
    let s = settle(&view([(1, chosen(5, T0 + DAY)), (2, auto(5, T0))]));
    assert_eq!(
        holder(&s, 5),
        Some(node(1)),
        "a person's claim beats an older automatic one"
    );
    let s = settle(&view([(1, chosen(5, T0 + DAY)), (2, chosen(5, T0))]));
    assert_eq!(
        holder(&s, 5),
        Some(node(2)),
        "the earlier person's claim keeps it"
    );
}

#[test]
fn implied_records_rank_after_every_seeded_rank_and_before_any_clock() {
    for rank in 1..=6 {
        let s = settle(&view([
            (1, Entry::Implied(m(5))),
            (2, Entry::Record(rec(Some(5), Class::Seeded, rank))),
        ]));
        assert_eq!(holder(&s, 5), Some(node(2)), "rank {rank}");
    }
    let s = settle(&view([(2, Entry::Implied(m(5))), (1, chosen(5, T0))]));
    assert_eq!(
        holder(&s, 5),
        Some(node(2)),
        "since {IMPLIED_SINCE} before any clock"
    );
    assert_eq!(
        s.standings.get(&node(1)),
        Some(&Standing::Displaced {
            lost: m(5),
            to: node(2)
        })
    );
}

#[test]
fn an_unknown_claim_leaves_its_number_unconfirmed_and_moves_nobody() {
    let s = settle(&view([
        (1, chosen(5, T0)),
        (2, Entry::Unknown(Some(m(5)))),
        (3, Entry::Unknown(None)),
    ]));
    assert_eq!(
        s.holders.get(&m(5)),
        Some(&Holder::Unconfirmed(vec![node(2)]))
    );
    assert_eq!(s.standings.get(&node(1)), Some(&Standing::Waiting(m(5))));
    assert!(
        !s.standings.contains_key(&node(2)),
        "an unknown claimant stands nowhere"
    );
    assert!(!s.standings.contains_key(&node(3)));
    // Negative control: read as an implied record, the stripped entry would hold m5 itself.
    let s = settle(&view([(1, chosen(5, T0)), (2, Entry::Implied(m(5)))]));
    assert_eq!(holder(&s, 5), Some(node(2)));
}

#[test]
fn one_machine_has_one_primary_and_its_other_nodes_are_second() {
    let mut second = rec(Some(9), Class::Auto, T0 + DAY);
    second.act = T0 + DAY;
    let s = settle(&view([
        (1, on(7, Entry::Record(rec(Some(8), Class::Seeded, 2)))),
        (2, on(7, Entry::Record(second))),
    ]));
    assert_eq!(s.standings.get(&node(1)), Some(&Standing::Holds(m(8))));
    assert_eq!(
        s.standings.get(&node(2)),
        Some(&Standing::Second { of: node(1) })
    );
    assert_eq!(holder(&s, 9), None, "a second node claims nothing");
    assert!(s.same_machine(node(1), node(2)));
}

#[test]
fn a_later_account_never_moves_the_machine_off_its_name() {
    // The second account numbered later, automatically: the machine keeps m8 whatever acts.
    let mut later = rec(Some(9), Class::Auto, T0 + DAY);
    later.act = T0 + 2 * DAY;
    let mut first = rec(Some(8), Class::Auto, T0);
    first.act = T0;
    let s = settle(&view([
        (2, on(7, Entry::Record(first))),
        (1, on(7, Entry::Record(later))),
    ]));
    assert_eq!(s.primary_of(node(1)), Some(node(2)));
    // A re-sign moves no field settle reads, so the primary stays.
    assert_eq!(s.standings.get(&node(2)), Some(&Standing::Holds(m(8))));
}

#[test]
fn two_unnumbered_nodes_of_one_machine_need_one_number() {
    let mut older = rec(None, Class::Auto, T0);
    older.act = T0;
    let mut newer = rec(None, Class::Auto, T0);
    newer.act = T0 + 5;
    let v = view([
        (1, on(7, Entry::Record(older))),
        (2, on(7, Entry::Record(newer))),
    ]);
    let s = settle(&v);
    let needing: Vec<_> = s
        .standings
        .iter()
        .filter(|(_, st)| **st == Standing::Unnumbered)
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(needing, vec![node(2)], "the newest act numbers the group");
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 0, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Keep);
    assert_eq!(choose(&f, node(2), None, &[]), Choice::Pick(m(1)));
}

#[test]
fn a_succession_outranks_the_old_node_until_it_takes_back() {
    let old = rec(Some(8), Class::Chosen, T0);
    let mut successor = rec(Some(8), Class::Chosen, T0);
    successor.base = Some(node(1));
    successor.tgen = 1;
    successor.sgen = 1;
    successor.act = T0 + 30 * DAY;
    let v = view([
        (1, on(7, Entry::Record(old.clone()))),
        (2, on(7, Entry::Record(successor.clone()))),
    ]);
    let s = settle(&v);
    assert_eq!(s.standings.get(&node(2)), Some(&Standing::Holds(m(8))));
    assert_eq!(
        s.standings.get(&node(1)),
        Some(&Standing::Second { of: node(2) })
    );
    // The successor moves on to m9: its sgen still outranks the old node's claim on m8.
    let mut moved = successor.clone();
    moved.num = Some(m(9));
    moved.class = Class::Auto;
    moved.since = T0 + 31 * DAY;
    moved.base = None;
    moved.tgen = 0;
    let s = settle(&view([
        (1, on(7, Entry::Record(old.clone()))),
        (2, on(7, Entry::Record(moved))),
    ]));
    assert_eq!(s.primary_of(node(1)), Some(node(2)));
    assert_eq!(holder(&s, 8), None, "the superseded node holds nothing");
    // The old node takes back at the successor's sgen + 1 and the next tgen.
    let mut back = old;
    back.base = Some(node(1));
    back.tgen = 2;
    back.sgen = 2;
    let s = settle(&view([
        (1, on(7, Entry::Record(back))),
        (2, on(7, Entry::Record(successor))),
    ]));
    assert_eq!(s.standings.get(&node(1)), Some(&Standing::Holds(m(8))));
}

#[test]
fn a_take_stands_where_the_taken_claim_stood() {
    // Puffin (1) holds m40 by a seeded claim at rank 2, ahead of an older build's implied claim
    // (3) at since 7; Heron (2) takes it.
    let holder_rec = rec(Some(40), Class::Seeded, 2);
    let mut take = rec(Some(40), Class::Chosen, 2);
    take.base = Some(node(1));
    take.tgen = 1;
    let s = settle(&view([
        (1, Entry::Record(holder_rec.clone())),
        (2, Entry::Record(take)),
        (3, Entry::Implied(m(40))),
    ]));
    assert_eq!(
        holder(&s, 40),
        Some(node(2)),
        "the taker outranks the implied runner-up"
    );
    assert_eq!(
        s.standings.get(&node(1)),
        Some(&Standing::Displaced {
            lost: m(40),
            to: node(2)
        })
    );
    // Negative control: a take with a fresh since loses to the implied claim at since 7.
    let mut fresh = rec(Some(40), Class::Chosen, T0);
    fresh.base = Some(node(1));
    fresh.tgen = 1;
    let s = settle(&view([
        (1, Entry::Record(holder_rec)),
        (2, Entry::Record(fresh)),
        (3, Entry::Implied(m(40))),
    ]));
    assert_eq!(holder(&s, 40), Some(node(3)));
}

#[test]
fn two_takes_of_one_claim_go_to_the_lower_taker_whichever_settle_reads_first() {
    // Node 3 is anchored and node 5 is not, so settle pushes 5's claim before 3's: equal claims
    // go by node id, not by the order settle meets them.
    let mut take = rec(Some(40), Class::Chosen, T0);
    take.base = Some(node(1));
    take.tgen = 1;
    let s = settle(&view([
        (1, chosen(40, T0)),
        (3, on(9, Entry::Record(take.clone()))),
        (5, Entry::Record(take)),
    ]));
    assert_eq!(holder(&s, 40), Some(node(3)));
    assert_eq!(
        s.standings.get(&node(5)),
        Some(&Standing::Displaced {
            lost: m(40),
            to: node(3)
        })
    );
}

#[test]
fn a_chain_origin_returning_after_its_middle_link_moved_is_dropped_by_base() {
    // b (1) ← c (2) ← d (3); c moved to m41; b registers again with its original claim.
    let mut c = rec(Some(41), Class::Auto, T0 + 5);
    c.act = T0 + 5;
    let mut d = rec(Some(40), Class::Chosen, T0);
    d.base = Some(node(1));
    d.tgen = 2;
    let s = settle(&view([
        (1, chosen(40, T0)),
        (2, Entry::Record(c)),
        (3, Entry::Record(d)),
    ]));
    assert_eq!(holder(&s, 40), Some(node(3)));
    assert_eq!(holder(&s, 41), Some(node(2)));
}

#[test]
fn m0_is_never_held() {
    let s = settle(&view([(1, Entry::Record(rec(None, Class::Auto, T0)))]));
    assert!(s.holders.is_empty());
    assert_eq!(s.standings.get(&node(1)), Some(&Standing::Unnumbered));
    assert_eq!(Number::new(0), None);
}

#[test]
fn stamps_pass_every_stamp_seen_that_is_not_past_the_clock() {
    assert_eq!(next_stamp(T0, []), T0);
    assert_eq!(next_stamp(T0, [T0 - 5, T0]), T0 + 1);
    assert_eq!(
        next_stamp(T0, [T0 + DAY]),
        T0,
        "a fast clock's stamp is not copied"
    );
    assert_eq!(next_stamp(T0, [T0 - 1]), T0);
}

#[test]
fn nodes_sharing_a_view_pick_distinct_numbers_above_top() {
    let v = view((1..=5).map(|n| (n, Entry::Record(rec(None, Class::Auto, T0)))));
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 178, &[], &labels);
    let picks: Vec<_> = (1..=5)
        .map(|n| match choose(&f, node(n), None, &[]) {
            Choice::Pick(num) => num.get(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(picks, vec![179, 180, 181, 182, 183]);
}

#[test]
fn a_want_is_taken_when_free_and_left_off_everyone_elses_list() {
    let mut kept = rec(None, Class::Auto, T0);
    kept.prev = Some(m(180));
    let v = view([
        (1, Entry::Record(kept)),
        (2, Entry::Record(rec(None, Class::Auto, T0))),
        (3, Entry::Record(rec(None, Class::Auto, T0))),
    ]);
    let s = settle(&v);
    let labels = BTreeMap::from([(
        node(3),
        Labels {
            admission: None,
            roster: Some(m(179)),
        },
    )]);
    let f = fleet(&v, &s, 178, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Want(m(180)));
    assert_eq!(choose(&f, node(3), None, &[]), Choice::Want(m(179)));
    assert_eq!(
        choose(&f, node(2), None, &[]),
        Choice::Pick(m(181)),
        "both wants are off the list"
    );
}

#[test]
fn a_roster_label_is_a_want_only_above_top() {
    let v = view([(1, Entry::Record(rec(None, Class::Auto, T0)))]);
    let s = settle(&v);
    let labels = BTreeMap::from([(
        node(1),
        Labels {
            admission: None,
            roster: Some(m(12)),
        },
    )]);
    let f = fleet(&v, &s, 178, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Pick(m(179)));
    let f = fleet(&v, &s, 11, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Want(m(12)));
}

#[test]
fn no_automatic_claim_lands_on_a_number_that_is_not_free() {
    let mut other = rec(Some(3), Class::Auto, T0);
    other.was = vec![Was {
        num: m(4),
        retired: false,
        lost: false,
    }];
    let mut gone = rec(Some(5), Class::Auto, T0);
    gone.anchor = Some(Anchor([2; 8]));
    let v = view([
        (1, Entry::Record(rec(None, Class::Auto, T0))),
        (2, Entry::Record(other)),
        (3, Entry::Unknown(Some(m(6)))),
    ]);
    let s = settle(&v);
    let kept = [
        Kept::Tombstone {
            node: node(9),
            last: gone,
        },
        Kept::Label {
            num: m(7),
            node: Some(node(2)),
        },
        Kept::Label {
            num: m(8),
            node: Some(node(2)),
        },
        Kept::Label {
            num: m(9),
            node: Some(node(2)),
        },
        Kept::Row {
            num: m(10),
            node: None,
            planned: true,
        },
        Kept::OlderHost(m(11)),
    ];
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 2, &kept, &labels);
    for num in 3..=11 {
        assert!(!f.is_free(node(1), m(num), false) || num == 11, "m{num}");
    }
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Pick(m(12)));
    // Negative control: with nothing kept, the first number above top is free.
    let f = fleet(&v, &s, 2, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Pick(m(5)));
}

#[test]
fn a_machines_own_tombstone_and_planned_row_are_its_wants() {
    let mut mine = rec(None, Class::Auto, T0);
    mine.anchor = Some(Anchor([7; 8]));
    mine.prev = Some(m(8));
    let mut last = rec(Some(8), Class::Chosen, T0 - DAY);
    last.anchor = Some(Anchor([7; 8]));
    let v = view([(1, Entry::Record(mine))]);
    let s = settle(&v);
    let kept = [Kept::Tombstone {
        node: node(4),
        last,
    }];
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 20, &kept, &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Want(m(8)));
    let mut stranger = rec(None, Class::Auto, T0);
    stranger.prev = Some(m(8));
    let v = view([(1, Entry::Record(stranger))]);
    let s = settle(&v);
    let f = fleet(&v, &s, 20, &kept, &labels);
    assert_eq!(
        choose(&f, node(1), None, &[]),
        Choice::Pick(m(21)),
        "another's tombstone"
    );
    let v = view([(1, Entry::Record(rec(None, Class::Auto, T0)))]);
    let s = settle(&v);
    let row = [Kept::Row {
        num: m(30),
        node: None,
        planned: true,
    }];
    let labels = BTreeMap::from([(
        node(1),
        Labels {
            admission: Some(m(30)),
            roster: None,
        },
    )]);
    let f = fleet(&v, &s, 40, &row, &labels);
    assert_eq!(
        choose(&f, node(1), None, &[]),
        Choice::Want(m(30)),
        "binds the planned row"
    );
}

#[test]
fn every_node_of_a_view_numbers_one_list_whatever_is_its_own() {
    // Node 1 lost m179 (and so lists it in `was`); node 3's row keeps m181 for it. Each skips
    // both, and so does every other node, so the residues stay distinct.
    let mut loser = rec(None, Class::Auto, T0);
    loser.was = vec![Was {
        num: m(179),
        retired: false,
        lost: true,
    }];
    let v = view([
        (1, Entry::Record(loser)),
        (2, Entry::Record(rec(None, Class::Auto, T0))),
        (3, Entry::Record(rec(None, Class::Auto, T0))),
    ]);
    let s = settle(&v);
    let kept = [Kept::Row {
        num: m(181),
        node: Some(node(3)),
        planned: false,
    }];
    let labels = BTreeMap::new();
    let t = top(&v, &kept, [], 178);
    assert_eq!(t, 179, "a was entry raises top");
    let picks: Vec<_> = [1, 2, 3]
        .map(|n| choose(&fleet(&v, &s, t, &kept, &labels), node(n), None, &[]))
        .to_vec();
    assert_eq!(
        picks,
        vec![
            Choice::Pick(m(180)),
            Choice::Pick(m(182)),
            Choice::Pick(m(183))
        ]
    );
    // A stale top below the lost number: still never picked, still one list.
    let picks: Vec<_> = [1, 2, 3]
        .map(|n| choose(&fleet(&v, &s, 178, &kept, &labels), node(n), None, &[]))
        .to_vec();
    assert_eq!(
        picks,
        vec![
            Choice::Pick(m(180)),
            Choice::Pick(m(182)),
            Choice::Pick(m(183))
        ]
    );
}

#[test]
fn a_blocked_ephemeral_or_deferring_node_picks_nothing() {
    let mut vm = rec(None, Class::Auto, T0);
    vm.ephemeral = true;
    let v = view([
        (1, Entry::Record(rec(None, Class::Auto, T0))),
        (2, Entry::Record(vm)),
        (3, auto(4, T0)),
    ]);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 4, &[], &labels);
    assert_eq!(
        choose(&f, node(1), Some(Blocked::NoFleetSeed), &[]),
        Choice::Blocked(Blocked::NoFleetSeed)
    );
    assert_eq!(
        choose(&f, node(2), None, &[]),
        Choice::Blocked(Blocked::Ephemeral)
    );
    assert_eq!(
        choose(&f, node(1), None, &[node(9), node(8), node(9)]),
        Choice::Defer(vec![node(8), node(9)])
    );
    assert_eq!(
        choose(&f, node(3), None, &[]),
        Choice::Keep,
        "a holder needs nothing"
    );
    assert_eq!(
        choose(&f, node(1), None, &[]),
        Choice::Pick(m(5)),
        "the ephemeral node takes no residue"
    );
}

#[test]
fn past_the_last_number_the_lowest_unseen_one_is_claimed() {
    let v = view([
        (1, Entry::Record(rec(None, Class::Auto, T0))),
        (2, auto(1, T0)),
        (3, auto(2, T0)),
    ]);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, Number::MAX, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Overflow(m(3)));
}

#[test]
fn top_is_the_highest_signed_number_and_falls_only_under_a_cap() {
    let mut typo = rec(Some(12), Class::Chosen, T0);
    typo.top = 9000;
    typo.was = vec![Was {
        num: m(9000),
        retired: true,
        lost: false,
    }];
    let v = view([(1, Entry::Record(typo.clone())), (2, Entry::Implied(m(40)))]);
    assert_eq!(top(&v, &[], [], 0), 9000);
    typo.cap = Some(Cap {
        value: 50,
        since: T0,
    });
    let v = view([(1, Entry::Record(typo)), (2, Entry::Implied(m(40)))]);
    assert_eq!(
        top(&v, &[], [], 8000),
        50,
        "the cap bounds tops and retired entries"
    );
    assert_eq!(top(&v, &[], [m(60)], 0), 60, "never a label");
}

#[test]
fn checks_name_the_holder_and_what_a_rename_would_do() {
    let me = rec(Some(8), Class::Auto, T0);
    let v = view([
        (1, Entry::Record(me)),
        (2, auto(20, T0)),
        (3, chosen(21, T0)),
        (4, Entry::Record(rec(Some(22), Class::Seeded, 2))),
        (5, Entry::Implied(m(23))),
        (6, Entry::Unknown(Some(m(24)))),
        (7, {
            let mut earlier = rec(Some(30), Class::Auto, T0);
            earlier.was = vec![Was {
                num: m(25),
                retired: false,
                lost: false,
            }];
            Entry::Record(earlier)
        }),
    ]);
    let s = settle(&v);
    let kept = [
        Kept::Tombstone {
            node: node(9),
            last: rec(Some(26), Class::Chosen, T0 - DAY),
        },
        Kept::Row {
            num: m(28),
            node: None,
            planned: true,
        },
    ];
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 30, &kept, &labels);
    let at = |text: &str| check(text, &f, node(1));
    assert_eq!(at("8"), Verdict::Same);
    assert_eq!(at("m40"), Verdict::Free);
    assert_eq!(at("m20"), Verdict::TakesAuto { from: node(2) });
    assert_eq!(at("m21"), Verdict::HeldByPerson { by: node(3) });
    assert_eq!(at("m22"), Verdict::HeldSeeded { by: node(4) });
    assert_eq!(at("m23"), Verdict::HeldByOlderBuild { by: node(5) });
    assert_eq!(at("m24"), Verdict::Unconfirmed { by: vec![node(6)] });
    assert_eq!(at("m25"), Verdict::EarlierName { of: node(7) });
    assert_eq!(at("m26"), Verdict::Retired { of: node(9) });
    assert_eq!(at("m28"), Verdict::Planned);
    assert!(matches!(at("m08"), Verdict::Grammar(_)));
    assert!(matches!(at("m0"), Verdict::Grammar(_)));
}

/// A number another Mac's vault label names refuses both a rename and a take, whether the label
/// is a bound node's or kept for a Mac no record names; this machine's own label does
/// not, and a signed holder's class still speaks first.
#[test]
fn another_macs_vault_label_refuses_a_rename() {
    let v = view([
        (1, Entry::Record(rec(Some(8), Class::Auto, T0))),
        (2, chosen(21, T0)),
    ]);
    let s = settle(&v);
    let kept = [
        Kept::Label {
            num: m(50),
            node: None,
        },
        Kept::Label {
            num: m(51),
            node: Some(node(1)),
        },
        Kept::Label {
            num: m(21),
            node: None,
        },
    ];
    let labels = BTreeMap::from([
        (
            node(3),
            Labels {
                admission: Some(m(60)),
                roster: Some(m(61)),
            },
        ),
        (
            node(1),
            Labels {
                admission: Some(m(62)),
                roster: None,
            },
        ),
    ]);
    let f = fleet(&v, &s, 30, &kept, &labels);
    let at = |text: &str| check(text, &f, node(1));
    assert_eq!(at("m50"), Verdict::Labelled { by: None });
    assert_eq!(at("m60"), Verdict::Labelled { by: Some(node(3)) });
    assert_eq!(at("m61"), Verdict::Labelled { by: Some(node(3)) });
    assert_eq!(at("m51"), Verdict::Free, "this machine's own label");
    assert_eq!(at("m62"), Verdict::Free, "this machine's own admission");
    assert_eq!(at("m21"), Verdict::HeldByPerson { by: node(2) });
    let labelled = Verdict::Labelled { by: None };
    assert!(!labelled.renames() && !labelled.takes());
    // Negative control: without the labels the same numbers read free.
    let unlabelled = BTreeMap::new();
    let bare = fleet(&v, &s, 30, &[], &unlabelled);
    for free in ["m50", "m60", "m61"] {
        assert_eq!(check(free, &bare, node(1)), Verdict::Free, "{free}");
    }
}

#[test]
fn a_second_node_and_a_stronger_sibling_refuse_a_rename() {
    let mut first = rec(Some(8), Class::Seeded, 2);
    first.anchor = Some(Anchor([7; 8]));
    let mut later = rec(Some(9), Class::Seeded, 3);
    later.anchor = Some(Anchor([7; 8]));
    let v = view([(1, Entry::Record(first.clone())), (2, Entry::Record(later))]);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 9, &[], &labels);
    assert_eq!(
        check("m40", &f, node(2)),
        Verdict::SecondNode { of: node(1) }
    );
    assert_eq!(
        check("m40", &f, node(1)),
        Verdict::OtherAccountNamed { node: node(2) },
        "a fresh chosen claim ranks behind the sibling's seeded one"
    );
    // A migration that waits counts with its kept NAME.
    let mut waits = rec(None, Class::Seeded, 3);
    waits.anchor = Some(Anchor([7; 8]));
    waits.waits = true;
    waits.prev = Some(m(5));
    let v = view([(1, Entry::Record(first.clone())), (2, Entry::Record(waits))]);
    let s = settle(&v);
    let f = fleet(&v, &s, 9, &[], &labels);
    assert_eq!(
        check("m40", &f, node(1)),
        Verdict::OtherAccountNamed { node: node(2) }
    );
    // Negative control: a sibling's automatic claim ranks behind any person's.
    let mut weak = rec(Some(9), Class::Auto, T0);
    weak.anchor = Some(Anchor([7; 8]));
    let v = view([(1, Entry::Record(first)), (2, Entry::Record(weak))]);
    let s = settle(&v);
    let f = fleet(&v, &s, 9, &[], &labels);
    assert_eq!(check("m40", &f, node(1)), Verdict::Free);
}

#[test]
fn a_retired_number_is_the_last_name_a_tombstone_kept() {
    let mut last = rec(Some(30), Class::Seeded, 1);
    last.was = vec![Was {
        num: m(26),
        retired: false,
        lost: false,
    }];
    let tomb = Kept::Tombstone {
        node: node(9),
        last,
    };
    let v = view([(1, Entry::Record(rec(Some(8), Class::Auto, T0)))]);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let only = [tomb];
    let f = fleet(&v, &s, 30, &only, &labels);
    assert_eq!(check("m30", &f, node(1)), Verdict::Retired { of: node(9) });
    assert_eq!(
        check("m26", &f, node(1)),
        Verdict::EarlierName { of: node(9) }
    );
}

#[test]
fn a_superseded_nodes_later_take_never_outranks_its_successor() {
    // P (1) took m8 over Z on a view that lacked its successor S (2); S's sgen decides first.
    let mut p = rec(Some(8), Class::Chosen, T0);
    p.anchor = Some(Anchor([7; 8]));
    p.base = Some(node(1));
    p.tgen = 3;
    let mut succ = rec(Some(8), Class::Chosen, T0);
    succ.anchor = Some(Anchor([7; 8]));
    succ.base = Some(node(1));
    succ.tgen = 1;
    succ.sgen = 1;
    let mut z = rec(Some(8), Class::Chosen, T0);
    z.base = Some(node(1));
    z.tgen = 2;
    let s = settle(&view([
        (1, Entry::Record(p)),
        (2, Entry::Record(succ)),
        (3, Entry::Record(z)),
    ]));
    assert_eq!(s.primary_of(node(1)), Some(node(2)));
    assert_eq!(holder(&s, 8), Some(node(3)), "Z's take of S's claim stands");
}

#[test]
fn no_two_machines_take_one_want() {
    let mut kept = rec(None, Class::Auto, T0);
    kept.prev = Some(m(30));
    let v = view([
        (1, Entry::Record(kept)),
        (2, Entry::Record(rec(None, Class::Auto, T0))),
    ]);
    let s = settle(&v);
    let labels = BTreeMap::from([(
        node(2),
        Labels {
            admission: Some(m(30)),
            roster: None,
        },
    )]);
    let f = fleet(&v, &s, 20, &[], &labels);
    let picks = [1, 2].map(|n| choose(&f, node(n), None, &[]));
    assert_eq!(picks, [Choice::Pick(m(21)), Choice::Pick(m(22))]);
}

#[test]
fn a_displaced_renamer_falls_back_to_its_previous_name() {
    let mut me = rec(Some(40), Class::Chosen, T0 + DAY);
    me.prev = Some(m(8));
    me.was = vec![Was {
        num: m(8),
        retired: false,
        lost: false,
    }];
    let v = view([(1, Entry::Record(me)), (2, chosen(40, T0))]);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 40, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Want(m(8)));
}

#[test]
fn a_top_past_the_last_number_still_leaves_each_node_its_own_place() {
    let mut far = rec(Some(3), Class::Auto, T0);
    far.top = 1_000_000;
    let v = view([
        (1, Entry::Record(rec(None, Class::Auto, T0))),
        (2, Entry::Record(rec(None, Class::Auto, T0))),
        (3, Entry::Record(far)),
    ]);
    assert_eq!(top(&v, &[], [], 2_000_000), Number::MAX);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, top(&v, &[], [], 0), &[], &labels);
    let picks = [1, 2].map(|n| choose(&f, node(n), None, &[]));
    assert_eq!(picks, [Choice::Overflow(m(1)), Choice::Overflow(m(2))]);
}

#[test]
fn a_migration_that_waits_picks_nothing_and_takes_no_place() {
    let mut waits = rec(None, Class::Seeded, 2);
    waits.waits = true;
    waits.prev = Some(m(8));
    let v = view([
        (1, Entry::Record(waits)),
        (2, Entry::Record(rec(None, Class::Auto, T0))),
    ]);
    let s = settle(&v);
    let labels = BTreeMap::new();
    let f = fleet(&v, &s, 10, &[], &labels);
    assert_eq!(choose(&f, node(1), None, &[]), Choice::Keep);
    assert_eq!(choose(&f, node(2), None, &[]), Choice::Pick(m(11)));
}

#[test]
fn every_renaming_verdict_leaves_this_machine_holding_the_number() {
    let mut me = rec(Some(8), Class::Auto, T0);
    me.was = vec![Was {
        num: m(3),
        retired: false,
        lost: true,
    }];
    let base = view([
        (1, Entry::Record(me.clone())),
        (2, auto(20, T0)),
        (3, chosen(21, T0)),
        (4, Entry::Record(rec(Some(22), Class::Seeded, 2))),
        (5, Entry::Implied(m(41))),
    ]);
    let kept = [Kept::Tombstone {
        node: node(9),
        last: rec(Some(26), Class::Seeded, 1),
    }];
    let labels = BTreeMap::new();
    for (text, take) in [
        ("m40", false),
        ("m20", false),
        ("m26", false),
        ("m21", true),
        ("m22", true),
    ] {
        let s = settle(&base);
        let f = fleet(&base, &s, 30, &kept, &labels);
        let verdict = check(text, &f, node(1));
        assert!(
            verdict.renames() || (take && verdict.takes()),
            "{text}: {verdict:?}"
        );
        let num = Number::parse(text).expect("a number");
        let written = renamed(&me, num, taken(&f, &verdict), T0 + DAY);
        assert_eq!(written.class, Class::Chosen);
        assert_eq!(written.prev, Some(m(8)));
        assert_eq!(written.was.first().map(|w| w.num), Some(m(8)));
        let mut after = base.clone();
        after.insert(node(1), Entry::Record(written));
        if matches!(verdict, Verdict::Retired { .. }) {
            // The tombstone's machine returns: its claim drops to the take.
            after.insert(node(9), Entry::Record(rec(Some(26), Class::Seeded, 1)));
        }
        let s = settle(&after);
        assert_eq!(holder(&s, num.get()), Some(node(1)), "{text}");
    }
    // Negative control: a plain rename onto a person's claim leaves it with that person.
    let plain = renamed(&me, m(21), None, T0 + DAY);
    let mut after = base.clone();
    after.insert(node(1), Entry::Record(plain));
    assert_eq!(holder(&settle(&after), 21), Some(node(3)));
}
