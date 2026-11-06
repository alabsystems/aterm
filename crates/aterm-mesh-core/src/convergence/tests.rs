// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust folds held to the island: the answer sheet over the island's grid, the mutants the
//! grid must see, every arrival order of each grid register read alike, and the island's
//! concrete registers read back in Rust.

use super::*;

/// The island file, read as text on every target (stock and cross builds included).
const ISLAND: &str = include_str!("../convergence_island.rs");

/// The island's grid size (`regSheetFrom 0 64`, eight views to a word).
const GRID: usize = 512;

/// The island's `shapeOrd`: shape `sh`'s causal order on positions.
fn shape_before(sh: usize, a: usize, b: usize) -> bool {
    a < b
        && match sh {
            0 => true,
            1 => !(a == 1 && b == 2),
            2 => a == 0,
            3 => !(b == 3 && (a == 1 || a == 2)),
            4 => !(a == 0 && b == 1),
            5 => !(a == 2 && b == 3),
            6 => false,
            _ => !((a == 0 || a == 1) && b == 2),
        }
}

/// The island's `ridP`: numbering `q` of the record ids, each its own inverse.
fn rid_p(q: usize, i: usize) -> usize {
    match q {
        0 => i,
        1 => 3 - i,
        2 if i % 2 == 0 => i + 1,
        2 => i - 1,
        _ => (i + 2) % 4,
    }
}

/// The island's `kindOf`: op `i`'s kind in view `k` (0, 1, 3 a `Put` of A, B, C; 2 a `Delete`).
fn kind_of(k: usize, i: usize) -> usize {
    match i {
        1 => k % 4,
        2 => (k / 4) % 4,
        3 => (k / 16) % 4,
        _ => 0,
    }
}

/// The island's `gridQ`: view `k`'s numbering, turning with the kinds and the shape.
const fn grid_q(k: usize) -> usize {
    (k + k / 64) % 4
}

/// View `k`'s kinds and shape under numbering `q`: its ops in position order, its shape and its
/// numbering. The island's `gridReg k` is `grid(k, grid_q(k))`.
fn grid(k: usize, q: usize) -> (Vec<Op>, usize, usize) {
    let ops = (0..4)
        .map(|i| Op {
            rid: rid_p(q, i),
            class: kind_of(k, i),
            delete: kind_of(k, i) == 2,
        })
        .collect();
    (ops, (k / 64) % 8, q)
}

/// The causal order of a view of shape `sh` under numbering `q`, on record ids.
fn grid_before(sh: usize, q: usize) -> impl Fn(usize, usize) -> bool {
    move |a, b| shape_before(sh, rid_p(q, a), rid_p(q, b))
}

/// The island's `regByte k`, under the kernels `k`.
fn grid_byte(view: usize, kernels: Kernels) -> u8 {
    let (ops, sh, q) = grid(view, grid_q(view));
    let r = Register::with_kernels(&ops, grid_before(sh, q), kernels);
    let winner = r.winner().map_or(0, |w| ops[w].rid + 1);
    let bits = [
        (r.conflict(), 8),
        (r.carries(1), 16),
        (r.carries(2), 32),
        (r.carries(3), 64),
        (r.is_sibling(0), 128),
    ];
    let byte = bits
        .iter()
        .fold(winner, |byte, &(on, v)| byte + if on { v } else { 0 });
    u8::try_from(byte).expect("a view's answer fits a byte")
}

/// The island's `regSheet`: eight views to a word, view `8w`'s byte first.
fn sheet(kernels: Kernels) -> Vec<u64> {
    (0..GRID / 8)
        .map(|w| {
            (0..8).fold(0, |word, n| {
                (word << 8) | u64::from(grid_byte(8 * w + n, kernels))
            })
        })
        .collect()
}

/// The words the island proves `regSheet` equal to, by `rfl`.
fn island_sheet() -> Option<Vec<u64>> {
    const STATED: &str = "theorem reg_sheet_is : regSheet =";
    let rest = &ISLAND[ISLAND.find(STATED)? + STATED.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|word| u64::from_str_radix(word.trim().strip_prefix("0x")?, 16).ok())
        .collect()
}

#[test]
fn the_island_states_its_grid_size() {
    assert!(ISLAND.contains(&format!(
        "def regSheet : List Nat := regSheetFrom 0 {}",
        GRID / 8
    )));
}

#[test]
fn answer_sheet_matches_the_island() {
    let computed = sheet(KERNELS);
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:016x}")).collect();
    assert!(
        island_sheet().as_ref() == Some(&computed),
        "the Rust folds answer otherwise than the island on the grid; the words they compute are\n{}",
        words.join(", ")
    );
}

/// The first sibling seen wins: no sibling beats another, so arrival order decides.
const FIRST_SEEN: Kernels = Kernels {
    beats: |_, _, _| false,
    ..KERNELS
};

/// The rules a register must not read, each one kernel changed.
const MUTANTS: [(&str, Kernels); 5] = [
    ("first sibling seen wins", FIRST_SEEN),
    (
        "the winner order ignores the rep",
        Kernels {
            beats: |_, _, own_less| own_less,
            ..KERNELS
        },
    ),
    (
        "a carry replaces every op in its past",
        Kernels {
            replaces: |below, _, _| below,
            ..KERNELS
        },
    ),
    (
        "no op takes a carry out",
        Kernels {
            drops: |_, _, _| false,
            ..KERNELS
        },
    ),
    (
        "a Delete carries a Delete winner beside a Put",
        Kernels {
            carries: |_, has_winner, winner_same, _| has_winner && winner_same,
            ..KERNELS
        },
    ),
];

