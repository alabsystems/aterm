// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `StuckVerdict` (`aterm_models_misc::stuck_verdict_model`): every
//! scenario of the design's §18 expectation table that the model covers is
//! replayed through the real step (`jobs::step::classify_tick`, through
//! `Jobs::absorb`), and each tick is projected onto the model's
//! `(cls, q band, v)`. The action fired is the one the tick's own record
//! names (`JobRow::tick`: progress, a refused reading, μ grown, how the job
//! counted) together with the class's change and the φ band's move; each
//! must be ENABLED in the model, land on the projected state, and leave every
//! law holding.
//!
//! The unread-input gate's runs are left out: the gate is its own machine
//! (`InputUnreadGate`), and its published word decides the verdict with no φ.
//!
//! NEGATIVE CONTROLS (§17's table): the real step with one injected defect
//! (`jobs::Fault`, a `cfg(any(test, debug_assertions))` seam). Each trace is
//! rejected at Buggy 0, admitted by its own mutant, and that mutant's run
//! breaks its own law — so a projection that accepts everything fails here.
//! The liveness controls (a CR redraw, idle churn, M7's logical-write noise
//! taken for progress) are held to the scenarios' own flags: the true stall
//! is never flagged.

#[path = "support/world.rs"]
mod world;

#[path = "support/scenarios.rs"]
mod scenarios;

use std::collections::{BTreeMap, BTreeSet};

use aterm_models_misc::stuck_verdict_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_vitals::jobs::step::Effective;
use aterm_vitals::jobs::{Fault, JobRow, Verdict};
use scenarios::{Scenario, Trace};

type S = BTreeMap<&'static str, i64>;

/// `T8` in the model: the φ band of 8.
const T8: i64 = 4;

/// The φ band: `0` under 1, `1` under 3, `2` under 5, `3` under 8, `4` from
/// 8. A quiet verdict held under 3 by its 2.5 hysteresis is still in band 2,
/// the band its quiet holds in.
fn band(r: &JobRow) -> i64 {
    let p = r.phi_milli.unwrap_or(0);
    let rank = rank(r.verdict);
    if rank == 1 && (2_500..3_000).contains(&p) {
        return 2;
    }
    match p {
        0..1_000 => 0,
        1_000..3_000 => 1,
        3_000..5_000 => 2,
        5_000..8_000 => 3,
        _ => 4,
    }
}

fn rank(v: Verdict) -> i64 {
    match v {
        Verdict::Stuck => 2,
        Verdict::Quiet | Verdict::Busy | Verdict::Slow => 1,
        _ => 0,
    }
}

fn cls(e: Effective) -> i64 {
    match e {
        Effective::Running => 0,
        Effective::Legit => 1,
        Effective::Capable => 2,
        Effective::Capped => 3,
        Effective::Exited => 4,
        Effective::Gate => panic!("the gate's runs are not projected"),
    }
}

fn fire(
    m: &Model,
    s: &mut S,
    action: &'static str,
    fired: &mut BTreeSet<&'static str>,
) -> Result<(), String> {
    if !m.action_enabled(action, s) {
        return Err(format!("`{action}` is not enabled in {s:?}"));
    }
    m.fire(action, s);
    fired.insert(action);
    Ok(())
}

