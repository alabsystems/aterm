// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `DiskAwait` (`aterm_models_harness::disk_await_model`): on every
//! reachable state of the model where the wait is parked, the real stepper
//! (`control::control_disk::decide`) is handed what the state stands for and
//! must take a step the model enables — met, unmet or stalled — with every
//! invariant holding after it, or park exactly where the model enables none
//! of them.
//!
//! A state stands for: `free_ok` (every term holds on the reading), `demand`
//! (the wait holds one), `watch` (0: the watch runs), `beat_old` (its beat is
//! [`STALL`] old) and `pass` — the last pass for this demand, AS THE REAL
//! CODE DERIVES IT: 0 none and 1 running adopt no pass; 2 a pass that RAN OUT
//! (`Applied` with no stop, its search finished) and 3 one stopped for
//! another reason (a figure it could not read, its search cut short), each
//! leaving free space below the need, are handed through
//! `Applied::ran_out` and `disk_board::spent` — the derivation `why=spent`
//! is answered on, not an input the test picks.
//!
//! Negative controls: a stepper that answers `watch-stalled` while the beat is
//! fresh, and a SPENT RULE that reads only free space after the pass (the
//! defect `UnmetStopped` stands for), each disagree with the model somewhere,
//! so a checker that accepts everything fails this test.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_harness::disk_await_model;
use aterm_spec::derive::Model;

use aterm_agent::harness::disk::{Applied, Stop, StopBy};

use crate::control::control_disk::{Decision, Facts, STALL, decide};

type S = BTreeMap<&'static str, i64>;

/// The need of the projected demand, and free space after a pass for it.
const NEED: u64 = 30 << 30;
const AFTER: u64 = 20 << 30;

/// What a pass in the model's `pass` state did, as the real pass reports it:
/// `None` for none adopted (0, 1); for 2 an `Applied` that ran out of every
/// tier; for 3 one stopped by a figure it could not read and one whose search
/// stopped short (both "stopped for another reason").
fn passes(pass: i64) -> Vec<Applied> {
    let unmeasured = Applied {
        stops: vec![Stop {
            by: StopBy::Unmeasured,
            free: None,
            target: NEED,
            short: 0,
            released: 0,
            left: 3,
            tier: Some(2),
        }],
        ..Applied::default()
    };
    let cut = Applied {
        unfinished: Some("the search for build directories stopped short".to_owned()),
        ..Applied::default()
    };
    match pass {
        2 => vec![Applied::default()],
        3 => vec![unmeasured, cut],
        _ => Vec::new(),
    }
}

/// The spent rule under test: the product's ([`crate::disk_board::spent`]).
type Rule = dyn Fn(Option<u64>, Option<u64>, bool) -> bool;

/// Every state `m` reaches.
fn reachable(m: &Model) -> Vec<S> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            let mut next = s.clone();
            if m.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        out.push(s);
    }
    out
}

/// What state `s` stands for, as the real stepper reads it: one projection per
/// pass the state stands for ([`passes`]), its `spent` DERIVED by `rule` from
/// the real `Applied::ran_out`.
fn project(s: &S, rule: &Rule) -> Vec<Facts> {
    let base = Facts {
        met: s["free_ok"] == 1,
        demand: s["demand"] == 1,
        running: s["watch"] == 0,
        beat_old: s["beat_old"] == 1,
        spent: false,
    };
    let derived: Vec<Facts> = passes(s["pass"])
        .iter()
        .map(|done| Facts {
            spent: rule(Some(NEED), Some(AFTER), done.ran_out()),
            ..base
        })
        .collect();
    if derived.is_empty() {
        vec![base]
    } else {
        derived
    }
}

/// The model's name for a decision's answer.
fn answer(d: Decision) -> Option<&'static str> {
    match d {
        Decision::Met => Some("Met"),
        Decision::Unmet => Some("Unmet"),
        Decision::Stalled => Some("Stall"),
        Decision::Park => None,
    }
}

/// Hold `stepper` to `m` on every parked reachable state, each pass it stands
/// for derived through `rule`.
fn conforms(m: &Model, stepper: &dyn Fn(&Facts) -> Decision, rule: &Rule) -> Result<usize, String> {
    let mut checked = 0;
    for s in reachable(m).into_iter().filter(|s| s["reply"] == 0) {
        for facts in project(&s, rule) {
            let d = stepper(&facts);
            match answer(d) {
                Some(action) => {
                    if !m.action_enabled(action, &s) {
                        return Err(format!(
                            "the stepper answers {d:?} where `{action}` is not enabled: {s:?}"
                        ));
                    }
                    let mut after = s.clone();
                    assert!(m.fire(action, &mut after));
                    for inv in &m.invariants {
                        if !m.check_invariant(inv.name, &after) {
                            return Err(format!(
                                "`{}` broken after `{action}`: {after:?}",
                                inv.name
                            ));
                        }
                    }
                }
                None => {
                    if let Some(a) = ["Met", "Unmet", "Stall"]
                        .into_iter()
                        .find(|a| m.action_enabled(a, &s))
                    {
                        return Err(format!("the stepper parks where `{a}` answers: {s:?}"));
                    }
                }
            }
            checked += 1;
        }
    }
    Ok(checked)
}

#[test]
fn the_real_stepper_takes_only_the_models_answers() {
    let m = disk_await_model();
    let checked =
        conforms(&m, &decide, &crate::disk_board::spent).expect("the real stepper conforms");
    assert!(checked >= 32, "every parked state: {checked}");
    // The stall is two looks of the watch, not one: a healthy gap between
    // looks never reads as a stall.
    assert!(STALL >= crate::harness_host::DISK_TICK * 2);
}

/// NEGATIVE CONTROL: a spent rule that reads only free space after the pass —
/// `why=spent` on a pass stopped by a figure it could not read, or whose
/// search stopped short (the defect `UnmetStopped` stands for) — is caught.
#[test]
fn a_spent_rule_blind_to_how_the_pass_ended_is_rejected() {
    let m = disk_await_model();
    let blind = |demand: Option<u64>, after: Option<u64>, _ran_out: bool| {
        demand.is_some_and(|need| after.is_none_or(|f| f < need))
    };
    let caught = conforms(&m, &decide, &blind).expect_err("the checker must reject it");
    assert!(caught.contains("Unmet"), "{caught}");
}

/// NEGATIVE CONTROL: answering `watch-stalled` while a long pass still beats
/// (the defect `StallFresh` stands for) is caught.
#[test]
fn a_stepper_that_stalls_on_a_fresh_beat_is_rejected() {
    let m = disk_await_model();
    let buggy = |f: &Facts| {
        if !f.met && f.demand && f.running && !f.spent {
            Decision::Stalled
        } else {
            decide(f)
        }
    };
    let caught =
        conforms(&m, &buggy, &crate::disk_board::spent).expect_err("the checker must reject it");
    assert!(caught.contains("Stall"), "{caught}");
}
