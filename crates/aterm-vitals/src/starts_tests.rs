// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The starts formula on stand-in and recorded coalition reads.

use super::*;

fn row(id: u64, started: u64, exited: u64) -> CoalTasks {
    CoalTasks {
        id,
        started,
        exited,
    }
}

fn read(newest: u64, rows: &[(u64, u64, u64)]) -> CoalTaskRead {
    CoalTaskRead::of(
        Some(newest),
        false,
        rows.iter().map(|&(id, s, e)| row(id, s, e)),
    )
}

/// Each answering coalition counts what it grew by; one made since the last read (above its
/// newest id) counts whole, its exits too.
#[test]
fn starts_count_each_coalitions_growth_and_a_new_coalition_whole() {
    let then = read(900, &[(1, 400, 380), (557, 9, 7), (900, 2, 0)]);
    let now = read(
        912,
        &[
            (1, 403, 382),
            (557, 9, 7),
            (900, 6, 1),
            (905, 3, 1),
            (912, 1, 0),
        ],
    );
    assert_eq!(
        coalition_moves(&then, &now),
        Some((3 + 4 + 3 + 1, 2 + 1 + 1))
    );
    assert_eq!(coalition_moves(&now, &now), Some((0, 0)), "nothing moved");
}

/// A coalition that no longer answers ended with its last members: each one it still held at
/// the last read is an exit, and none is a start.
#[test]
fn exits_close_a_gone_coalitions_live_members() {
    let then = read(
        900,
        &[(1, 400, 380), (557, 9, 7), (700, 12, 9), (900, 2, 0)],
    );
    let now = read(900, &[(1, 400, 380), (900, 2, 0)]);
    assert_eq!(coalition_moves(&then, &now), Some((0, (9 - 7) + (12 - 9))));
}

/// NEGATIVE CONTROLS for the count's refusals, each beside the same reads counted: a list cut at
/// its buffer (either read), a coalition answering now at or below the last read's newest id
/// that did not answer then (it has no base; the newest itself included), and a counter that
/// ran backwards (starts or exits).
#[test]
fn a_truncated_list_or_an_unbased_coalition_gives_no_rate() {
    let then = read(900, &[(1, 400, 380), (900, 2, 0)]);
    let now = read(901, &[(1, 401, 380), (900, 2, 0), (901, 1, 0)]);
    assert_eq!(coalition_moves(&then, &now), Some((2, 0)), "the control");

    let cut = |r: &CoalTaskRead| CoalTaskRead::of(r.newest, true, r.rows().iter().copied());
    assert_eq!(coalition_moves(&cut(&then), &now), None, "then cut");
    assert_eq!(coalition_moves(&then, &cut(&now)), None, "now cut");

    let unbased = read(901, &[(1, 401, 380), (557, 9, 7), (900, 2, 0), (901, 1, 0)]);
    assert_eq!(
        coalition_moves(&then, &unbased),
        None,
        "557 ≤ 900 has no base"
    );

    let unanswered = read(900, &[(1, 400, 380)]);
    let answers = read(901, &[(1, 401, 380), (900, 2, 0), (901, 1, 0)]);
    assert_eq!(
        coalition_moves(&unanswered, &answers),
        None,
        "900, the newest then, listed but unanswered, has no base"
    );
    let above = read(901, &[(1, 401, 380), (901, 1, 0)]);
    assert_eq!(
        coalition_moves(&unanswered, &above),
        Some((2, 0)),
        "the control: only 901 is above the newest"
    );

    let backwards = read(901, &[(1, 399, 380), (900, 2, 0), (901, 1, 0)]);
    assert_eq!(coalition_moves(&then, &backwards), None, "starts backwards");
    let backwards = read(901, &[(1, 401, 379), (900, 2, 0), (901, 1, 0)]);
    assert_eq!(coalition_moves(&then, &backwards), None, "exits backwards");
}

/// NEGATIVE CONTROL: a read in which no coalition answered, beside one in which some did, counts
/// nothing either way. After the answered read it would close every coalition at once (740
/// coalitions' live tasks as exits in one interval); before one whose list named no newest id it
/// would count every coalition whole as new. Two empty reads moved nothing.
#[test]
fn a_read_in_which_no_coalition_answered_gives_no_count() {
    let full = read(900, &[(1, 400, 380), (557, 9, 7), (900, 2, 0)]);
    let silent = read(900, &[]);
    assert_eq!(
        coalition_moves(&full, &full),
        Some((0, 0)),
        "the control: the same answers moved nothing"
    );
    assert_eq!(
        coalition_moves(&full, &silent),
        None,
        "every coalition gone"
    );
    assert_eq!(
        coalition_moves(&silent, &full),
        None,
        "every coalition back"
    );
    let unlisted = CoalTaskRead::of(None, false, []);
    assert_eq!(
        coalition_moves(&unlisted, &full),
        None,
        "every coalition new above no newest id"
    );
    assert_eq!(coalition_moves(&silent, &unlisted), Some((0, 0)));
}