/// Replay `t` on `m`: the error names the first tick the model refuses.
fn replay(m: &Model, t: &Trace, fired: &mut BTreeSet<&'static str>) -> Result<S, String> {
    let mut s = m.init_state();
    for (rel, row, _) in &t.ticks {
        let Some(r) = row else { continue };
        let next = cls(r.tick.effective);
        let (b, v) = (band(r), rank(r.verdict));
        let step = |s: &mut S, fired: &mut BTreeSet<&'static str>| -> Result<(), String> {
            let now = s["cls"];
            match (now, next) {
                (4, _) => return Ok(()),
                (_, 4) => fire(m, s, "Exit", fired)?,
                (_, 0) if r.tick.progress.is_some() => fire(m, s, "Progress", fired)?,
                (0, 0) => {}
                (2 | 3, 1) => fire(m, s, "BecomeLegit", fired)?,
                (0, 1) => {
                    fire(m, s, "BecomeSuspect", fired)?;
                    fire(m, s, "BecomeLegit", fired)?;
                }
                (1, 1) => {}
                (0 | 1, 2 | 3) => {
                    fire(m, s, "BecomeSuspect", fired)?;
                    if next == 3 {
                        fire(m, s, "Relabel", fired)?;
                    }
                }
                (2, 3) | (3, 2) => fire(m, s, "Relabel", fired)?,
                (2, 2) | (3, 3) => {}
                (a, b) => return Err(format!("no model step takes cls {a} to {b}")),
            }
            // A legitimate wait's time passing, a band at a time: at Buggy 0
            // `LegitTick` holds `q` at 0 (a stutter), so a legit row whose φ
            // left band 0 is refused right here.
            if next == 1 {
                while s["q"] < b {
                    let q = s["q"];
                    fire(m, s, "LegitTick", fired)?;
                    if s["q"] <= q {
                        break;
                    }
                }
            }
            if (2..=3).contains(&next) {
                if r.tick.refused {
                    fire(m, s, "RefusedTick", fired)?;
                }
                if r.tick.mu_grew {
                    while s["q"] > b {
                        fire(m, s, "MuGrows", fired)?;
                    }
                }
                while s["q"] < b {
                    fire(m, s, "QuietTick", fired)?;
                }
                if s["cls"] == 2 && s["q"] >= T8 {
                    fire(m, s, "Watch", fired)?;
                }
            }
            Ok(())
        };
        step(&mut s, fired).map_err(|e| format!("at {rel} ms ({}): {e}", r.wire()))?;
        let want_q = if next == 4 { s["q"] } else { b };
        if (s["cls"], s["q"], s["v"]) != (next, want_q, v) {
            return Err(format!(
                "at {rel} ms the model is at (cls {}, q {}, v {}) but the step read (cls {next}, q {want_q}, v {v}): {}",
                s["cls"],
                s["q"],
                s["v"],
                r.wire()
            ));
        }
        for inv in &m.invariants {
            if !m.check_invariant(inv.name, &s) {
                return Err(format!("at {rel} ms `{}` breaks: {}", inv.name, r.wire()));
            }
        }
    }
    Ok(s)
}

