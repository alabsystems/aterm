// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CR1 across real rounds: a first round with a peer repairs the tables whose witnesses differ
//! before the regular turn (`crate::replica::witness`), a peer from before CR1 keeps the scan,
//! and a deferred table is taken again in the next round.

use super::*;

/// `count` tables two writers hold, each empty on both, sorted by digest.
pub(crate) fn tables(lab: &Lab, count: usize) -> Vec<(Scope, [PathBuf; 2])> {
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

pub(crate) fn endpoint(lab: &Lab, tables: &[(Scope, [PathBuf; 2])], node: usize) -> Endpoint {
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
pub(crate) fn write(lab: &Lab, tables: &[(Scope, [PathBuf; 2])], table: usize, node: usize) {
    let bootstrap = lab.bootstrap(node);
    let session = Session::open(&bootstrap).unwrap();
    let (scope, paths) = &tables[table];
    Store::open(&session, &paths[node], *scope)
        .unwrap()
        .commit_catalog(&session, b"written while the peer was away")
        .unwrap();
}

/// The records Mac `node` holds of `table`.
pub(crate) fn held(
    lab: &Lab,
    tables: &[(Scope, [PathBuf; 2])],
    table: usize,
    node: usize,
) -> usize {
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
pub(super) fn selected(repair: &PeerRepair, tables: &[(Scope, [PathBuf; 2])]) -> usize {
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

/// C3: a table the dial scheduler led this side's round with ([`PeerRepair::lead`], what a walk
/// hands a dial for a table that moved here) is its next round's, ahead of the scan's turn and
/// without asking the peer anything, and the scan goes on from where it was. An ask
/// ([`PeerRepair::ask`]) inside the gap the round would otherwise wait out finds the table that
/// differs, which is taken first, and the scan's place stays. The endpoint keeps each peer's
/// hint and a round starts from that copy; the lead goes with the round's request, which the
/// lease keeps.
///
/// The merge of 2026-11-13 ported this law from main's announce, which named the table to the
/// endpoint's every hint, to the lead the walk hands one dial; an unnamed append, which no look
/// makes, became the ask.
///
/// NEGATIVE CONTROL (2026-11-12, as an announce): with the announce naming no table, the round
/// after the write took the scan's next table (`the led table, not the scan's next`: `left: (1,
/// false)`, `right: (3, false)`).
#[test]
fn a_led_table_is_the_next_rounds_before_the_scans_turn() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 4);
    let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    let mut round = |ours: &mut PeerRepair| {
        exchange_round(&endpoints[0], &endpoints[1], ours, &mut theirs);
        let report = ours.application().unwrap();
        (selected(ours, &tables), report.witnesses_asked())
    };
    assert_eq!(
        round(&mut ours),
        (0, true),
        "a first round asks, and nothing differs"
    );

    write(&lab, &tables, 3, 0);
    assert!(ours.lead(tables[3].0.digest()));
    assert_eq!(
        round(&mut ours),
        (3, false),
        "the led table, not the scan's next"
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
    ours.ask();
    assert_eq!(
        round(&mut ours),
        (0, true),
        "an ask finds the table that differs, which comes first"
    );
    assert_eq!(held(&lab, &tables, 0, 1), 1);
    assert_eq!(
        round(&mut ours),
        (2, false),
        "and the scan goes on from where it was"
    );
}

/// Led tables leave the regular scan its place and its turns, over four common tables and
/// rounds that move nothing. With table 3 led before every second round, the rounds between go
/// on through tables 1, 2, 3 and 0: every table is selected within eight rounds. With it led
/// before every round, each fifth round is the scan's (the `TABLE_BURST`th led round in a row
/// yields to it): every table is selected within fifteen. And two tables led before every round
/// take turns.
///
/// The merge of 2026-11-13 ported this law from main's announced tables to the leads that
/// replaced them, with the same sequences.
///
/// NEGATIVE CONTROL (2026-11-12, as an announce): with the announced tables kept among those
/// whose witnesses differed, which the regular turn visits and follows, tables 1 and 2 were
/// never selected (`left: [3, 0, 3, 0, 3, 0, 3, 0]`); `a_lead_survives_the_witness_answer`
/// holds the lead out of that set (`LEAD_IN_DIFFERING`).
/// MUTANT (`LEADS_UNYIELDING`): a run of led rounds never yields, and the scan has no round
/// (`led before every round: each fifth round is the scan's`).
/// NEGATIVE CONTROL (2026-11-12, as an announce): with the announced table taken always the
/// first of them, table 3 waited behind table 1 (`two led tables take turns`: `left: [1, 1, 1,
/// 1]`, `right: [1, 3, 1, 3]`).
#[test]
fn led_tables_leave_the_scan_its_place_and_its_turns() {
    for unyielding in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 4);
        let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        // The tables `rounds` rounds select, `led` asked of the round before each whose number
        // `when` passes.
        let mut rounds = |rounds: usize, led: &[usize], when: &dyn Fn(usize) -> bool| {
            (0..rounds)
                .map(|n| {
                    if when(n) {
                        for table in led {
                            assert!(ours.lead(tables[*table].0.digest()));
                        }
                    }
                    LEADS_UNYIELDING.with(|flag| flag.set(unyielding));
                    exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
                    LEADS_UNYIELDING.with(|flag| flag.set(false));
                    selected(&ours, &tables)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rounds(1, &[], &|_| false), [0], "the scan's first table");
        assert_eq!(
            rounds(8, &[3], &|n| n % 2 == 0),
            [3, 1, 3, 2, 3, 3, 3, 0],
            "led before every second round: the scan keeps its place between"
        );
        let every = rounds(15, &[3], &|_| true);
        if unyielding {
            assert_eq!(every, [3; 15], "MUTANT: the scan has no round");
            continue;
        }
        assert_eq!(
            every,
            [3, 3, 3, 3, 1, 3, 3, 3, 3, 2, 3, 3, 3, 3, 3],
            "led before every round: each fifth round is the scan's"
        );
        assert_eq!(
            rounds(4, &[1, 3], &|_| true),
            [1, 3, 1, 3],
            "two led tables take turns"
        );
    }
}

/// A lead names at most a round's list of tables ([`MAX_TABLES`]): one past that is not kept,
/// and the scan reaches its table at its turn. A table led already is led once. The tables a
/// round leads because the lists newly share them count against a bound of their own, so a full
/// list of them still leaves the scheduler's lead its place, first.
/// MUTANT (`LEADS_UNRANKED`): the scheduler's lead does not go first.
///
/// The merge of 2026-11-13 ported this law from main's announce, which asked for witnesses past
/// the bound; a lead past it is refused instead, and the walk's dial still settles its seat.
#[test]
fn a_lead_past_a_lists_bound_is_not_kept() {
    let table = |n: u8| -> Digest { [n; 32] };
    let mut repair = Repair::default();
    for n in 0..MAX_TABLES {
        assert!(repair.lead(table(n as u8)), "table {n} of a full list");
    }
    assert!(repair.lead(table(3)), "a table led already");
    assert!(!repair.lead(table(200)), "one past the bound is not kept");
    assert_eq!(repair.owed(), Some(Owed::Lead));

    // The tables a round leads because the lists newly share them have a bound of their own,
    // which leaves the scheduler's whole: a write here is led however many there are.
    for mutant in [false, true] {
        let mut repair = Repair::default();
        for n in 0..MAX_TABLES {
            repair.take_up(table(n as u8));
        }
        repair.take_up(table(201));
        assert!(
            repair.lead(table(200)),
            "a full list of newly shared tables refuses no lead the scheduler names"
        );
        let common: BTreeSet<Digest> = (0..MAX_TABLES)
            .map(|n| table(n as u8))
            .chain([table(200), table(201)])
            .collect();
        LEADS_UNRANKED.with(|flag| flag.set(mutant));
        let led = repair.led(&common);
        LEADS_UNRANKED.with(|flag| flag.set(false));
        assert_eq!(
            led == Some(table(200)),
            !mutant,
            "the scheduler's lead goes first (MUTANT {mutant})"
        );
        assert!(
            !repair.lead.contains_key(&table(201)),
            "one past the newly shared bound is not kept"
        );
    }
}

/// A round that trades table lists leaves, on each side, what they showed of both sides' tables
/// ([`crate::replica::Listing::takes`]), which the dial scheduler's walk reads
/// (`crate::runtime::discovery`): a table both listed may go to the peer; one this side listed
/// and the peer did not may not, for the peer holds none of it or cannot take it. A table this
/// side did not list may go only where this side's store of it is younger than the round (as a
/// Mac's store of a table its peer created, enrolled after their last lists), whose lists could
/// say nothing of it; one this side held then and did not list (its policy excludes it) may not,
/// however often it is written. A peer no round traded lists with may be offered anything.
/// MUTANT (`TAKES_ANY_UNLISTED`): the responder may give the initiator the third table, which the
/// responder held before the round and did not list.
/// MUTANT (`TAKES_ONLY_LISTED`): it may not, even once its store is younger than the round.
#[test]
fn a_round_leaves_what_the_peer_may_take_of_each_table() {
    let lab = Lab::new(&[Role::Writer; 2]);
    let tables = tables(&lab, 3);
    let digest = |table: usize| tables[table].0.digest();
    // Whether `repair`'s lists let a round led to `table` give the peer some of it, this side
    // holding a store of it since `held`; anything, before a round traded lists.
    let takes = |repair: &PeerRepair, table: usize, held: Option<Instant>| {
        repair
            .listed()
            .is_none_or(|lists| lists.takes(&digest(table), held))
    };
    // Mac 1 lists only the first two.
    let endpoints = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables[..2], 1)];
    let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
    let before = Instant::now();
    assert!(ours.listed().is_none(), "no round has traded lists");
    exchange_round(&endpoints[0], &endpoints[1], &mut ours, &mut theirs);
    let after = Instant::now();
    for (side, repair) in [("initiator", &ours), ("responder", &theirs)] {
        assert!(repair.listed().is_some(), "the {side}'s round traded lists");
        assert_eq!(
            [0, 1, 2].map(|table| takes(repair, table, Some(before))),
            [true, true, false],
            "the {side}, its stores held before the round"
        );
    }
    assert!(
        !takes(&ours, 2, Some(after)),
        "the initiator listed the third table and the responder did not, however young its store"
    );
    for (mutant, cell) in [
        (None, None),
        (Some("TAKES_ANY_UNLISTED"), Some(&TAKES_ANY_UNLISTED)),
        (Some("TAKES_ONLY_LISTED"), Some(&TAKES_ONLY_LISTED)),
    ] {
        let read = |held| {
            if let Some(cell) = cell {
                cell.with(|flag| flag.set(true));
            }
            let takes = takes(&theirs, 2, held);
            if let Some(cell) = cell {
                cell.with(|flag| flag.set(false));
            }
            takes
        };
        let expected = match mutant {
            None => [false, true, false],
            Some("TAKES_ANY_UNLISTED") => [true, true, true],
            _ => [false, false, false],
        };
        assert_eq!(
            [read(Some(before)), read(Some(after)), read(None)],
            expected,
            "the responder, which did not list the third table: its store held before the \
             round, taken up after it, unknown (MUTANT {mutant:?})"
        );
    }
}

/// A table both sides list that their last round to trade lists did not find both listing is the
/// next round's before the scan's turn, whichever side begins it: the side that took it up since
/// may hold none of it. Mac 0 holds a record of its fourth table, which Mac 1 does not list in their
/// first round and lists in their second, as a Mac lists a table it enrolled once a round brought
/// the declaration. Begun by Mac 0, whose first lists found Mac 1 leaving the table out, or by
/// Mac 1, whose first lists did not hold it, the second round takes the table, and Mac 1 holds
/// the record. Each round after it is the scan's.
///
/// The recipient of `process_sync_tests`' later table takes it up so, in whichever round with the
/// source comes after the one that brought the declaration; before this, that round's regular
/// turn reached the table only where the scan's place happened to fall before it.
/// MUTANT (`TAKEN_UP_UNLED`): the second round takes the scan's next table, and Mac 1 holds
/// nothing.
#[test]
fn a_table_the_lists_newly_share_is_the_next_rounds_before_the_scans_turn() {
    let late = 3;
    for mac1_begins in [false, true] {
        for mutant in [false, true] {
            let lab = Lab::new(&[Role::Writer; 2]);
            let tables = tables(&lab, 4);
            write(&lab, &tables, late, 0);
            let full = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
            // Mac 1 before it takes the fourth table up.
            let before = endpoint(&lab, &tables[..late], 1);
            let (mut zero, mut one) = (PeerRepair::default(), PeerRepair::default());
            // One round begun by Mac 1, or by Mac 0, as Mac 1's endpoint is `mac1`; the table it
            // took, from its beginner's state.
            let mut round = |mac1: &Endpoint| {
                let beginner = if mac1_begins {
                    exchange_round(mac1, &full[0], &mut one, &mut zero);
                    &one
                } else {
                    exchange_round(&full[0], mac1, &mut zero, &mut one);
                    &zero
                };
                selected(beginner, &tables)
            };
            let first = round(&before);
            assert_ne!(first, late, "Mac 1 does not list the fourth table");
            TAKEN_UP_UNLED.with(|flag| flag.set(mutant));
            let second = round(&full[1]);
            TAKEN_UP_UNLED.with(|flag| flag.set(false));
            let case = format!("begun by Mac {}, MUTANT {mutant}", u8::from(mac1_begins));
            if mutant {
                assert_ne!(second, late, "the scan's next table ({case})");
                assert_eq!(held(&lab, &tables, late, 1), 0, "{case}");
                continue;
            }
            assert_eq!(second, late, "the table the lists newly share ({case})");
            assert_eq!(
                held(&lab, &tables, late, 1),
                1,
                "Mac 1 holds the record ({case})"
            );
            assert_ne!(round(&full[1]), late, "the scan goes on ({case})");
        }
    }
}

/// A led table stays led through a round that breaks off before its exchange of the table ends,
/// so the next round takes it before the scan's turn. Mac 0 holds a record of its fourth table,
/// which Mac 1 does not list in their first round and lists in their second, as in
/// [`a_table_the_lists_newly_share_is_the_next_rounds_before_the_scans_turn`]; Mac 0 begins each
/// round. The second leads the table, and Mac 1's receive of Mac 0's batch breaks off
/// ([`crate::replica::BEFORE_INGEST`]), so Mac 1 holds none of it. That round's lists already
/// showed the table shared, so the third finds nothing newly shared: it takes the table by the
/// lead the second left, and Mac 1 holds the record.
///
/// This is the newly shared table of `auto_enrollment_tests` where the round that leads it breaks
/// off (an I/O timeout or a busy admission under load): between two Macs at rest, the record then
/// waited for an audit or a scan turn that happened to fall on the table.
/// MUTANT (`LEAD_SPENT_ON_SELECTION`, as before 2026-11-14): the broken round spends the lead,
/// and the third round is the scan's turn.
#[test]
fn a_lead_outlives_a_round_that_breaks_off_before_its_table_is_exchanged() {
    let late = 3;
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 4);
        let digest = tables[late].0.digest();
        write(&lab, &tables, late, 0);
        let full = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        // Mac 1 before it takes the fourth table up.
        let before = endpoint(&lab, &tables[..late], 1);
        let (mut zero, mut one) = (PeerRepair::default(), PeerRepair::default());
        exchange_round(&full[0], &before, &mut zero, &mut one);
        assert_ne!(
            selected(&zero, &tables),
            late,
            "Mac 1 does not list the fourth table"
        );

        let hook: crate::replica::IngestHook =
            Box::new(|_| Err(Error::new("the round broke off before the ingest")));
        crate::replica::BEFORE_INGEST
            .lock()
            .unwrap()
            .push((digest, hook));
        LEAD_SPENT_ON_SELECTION.with(|flag| flag.set(mutant));
        exchange_round(&full[0], &full[1], &mut zero, &mut one);
        LEAD_SPENT_ON_SELECTION.with(|flag| flag.set(false));
        let broken = zero
            .application_interrupted()
            .expect("the led round broke off");
        assert_eq!(
            (broken.table, broken.sent),
            (Some(digest), 0),
            "the round led the table and delivered none of it (MUTANT {mutant})"
        );
        assert_eq!(held(&lab, &tables, late, 1), 0, "MUTANT {mutant}");

        exchange_round(&full[0], &full[1], &mut zero, &mut one);
        let took = zero.application().unwrap();
        if mutant {
            assert!(took.regular(), "MUTANT: the scan's turn");
            continue;
        }
        assert_eq!(
            (took.table, took.regular(), took.sent),
            (Some(digest), false, 1),
            "the table the broken round led"
        );
        assert_eq!(held(&lab, &tables, late, 1), 1, "Mac 1 holds the record");
    }
}

