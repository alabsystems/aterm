// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CR1 across real rounds: a first round with a peer repairs the tables whose witnesses differ
//! before the regular turn (`crate::replica::witness`), a peer from before CR1 keeps the scan,
//! and a deferred table is taken again in the next round.

use super::*;

/// `count` tables two writers hold, each empty on both, sorted by digest.
fn tables(lab: &Lab, count: usize) -> Vec<(Scope, [PathBuf; 2])> {
    let mut tables: Vec<_> = (0..count)
        .map(|table| {
            let paths: [PathBuf; 2] = std::array::from_fn(|node| {
                lab._home.path().join(format!("witness-{table}-{node}"))
            });
            let bootstrap = lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let scope = Store::create(&session, &paths[0]).unwrap().scope();
            drop(session);
            let bootstrap = lab.bootstrap(1);
            let session = Session::open(&bootstrap).unwrap();
            Store::join(&session, &paths[1], scope).unwrap();
            (scope, paths)
        })
        .collect();
    tables.sort_by_key(|(scope, _)| scope.digest());
    tables
}

fn endpoint(lab: &Lab, tables: &[(Scope, [PathBuf; 2])], node: usize) -> Endpoint {
    Endpoint::from_bootstrap(&lab.bootstrap(node))
        .unwrap()
        .unwrap()
        .with_tables(
            lab.paths[node].clone(),
            tables
                .iter()
                .map(|(scope, paths)| (*scope, paths[node].clone()))
                .collect(),
        )
        .unwrap()
}

/// Mac `node` writes one record to `table`.
fn write(lab: &Lab, tables: &[(Scope, [PathBuf; 2])], table: usize, node: usize) {
    let bootstrap = lab.bootstrap(node);
    let session = Session::open(&bootstrap).unwrap();
    let (scope, paths) = &tables[table];
    Store::open(&session, &paths[node], *scope)
        .unwrap()
        .commit_catalog(&session, b"written while the peer was away")
        .unwrap();
}

/// The records Mac `node` holds of `table`.
fn held(lab: &Lab, tables: &[(Scope, [PathBuf; 2])], table: usize, node: usize) -> usize {
    let bootstrap = lab.bootstrap(node);
    let session = Session::open(&bootstrap).unwrap();
    let (scope, paths) = &tables[table];
    Store::open(&session, &paths[node], *scope)
        .unwrap()
        .inventory(&session)
        .unwrap()
        .count()
}

/// Hints read receipts without opening a store. A full observer is the independent healthy
/// inventory oracle; putting that observer back in the witness path breaks the open count.
#[test]
fn ordering_witnesses_open_no_table_store() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 3);
    write(&lab, &tables, 1, 0);
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let salt = [0x49; 16];
    let expected: Vec<_> = tables
        .iter()
        .map(|(scope, paths)| {
            let store = Store::observe(&session, &paths[0], *scope).unwrap();
            let known = store.inventory_unchecked();
            crate::replica::witness::witness(
                &salt,
                &scope.digest(),
                Some(InventorySummary::of_sorted(known.len(), known).fingerprint),
            )
        })
        .collect();
    drop(session);
    let replica = Replica::new(
        lab.paths[0].clone(),
        tables
            .iter()
            .map(|(scope, paths)| (*scope, paths[0].clone()))
            .collect(),
    )
    .unwrap();
    let peer_bootstrap = lab.bootstrap(1);
    let peer = Session::observe(&peer_bootstrap).unwrap().actor();
    let round = replica.round(Some(peer)).unwrap();
    let common = tables.iter().map(|(scope, _)| scope.digest()).collect();
    let before = crate::table::store::test_opens_and_checks();
    let actual = round.table_witnesses(peer, &common, &salt, tables.len());
    assert_eq!(actual, expected);
    assert!(
        actual
            .iter()
            .all(|hint| *hint != crate::replica::witness::UNKNOWN)
    );
    assert_eq!(crate::table::store::test_opens_and_checks(), before);
}

/// Compare a recently written local table's hint read with its previous full observer read.
/// This does not measure a network round or its subscription admission. Allocation counting
/// runs separately from instruction sampling, and each sample reverses the pair's order.
#[test]
#[ignore = "manual paired receipt hint allocation and instruction measurement"]
fn measure_receipt_hint_work() {
    measure_receipt_hint_work_after(false, false);
}

/// Let the production journal and vault cache window elapse, warm both arms, then measure the
/// same local reads as the recent-write probe. Both use the unchanged production cache rules.
#[test]
#[ignore = "manual paired settled receipt hint allocation and instruction measurement"]
fn measure_settled_receipt_hint_work() {
    measure_receipt_hint_work_after(true, false);
}

