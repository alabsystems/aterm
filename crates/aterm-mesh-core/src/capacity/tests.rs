// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kernels held to the island: the answer sheet over the island's grid, and the replaced
//! rules the grid must see.

use super::*;

/// The island file, read as text on every target (stock and cross builds included).
const ISLAND: &str = include_str!("../capacity_island.rs");

/// The island's `gridValue`: the points where a sum or a difference of u64s can wrap.
const GRID: [u64; 6] = [0, 1, 2, 3, 1 << 63, u64::MAX];

/// The island's sheet size: 1,306 bits in 21 words of 64.
const WORDS: usize = 21;

/// What one answer sheet reads: the room, the stop and the writer's wait, the kernels' or a
/// replaced rule's.
struct Rule {
    room: fn(u64, u64, u64, u64) -> bool,
    judges: fn(bool) -> bool,
    waits: fn(bool, bool, bool) -> bool,
}

const SHIPPED: Rule = Rule {
    room: keeps_room,
    judges: stop_judges,
    waits: writer_waits,
};

/// The island's `bitWith` for bit `i`, under `rule`.
fn bit(i: usize, rule: &Rule) -> bool {
    let odd = |k: usize| k % 2 == 1;
    if i < 1296 {
        (rule.room)(
            GRID[i / 216],
            GRID[(i / 36) % 6],
            GRID[(i / 6) % 6],
            GRID[i % 6],
        )
    } else if i < 1298 {
        (rule.judges)(odd(i - 1296))
    } else if i < 1306 {
        let k = i - 1298;
        (rule.waits)(odd(k / 4), odd(k / 2), odd(k))
    } else {
        false
    }
}

/// The island's `sheet`: 64 bits to a word, the lowest bit index highest.
fn sheet(rule: &Rule) -> Vec<u64> {
    (0..WORDS)
        .map(|w| (0..64).fold(0, |word, n| (word << 1) | u64::from(bit(64 * w + n, rule))))
        .collect()
}

/// The words the island proves `sheet` equal to, by `decide`.
fn island_sheet() -> Option<Vec<u64>> {
    const STATED: &str = "theorem sheet_is : sheet =";
    let rest = &ISLAND[ISLAND.find(STATED)? + STATED.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|word| u64::from_str_radix(word.trim().strip_prefix("0x")?, 16).ok())
        .collect()
}

#[test]
fn the_island_states_its_grid() {
    assert!(ISLAND.contains(&format!(
        "def sheet : List Nat := sheetWith spec_keeps_room 0 {WORDS}"
    )));
    assert!(ISLAND.contains("theorem keeps_room_means : KeepsRoomMeans := kr_means_at"));
    for value in ["9223372036854775808", "18446744073709551615"] {
        assert!(ISLAND.contains(value), "the grid takes {value}");
    }
}

#[test]
fn answer_sheet_matches_the_island() {
    let computed = sheet(&SHIPPED);
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:x}")).collect();
    assert!(
        island_sheet().as_ref() == Some(&computed),
        "the Rust kernels answer otherwise than the island on the grid; the words they compute \
         are\n{}",
        words.join(", ")
    );
}

/// Each rule PF6 replaced, or one a careless edit would make, answers otherwise somewhere on the
/// grid: a sum that wraps, a room that forgets `need` (a heal before F4), a stop that judges the
/// re-keys writers wait on (`STOP_HOLDS_REMOVALS`), and a writer that waits on a rotation it
/// minted itself.
#[test]
fn the_grid_sees_every_replaced_rule() {
    let island = island_sheet().expect("the island states its answer sheet");
    let mutants = [
        (
            "a wrapping sum",
            Rule {
                room: |held, cost, need, limit| held.wrapping_add(cost).wrapping_add(need) <= limit,
                ..SHIPPED
            },
        ),
        (
            "a room without need",
            Rule {
                room: |held, cost, _, limit| held <= limit && cost <= limit - held,
                ..SHIPPED
            },
        ),
        (
            "a stop that judges every re-key",
            Rule {
                judges: |_| true,
                ..SHIPPED
            },
        ),
        (
            "a writer that waits on its own rotation",
            Rule {
                waits: |waits_for_key, _, rotation_due| waits_for_key && rotation_due,
                ..SHIPPED
            },
        ),
    ];
    for (name, mutant) in mutants {
        assert_ne!(sheet(&mutant), island, "the grid misses {name}");
    }
}

#[test]
fn the_room_holds_at_the_bound_and_refuses_past_it() {
    assert!(keeps_room(12_000, 144, 144, 12_288));
    assert!(!keeps_room(12_000, 145, 144, 12_288));
    assert!(
        !keeps_room(u64::MAX, 1, 0, u64::MAX),
        "no sum wraps past the bound"
    );
    assert!(
        !keeps_room(1, 0, 0, 0),
        "a log past its bound takes nothing"
    );
}
