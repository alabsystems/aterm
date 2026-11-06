// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust folds held to the island: each answer sheet over the island's grid, recomputed from
//! the Rust and read from the island's text, and the replaced rules each grid must see.

use super::*;

/// The island file, read as text on every target (stock and cross builds included).
const ISLAND: &str = include_str!("../no_lost_edit_island.rs");

/// The words the island proves `<sheet>` equal to, by `rfl` (`theorem <sheet>_is : <sheet> =`).
fn island_words(sheet: &str) -> Option<Vec<u64>> {
    let stated = format!("theorem {sheet}_is : {sheet} =");
    let rest = &ISLAND[ISLAND.find(&stated)? + stated.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|word| u64::from_str_radix(word.trim().strip_prefix("0x")?, 16).ok())
        .collect()
}

/// The island's `pack b`: the first answer lowest.
fn pack(base: u64, answers: impl DoubleEndedIterator<Item = u64>) -> u64 {
    answers.rev().fold(0, |word, answer| answer + base * word)
}

/// The island's `wordsFrom b per f 0 count`.
fn words(base: u64, per: usize, count: usize, case: impl Fn(usize) -> u64) -> Vec<u64> {
    (0..count)
        .map(|w| pack(base, (w * per..(w + 1) * per).map(&case)))
        .collect()
}

/// Where the island does not state `sheet` as `computed`: the words the Rust computes, as the
/// island would state them.
fn mismatch(sheet: &str, computed: &[u64]) -> Option<String> {
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:016x}")).collect();
    (island_words(sheet).as_deref() != Some(computed))
        .then(|| format!("{sheet}: [{}]", words.join(", ")))
}

/// The island's `digit4 c x`: URL (or answer) x's two bits.
const fn digit4(c: usize, x: usize) -> usize {
    match x {
        0 => c % 4,
        1 => (c / 4) % 4,
        _ => (c / 16) % 4,
    }
}

/// A reading of the gate: the decision's, or a mutant's.
type GateRule = fn(bool, u64, u64, bool) -> Gate;

/// The island's `gateCase k`, under `gate`.
fn gate_case(k: usize, gate: GateRule) -> u64 {
    let held = ((k / 2) % 3 + 1) as u64;
    match gate(k % 2 == 1, held, 2, (k / 6) % 2 == 1) {
        Gate::Descends => 0,
        Gate::Waits => 1,
        Gate::Reach => 2,
    }
}

fn gate_sheet(gate: GateRule) -> Vec<u64> {
    vec![pack(4, (0..12).map(|k| gate_case(k, gate)))]
}

/// A reading of what a head leaves of a reach: the decision's, or a mutant's.
type ReachRule = fn(Option<&[(bool, bool)]>) -> Settle;

/// The island's `reachCase k`, under `settle`.
fn reach_case(k: usize, settle: ReachRule) -> u64 {
    let (len, combo) = match k {
        0 | 1 => (0, 0),
        2..=5 => (1, k - 2),
        6..=21 => (2, k - 6),
        _ => (3, k - 22),
    };
    let reach: Vec<(bool, bool)> = (0..len)
        .map(|x| (digit4(combo, x) % 2 == 1, digit4(combo, x) / 2 == 1))
        .collect();
    match settle((k != 0).then_some(&reach[..])) {
        Settle::Goes => 0,
        Settle::Stays => 1,
        Settle::Narrows(left) => 2 + left.iter().map(|&at| 1 << at).sum::<u64>(),
    }
}

fn reach_sheet(settle: ReachRule) -> Vec<u64> {
    words(16, 16, 6, |k| reach_case(k, settle))
}

/// A reading of a publish's answers: the decision's, or a mutant's.
type ForgetRule = fn(Vec<(bool, bool)>) -> bool;

/// The island's `forgetsCase k`, under `forgets`.
fn forgets_case(k: usize, forgets: ForgetRule) -> u64 {
    let (len, combo) = match k {
        0 => (0, 0),
        1..=4 => (1, k - 1),
        5..=20 => (2, k - 5),
        _ => (3, k - 21),
    };
    let answers = (0..len)
        .map(|x| (digit4(combo, x) % 2 == 1, digit4(combo, x) / 2 == 1))
        .collect();
    u64::from(forgets(answers))
}

fn forgets_sheet(forgets: ForgetRule) -> Vec<u64> {
    words(2, 64, 2, |k| forgets_case(k, forgets))
}

/// A reading of a pass: the per-write verdict and the pass's gate, the decision's or a mutant's.
struct PassRule {
    verdict: fn(bool, bool, bool, bool) -> Verdict,
    signs: fn(bool, bool, bool) -> bool,
}