/// Keep the writing open measured too: a retained-wire optimization must account for opens
/// that take a fresh journal read and return owned ciphertext rather than a retained observation.
#[test]
#[ignore = "manual paired settled writing open allocation and instruction measurement"]
fn measure_settled_writer_hint_work() {
    measure_receipt_hint_work_after(true, true);
}

fn measure_receipt_hint_work_after(settled: bool, writer_reference: bool) {
    use aterm_bench::measurement_clock::{counters, declare_interactive};
    use std::hint::black_box;
    const CALLS: usize = 8;
    let _ = declare_interactive();
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    for records in [1, 128, 1024] {
        let session = Session::open(&bootstrap).unwrap();
        let path = lab._home.path().join(format!("hint-{records}"));
        let mut store = Store::create(&session, &path).unwrap();
        let scope = store.scope();
        for _ in 0..records {
            store.commit_catalog(&session, &[0x57; 1024]).unwrap();
        }
        assert_eq!(store.inventory(&session).unwrap().len(), records);
        drop(store);
        drop(session);
        let session = Session::observe(&bootstrap).unwrap();
        let probe = |candidate| {
            if candidate {
                let ids = Store::repair_ids_region(
                    &session,
                    &path,
                    scope,
                    crate::table::store::Region::Primary,
                )
                .unwrap()
                .unwrap();
                InventorySummary::of(&ids).fingerprint
            } else {
                let store = if writer_reference {
                    Store::open(&session, &path, scope).unwrap()
                } else {
                    Store::observe_if_exists(&session, &path, scope)
                        .unwrap()
                        .unwrap()
                };
                let ids = store.inventory_unchecked();
                InventorySummary::of_sorted(ids.len(), ids).fingerprint
            }
        };
        assert_eq!(probe(false), probe(true));
        if settled {
            std::thread::sleep(
                aterm_secrets::client::test_settled_after() + std::time::Duration::from_millis(100),
            );
            for _ in 0..8 {
                assert_eq!(probe(false), probe(true));
            }
        }
        for sample in 0..5 {
            for candidate in if sample % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let run = || {
                    for _ in 0..CALLS {
                        black_box(probe(candidate));
                    }
                };
                let (_, allocations) = aterm_heap_count::allocations_in(run);
                let before = counters();
                run();
                let after = counters();
                let instructions = before
                    .zip(after)
                    .map(|(before, after)| after.instructions.saturating_sub(before.instructions));
                eprintln!(
                    "RECEIPT-HINT records={records} candidate={candidate} sample={sample} calls={CALLS} allocations={allocations} instructions={instructions:?} settled={settled} writer_reference={writer_reference}"
                );
            }
        }
    }
}

/// The table the initiator's last round selected, by its place in `tables`.
fn selected(repair: &PeerRepair, tables: &[(Scope, [PathBuf; 2])]) -> usize {
    let table = repair.application().unwrap().table;
    tables
        .iter()
        .position(|(scope, _)| Some(scope.digest()) == table)
        .expect("a provisioned table was selected")
}

/// A Mac back after its peer wrote to two tables, neither first in the scan's order: its first
/// round asks, repairs those two before any other, and the scan then goes on from the last.
/// MUTANT (`UNWITNESSED`, the order from before CR1): the moved tables wait for their turns.
#[test]
fn a_first_round_repairs_the_tables_whose_witnesses_differ_before_the_regular_turn() {
    for unwitnessed in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 5);
        for table in [1, 3] {
            write(&lab, &tables, table, 1);
        }
        let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        let mut order = Vec::new();
        for _ in 0..4 {
            UNWITNESSED.with(|flag| flag.set(unwitnessed));
            exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
            UNWITNESSED.with(|flag| flag.set(false));
            order.push(selected(&ours, &tables));
        }
        if unwitnessed {
            assert_eq!(order, [0, 1, 2, 3], "MUTANT: the scan's order alone");
        } else {
            assert_eq!(
                order,
                [1, 3, 4, 0],
                "the tables that differ first, then the scan from the last of them"
            );
        }
        for table in 0..5 {
            let written = usize::from(table == 1 || table == 3);
            assert_eq!(held(&lab, &tables, table, 0), written);
            assert_eq!(held(&lab, &tables, table, 1), written);
        }
    }
}

