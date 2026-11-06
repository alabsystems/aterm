// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust folds held to the island: the answer sheet over the island's grid, the mutants the
//! grid must see, and the island's concrete views read back in Rust.

use super::*;

/// The island file, read as text on every target (stock and cross builds included).
const ISLAND: &str = include_str!("../revocation_island.rs");

/// The island's `gridBefore`: `a < b`, less the pairs the shape makes concurrent.
fn grid_before(shape: usize, a: usize, b: usize) -> bool {
    a < b && !(shape % 2 == 1 && a == 3 && b == 4) && !(shape / 2 == 1 && a == 5 && b == 6)
}

/// The island's `gridTable k`, as rows, `Remove`s and the causal order's shape.
fn grid(k: usize) -> (Vec<Row>, Vec<Remove>, usize) {
    let bit = |shift: u32| (k >> shift) & 1;
    let rows = vec![
        Row {
            actor: 0,
            recipient: 0,
            entry: 0,
        },
        Row {
            actor: bit(0),
            recipient: bit(1),
            entry: 2 * ((k / 4) % 4),
        },
    ];
    let config = (k / 16) % 4;
    let removes_n = match config {
        0 => 0,
        1 => 1,
        _ => 2,
    };
    let removes = (0..removes_n)
        .map(|j| Remove {
            actor: if j == 0 { bit(6) } else { bit(7) },
            record: if j == 0 { 3 } else { 5 },
            live: !(j == 1 && config == 3),
            quorum: j == 0 && bit(10) == 1,
        })
        .collect();
    (rows, removes, (k / 256) % 4)
}

/// A view of the grid, its causal order behind a reference.
type View<'a> = Table<'a, &'a dyn Fn(usize, usize) -> bool>;