/// Rows arrive in the kernel's list order; the read orders them by id and keeps one per id, so
/// the walk meets each coalition once.
#[test]
fn a_read_orders_its_rows_by_id() {
    let r = CoalTaskRead::of(
        Some(900),
        false,
        [
            row(900, 2, 0),
            row(1, 400, 380),
            row(557, 9, 7),
            row(557, 9, 7),
        ],
    );
    let ids: Vec<u64> = r.rows().iter().map(|r| r.id).collect();
    assert_eq!(ids, [1, 557, 900]);
    assert_eq!(r.get(557), Some(row(557, 9, 7)));
    assert_eq!(r.get(558), None);
}

/// aterm's own starts are its coalition's growth, where it answered at both reads under the
/// same id; Linux counts forks and says no exits; reads of two kinds count nothing.
#[test]
fn moved_counts_aterms_own_coalition_and_linux_forks() {
    let then = StartCount::Coalitions {
        read: read(900, &[(1, 400, 380), (557, 9, 7)]),
        own: Some(557),
    };
    let now = StartCount::Coalitions {
        read: read(900, &[(1, 401, 380), (557, 12, 8)]),
        own: Some(557),
    };
    assert_eq!(
        moved(&then, &now),
        Some(Moved {
            starts: 4,
            exits: Some(1),
            own_starts: Some(3),
        })
    );
    let unowned = StartCount::Coalitions {
        read: read(900, &[(1, 401, 380), (557, 12, 8)]),
        own: None,
    };
    assert_eq!(moved(&then, &unowned).and_then(|m| m.own_starts), None);
    assert_eq!(
        moved(&StartCount::Forks(31_000), &StartCount::Forks(31_337)),
        Some(Moved {
            starts: 337,
            exits: None,
            own_starts: None,
        })
    );
    assert_eq!(
        moved(&StartCount::Forks(31_337), &StartCount::Forks(31_000)),
        None
    );
    assert_eq!(moved(&then, &StartCount::Forks(31_337)), None);
}

#[test]
fn a_flow_is_its_counts_over_the_interval() {
    let m = Moved {
        starts: 37,
        exits: Some(38),
        own_starts: Some(4),
    };
    assert_eq!(
        TaskFlow::of(m, 2.0),
        Some(TaskFlow {
            starts_ps: 18.5,
            exits_ps: Some(19.0),
            aterm_starts_ps: Some(2.0),
        })
    );
    assert_eq!(TaskFlow::of(m, 0.0), None);
    assert_eq!(TaskFlow::of(m, f64::NAN), None);
}

/// Two pairs of this Mac's reads, one second apart (2026-11-05, 740 then 737 coalitions listed,
/// and 736 then 739), each row as `(id, started, exited)` in the kernel's list order, trimmed to
/// the coalitions that moved, ended or began and a few that held still. In the first, two
/// coalitions grew (1438 by 3 starts and 3 exits, 1513 by 2 and 2) and three ended with one live
/// member each (54319, 54321, 54325): 5 starts and 3 + 2 + 3 = 8 exits. In the second, three
/// coalitions began above the last read's newest id 54355 (54357, 54359, 54361), each with two
/// tasks started and one ended: 6 starts and 3 exits.
#[test]
fn the_starts_formula_matches_recorded_coalition_reads() {
    let then = read(
        54_329,
        &[
            (54_325, 2, 1),
            (54_319, 2, 1),
            (1438, 3_909_893, 3_909_861),
            (54_323, 2, 1),
            (243, 2, 1),
            (54_327, 2, 1),
            (1513, 7_133_553, 7_133_532),
            (54_321, 2, 1),
            (54_329, 2, 1),
            (1, 192, 190),
        ],
    );
    let now = read(
        54_329,
        &[
            (1438, 3_909_896, 3_909_864),
            (54_323, 2, 1),
            (243, 2, 1),
            (54_327, 2, 1),
            (1513, 7_133_555, 7_133_534),
            (54_329, 2, 1),
            (1, 192, 190),
        ],
    );
    assert_eq!(
        coalition_moves(&then, &now),
        Some((5, 8)),
        "grown and ended"
    );

    let then = read(
        54_355,
        &[
            (54_339, 2, 1),
            (54_353, 2, 1),
            (54_355, 2, 1),
            (1438, 3_909_977, 3_909_945),
            (243, 2, 1),
            (1513, 7_135_026, 7_135_008),
            (1, 192, 190),
        ],
    );
    let now = read(
        54_361,
        &[
            (54_339, 2, 1),
            (54_353, 2, 1),
            (54_355, 2, 1),
            (54_359, 2, 1),
            (54_357, 2, 1),
            (1438, 3_909_977, 3_909_945),
            (243, 2, 1),
            (1513, 7_135_026, 7_135_008),
            (54_361, 2, 1),
            (1, 192, 190),
        ],
    );
    assert_eq!(coalition_moves(&then, &now), Some((6, 3)), "begun");
    let moved = moved(
        &StartCount::Coalitions {
            read: then,
            own: Some(1438),
        },
        &StartCount::Coalitions {
            read: now,
            own: Some(1438),
        },
    )
    .expect("a based pair");
    assert_eq!(
        TaskFlow::of(moved, 1.0),
        Some(TaskFlow {
            starts_ps: 6.0,
            exits_ps: Some(3.0),
            aterm_starts_ps: Some(0.0),
        })
    );
}