/// A led table whose exchange breaks off in every round is spent by the second round that takes
/// it, so the pair is left nothing pending ([`PeerRepair::pending`]) and the dial scheduler rests
/// it until its audit. As in
/// [`a_lead_outlives_a_round_that_breaks_off_before_its_table_is_exchanged`], Mac 0 holds a
/// record of a table Mac 1 lists from their second round on, which that round leads; here Mac
/// 1's receive of every batch of it breaks off ([`crate::replica::BEFORE_INGEST`]), as for a
/// table whose exchange fails each time (a store the peer cannot enroll). The second round and
/// the third take the table and break off; the fourth takes the scan's turn on another table and
/// completes, and nothing is pending after it.
///
/// On 9f0e991ea2e the lead outlived every such round: each fifth round, the scan's, completed
/// with the lead still pending, so the scheduler followed the pair up at the follow-up delay and
/// the next four rounds broke off, without end. Main spent the lead as the first round selected
/// the table.
/// MUTANT (`LEAD_OUTLIVES_EVERY_BREAK`): four rounds take the table and break off, and the first
/// to complete, the scan's, leaves the lead pending.
#[test]
fn a_lead_whose_exchange_breaks_off_each_time_is_spent_by_the_second_round_that_takes_it() {
    let late = 2;
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 3);
        let digest = tables[late].0.digest();
        write(&lab, &tables, late, 0);
        let full = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        // Mac 1 before it takes the third table up.
        let before = endpoint(&lab, &tables[..late], 1);
        let (mut zero, mut one) = (PeerRepair::default(), PeerRepair::default());
        exchange_round(&full[0], &before, &mut zero, &mut one);
        assert_eq!(selected(&zero, &tables), 0, "the scan's first table");

        // Each round that sends Mac 1 a batch of the table breaks off there.
        let arm = || {
            let mut hooks = crate::replica::BEFORE_INGEST.lock().unwrap();
            if !hooks.iter().any(|(table, _)| *table == digest) {
                let hook: crate::replica::IngestHook =
                    Box::new(|_| Err(Error::new("the table's exchange fails each time")));
                hooks.push((digest, hook));
            }
        };
        // Each round's table, whether it broke off, and whether it left work pending, up to the
        // first that completes.
        let mut rounds = Vec::new();
        loop {
            arm();
            LEAD_OUTLIVES_EVERY_BREAK.with(|flag| flag.set(mutant));
            exchange_round(&full[0], &full[1], &mut zero, &mut one);
            LEAD_OUTLIVES_EVERY_BREAK.with(|flag| flag.set(false));
            let (report, broke) = match zero.application() {
                Some(report) => (report, false),
                None => (
                    zero.application_interrupted()
                        .expect("the round reached its table"),
                    true,
                ),
            };
            let table = tables
                .iter()
                .position(|(scope, _)| Some(scope.digest()) == report.table)
                .expect("a provisioned table was selected");
            rounds.push((table, broke, zero.pending()));
            if !broke {
                break;
            }
            assert!(rounds.len() < 8, "a round completes: {rounds:?}");
        }
        crate::replica::BEFORE_INGEST
            .lock()
            .unwrap()
            .retain(|(table, _)| *table != digest);
        assert_eq!(held(&lab, &tables, late, 1), 0, "MUTANT {mutant}");
        if mutant {
            assert_eq!(
                rounds,
                [
                    (late, true, false),
                    (late, true, false),
                    (late, true, false),
                    (late, true, false),
                    (1, false, true)
                ],
                "MUTANT: the lead outlives every break, and the scan's round leaves it pending"
            );
            continue;
        }
        assert_eq!(
            rounds,
            [(late, true, false), (late, true, false), (1, false, false)],
            "two rounds take the table and break off, and the scan's round leaves nothing pending"
        );
    }
}