/// Within [`WITNESS_GAP_MS`] of its last round with a peer a Mac does not ask again, and the scan
/// keeps its order; after a longer gap (a peer that slept or was cut off) it asks again, and the
/// table written meanwhile comes first.
#[test]
fn a_peer_heard_again_after_a_long_gap_is_asked_again() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 4);
    let at = |secs: u64| [0, 1].map(|node| endpoint(&lab, &tables, node).with_ring_clock(secs));
    let start = 1_800_000_000;
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    let endpoints = at(start);
    exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
    assert_eq!(
        selected(&ours, &tables),
        0,
        "nothing differs: the scan's order"
    );
    write(&lab, &tables, 3, 1);
    let soon = start + 60;
    let endpoints = at(soon);
    exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
    assert_eq!(
        selected(&ours, &tables),
        1,
        "within the gap nothing is asked"
    );
    let endpoints = at(soon + WITNESS_GAP_MS / 1000);
    exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
    assert_eq!(
        selected(&ours, &tables),
        3,
        "after the gap the moved table comes first"
    );
    assert_eq!(held(&lab, &tables, 3, 0), 1);
}

/// A peer from before CR1 reads an ask as a table it does not hold and answers none; a Mac
/// from before CR1 asks nothing. Either way the round completes in the scan's order.
#[test]
fn a_peer_from_before_cr1_neither_asks_nor_answers_and_keeps_the_scan() {
    for old in [1, 0] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 3);
        write(&lab, &tables, 2, 1);
        let endpoints =
            [0, 1].map(|node| endpoint(&lab, &tables, node).with_witnesses(node != old));
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        let mut order = Vec::new();
        for _ in 0..3 {
            exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
            order.push(selected(&ours, &tables));
        }
        assert_eq!(order, [0, 1, 2], "old peer {old}: the scan's order");
        assert_eq!(held(&lab, &tables, 2, 0), 1);
    }
}

/// C3: a table this side wrote to and its endpoint announced is its next round's, ahead of the
/// scan's turn and without asking the peer anything, and the scan goes on from where it was.
/// An append whose table went unnamed has the next round ask which tables differ, inside the
/// gap it would otherwise wait out, and the table that differs is taken the same way: the
/// scan's place stays. The endpoint keeps each peer's hint and a round starts from that copy,
/// so the announce goes to the endpoint, not into the caller's repair.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Repair::announce`] naming no table, the round after the
/// write took the scan's next table (`the announced table, not the scan's next`: `left: (1,
/// false)`, `right: (3, false)`).
#[test]
fn an_announced_table_is_the_next_rounds_before_the_scans_turn() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 4);
    let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    let mut round = |ours: &mut PeerRepair| {
        exchange_round(&endpoints[0], &endpoints[1], ours, &mut theirs);
        let report = ours.application().unwrap();
        (selected(ours, &tables), report.witnesses_asked)
    };
    assert_eq!(
        round(&mut ours),
        (0, true),
        "a first round asks, and nothing differs"
    );

    write(&lab, &tables, 3, 0);
    endpoints[0].announce(&BTreeSet::from([tables[3].0.digest()]), false);
    assert_eq!(
        round(&mut ours),
        (3, false),
        "the announced table, not the scan's next"
    );
    assert_eq!(
        held(&lab, &tables, 3, 1),
        1,
        "that round carried the record"
    );
    assert_eq!(
        round(&mut ours),
        (1, false),
        "the scan goes on from where it was"
    );

    write(&lab, &tables, 0, 0);
    endpoints[0].announce(&BTreeSet::new(), true);
    assert_eq!(
        round(&mut ours),
        (0, true),
        "an unnamed append asks, and the table that differs comes first"
    );
    assert_eq!(held(&lab, &tables, 0, 1), 1);
    assert_eq!(
        round(&mut ours),
        (2, false),
        "and the scan goes on from where it was"
    );
}

