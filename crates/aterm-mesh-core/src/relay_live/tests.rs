// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust kernels held to the island: the answer sheet over the island's grid, the mutants the
//! island's runs refute, and the codes the callers match on.

use super::{
    ASK_LINEAGE, ASK_NEW_LINK, ASK_NONE, GONE, KEPT, MOVED, asks_again, cursor, requests, runs,
    start_repair,
};

const ISLAND: &str = include_str!("../relay_live_island.rs");

/// The five decisions, as the sheet reads them.
struct Kernels {
    cursor: fn(bool, bool, bool, bool) -> u8,
    start_repair: fn(u8) -> u8,
    requests: fn(bool, bool) -> bool,
    runs: fn(bool, bool) -> bool,
    asks_again: fn(bool, bool) -> bool,
}

const DECISION: Kernels = Kernels {
    cursor,
    start_repair,
    requests,
    runs,
    asks_again,
};

/// Bit `i` of `k` (the island's `bit`).
const fn bit(k: u8, i: u8) -> bool {
    (k >> i) & 1 == 1
}

/// The island's `sheet`: the cursor over (has, fresh, same, continues) as bits 3..0 of 0..15,
/// the start over the codes 0..3, then requests, runs and asks_again over bits 1..0 of 0..3.
fn sheet(k: &Kernels) -> Vec<u8> {
    let mut out = Vec::new();
    for c in 0..16 {
        out.push((k.cursor)(bit(c, 3), bit(c, 2), bit(c, 1), bit(c, 0)));
    }
    for c in 0..4 {
        out.push((k.start_repair)(c));
    }
    for c in 0..4 {
        out.push(u8::from((k.requests)(bit(c, 1), bit(c, 0))));
    }
    for c in 0..4 {
        out.push(u8::from((k.runs)(bit(c, 1), bit(c, 0))));
    }
    for c in 0..4 {
        out.push(u8::from((k.asks_again)(bit(c, 1), bit(c, 0))));
    }
    out
}

/// The numbers the island proves `sheet` equal to, by `rfl`.
fn island_sheet() -> Option<Vec<u8>> {
    const STATED: &str = "theorem sheet_is : sheet =";
    let rest = &ISLAND[ISLAND.find(STATED)? + STATED.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',').map(|n| n.trim().parse().ok()).collect()
}

#[test]
fn the_island_states_its_grid() {
    for range in ["(List.range 16)", "(List.range 4)"] {
        assert!(ISLAND.contains(range), "the island's grid names {range}");
    }
    assert_eq!(island_sheet().map(|s| s.len()), Some(16 + 4 + 4 + 4 + 4));
}

#[test]
fn answer_sheet_matches_the_island() {
    assert_eq!(
        island_sheet(),
        Some(sheet(&DECISION)),
        "relay_live.rs's kernels and relay_live_island.rs's spec disagree"
    );
}

/// The island's mutants (section 7), each a decision one of its runs refutes, and one more the
/// gap rule's negative control takes: a gap notice under `NO_INVENTORY_ON_GAP` still asks.
#[test]
fn the_sheet_sees_every_mutant() {
    let island = island_sheet().expect("the island states its answer sheet");
    let kept_on_move = Kernels {
        cursor: |has, fresh, _, _| u8::from(has && fresh),
        ..DECISION
    };
    let moved_asks_nothing = Kernels {
        start_repair: |c| if c == 0 { 2 } else { 0 },
        ..DECISION
    };
    let gap_deaf = Kernels {
        requests: |_, _| false,
        ..DECISION
    };
    let gap_blind_to_control = Kernels {
        requests: |_, _| true,
        ..DECISION
    };
    let quiet_blind = Kernels {
        runs: |remembers, _| !remembers,
        ..DECISION
    };
    let no_second_ask = Kernels {
        asks_again: |_, _| false,
        ..DECISION
    };
    for (name, mutant) in [
        ("keptOnMove", kept_on_move),
        ("movedAsksNothing", moved_asks_nothing),
        ("gapDeaf", gap_deaf),
        ("gap blind to its control", gap_blind_to_control),
        ("quietBlind", quiet_blind),
        ("noSecondAsk", no_second_ask),
    ] {
        assert_ne!(sheet(&mutant), island, "the sheet misses the mutant {name}");
    }
}

/// The codes the callers match on are the kernels' own, and every code outside the cursor's
/// asks a new link's exchange (the island's `start_repair_refuses`).
#[test]
fn the_codes_are_the_kernels() {
    assert_eq!(cursor(true, true, true, false), KEPT);
    assert_eq!(cursor(true, true, false, true), MOVED);
    assert_eq!(cursor(true, false, true, true), GONE);
    assert_eq!(cursor(false, true, true, true), GONE);
    assert_eq!(start_repair(KEPT), ASK_NONE);
    assert_eq!(start_repair(MOVED), ASK_LINEAGE);
    assert_eq!(start_repair(GONE), ASK_NEW_LINK);
    for code in 3..=u8::MAX {
        assert_eq!(start_repair(code), ASK_NEW_LINK, "code {code}");
    }
}
