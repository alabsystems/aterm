// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust kernel and fold held to the island: the answer sheets over the island's grids, and
//! the mutants the island's runs refute.

use super::{Kernel, Trigger, digest_asks};

const ISLAND: &str = include_str!("../relay_digest_island.rs");

/// Bit `i` of `k` (the island's `flag`).
const fn flag(k: u32, i: u32) -> bool {
    (k >> i) & 1 == 1
}

/// The island's `kernelSheet`: the kernel over (differs, steady, follows, asked, off) as bits
/// 4..0 of 0..31.
fn kernel_sheet(k: Kernel) -> Vec<u64> {
    (0..32)
        .map(|c| {
            u64::from(k(
                flag(c, 4),
                flag(c, 3),
                flag(c, 2),
                flag(c, 1),
                flag(c, 0),
            ))
        })
        .collect()
}

/// An event of the island's `Ev`: a row (the neighbour's digest of one store, this Mac's own
/// latest row's, 0 where unknown, and whether the relay follows), or an exchange that failed.
#[derive(Clone, Copy)]
enum Ev {
    Row(u8, u8, bool),
    Failed,
}

const IN_STEP: Ev = Ev::Row(1, 1, true);
const APART: Ev = Ev::Row(2, 1, true);
const UNKNOWN: Ev = Ev::Row(0, 1, true);
const OTHER: Ev = Ev::Row(1, 2, true);

/// The island's `letter`.
const fn letter(c: usize) -> Ev {
    match c {
        0 => IN_STEP,
        1 => APART,
        2 => UNKNOWN,
        3 => OTHER,
        _ => Ev::Failed,
    }
}

/// How a run reads a failed exchange: as the trigger does, or (a mutant) not at all.
type OnFailed = fn(&mut Trigger<u8>);

/// The island's `runM` under the kernel `k`: each event's answer, the fold's memory carried on.
fn run(k: Kernel, on_failed: OnFailed, trace: &[Ev]) -> Vec<bool> {
    let known = |d: u8| (d != 0).then_some(d);
    let mut trigger = Trigger::default();
    trace
        .iter()
        .map(|event| match *event {
            Ev::Row(theirs, mine, follows) => {
                trigger.step(k, known(theirs), known(mine), follows, false)
            }
            Ev::Failed => {
                on_failed(&mut trigger);
                false
            }
        })
        .collect()
}

/// The island's `foldSheet`: trace k (0..124) is apart, then letters `k % 5`, `(k / 5) % 5`,
/// `(k / 25) % 5`; its asks are bits 0..3 of a nibble, sixteen traces to a word, trace 16w's
/// nibble first, the last word's tail 0.
fn fold_sheet(k: Kernel, on_failed: OnFailed) -> Vec<u64> {
    let nibble = |t: usize| -> u64 {
        if t >= 125 {
            return 0;
        }
        let trace = [
            APART,
            letter(t % 5),
            letter((t / 5) % 5),
            letter((t / 25) % 5),
        ];
        run(k, on_failed, &trace)
            .iter()
            .enumerate()
            .fold(0, |nib, (i, &ask)| nib | (u64::from(ask) << i))
    };
    (0..8)
        .map(|w| (0..16).fold(0, |word, n| (word << 4) | nibble(16 * w + n)))
        .collect()
}

/// The numbers the island proves `<name> = [..]`, by `rfl`.
fn island_sheet(name: &str) -> Option<Vec<u64>> {
    let stated = format!("theorem {name} : ");
    let rest = &ISLAND[ISLAND.find(&stated)? + stated.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|n| {
            let n = n.trim();
            n.strip_prefix("0x")
                .map_or_else(|| n.parse().ok(), |hex| u64::from_str_radix(hex, 16).ok())
        })
        .collect()
}

#[test]
fn the_island_states_its_grids() {
    assert!(ISLAND.contains("(List.range 32)"));
    assert!(ISLAND.contains(
        "def traceOf (k : Nat) : List Ev := [apart, letter (k % 5), letter ((k / 5) % 5), letter ((k / 25) % 5)]"
    ));
    assert!(ISLAND.contains("def foldSheet : List Nat := sheetFrom 0 8"));
    assert_eq!(island_sheet("kernel_sheet_is").map(|s| s.len()), Some(32));
    assert_eq!(island_sheet("fold_sheet_is").map(|s| s.len()), Some(8));
}

#[test]
fn answer_sheets_match_the_island() {
    assert_eq!(
        island_sheet("kernel_sheet_is"),
        Some(kernel_sheet(digest_asks)),
        "relay_digest.rs's kernel and relay_digest_island.rs's spec disagree"
    );
    let computed = fold_sheet(digest_asks, Trigger::failed);
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:016x}")).collect();
    assert!(
        island_sheet("fold_sheet_is").as_ref() == Some(&computed),
        "the Rust trigger answers otherwise than the island's fold on the grid; the words it \
         computes are\n{}",
        words.join(", ")
    );
}

/// The island's mutants (section 3), each a decision one of its runs refutes; and the fold's
/// own, a trigger that keeps what it asked through a failed exchange.
#[test]
fn the_sheets_see_every_mutant() {
    let kernel = island_sheet("kernel_sheet_is").expect("the island states its kernel sheet");
    let fold = island_sheet("fold_sheet_is").expect("the island states its fold sheet");
    let mutants: [(&str, Kernel); 6] = [
        ("inStepAsks", |_, s, f, a, o| s && f && !a && !o),
        ("restless", |d, _, f, a, o| d && f && !a && !o),
        ("followsBlind", |d, s, _, a, o| d && s && !a && !o),
        ("repeats", |d, s, f, _, o| d && s && f && !o),
        ("deaf", |_, _, _, _, _| false),
        ("controlBlind", |d, s, f, a, _| d && s && f && !a),
    ];
    for (name, mutant) in mutants {
        assert_ne!(
            kernel_sheet(mutant),
            kernel,
            "the kernel sheet misses the mutant {name}"
        );
    }
    // The fold's grid holds the relay following and the control off, so it sees the four
    // mutants that decide otherwise there.
    for (name, mutant) in mutants {
        if !matches!(name, "followsBlind" | "controlBlind") {
            assert_ne!(
                fold_sheet(mutant, Trigger::failed),
                fold,
                "the fold sheet misses the mutant {name}"
            );
        }
    }
    assert_ne!(
        fold_sheet(digest_asks, |_| {}),
        fold,
        "the fold sheet misses a trigger that keeps its ask through a failure"
    );
}

/// The island's named runs, on the Rust trigger.
#[test]
fn the_islands_runs_hold_of_the_trigger() {
    let spec = |trace: &[Ev]| run(digest_asks, Trigger::failed, trace);
    assert_eq!(spec(&[APART, APART, APART]), [false, true, false]);
    assert_eq!(
        spec(&[APART, APART, Ev::Failed, APART]),
        [false, true, false, true]
    );
    assert_eq!(
        spec(&[APART, APART, OTHER, OTHER]),
        [false, true, false, true]
    );
    assert_eq!(
        spec(&[
            IN_STEP,
            IN_STEP,
            UNKNOWN,
            UNKNOWN,
            Ev::Row(2, 1, false),
            Ev::Row(2, 1, false)
        ]),
        [false; 6]
    );
    let mut off = Trigger::default();
    assert!(!off.row(Some(2u8), Some(1), true, true));
    assert!(!off.row(Some(2), Some(1), true, true));
}