/// Announced tables leave the regular scan its place and its turns, over four common tables
/// and rounds that move nothing. With table 3 announced before every second round, the rounds
/// between go on through tables 1, 2, 3 and 0: every table is selected within eight rounds.
/// With it announced before every round, each fifth round is the scan's (the `TABLE_BURST`th
/// announced round in a row yields to it): every table is selected within fifteen. And two
/// tables announced before every round take turns.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Repair::announce`] naming its tables among those whose
/// witnesses differed, which the regular turn visits and follows, as it did, tables 1 and 2
/// were never selected (`announced before every second round: the scan keeps its place
/// between`: `left: [3, 0, 3, 0, 3, 0, 3, 0]`, `right: [3, 1, 3, 2, 3, 3, 3, 0]`).
/// NEGATIVE CONTROL (2026-11-12): with an announced table's round never yielding, the scan had
/// no round (`announced before every round: each fifth round is the scan's`: `left: [3, 3, 3,
/// 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3]`).
/// NEGATIVE CONTROL (2026-11-12): with the announced table taken always the first of them,
/// table 3 waited behind table 1 (`two announced tables take turns`: `left: [1, 1, 1, 1]`,
/// `right: [1, 3, 1, 3]`).
#[test]
fn announced_tables_leave_the_scan_its_place_and_its_turns() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 4);
    let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    // The tables `rounds` rounds select, `announced` named to the endpoint before each round
    // whose number `when` passes.
    let mut rounds = |rounds: usize, announced: &[usize], when: &dyn Fn(usize) -> bool| {
        let named: BTreeSet<_> = announced.iter().map(|n| tables[*n].0.digest()).collect();
        (0..rounds)
            .map(|n| {
                if when(n) {
                    endpoints[0].announce(&named, false);
                }
                exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
                selected(&ours, &tables)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(rounds(1, &[], &|_| false), [0], "the scan's first table");
    assert_eq!(
        rounds(8, &[3], &|n| n % 2 == 0),
        [3, 1, 3, 2, 3, 3, 3, 0],
        "announced before every second round: the scan keeps its place between"
    );
    assert_eq!(
        rounds(15, &[3], &|_| true),
        [3, 3, 3, 3, 1, 3, 3, 3, 3, 2, 3, 3, 3, 3, 3],
        "announced before every round: each fifth round is the scan's"
    );
    assert_eq!(
        rounds(4, &[1, 3], &|_| true),
        [1, 3, 1, 3],
        "two announced tables take turns"
    );
}

/// An announce names at most a round's list of tables ([`MAX_TABLES`]); one past that, like an
/// append whose table went unnamed, has the next round ask for witnesses instead.
///
/// NEGATIVE CONTROL (2026-11-12): with [`Repair::announce`] naming no table, the hint named
/// none of the 64 (`left: ({}, false)`).
#[test]
fn an_announce_past_a_lists_bound_asks_for_witnesses() {
    let table = |n: u8| -> Digest { [n; 32] };
    let listed = || Repair {
        listed_ms: Some(1),
        ..Repair::default()
    };
    let full: BTreeSet<_> = (0..MAX_TABLES).map(|n| table(n as u8)).collect();
    let mut repair = listed();
    repair.announce(&full, false);
    assert_eq!(repair.announced(), (&full, false));
    repair.announce(&BTreeSet::from([table(3)]), false);
    assert_eq!(repair.announced(), (&full, false), "a table named already");
    repair.announce(&BTreeSet::from([table(200)]), false);
    assert_eq!(repair.announced(), (&full, true), "one past the bound asks");
    let mut repair = listed();
    repair.announce(&BTreeSet::new(), true);
    assert_eq!(repair.announced(), (&BTreeSet::new(), true));
}

/// Two rounds on `tables`' first table, unreadable on Mac 1 (its checkpoint gone), each deferred,
/// then the scan's next: the initiator's repair after each round.
fn deferred_rounds(passes: bool) -> (Vec<PeerRepair>, Vec<usize>) {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 3);
    std::fs::remove_file(tables[0].1[1].join("table.checkpoint")).unwrap();
    let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    let mut repairs = Vec::new();
    let mut order = Vec::new();
    for _ in 0..3 {
        DEFERRAL_PASSES.with(|flag| flag.set(passes));
        exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
        DEFERRAL_PASSES.with(|flag| flag.set(false));
        order.push(selected(&ours, &tables));
        repairs.push(ours.clone());
    }
    (repairs, order)
}

/// A deferred round of a round that moved nothing, as its initiator kept it: the retry of
/// [`a_deferred_table_is_taken_again_next_round`] (`crate::runtime::discovery`'s outcome test).
pub(crate) fn deferred_repair() -> PeerRepair {
    let (repairs, order) = deferred_rounds(false);
    assert_eq!(order[1], 0);
    repairs[1].clone()
}

/// A table its peer cannot read now is deferred: no success, so the next round takes it again
/// rather than leaving it until the scan comes round; a second deferral yields to the scan.
/// MUTANT (`DEFERRAL_PASSES`, as before CR1): the scan moves on at once.
#[test]
fn a_deferred_table_is_taken_again_next_round() {
    let (repairs, order) = deferred_rounds(false);
    assert_eq!(order, [0, 0, 1]);
    let deferred: Vec<_> = repairs
        .iter()
        .map(|repair| repair.application().unwrap().table_deferred)
        .collect();
    assert_eq!(deferred, [true, true, false]);
    let retry = repairs[1].application().unwrap();
    assert_eq!((retry.sent, retry.received, retry.deferred), (0, 0, 1));
    let (_, order) = deferred_rounds(true);
    assert_eq!(
        order,
        [0, 1, 2],
        "MUTANT: the scan passes the deferred table"
    );
}

/// Fresh peers whose tables moved alike still start apart: a Mac back among three peers, each of
/// which wrote to the same two of five tables, takes the first in its first round with one peer
/// and the second in its first with the next, as the regular scan spreads fresh peers.
/// MUTANT (`UNWITNESSED`, the order from before CR1): the scan spreads them over all five.
#[test]
fn fresh_peers_still_start_apart_on_the_tables_that_differ() {
    for unwitnessed in [false, true] {
        let lab = Lab::new(&[Role::Writer; 4]);
        let mut tables: Vec<(Scope, [PathBuf; 4])> = (0..5)
            .map(|table| {
                let paths: [PathBuf; 4] = std::array::from_fn(|node| {
                    lab._home.path().join(format!("apart-{table}-{node}"))
                });
                let bootstrap = lab.bootstrap(0);
                let session = Session::open(&bootstrap).unwrap();
                let scope = Store::create(&session, &paths[0]).unwrap().scope();
                drop(session);
                for (node, path) in paths.iter().enumerate().skip(1) {
                    let bootstrap = lab.bootstrap(node);
                    let session = Session::open(&bootstrap).unwrap();
                    Store::join(&session, path, scope).unwrap();
                }
                (scope, paths)
            })
            .collect();
        tables.sort_by_key(|(scope, _)| scope.digest());
        for node in 1..4 {
            for table in [1, 3] {
                let (scope, paths) = &tables[table];
                let bootstrap = lab.bootstrap(node);
                let session = Session::open(&bootstrap).unwrap();
                Store::open(&session, &paths[node], *scope)
                    .unwrap()
                    .commit_catalog(&session, b"written while the peer was away")
                    .unwrap();
            }
        }
        let endpoints: Vec<_> = (0..4)
            .map(|node| {
                Endpoint::from_bootstrap(&lab.bootstrap(node))
                    .unwrap()
                    .unwrap()
                    .with_tables(
                        lab.paths[node].clone(),
                        tables
                            .iter()
                            .map(|(scope, paths)| (*scope, paths[node].clone()))
                            .collect(),
                    )
                    .unwrap()
            })
            .collect();
        let mut order = Vec::new();
        for peer in 1..4 {
            let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
            UNWITNESSED.with(|flag| flag.set(unwitnessed));
            exchange_round(&endpoints[0], &endpoints[peer], &mut ours, &mut theirs);
            UNWITNESSED.with(|flag| flag.set(false));
            let table = ours.application().unwrap().table;
            order.push(
                tables
                    .iter()
                    .position(|(scope, _)| Some(scope.digest()) == table)
                    .unwrap(),
            );
        }
        if unwitnessed {
            assert_eq!(
                order,
                [0, 1, 2],
                "MUTANT: the scan's spread over every table"
            );
        } else {
            assert_eq!(
                order,
                [1, 3, 1],
                "fresh peers start apart on the tables that moved"
            );
        }
    }
}

/// Two Macs that dial each other in turn, sharing each side's state of the other across both
/// directions as the peer cache does: the first round asks, the other side asks back once, and
/// after that neither asks within [`WITNESS_GAP_MS`], so the regular scan goes on.
/// MUTANT (`ECHOES`, CR1's first cut): each ask is echoed, and every round asks again.
#[test]
fn two_macs_dialling_each_other_ask_once_each_not_every_round() {
    for echoes in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 4);
        write(&lab, &tables, 2, 1);
        let start = 1_800_000_000;
        let endpoints = [0, 1].map(|node| {
            endpoint(&lab, &tables, node)
                .with_ring_clock(start)
                .with_echoes(echoes)
        });
        let mut repairs = [PeerRepair::default(), PeerRepair::default()];
        let mut asked = Vec::new();
        for round in 0..6 {
            let (from, to) = if round % 2 == 0 { (0, 1) } else { (1, 0) };
            let [a, b] = &mut repairs;
            let (ours, theirs) = if from == 0 { (a, b) } else { (b, a) };
            exchange_round(&endpoints[from], &endpoints[to], ours, theirs);
            asked.push(ours.application().unwrap().witnesses_asked);
        }
        if echoes {
            assert_eq!(asked, [true; 6], "MUTANT: every round asks");
        } else {
            assert_eq!(
                asked,
                [true, true, false, false, false, false],
                "an ask, its one echo, then the scan"
            );
        }
        assert_eq!(held(&lab, &tables, 2, 0), 1);
    }
}