const PASS: PassRule = PassRule {
    verdict: Verdict::of,
    signs: pass_signs,
};

fn verdict_sheet(rule: &PassRule) -> Vec<u64> {
    let bit = |k: usize, b: usize| (k >> b) & 1 == 1;
    let verdict = |k: usize| match (rule.verdict)(bit(k, 0), bit(k, 1), bit(k, 2), bit(k, 3)) {
        Verdict::Leaves => 0,
        Verdict::Done => 1,
        Verdict::Overtaken => 2,
        Verdict::Signs => 3,
    };
    vec![
        pack(4, (0..16).map(verdict)),
        pack(
            2,
            (0..8).map(|k| u64::from((rule.signs)(bit(k, 0), bit(k, 1), bit(k, 2)))),
        ),
    ]
}

/// A reading of a landing: the decision's, or a mutant's.
type LandRule = fn(&[(bool, &[usize])], usize) -> Landing;

/// The island's `landCase k`, under `land`: three sets, set `s`'s digit (base 8) its stored bit
/// and whether it writes name 0 and name 1.
fn land_case(k: usize, land: LandRule) -> u64 {
    let digit = |s: usize| (k / 8usize.pow(s as u32)) % 8;
    let names: Vec<Vec<usize>> = (0..3)
        .map(|s| {
            [(2, 0), (4, 1)]
                .into_iter()
                .filter(|&(b, _)| (digit(s) / b) % 2 == 1)
                .map(|(_, name)| name)
                .collect()
        })
        .collect();
    let sets: Vec<(bool, &[usize])> = (0..3).map(|s| (digit(s) % 2 == 1, &names[s][..])).collect();
    let landing = land(&sets, 2);
    let mut code = 0;
    for s in 0..3 {
        for (at, &name) in names[s].iter().enumerate() {
            if landing.keeps[s][at] {
                code |= 1 << (2 * s + name);
            }
        }
    }
    let waits = |name: usize| landing.waits[name].map_or(0, |s| s as u64 + 1);
    code + 64 * (waits(0) + 4 * waits(1))
}

fn land_sheet(land: LandRule) -> Vec<u64> {
    words(1024, 6, 86, |k| land_case(k, land))
}

/// A reading of a server's compare-and-set: the decision's, or a mutant's.
type TakeoverRule = fn(TakeoverFacts) -> Takeover;

/// The island's `takeoverCase k`, under `rule`: k's bits are the facts in `TakeoverFacts`' order.
fn takeover_case(k: usize, rule: TakeoverRule) -> u64 {
    let bit = |b: usize| (k >> b) & 1 == 1;
    let facts = TakeoverFacts {
        pin_ok: bit(0),
        replaced: bit(1),
        not_newer: bit(2),
        same_version: bit(3),
        chain_ok: bit(4),
        names_parents: bit(5),
        held_parented: bit(6),
        among_parents: bit(7),
    };
    match rule(facts) {
        Takeover::Takes => 0,
        Takeover::Holds => 1,
        Takeover::PinRefused => 2,
        Takeover::NotNewer => 3,
        Takeover::BreaksChain => 4,
        Takeover::OffParents => 5,
    }
}

fn takeover_sheet(rule: TakeoverRule) -> Vec<u64> {
    words(8, 16, 16, |k| takeover_case(k, rule))
}

/// The unversioned hold's two decisions over all four arguments each: the adoption's, then the
/// removal's, one bit per case (the island's kernels `spec_hold_adopts`, `spec_unversioned_goes`).
fn legacy_sheet(adopts: fn(bool, bool) -> bool, goes: fn(bool, bool) -> bool) -> Vec<u64> {
    let bit = |k: usize, b: usize| (k >> b) & 1 == 1;
    vec![pack(
        2,
        (0..8).map(|k| {
            let rule = if k < 4 { adopts } else { goes };
            u64::from(rule(bit(k, 0), bit(k, 1)))
        }),
    )]
}

/// The direct draft's three decisions over all their arguments, one bit each: the keep gate's
/// (its hold is there), the spine's settle (it drafted records, they were kept), then the
/// draft's refusal (it writes, a pending set does not open, a held draft does not read); the
/// island's `draftCase`.
fn draft_sheet(
    keeps: fn(bool) -> bool,
    settles: fn(bool, bool) -> bool,
    refuses: fn(bool, bool, bool) -> bool,
) -> Vec<u64> {
    let bit = |k: usize, b: usize| (k >> b) & 1 == 1;
    vec![pack(
        2,
        (0..14).map(|k| {
            u64::from(match k {
                0..=1 => keeps(k == 1),
                2..=5 => settles(bit(k - 2, 0), bit(k - 2, 1)),
                _ => refuses(bit(k - 6, 0), bit(k - 6, 1), bit(k - 6, 2)),
            })
        }),
    )]
}