#[test]
fn every_covered_scenario_is_a_run_of_the_model() {
    let m = interp::with_buggy(&stuck_verdict_model(), 0);
    let mut fired = BTreeSet::new();
    let mut bad = Vec::new();
    let all: Vec<Scenario> = scenarios::all();
    let covered: Vec<&Scenario> = all.iter().filter(|s| s.model).collect();
    assert!(covered.len() >= 40, "{} scenarios projected", covered.len());
    for s in covered {
        let t = scenarios::run(s, None);
        if let Err(e) = replay(&m, &t, &mut fired) {
            bad.push(format!("{}: {e}", s.name));
        }
    }
    assert!(
        bad.is_empty(),
        "{} scenario(s) the model refuses:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
    // The binding drives every action of the healthy lane but `LegitTick`,
    // which at Buggy 0 is a stutter (a legit wait holds `q = v = 0`): the
    // `AccrueOnLegit` control drives it, and every legit row is held to band 0.
    for a in [
        "Progress",
        "QuietTick",
        "RefusedTick",
        "MuGrows",
        "BecomeLegit",
        "BecomeSuspect",
        "Relabel",
        "Exit",
        "Watch",
    ] {
        assert!(fired.contains(a), "no scenario drives `{a}`: {fired:?}");
    }
}

/// A defect's trace: rejected at Buggy 0; admitted at its own dial, where
/// its own law breaks.
fn caught(fault: Fault, scenario: &str, dial: i64, law: &str) {
    let s = scenarios::named(scenario);
    let t = scenarios::run(&s, Some(fault));
    let model = stuck_verdict_model();
    let healthy = interp::with_buggy(&model, 0);
    let mut fired = BTreeSet::new();
    let refused = replay(&healthy, &t, &mut fired);
    assert!(
        refused.is_err(),
        "{fault:?} on {scenario}: the healthy model admits the defect"
    );
    let mutant = interp::with_buggy(&model, dial);
    let mut lenient = mutant.clone();
    lenient.invariants.retain(|i| i.name != law);
    let mut fired = BTreeSet::new();
    let end = replay(&lenient, &t, &mut fired).unwrap_or_else(|e| {
        panic!("{fault:?} on {scenario}: Buggy {dial} does not admit the trace: {e}")
    });
    if fault == Fault::AccrueOnLegit {
        assert!(
            fired.contains("LegitTick"),
            "the accrual is never a `LegitTick`"
        );
    }
    assert!(
        !mutant.check_invariant(law, &end),
        "{fault:?} on {scenario}: Buggy {dial} admits the trace but `{law}` holds"
    );
}

#[test]
fn accruing_on_a_legit_wait_is_caught_by_legit_wait_never_accrues() {
    caught(
        Fault::AccrueOnLegit,
        "tailf.pty",
        2,
        "LegitWaitNeverAccrues",
    );
}

#[test]
fn resetting_the_age_on_a_refused_read_is_caught_by_refused_reading_is_not_zero_progress() {
    caught(
        Fault::RefusedResetsAge,
        "sleep_eperm.setsid",
        3,
        "RefusedReadingIsNotZeroProgress",
    );
}

#[test]
fn letting_unnamed_reach_stuck_is_caught_by_unnamed_never_enters_stuck() {
    caught(
        Fault::UnnamedReachesStuck,
        "flock.pty",
        4,
        "UnnamedNeverEntersStuck",
    );
}

#[test]
fn never_raising_a_named_wait_is_caught_by_flagged_on_time() {
    caught(Fault::NeverRaiseNamed, "curl.setsid", 6, "FlaggedOnTime");
}

/// A liveness defect: the healthy step flags the scenario's true stall, the
/// defective one takes a reading that is no progress for progress and never
/// does.
fn starves(fault: Fault, scenario: &str) {
    let s = scenarios::named(scenario);
    let good = scenarios::run(&s, None);
    assert!(
        scenarios::check(&s, &good).is_empty(),
        "{scenario} holds unfaulted"
    );
    assert!(
        good.ticks
            .iter()
            .all(|(_, r, _)| r.as_ref().is_none_or(|r| r.tick.progress.is_none())),
        "{scenario} has no real progress"
    );
    let bad = scenarios::run(&s, Some(fault));
    assert!(
        bad.ticks
            .iter()
            .all(|(_, r, _)| r.as_ref().is_none_or(|r| r.verdict != Verdict::Stuck)),
        "{fault:?} on {scenario}: the stall is still flagged"
    );
    assert!(
        !scenarios::check(&s, &bad).is_empty(),
        "{fault:?} on {scenario}: the expectations pass a trace that never flags"
    );
}

#[test]
fn a_cr_redraw_counted_as_progress_never_flags_the_spinner_over_a_silent_socket() {
    starves(Fault::RawBytesProgress, "spinsocket.pty");
    starves(Fault::RawBytesProgress, "spinsocket.setsid");
}

#[test]
fn churn_without_work_counted_as_progress_never_flags_the_sleep_poll_loop() {
    starves(Fault::ChurnWithoutWork, "sleeppoll.pty");
}

#[test]
fn logical_write_noise_counted_as_progress_never_flags_the_spinner() {
    starves(Fault::AnyLogicalWrites, "spinsocket.setsid");
}