/// A reading of whether a recipient is revoked: the decision's, or a mutant's.
type Revoked = fn(&View<'_>, usize) -> bool;

/// What one answer sheet reads of a view: the island's `revoked`, `current`, `gate` and
/// `revokes`.
struct Reading {
    revoked: Revoked,
    current: fn(&View<'_>, usize) -> bool,
    gate: fn(&View<'_>, usize) -> bool,
    revokes: fn(&View<'_>, usize) -> bool,
}

const DECISION: Reading = Reading {
    revoked: |t, r| t.revoked(r),
    current: |t, r| t.current(r),
    gate: |t, e| t.gate(e),
    revokes: |t, i| t.revokes(i),
};

/// The island's `gridByte` of view `k`, under `reading`.
fn grid_byte(k: usize, reading: &Reading) -> u8 {
    let (rows, removes, shape) = grid(k);
    let before = move |a: usize, b: usize| grid_before(shape, a, b);
    let before: &dyn Fn(usize, usize) -> bool = &before;
    let t = Table::new(&rows, &removes, before);
    let bits = [
        (reading.revoked)(&t, 0),
        (reading.revoked)(&t, 1),
        (reading.current)(&t, 0),
        (reading.current)(&t, 1),
        (reading.gate)(&t, 4),
        (reading.gate)(&t, 7),
        (reading.revokes)(&t, 0),
        (reading.revokes)(&t, 1),
    ];
    bits.iter()
        .enumerate()
        .fold(0, |byte, (n, &on)| byte | (u8::from(on) << n))
}

/// The island's `sheet`: eight views to a word, view `8w`'s byte first.
fn sheet(reading: &Reading) -> Vec<u64> {
    (0..GRID / 8)
        .map(|w| {
            (0..8).fold(0, |word, n| {
                (word << 8) | u64::from(grid_byte(8 * w + n, reading))
            })
        })
        .collect()
}

/// The island's grid size (`sheetFrom 0 256`, eight views to a word).
const GRID: usize = 2048;

/// The words the island proves `sheet` equal to, by `rfl`.
fn island_sheet() -> Option<Vec<u64>> {
    const STATED: &str = "theorem sheet_is : sheet =";
    let rest = &ISLAND[ISLAND.find(STATED)? + STATED.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|word| u64::from_str_radix(word.trim().strip_prefix("0x")?, 16).ok())
        .collect()
}

#[test]
fn the_island_states_its_grid_size() {
    assert!(ISLAND.contains(&format!("def sheet : List Nat := sheetFrom 0 {}", GRID / 8)));
}

#[test]
fn answer_sheet_matches_the_island() {
    let computed = sheet(&DECISION);
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:016x}")).collect();
    assert!(
        island_sheet().as_ref() == Some(&computed),
        "the Rust folds answer otherwise than the island on the grid; the words they compute are\n{}",
        words.join(", ")
    );
}

/// The rule before A15: a removed seat is skipped once another seat that is not out entered
/// after every `Remove` that took it (eval.rs `superseded_by`).
fn revoked_pre_a15(t: &View<'_>, r: usize) -> bool {
    let superseded = |i: usize| {
        (0..t.rows().len()).any(|m| m != i && !t.is_out(m) && t.takers_before(i, t.rows()[m].entry))
    };
    (0..t.rows().len()).any(|i| t.rows()[i].recipient == r && !superseded(i) && t.revokes(i))
}

/// A15's loop: any seat that is not out and binds the recipient keeps it, whenever it entered.
fn revoked_any_holder(t: &View<'_>, r: usize) -> bool {
    let rows = t.rows();
    (0..rows.len()).any(|i| {
        rows[i].recipient == r
            && row_revokes(
                t.is_out(i),
                (0..rows.len()).any(|m| rows[m].recipient == rows[i].recipient && !t.is_out(m)),
            )
    })
}

/// eval.rs branch (a): a seat of the same recipient whose entry has a higher index is newer.
fn revoked_index_order(t: &View<'_>, r: usize) -> bool {
    let rows = t.rows();
    (0..rows.len()).any(|i| {
        rows[i].recipient == r
            && row_revokes(
                t.is_out(i),
                (0..rows.len()).any(|m| {
                    rows[m].recipient == rows[i].recipient
                        && !t.is_out(m)
                        && rows[i].entry < rows[m].entry
                }),
            )
    })
}

/// PF1's first gate: every `Remove` that took a revoking row lies in the epoch's past, whether
/// or not a live row binds its recipient.
fn gate_every_taker(t: &View<'_>, e: usize) -> bool {
    (0..t.rows().len()).all(|i| !t.revokes(i) || t.takers_before(i, e))
}

#[test]
fn the_grid_sees_every_replaced_rule() {
    let island = island_sheet().expect("the island states its answer sheet");
    let mutants = [
        (
            "pre-A15 superseded skip",
            Reading {
                revoked: revoked_pre_a15,
                ..DECISION
            },
        ),
        (
            "any holder keeps it",
            Reading {
                revoked: revoked_any_holder,
                ..DECISION
            },
        ),
        (
            "index order",
            Reading {
                revoked: revoked_index_order,
                ..DECISION
            },
        ),
        (
            "every-taker gate",
            Reading {
                gate: gate_every_taker,
                ..DECISION
            },
        ),
    ];
    for (name, mutant) in mutants {
        assert_ne!(
            sheet(&mutant),
            island,
            "the grid cannot tell the {name} rule apart"
        );
    }
}

/// The island's tables of section 9, read back in Rust.
fn table<'a>(
    rows: &'a [Row],
    removes: &'a [Remove],
    before: &'a dyn Fn(usize, usize) -> bool,
) -> View<'a> {
    Table::new(rows, removes, before)
}

const fn row(actor: usize, recipient: usize, entry: usize) -> Row {
    Row {
        actor,
        recipient,
        entry,
    }
}

const fn remove(actor: usize, record: usize) -> Remove {
    Remove {
        actor,
        record,
        live: true,
        quorum: false,
    }
}

#[test]
fn a_moved_seats_old_recipient_stays_revoked() {
    let rows = [row(0, 0, 0), row(1, 1, 2)];
    let removes = [remove(0, 1)];
    let total: &dyn Fn(usize, usize) -> bool = &|a, b| a < b;
    let t = table(&rows, &removes, total);
    assert!(t.revoked(0) && t.current(1) && !t.current(0));
    assert!(!revoked_pre_a15(&t, 0), "the rule before A15 misses it");
    assert!(!t.gate(0), "an epoch before the Remove keeps the gate shut");
    assert!(t.gate(2), "an epoch after it opens the gate");
}

#[test]
fn a_co_holder_that_entered_before_the_remove_does_not_keep_it() {
    let rows = [row(0, 0, 0), row(1, 0, 1)];
    let removes = [remove(0, 2)];
    let total: &dyn Fn(usize, usize) -> bool = &|a, b| a < b;
    let t = table(&rows, &removes, total);
    assert!(t.revoked(0));
    assert!(!revoked_any_holder(&t, 0));
}

#[test]
fn an_admit_concurrent_with_the_remove_does_not_restore() {
    let rows = [row(0, 0, 0), row(1, 0, 2)];
    let removes = [remove(0, 1)];
    let concurrent: &dyn Fn(usize, usize) -> bool = &|a, b| a == 0 && b != 0;
    let t = table(&rows, &removes, concurrent);
    assert!(t.revoked(0));
    assert!(!revoked_index_order(&t, 0));
}

#[test]
fn either_arrival_order_gives_one_answer() {
    // Two seats of recipient 0 entered concurrently (records 1 and 2); a Remove of the first
    // (record 3) follows both. Numbered in either arrival order, the decision answers the same;
    // the index rule does not.
    let racing: &dyn Fn(usize, usize) -> bool = &|a, b| a < b && !(a == 1 && b == 2);
    let swap = |x: usize| match x {
        1 => 2,
        2 => 1,
        x => x,
    };
    let swapped: &dyn Fn(usize, usize) -> bool = &move |a, b| racing(swap(a), swap(b));
    let removes = [remove(0, 3)];
    let first = [row(0, 0, 1), row(1, 0, 2)];
    let second = [row(0, 0, 2), row(1, 0, 1)];
    let (a, b) = (
        table(&first, &removes, racing),
        table(&second, &removes, swapped),
    );
    assert_eq!((a.revoked(0), a.current(0)), (true, false));
    assert_eq!((b.revoked(0), b.current(0)), (true, false));
    assert_ne!(revoked_index_order(&a, 0), revoked_index_order(&b, 0));
}

#[test]
fn a_quorum_removal_is_out_for_good_and_a_void_lifts_a_removal() {
    let total: &dyn Fn(usize, usize) -> bool = &|a, b| a < b;
    // Actor 0 removed by a quorum (record 1), admitted again under the same actor (record 2).
    let rows = [row(0, 0, 0), row(0, 1, 2)];
    let quorum = [Remove {
        quorum: true,
        ..remove(0, 1)
    }];
    let t = table(&rows, &quorum, total);
    assert!(t.is_out(1) && t.revoked(1) && !t.current(1));
    let plain = [remove(0, 1)];
    assert!(
        table(&rows, &plain, total).current(1),
        "a plain Remove is not"
    );
    let void = [Remove {
        live: false,
        ..remove(0, 1)
    }];
    let lifted = table(&rows[..1], &void, total);
    assert!(!lifted.revoked(0) && lifted.current(0));
}

#[test]
fn one_concurrent_remove_of_a_recipient_no_live_row_binds_leaves_the_gate_open() {
    // The island's `twice`: one row (entry 0), taken by two `Remove`s (records 1 and 2); the
    // epoch (record 3) has record 1 in its past and is concurrent with record 2.
    let rows = [row(0, 0, 0)];
    let removes = [remove(0, 1), remove(0, 2)];
    let racing: &dyn Fn(usize, usize) -> bool = &|a, b| a < b && !(a == 2 && b == 3);
    let t = table(&rows, &removes, racing);
    assert!(t.revoked(0) && !t.bound_live(0));
    assert!(t.gate(3), "every Mac holding the epoch holds record 1");
    assert!(!gate_every_taker(&t, 3), "PF1's first gate called it due");
    assert!(!t.gate(0), "an epoch before both Removes is due");
}

#[test]
fn head_due_is_the_writers_rule() {
    assert!(head_due(false, false) && head_due(true, true) && !head_due(true, false));
    assert!(
        wraps_removed(true, true) && !wraps_removed(true, false) && !wraps_removed(false, true)
    );
}