#[test]
fn the_grid_sees_every_replaced_rule() {
    let island = island_sheet().expect("the island states its answer sheet");
    for (name, mutant) in MUTANTS {
        assert_ne!(
            sheet(mutant),
            island,
            "the grid cannot tell the rule \"{name}\" apart"
        );
    }
}

/// What a register reads, by record id: the winner, the conflict, and each record's sibling,
/// carry and rep.
type Reading = (Option<usize>, bool, Vec<(usize, bool, bool, usize)>);

fn reading<B: Fn(usize, usize) -> bool>(ops: &[Op], r: &Register<'_, B>) -> Reading {
    let mut by_rid: Vec<(usize, bool, bool, usize)> = (0..ops.len())
        .map(|i| (ops[i].rid, r.is_sibling(i), r.carries(i), r.rep(i)))
        .collect();
    by_rid.sort_unstable();
    (r.winner().map(|w| ops[w].rid), r.conflict(), by_rid)
}

/// The permutations of `0..n`, by Heap's algorithm.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut all = vec![order.clone()];
    let mut counters = vec![0; n];
    let mut at = 0;
    while at < n {
        if counters[at] < at {
            if at % 2 == 0 {
                order.swap(0, at);
            } else {
                order.swap(counters[at], at);
            }
            all.push(order.clone());
            counters[at] += 1;
            at = 0;
        } else {
            counters[at] = 0;
            at += 1;
        }
    }
    all
}

/// How many of the grid's registers, under each of the four numberings, some arrival order (an
/// order of their ops that lists each record after its causal past) reads otherwise than the
/// grid's own order, under the kernels `k`.
fn arrival_disagreements(kernels: Kernels) -> (usize, usize) {
    let orders = permutations(4);
    let (mut orders_read, mut wrong) = (0, 0);
    for (view, q) in (0..GRID).flat_map(|view| (0..4).map(move |q| (view, q))) {
        let (ops, sh, q) = grid(view, q);
        let before = grid_before(sh, q);
        let want = reading(&ops, &Register::with_kernels(&ops, &before, kernels));
        let mut differs = false;
        for order in &orders {
            let arrived: Vec<Op> = order.iter().map(|&i| ops[i]).collect();
            let causal =
                (0..4).all(|a| (0..4).all(|b| !before(arrived[b].rid, arrived[a].rid) || b < a));
            if !causal {
                continue;
            }
            orders_read += 1;
            let got = reading(
                &arrived,
                &Register::with_kernels(&arrived, &before, kernels),
            );
            differs |= got != want;
        }
        wrong += usize::from(differs);
    }
    (orders_read, wrong)
}

/// Law 5 on the Rust fold: every arrival order of each of the grid's 512 registers under each
/// numbering of its records (each order of its four ops that lists a record after its causal
/// past) reads the same winner, conflict, siblings, carries and reps.
/// NEGATIVE CONTROL: a register whose first sibling seen wins reads some arrival orders apart.
#[test]
fn every_arrival_order_of_a_grid_register_reads_it_alike() {
    let (orders, wrong) = arrival_disagreements(KERNELS);
    assert!(
        orders > 8 * GRID,
        "the grid's shapes have many arrival orders: {orders}"
    );
    assert_eq!(
        wrong, 0,
        "a grid register reads otherwise in another arrival order"
    );
    let (_, control) = arrival_disagreements(FIRST_SEEN);
    assert!(control > 0, "the first-seen mutant goes unnoticed");
}

/// The island's `macA` and `macB`: two concurrent `Put`s (records 5 and 9) of one name on a
/// `Put` of another value (record 7), received in either order.
#[test]
fn concurrent_edits_read_alike_in_either_arrival_order() {
    let before = |a: usize, b: usize| a == 7 && b != 7;
    let op = |rid, class| Op {
        rid,
        class,
        delete: false,
    };
    let mac_a = [op(7, 2), op(5, 0), op(9, 1)];
    let mac_b = [op(7, 2), op(9, 1), op(5, 0)];
    for ops in [&mac_a, &mac_b] {
        let r = Register::new(ops, before);
        assert_eq!(r.winner().map(|w| ops[w].rid), Some(5));
        assert!(r.conflict());
        let first = Register::with_kernels(ops, before, FIRST_SEEN);
        assert_eq!(
            first.winner().map(|w| ops[w].rid),
            Some(ops[1].rid),
            "the first sibling seen wins under the mutant"
        );
    }
}

/// The island's `inOrder`: a reseal (record 2) of record 1 beside an edit (record 3), and record
/// 4 of the edit's value on their merge: the edit took the reseal out of what 4's writer held,
/// so 4 carries the edit.
#[test]
fn a_write_on_a_reseal_beside_an_edit_carries_the_edit() {
    let before = |a: usize, b: usize| (a == 1 && b != 1) || (b == 4 && (a == 2 || a == 3));
    let op = |rid, class| Op {
        rid,
        class,
        delete: false,
    };
    let ops = [op(1, 0), op(2, 0), op(3, 1), op(4, 1)];
    let r = Register::new(&ops, before);
    assert!(r.carries(1) && !r.carries(2) && r.carries(3));
    assert_eq!(r.rep(3), 3);
    assert_eq!(r.winner(), Some(3));
    assert!(!r.conflict());
}