/// A table this side's scheduler named (a write here, which a walk hands its dial to lead with)
/// goes before the tables a round leads only because the lists newly share them, however their
/// digests sort. Mac 1 lists the first and the last of Mac 0's five tables in their first round,
/// and all five in their second, which so finds three tables newly shared. Between the two
/// rounds Mac 0 writes the last table and leads it: the second round takes it and carries the
/// record, and the three newly shared tables follow in turn.
///
/// On 9f0e991ea2e the leads were one queue in digest order, so a written table waited behind
/// each newly shared one that sorted before it, each up to `TABLE_BURST` rounds where it moved
/// full batches, while the walk's dial that led it settled its seat for it.
/// MUTANT (`LEADS_UNRANKED`): the newly shared tables go first, and the written table's record
/// waits for the fourth round after.
#[test]
fn a_table_the_scheduler_named_goes_before_the_tables_the_lists_newly_share() {
    let written = 4;
    for mutant in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 5);
        let full = [endpoint(&lab, &tables, 0), endpoint(&lab, &tables, 1)];
        let first: Vec<_> = [0, written].map(|table| tables[table].clone()).into();
        let before = endpoint(&lab, &first, 1);
        let (mut zero, mut one) = (PeerRepair::default(), PeerRepair::default());
        exchange_round(&full[0], &before, &mut zero, &mut one);
        assert_eq!(selected(&zero, &tables), 0, "the scan's first table");

        write(&lab, &tables, written, 0);
        assert!(zero.lead(tables[written].0.digest()));
        let mut order = Vec::new();
        let mut carried = Vec::new();
        for _ in 0..4 {
            LEADS_UNRANKED.with(|flag| flag.set(mutant));
            exchange_round(&full[0], &full[1], &mut zero, &mut one);
            LEADS_UNRANKED.with(|flag| flag.set(false));
            order.push(selected(&zero, &tables));
            carried.push(held(&lab, &tables, written, 1));
        }
        if mutant {
            assert_eq!(
                (order, carried),
                (vec![1, 2, 3, written], vec![0, 0, 0, 1]),
                "MUTANT: the written table waits behind the newly shared ones"
            );
            continue;
        }
        assert_eq!(
            (order, carried),
            (vec![written, 1, 2, 3], vec![1, 1, 1, 1]),
            "the written table first, then the newly shared ones in turn"
        );
    }
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
            asked.push(ours.application().unwrap().witnesses_asked());
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