#[test]
fn every_sheet_matches_the_island() {
    let mismatches: Vec<String> = [
        mismatch("gateSheet", &gate_sheet(Gate::of)),
        mismatch("reachSheet", &reach_sheet(settle_reach)),
        mismatch("forgetsSheet", &forgets_sheet(publish_forgets)),
        mismatch("verdictSheet", &verdict_sheet(&PASS)),
        mismatch("landSheet", &land_sheet(land)),
        mismatch("takeoverSheet", &takeover_sheet(Takeover::of)),
        mismatch("legacySheet", &legacy_sheet(hold_adopts, unversioned_goes)),
        mismatch(
            "draftSheet",
            &draft_sheet(drafted_keeps, spine_settles, draft_refuses),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    assert!(
        mismatches.is_empty(),
        "the Rust folds answer otherwise than the island; the words they compute are\n{}",
        mismatches.join("\n")
    );
}

/// Settling by sequence alone (W2 review 1): a head that passed the gate drops the hold whatever
/// servers it may have reached.
fn settle_by_sequence(reach: Option<&[(bool, bool)]>) -> Settle {
    reach.map_or(Settle::Stays, |_| Settle::Goes)
}

/// A reach unknown read as reaching none (a hold whose servers could not be written down goes).
fn unknown_as_none(reach: Option<&[(bool, bool)]>) -> Settle {
    settle_reach(Some(reach.unwrap_or(&[])))
}

#[test]
fn the_grids_see_every_replaced_rule() {
    let island = |sheet| island_words(sheet).expect("the island states its sheet");
    for (name, rule) in [
        ("settle by sequence alone", settle_by_sequence as ReachRule),
        ("an unknown reach read as none", unknown_as_none),
    ] {
        assert_ne!(
            reach_sheet(rule),
            island("reachSheet"),
            "the reach grid cannot tell the {name} rule apart"
        );
    }
    for (name, rule) in [
        (
            "a head no server took settles by reach",
            (|named, held, head, _| Gate::of(named, held, head, true)) as GateRule,
        ),
        (
            "a higher sequence settles by reach",
            |named, _, head, took| Gate::of(named, head, head, took),
        ),
    ] {
        assert_ne!(
            gate_sheet(rule),
            island("gateSheet"),
            "the gate grid cannot tell the {name} rule apart"
        );
    }
    let any_error_forgets: ForgetRule = |answers| answers.iter().all(|&(stored, _)| !stored);
    assert_ne!(
        forgets_sheet(any_error_forgets),
        island("forgetsSheet"),
        "the answers grid cannot tell a lost reply read as a refusal apart"
    );
    for (name, rule) in [
        (
            "a write signed over any value",
            PassRule {
                verdict: |deferred, done, _, take_back| {
                    Verdict::of(deferred, done, true, take_back)
                },
                ..PASS
            },
        ),
        (
            "a write of a name a pending set writes signed (race-undeferred)",
            PassRule {
                verdict: |_, done, stands, take_back| Verdict::of(false, done, stands, take_back),
                ..PASS
            },
        ),
        (
            "a take-back signed as a Delete",
            PassRule {
                verdict: |deferred, done, stands, _| Verdict::of(deferred, done, stands, false),
                ..PASS
            },
        ),
        (
            "a pass over a pending set that does not open (unread-as-none)",
            PassRule {
                signs: |due, opens, _| pass_signs(due, opens, true),
                ..PASS
            },
        ),
    ] {
        assert_ne!(
            verdict_sheet(&rule),
            island("verdictSheet"),
            "the pass grid cannot tell the {name} rule apart"
        );
    }
    let drop_all: LandRule = |sets, names| {
        let mut landing = land(sets, names);
        if sets.iter().any(|&(stored, _)| stored) {
            for keeps in &mut landing.keeps {
                keeps.fill(false);
            }
        }
        landing
    };
    let earliest_wins: LandRule = |sets, names| {
        let mut landing = land(sets, names);
        for (name, waits) in landing.waits.iter_mut().enumerate() {
            *waits = sets
                .iter()
                .position(|&(stored, writes)| stored && writes.contains(&name));
        }
        landing
    };
    for (name, rule) in [
        ("landing one set drops every other", drop_all),
        ("an earlier set's write over a later one's", earliest_wins),
    ] {
        assert_ne!(
            land_sheet(rule),
            island("landSheet"),
            "the land grid cannot tell the {name} rule apart"
        );
    }
    for (name, rule) in [
        (
            "a newer version taken whatever its parents",
            (|facts| {
                Takeover::of(TakeoverFacts {
                    among_parents: true,
                    ..facts
                })
            }) as TakeoverRule,
        ),
        ("a replaced pin judged by sequence", |facts| {
            Takeover::of(TakeoverFacts {
                replaced: false,
                ..facts
            })
        }),
        (
            "a version of the held sequence read as the version held",
            |facts| {
                Takeover::of(TakeoverFacts {
                    same_version: facts.not_newer,
                    ..facts
                })
            },
        ),
        ("a newer version taken without the held chain", |facts| {
            Takeover::of(TakeoverFacts {
                chain_ok: true,
                ..facts
            })
        }),
    ] {
        assert_ne!(
            takeover_sheet(rule),
            island("takeoverSheet"),
            "the takeover grid cannot tell the {name} rule apart"
        );
    }
    assert_ne!(
        legacy_sheet(hold_adopts, |reads, _| reads),
        island("legacySheet"),
        "the legacy grid cannot tell an unversioned hold removed before the store holds it apart"
    );
    for (name, sheet) in [
        (
            "a keep after its hold went (KEEPS_SUPERSEDED)",
            draft_sheet(|_| true, spine_settles, draft_refuses),
        ),
        (
            "a spine settled by a version whose records were not kept",
            draft_sheet(drafted_keeps, |_, _| true, draft_refuses),
        ),
        (
            "a held draft that does not read, read as none",
            draft_sheet(drafted_keeps, spine_settles, |writes, waiting, _| {
                writes && waiting
            }),
        ),
    ] {
        assert_ne!(
            sheet,
            island("draftSheet"),
            "the draft grid cannot tell {name} apart"
        );
    }
}

/// The six answers partition the facts (the island's `takeover_partitions`), so every refusal is
/// the one the code always gave first: the pin's, then the sequence's, the chain's and the
/// parents'.
#[test]
fn takeover_refuses_in_the_codes_order() {
    let facts = |pin_ok, not_newer, chain_ok, among_parents| TakeoverFacts {
        pin_ok,
        replaced: false,
        not_newer,
        same_version: false,
        chain_ok,
        names_parents: true,
        held_parented: true,
        among_parents,
    };
    assert_eq!(
        Takeover::of(facts(false, true, false, false)),
        Takeover::PinRefused
    );
    assert_eq!(
        Takeover::of(facts(true, true, false, false)),
        Takeover::NotNewer
    );
    assert_eq!(
        Takeover::of(facts(true, false, false, false)),
        Takeover::BreaksChain
    );
    assert_eq!(
        Takeover::of(facts(true, false, true, false)),
        Takeover::OffParents
    );
    assert_eq!(
        Takeover::of(facts(true, false, true, true)),
        Takeover::Takes
    );
    let replaced = TakeoverFacts {
        replaced: true,
        ..facts(true, true, false, false)
    };
    assert_eq!(Takeover::of(replaced), Takeover::Takes);
}

#[test]
fn settle_reach_reads_each_server() {
    assert_eq!(settle_reach(None), Settle::Stays);
    assert_eq!(settle_reach(Some(&[])), Settle::Goes);
    assert_eq!(
        settle_reach(Some(&[(true, true), (false, true), (true, false)])),
        Settle::Narrows(vec![1, 2])
    );
    assert_eq!(
        settle_reach(Some(&[(true, true), (true, true)])),
        Settle::Goes
    );
    assert_eq!(Gate::of(true, 9, 1, false), Gate::Descends);
    assert_eq!(Gate::of(false, 3, 2, true), Gate::Waits);
    assert_eq!(Gate::of(false, 2, 2, false), Gate::Waits);
    assert_eq!(Gate::of(false, 2, 2, true), Gate::Reach);
}

#[test]
fn a_later_set_replaces_an_earlier_ones_write() {
    // Sets of v1 (names 0 and 1) and v2 (name 0); only v2's version is stored: v2 lands, its
    // write of 0 waits, v1 keeps only its write of 1.
    let landing = land(&[(false, &[0, 1]), (true, &[0])], 2);
    assert_eq!(landing.lands, vec![false, true]);
    assert_eq!(landing.keeps, vec![vec![false, true], vec![false]]);
    assert_eq!(landing.waits, vec![Some(1), None]);
    // Both stored: v2's write of 0 waits over v1's, v1's write of 1 waits.
    let landing = land(&[(true, &[0, 1]), (true, &[0])], 2);
    assert_eq!(landing.waits, vec![Some(1), Some(0)]);
}
