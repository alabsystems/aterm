// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust kernel held to the island: the answer sheet over the island's grid, the law over a
//! grid of offsets, and the mutants the island's counterexamples refute.

use super::{move_opens, shows_anew};

const ISLAND: &str = include_str!("../inbox_move_island.rs");

/// The island's `sheet`: the kernel over (may deliver, owed unmade) as bits 1..0 of 0..3.
fn sheet(opens: fn(bool, bool) -> bool) -> Vec<u8> {
    (0..4u8)
        .map(|k| u8::from(opens(k >> 1 & 1 == 1, k & 1 == 1)))
        .collect()
}

/// The numbers the island proves its answer sheet `name` equal to, by `rfl`.
fn island_sheet(name: &str) -> Option<Vec<u8>> {
    let stated = format!("theorem {name}_is : {name} =");
    let rest = &ISLAND[ISLAND.find(&stated)? + stated.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',').map(|n| n.trim().parse().ok()).collect()
}

/// Section 1's law over every offset below 6: where the old node's bridge, once it may deliver no
/// more, committed (`c`) or handed over (`o`) what it delivered (`d`), and a hand-over counts as
/// made once the commit reaches it, a move `opens` lets through resumes past `d`.
fn keeps_the_law(opens: fn(bool, bool) -> bool) -> bool {
    for may_deliver in [false, true] {
        for owed_unmade in [false, true] {
            for (d, c, o) in
                (0..6u8).flat_map(|d| (0..6u8).flat_map(move |c| (0..6u8).map(move |o| (d, c, o))))
            {
                let gone = may_deliver || d <= c || d <= o;
                let made = owed_unmade || o <= c;
                if gone && made && opens(may_deliver, owed_unmade) && d > c {
                    return false;
                }
            }
        }
    }
    true
}

#[test]
fn the_island_states_its_grid() {
    assert!(ISLAND.contains("(List.range 4)"), "the island's grid");
    assert_eq!(island_sheet("sheet").map(|s| s.len()), Some(4));
    assert_eq!(island_sheet("showSheet").map(|s| s.len()), Some(4));
}

#[test]
fn answer_sheet_matches_the_island() {
    assert_eq!(
        island_sheet("sheet"),
        Some(sheet(move_opens)),
        "inbox_move.rs's kernel and inbox_move_island.rs's spec disagree"
    );
    assert_eq!(
        island_sheet("showSheet"),
        Some(sheet(shows_anew)),
        "inbox_move.rs's endpoint kernel and inbox_move_island.rs's spec disagree"
    );
}

/// A move the kernel opens resumes past every record the old node's bridge delivered.
/// NEGATIVE CONTROL: the island's counterexample kernels (section 2), each dropping one input,
/// break the law, and the sheet tells each from the kernel.
#[test]
fn a_move_the_kernel_opens_resumes_past_every_delivery() {
    assert!(keeps_the_law(move_opens));
    let island = island_sheet("sheet").expect("the island states its answer sheet");
    let ungated: fn(bool, bool) -> bool = |_, owed_unmade| !owed_unmade;
    let unhanded: fn(bool, bool) -> bool = |may_deliver, _| !may_deliver;
    for (name, mutant) in [("ungated", ungated), ("unhanded", unhanded)] {
        assert!(
            ISLAND.contains(&format!("def {name} ")),
            "the island names {name}"
        );
        assert!(!keeps_the_law(mutant), "{name} keeps the law");
        assert_ne!(sheet(mutant), island, "the sheet misses {name}");
    }
}

/// Section 4's law: where the endpoint remembers exactly the mail it showed, by an offset the
/// next delivery of it names, `shows` shows each piece of mail once.
fn shows_once(shows: fn(bool, bool) -> bool) -> bool {
    [false, true].into_iter().all(|off_known| {
        [false, true].into_iter().all(|was_known| {
            let shown_before = off_known || was_known;
            shows(off_known, was_known) == !shown_before
        })
    })
}

/// The endpoint shows each piece of mail once across the move.
/// NEGATIVE CONTROL: the island's counterexample kernels (section 4), the offset-only one (what
/// the endpoint did before `was=`) and the origin-only one, break the law, and the sheet tells
/// each from the kernel.
#[test]
fn the_endpoint_shows_each_mail_once_across_the_move() {
    assert!(shows_once(shows_anew));
    let island = island_sheet("showSheet").expect("the island states the endpoint's sheet");
    let offset_only: fn(bool, bool) -> bool = |off_known, _| !off_known;
    let was_only: fn(bool, bool) -> bool = |_, was_known| !was_known;
    for (name, mutant) in [("offsetOnly", offset_only), ("wasOnly", was_only)] {
        assert!(
            ISLAND.contains(&format!("def {name} ")),
            "the island names {name}"
        );
        assert!(!shows_once(mutant), "{name} keeps the law");
        assert_ne!(sheet(mutant), island, "the sheet misses {name}");
    }
}