/// One round Mac `from` initiates with Mac `to` on endpoints whose ring clock reads `secs`: the
/// table it took, by its place in `tables`, the turn that took it and why it asked
/// ([`Report::turn`], [`Report::asked`]). The report's regular turn is its turn's.
fn turned(
    lab: &Lab,
    tables: &[(Scope, [PathBuf; 2])],
    secs: u64,
    [from, to]: [usize; 2],
    ours: &mut PeerRepair,
    theirs: &mut PeerRepair,
) -> (usize, Option<TableTurn>, Option<Ask>) {
    let endpoints = [0, 1].map(|node| endpoint(lab, tables, node).with_ring_clock(secs));
    exchange_round(&endpoints[from], &endpoints[to], ours, theirs);
    let report = ours.application().unwrap();
    assert_eq!(
        report.regular(),
        report.turn == Some(TableTurn::Regular),
        "{report:?}"
    );
    (selected(ours, tables), report.turn, report.asked)
}

/// What a round this side initiates reports of the turn that took its table and of why it asked
/// the peer which tables differ ([`Report::turn`], [`Report::asked`]), across real rounds.
///
/// A Mac whose peer wrote to two tables, one of them more than a [`BATCH`]: its first round asks
/// as a first round and visits the first table that differs; a round within [`WITNESS_GAP_MS`]
/// asks nothing and visits the other, a full batch; the round after the gap asks again and takes
/// that table's next batch; an audit asks and takes the scan's turn; and once the peer asks this
/// side in a round it began, this side's next round asks back. A table its peer cannot read is
/// deferred and taken again by its retry; an audit asked of the round after a deferral asks, and
/// no retry takes its table.
/// MUTANT (`UNWITNESSED`): the first round's visit reads as the scan's turn.
/// MUTANT (`AUDIT_FOLLOWS`): the audit's round takes the deferred table's retry.
#[test]
fn a_round_reports_which_turn_took_its_table_and_why_it_asked() {
    use TableTurn::{Batch, Regular, Retry, Visit};
    let start = 1_800_000_000;
    let gap = WITNESS_GAP_MS / 1000;
    for unwitnessed in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 4);
        write(&lab, &tables, 1, 1);
        for _ in 0..=BATCH {
            write(&lab, &tables, 3, 1);
        }
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        UNWITNESSED.with(|flag| flag.set(unwitnessed));
        let first = turned(&lab, &tables, start, [0, 1], &mut ours, &mut theirs);
        UNWITNESSED.with(|flag| flag.set(false));
        if unwitnessed {
            assert_eq!(
                first,
                (0, Some(Regular), Some(Ask::First)),
                "MUTANT: the scan's turn"
            );
            continue;
        }
        assert_eq!(first, (1, Some(Visit), Some(Ask::First)));
        let mut asked = Vec::new();
        let mut clock = start + 60;
        asked.push(turned(&lab, &tables, clock, [0, 1], &mut ours, &mut theirs));
        clock += gap;
        asked.push(turned(&lab, &tables, clock, [0, 1], &mut ours, &mut theirs));
        ours.audit();
        clock += 10;
        asked.push(turned(&lab, &tables, clock, [0, 1], &mut ours, &mut theirs));
        clock += gap;
        turned(&lab, &tables, clock, [1, 0], &mut theirs, &mut ours);
        clock += 10;
        asked.push(turned(&lab, &tables, clock, [0, 1], &mut ours, &mut theirs));
        assert_eq!(
            asked,
            [
                (3, Some(Visit), None),
                (3, Some(Batch), Some(Ask::Gap)),
                (0, Some(Regular), Some(Ask::Audit)),
                (1, Some(Regular), Some(Ask::Peer)),
            ],
            "a visit within the gap, a full batch's next after it, an audit, the peer's ask back"
        );
        assert_eq!(held(&lab, &tables, 3, 0), BATCH + 1);
    }

    for follows in [false, true] {
        let lab = Lab::new(&[Role::Writer; 2]);
        let tables = tables(&lab, 3);
        std::fs::remove_file(tables[0].1[1].join("table.checkpoint")).unwrap();
        let (mut ours, mut theirs) = (PeerRepair::default(), PeerRepair::default());
        let mut rounds = Vec::new();
        for _ in 0..5 {
            rounds.push(turned(&lab, &tables, start, [0, 1], &mut ours, &mut theirs));
        }
        assert_eq!(
            rounds,
            [
                (0, Some(Visit), Some(Ask::First)),
                (0, Some(Retry), None),
                (1, Some(Regular), None),
                (2, Some(Regular), None),
                (0, Some(Regular), None),
            ],
            "a deferred table's retry, once"
        );
        assert!(ours.application().unwrap().table_deferred);
        ours.audit();
        AUDIT_FOLLOWS.with(|flag| flag.set(follows));
        let audit = turned(&lab, &tables, start, [0, 1], &mut ours, &mut theirs);
        AUDIT_FOLLOWS.with(|flag| flag.set(false));
        if follows {
            assert_eq!(
                audit,
                (0, Some(Retry), Some(Ask::Audit)),
                "MUTANT: the retry takes the audit's table"
            );
        } else {
            assert_eq!(
                audit,
                (0, Some(Visit), Some(Ask::Audit)),
                "the audit's visit"
            );
        }
    }
}
