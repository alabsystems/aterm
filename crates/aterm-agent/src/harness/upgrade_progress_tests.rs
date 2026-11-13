// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance tests binding `HarnessUpgradeProgress` to the upgrade reducer and classifier.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_harness::harness_upgrade_progress_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::*;
use crate::harness::upgrade::{Facts, Press, Step};
use crate::harness::upgrade_drive::watch::look_word;
use crate::harness::upgrade_drive::{WATCH_GAP, at_point};

const TARGET: &str = "2.1.284";

fn main_today() -> Model {
    interp::with_buggy(&harness_upgrade_progress_model(), 0)
}

fn latched(name: &str) -> Model {
    let mut m = main_today();
    for v in &mut m.vars {
        if v.name == name {
            v.init = 1;
        }
    }
    m
}

fn reach(m: &Model) -> Vec<State> {
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &m.actions {
            queue.extend(m.successors(a.name, &s));
        }
        out.push(s);
    }
    out
}

fn with(s: &State, var: &'static str, value: i64) -> State {
    let mut s = s.clone();
    s.insert(var, value);
    s
}

const BEHIND: [u64; 3] = [3_600, STALLED_AFTER_S + 3_600, MOVE_BUDGET_S + 3_600];

fn phase_of(s: &State) -> Phase {
    let window = if s["clock"] == 1 {
        upgrade::REASK_S + 60
    } else {
        60
    };
    match s["phase"] {
        0 => Phase::Pending,
        1 => Phase::Announced {
            at_s: NOW - window,
            asks: upgrade::MAX_ASKS,
        },
        2 => Phase::Failed(upgrade::GAVE_UP.to_string()),
        _ => Phase::Done,
    }
}

/// Where a look is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    /// At a point the loop offers: at a settled break, the break's step (`background_point`). A
    /// standing press is not this step's: it stands aside ([`Press::Stands`]).
    Point,
    /// The watch's dry read, off any point: it never knows a break, and reads a standing press
    /// as its lane would ([`Press::Acting`]).
    Read,
    /// The press's own lane (`upgrade_drive::press`), off any point.
    Press,
}

/// The owner's word in state `s`: their Now (a press writes one too), else none.
fn request_of(s: &State) -> Request {
    if s["now"] == 1 {
        Request::Now
    } else {
        Request::None
    }
}

fn facts(s: &State, look: Look) -> Facts {
    let clock = s["clock"] == 1;
    let mut f = Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        background: if s["work"] == 1 {
            vec!["zsh".to_string()]
        } else {
            Vec::new()
        },
        quiet_s: 3_600,
        ready_s: if s["ready"] == 1 && clock {
            upgrade::REASK_S
        } else {
            0
        },
        idle_looks: upgrade::IDLE_LOOKS,
        failed_s: if clock { upgrade::RETRY_S } else { 60 },
        rung: upgrade::rung(BEHIND[0]),
        still_s: 3_600,
        press: match (s["pressed"], look) {
            (0, _) => Press::None,
            (_, Look::Point) => Press::Stands,
            (_, Look::Read | Look::Press) => Press::Acting,
        },
        ..Facts::default()
    };
    match s["posture"] {
        0 => {
            f.status = "busy".to_string();
            f.status_age_s = 5;
            f.busy_footer = true;
            f.idle_looks = 0;
            f.quiet_s = 0;
        }
        1 => {
            f.status = "busy".to_string();
            f.idle_looks = 0;
            f.background_point = look == Look::Point;
        }
        3 => f.limited = true,
        4 => f.approval_box = true,
        _ => {}
    }
    f
}

fn real_step(s: &State, look: Look) -> (Step, Facts) {
    let f = facts(s, look);
    let step = upgrade::requested_step(
        &request_of(s),
        &phase_of(s),
        &f,
        s["ready"] == 1,
        NOW,
        TARGET,
    );
    let step = if look == Look::Point && s["posture"] == 1 {
        upgrade::break_step(step, &f)
    } else {
        step
    };
    (step, f)
}

fn dry_word(s: &State, step: &Step, f: &Facts) -> String {
    let act = match step {
        Step::Wait(why) | Step::Tell(why) => return format!("wait:{why}"),
        Step::Announce => "would-announce".to_string(),
        Step::Terminate => "would-restart".to_string(),
        Step::Fresh => "would-restart:fresh".to_string(),
        Step::GiveUp => "gave-up".to_string(),
        Step::Rearm => format!("would-rearm:{}", upgrade::GAVE_UP),
        Step::Void(why) => format!("would-void:{why}"),
    };
    match upgrade::held_point(&request_of(s), step, f, NOW, TARGET) {
        Some(held) => at_point(&act, &upgrade::wait_word(held, &f.status)),
        None => act,
    }
}

fn class(word: &str) -> i64 {
    match wait_class(word) {
        WaitClass::OwnWork => 0,
        WaitClass::Person => 1,
        WaitClass::Limit => 2,
        _ => 3,
    }
}

fn break_arm_094(s: &State, look: Look) -> Step {
    let (step, f) = real_step(s, look);
    if f.background_point {
        Step::Wait("background")
    } else {
        step
    }
}

fn terminal_stop(s: &State, look: Look) -> Step {
    let (step, _) = real_step(s, look);
    if matches!(phase_of(s), Phase::Failed(_)) {
        Step::Wait("failed")
    } else {
        step
    }
}

fn lands(step: Step) -> Option<i64> {
    match step {
        Step::Announce => Some(1),
        Step::GiveUp => Some(2),
        Step::Terminate | Step::Fresh => Some(3),
        Step::Rearm => Some(0),
        Step::Wait(_) | Step::Void(_) | Step::Tell(_) => None,
    }
}

#[test]
fn the_real_reducer_is_the_derived_machine() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let today = main_today();
    let states = reach(&today);
    assert!(
        states.len() > 500,
        "the machine has a space: {}",
        states.len()
    );
    let kept = latched("keep");
    let buggy = interp::with_buggy(&harness_upgrade_progress_model(), 1);
    let (mut idle_acts, mut break_acts, mut reads, mut acting, mut pressed) = (0, 0, 0, 0, 0);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    for s in states.iter().filter(|s| s["phase"] <= 2) {
        let (step, f) = real_step(s, Look::Press);
        let presses = s["pressed"] == 1 && lands(step) == Some(3);
        assert_eq!(
            today.action_enabled("RestartOnPress", s),
            presses,
            "RestartOnPress at {s:?}: the press's step {step:?}\n{f:?}"
        );
        if presses {
            let mut after = s.clone();
            assert!(today.fire("RestartOnPress", &mut after));
            assert_eq!(after["phase"], 3, "{s:?}: {step:?}");
            pressed += 1;
            if s["work"] == 1 {
                // The press read as the Now it writes (`PressReadAsNow`): no press, the owner's
                // Now alone.
                let as_now = Facts {
                    press: Press::None,
                    ..f.clone()
                };
                let now_step = upgrade::requested_step(
                    &Request::Now,
                    &phase_of(s),
                    &as_now,
                    s["ready"] == 1,
                    NOW,
                    TARGET,
                );
                if lands(now_step) != Some(3) {
                    caught.insert("PressReadAsNow");
                }
            }
        }
        if buggy.action_enabled("PressPassesInput", s) {
            assert_eq!(lands(step), None, "{s:?}: the press waits for {f:?}");
            caught.insert("PressPassesInput");
        }
        if buggy.action_enabled("NowAsPress", s) {
            for look in [Look::Point, Look::Read] {
                let (step, _) = real_step(s, look);
                assert_ne!(lands(step), Some(3), "{s:?} at {look:?}: {step:?}");
            }
            caught.insert("NowAsPress");
        }
        for (action, posture) in [("StepAtIdle", 2), ("RestartAtIdle", 2), ("StepAtBreak", 1)] {
            let (step, f) = real_step(s, Look::Point);
            let landed = lands(step);
            let acts = s["posture"] == posture
                && match action {
                    "StepAtIdle" => landed.is_some_and(|p| p != 3),
                    "RestartAtIdle" => landed == Some(3),
                    _ => landed.is_some(),
                };
            assert_eq!(
                today.action_enabled(action, s),
                acts,
                "{action} at {s:?}: the real step {step:?}\n{f:?}"
            );
            if acts {
                let mut after = s.clone();
                assert!(today.fire(action, &mut after));
                assert_eq!(Some(after["phase"]), lands(step), "{s:?}: {step:?}");
                if posture == 2 {
                    idle_acts += 1;
                } else {
                    break_acts += 1;
                }
            }
            if today.action_enabled(action, s) {
                if lands(break_arm_094(s, Look::Point)).is_none() {
                    caught.insert("0.94's break arm");
                }
                if lands(terminal_stop(s, Look::Point)).is_none() {
                    caught.insert("the terminal stop");
                }
            }
        }
        if buggy.action_enabled("RestartOverWork", s) {
            let (step, _) = real_step(s, Look::Point);
            assert_ne!(step, Step::Terminate, "{s:?}");
            caught.insert("RestartOverWork");
        }
        let (step, f) = real_step(s, Look::Read);
        let dry = dry_word(s, &step, &f);
        let word = look_word(&dry, true, || f.status.clone());
        assert!(!word.is_empty(), "{s:?}: every read is worded: `{dry}`");
        let mut looked = s.clone();
        if step == Step::Rearm {
            assert!(today.fire("WatchRearms", &mut looked), "{s:?}");
        }
        assert!(today.fire("Watch", &mut looked), "{s:?}");
        assert_eq!(
            class(&word),
            looked["cause"],
            "{s:?}: `{dry}` records `{word}`"
        );
        reads += 1;
        assert_eq!(
            today.action_enabled("WatchRearms", s),
            step == Step::Rearm,
            "{s:?}: {step:?}"
        );
        if !dry.starts_with("wait:") {
            acting += 1;
            assert_eq!(
                look_word(&dry, false, || f.status.clone()),
                "",
                "{s:?}: a Codex record's `{dry}`"
            );
            let old = s["cause"];
            let mut latched = with(s, "keep", 1);
            assert!(kept.fire("Watch", &mut latched), "{s:?}");
            assert_eq!(latched["cause"], old, "{s:?}: the latch is that look");
            if old != looked["cause"] {
                caught.insert("RecordedWordStands");
            }
            let blind = look_word(
                dry.split_once(crate::harness::upgrade_drive::AT_POINT)
                    .map_or(dry.as_str(), |(act, _)| act),
                true,
                || f.status.clone(),
            );
            assert_eq!(blind, upgrade::NEXT_POINT, "{s:?}: `{dry}`");
            if class(&blind) != looked["cause"] && looked["cause"] == 1 {
                caught.insert("the posture-blind word");
            }
        }
    }
    assert!(
        idle_acts > 0 && break_acts > 0 && reads > 0 && acting > 0 && pressed > 0,
        "{idle_acts} {break_acts} {reads} {acting} {pressed}"
    );
    assert_eq!(
        caught,
        BTreeSet::from([
            "0.94's break arm",
            "NowAsPress",
            "PressPassesInput",
            "PressReadAsNow",
            "RecordedWordStands",
            "RestartOverWork",
            "the posture-blind word",
            "the terminal stop"
        ])
    );

    let mut anchored: Vec<(&str, &str)> = xref::refinements()
        .filter(|a| a.machine == "HarnessUpgradeProgress")
        .map(|a| {
            assert_eq!(
                a.project,
                "aterm_agent::harness::upgrade_drive::progress_projection"
            );
            (a.action, a.rust_method)
        })
        .collect();
    anchored.sort_unstable();
    assert_eq!(
        anchored,
        [
            ("LineTypedAtIdle", "look_types"),
            ("LookClosesAtAPoint", "look_at"),
            ("NextTargetTaken", "look_holds"),
            ("RestartAtIdle", "gate_restart"),
            ("RestartOnPress", "press_gate"),
            ("StepAtBreak", "break_step"),
            ("StepAtIdle", "next_step"),
            ("Watch", "watch_with"),
            ("WatchRearms", "watch_with"),
        ]
    );
    let entered = xref::entered_anchor_ids();
    for id in [
        "HarnessUpgradeProgress::StepAtIdle @ next_step",
        "HarnessUpgradeProgress::StepAtBreak @ break_step",
        "HarnessUpgradeProgress::RestartAtIdle @ gate_restart",
        "HarnessUpgradeProgress::RestartOnPress @ press_gate",
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
    }
}

/// The record of a finished move in state `s`, its look's clock set against [`NOW`] as `age`
/// says, and whether its resumed build warned.
fn look_record(s: &State) -> (St, bool) {
    use crate::harness::relaunch::{RESTORE_LOOK, RESTORE_TYPE_WITHIN_S};
    let passed = match s["age"] {
        0 => None,
        1 => Some(RESTORE_TYPE_WITHIN_S / 2),
        _ => Some(RESTORE_TYPE_WITHIN_S),
    };
    let confirm_by = match (s["closed"], passed) {
        (1, _) => 0,
        (_, None) => NOW + RESTORE_LOOK.as_secs(),
        (_, Some(passed)) => NOW - passed,
    };
    let st = St {
        phase: Phase::Done,
        from: "2.1.283".to_string(),
        to: TARGET.to_string(),
        resumed_on: TARGET.to_string(),
        done_at: NOW - 3_600,
        confirm_by,
        ..St::default()
    };
    (st, s["owes"] == 1)
}

/// A finished move's look ends on its own clock: past it, any point closes it and the next build
/// is taken, a break included; only its line is an idle point's, typed there however late.
///
/// NEGATIVE CONTROLS: the lane before the look had a clock, which held the visit and the owed
/// continuation on `St::confirming` alone (`OnlyIdleClosesALook`: under it the model's point
/// leaves the newer build hidden exactly where that read still holds and the look's clock does
/// not), and a step typing the look's line wherever the agent rests (`LineTypedAtBreak`: under it
/// the model types at exactly the breaks the real `look_types` refuses).
#[test]
fn a_finished_moves_look_is_the_derived_machine() {
    use crate::harness::relaunch::{LookAt, look_at, look_types};
    let today = main_today();
    let done: Vec<State> = reach(&today)
        .into_iter()
        .filter(|s| s["phase"] == 3)
        .collect();
    let idle_only = latched("idle_only");
    let any_point = latched("any_point");
    let (mut closes, mut typed, mut late, mut withheld, mut taken, mut waits) = (0, 0, 0, 0, 0, 0);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    for s in &done {
        assert_eq!((s["hid"], s["off_idle"]), (0, 0), "{s:?}");
        let (st, warned) = look_record(s);
        let at = look_at(st.confirm_by, NOW, warned);
        let holds = st.confirming() && at.holds();
        let at_break = s["rests"] == 1;
        assert_eq!(
            today.action_enabled("LookClosesAtAPoint", s),
            st.confirming() && !at.holds(),
            "{s:?}: {at:?}"
        );
        assert_eq!(
            today.action_enabled("LineTypedAtIdle", s),
            st.confirming() && look_types(at, at_break),
            "{s:?}: {at:?}"
        );
        assert_eq!(
            today.action_enabled("NextTargetTaken", s),
            s["next"] == 1 && !holds,
            "{s:?}: {at:?}"
        );
        if today.action_enabled("LookClosesAtAPoint", s) {
            closes += 1;
        }
        let mut after = s.clone();
        if today.fire("LineTypedAtIdle", &mut after) {
            assert_eq!((after["closed"], after["off_idle"]), (1, 0), "{s:?}");
            typed += 1;
            late += usize::from(at == LookAt::Lapsed);
        } else if st.confirming() && warned {
            assert!(at_break, "{s:?}: only a break withholds an owed line");
            withheld += 1;
        }
        let mut after = s.clone();
        if today.fire("NextTargetTaken", &mut after) {
            assert_eq!((after["next"], after["closed"]), (2, 1), "{s:?}");
            taken += 1;
        }
        if s["next"] == 1 && holds {
            waits += 1;
        }
        let mut anywhere = with(s, "any_point", 1);
        if any_point.fire("LineTypedAtIdle", &mut anywhere) && anywhere["off_idle"] == 1 {
            assert!(
                at_break && look_types(at, false) && !look_types(at, true),
                "{s:?}: the model types where the real step refuses"
            );
            caught.insert("LineTypedAtBreak");
        }
        let mut old = with(s, "idle_only", 1);
        if idle_only.fire("NextTargetTaken", &mut old) && old["hid"] == 1 {
            assert_eq!(old["next"], 1, "{s:?}: the newer build stays hidden");
            assert!(
                st.confirming() && !holds,
                "{s:?}: the old read holds, the look's clock does not"
            );
            caught.insert("OnlyIdleClosesALook");
        }
    }
    assert!(
        closes > 0 && typed > 0 && late > 0 && withheld > 0 && taken > 0 && waits > 0,
        "{closes} {typed} {late} {withheld} {taken} {waits}"
    );
    assert_eq!(
        caught,
        BTreeSet::from(["LineTypedAtBreak", "OnlyIdleClosesALook"])
    );
}

/// No law of the model is one nothing breaks: the non-vacuity sweep finds no fault, and each
/// action dead at the committed config is a negative control that breaks a law alone.
#[test]
fn every_law_of_the_model_is_broken_by_a_mutant() {
    let model = harness_upgrade_progress_model();
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let controls = aterm_spec::verify::audit_dead_negative_controls(std::slice::from_ref(&model));
    assert_eq!(controls, Ok(9), "the model's mutants, each a control");
}

/// A newer build behind a finished move's look is taken, as a law of every fair behaviour and not
/// of single steps alone: the interpreter's full audit of the model's three liveness laws, each
/// with the fairness it needs and no more, and each broken by its mutants alone.
#[test]
fn the_models_liveness_laws_hold_and_their_mutants_break_them() {
    let m = harness_upgrade_progress_model();
    for (live, mutants) in [
        (aterm_models_harness::harness_upgrade_progress_liveness(), 2),
        (aterm_models_harness::harness_upgrade_press_liveness(), 1),
        (aterm_models_harness::harness_upgrade_look_liveness(), 1),
    ] {
        assert_eq!(
            aterm_spec::verify::audit_liveness(&m, &live),
            Ok(mutants),
            "{}",
            live.name
        );
    }
}

fn quiet_facts(s: &State) -> Facts {
    let mut f = facts(s, Look::Read);
    f.quiet = true;
    f.unnoticed = BEHIND[0] < upgrade::QUIET_S;
    if s["posture"] == 3 && s["reset"] == 1 {
        f.limited = false;
    }
    f
}

fn read_word(s: &State, f: &Facts) -> String {
    let step = upgrade::requested_step(
        &request_of(s),
        &phase_of(s),
        f,
        s["ready"] == 1,
        NOW,
        TARGET,
    );
    look_word(&dry_word(s, &step, f), true, || f.status.clone())
}

fn quiet_truth(s: &State) -> Option<BTreeSet<i64>> {
    if s["pressed"] == 1 {
        // A standing press waits only for a held tab, a wall (a limit whose reset passed is
        // none: the quiet read sees it gone) or a running turn.
        return match (s["posture"], s["reset"]) {
            (0, _) => Some(BTreeSet::from([0])),
            (3, 0) => Some(BTreeSet::from([2])),
            (4, _) => Some(BTreeSet::from([1])),
            _ => None,
        };
    }
    if s["phase"] == 2 && s["clock"] == 0 {
        return Some(BTreeSet::from([0]));
    }
    let idle = s["posture"] == 2 || (s["posture"] == 3 && s["reset"] == 1);
    let work = s["work"] == 1;
    if s["phase"] == 1 && s["clock"] == 1 && idle && work {
        return None;
    }
    match s["posture"] {
        _ if idle && !work => None,
        _ if idle => Some(BTreeSet::from([0])),
        0 | 1 => Some(BTreeSet::from([0])),
        3 => Some(BTreeSet::from([2])),
        _ if work => Some(BTreeSet::from([0, 1])),
        _ => Some(BTreeSet::from([1])),
    }
}

fn quiet_bound(s: &State, word: &str) -> bool {
    match quiet_truth(s) {
        None => word == upgrade::NEXT_POINT,
        Some(classes) => word != upgrade::NEXT_POINT && classes.contains(&class(word)),
    }
}

#[test]
fn the_quiet_upgrades_read_records_what_holds_its_restart() {
    let today = main_today();
    let states = reach(&today);
    let (mut free, mut held) = (0, 0);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    for s in states.iter().filter(|s| s["phase"] <= 2) {
        let f = quiet_facts(s);
        let word = read_word(s, &f);
        assert!(
            quiet_bound(s, &word),
            "{s:?}: the quiet read records `{word}`, where {:?} holds it\n{f:?}",
            quiet_truth(s)
        );
        if quiet_truth(s).is_none() {
            free += 1;
        } else {
            held += 1;
        }
        let asking = Facts {
            quiet: false,
            ..f.clone()
        };
        if !quiet_bound(s, &read_word(s, &asking)) {
            caught.insert("the ask flow's read");
        }
        let blind_to_work = Facts {
            background: Vec::new(),
            ..f.clone()
        };
        if !quiet_bound(s, &read_word(s, &blind_to_work)) {
            caught.insert("RestartOverWork");
        }
        let before_c9 = Facts {
            limited: f.limited || s["posture"] == 3,
            ..f.clone()
        };
        if !quiet_bound(s, &read_word(s, &before_c9)) {
            caught.insert("main before C9");
        }
        let step = upgrade::requested_step(
            &request_of(s),
            &phase_of(s),
            &f,
            s["ready"] == 1,
            NOW,
            TARGET,
        );
        let dry = dry_word(s, &step, &f);
        let act = dry
            .split_once(crate::harness::upgrade_drive::AT_POINT)
            .map_or(dry.as_str(), |(act, _)| act);
        if !quiet_bound(s, &look_word(act, true, || f.status.clone())) {
            caught.insert("the posture-blind word");
        }
    }
    assert!(free > 0 && held > 0, "{free} {held}");
    assert_eq!(
        caught,
        BTreeSet::from([
            "RestartOverWork",
            "main before C9",
            "the ask flow's read",
            "the posture-blind word",
        ])
    );
}

fn row(s: &State, behind: u64, progress: u64) -> Row {
    let phase = match phase_of(s) {
        Phase::Announced { asks, .. } if progress != NOW - 60 => Phase::Announced {
            at_s: progress,
            asks,
        },
        p => p,
    };
    let wait = match (s["cause"], s["phase"]) {
        (0, 0) => "not-idle:busy",
        (0, 1) => "awaiting-ready",
        (0, _) => "failed",
        (1, _) => "box",
        (2, _) => "limited",
        _ => "",
    };
    let clock = s["clock"] == 1;
    Row {
        agent: upgrade::Agent::Claude,
        phase,
        behind_since: NOW - behind,
        wait: wait.to_string(),
        wait_since: NOW - 60,
        last_stop: if s["rearmed"] == 1 {
            upgrade::GAVE_UP.to_string()
        } else {
            String::new()
        },
        stop_streak: u32::from(s["phase"] == 2),
        streak_why: if s["phase"] == 2 {
            upgrade::GAVE_UP.to_string()
        } else {
            String::new()
        },
        retry_at: match (s["phase"], clock) {
            (2, true) => NOW - 60,
            (2, false) => NOW + 600,
            _ => 0,
        },
        progress_at: if clock && progress == NOW - 60 {
            NOW - upgrade::REASK_S - 60
        } else {
            progress
        },
        minted_at: progress,
        limit_until: match (s["cause"], s["reset"]) {
            (2, 1) => NOW - LIMIT_GRACE_S - 60,
            (2, _) => NOW + 3_600,
            _ => 0,
        },
        request: request_of(s),
        request_at: if s["now"] == 1 { progress } else { 0 },
        pressed_at: if s["pressed"] == 1 { progress } else { 0 },
        pressed_to: if s["pressed"] == 1 {
            pending(3_600).to
        } else {
            String::new()
        },
        ..pending(3_600)
    }
}

fn named(rows: &[Row], from: u64, horizon: u64) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    let mut t = from;
    while let Some(next) = next_change(rows, t) {
        if next > horizon {
            break;
        }
        out.insert(next);
        t = next;
    }
    out
}

fn turns(row: &Row, from: u64, horizon: u64, candidates: &[u64]) -> BTreeSet<u64> {
    candidates
        .iter()
        .copied()
        .filter(|t| *t > from && *t <= horizon)
        .filter(|t| row.attention(t - 1).level() != row.attention(*t).level())
        .collect()
}

fn deadlines(row: &Row) -> Vec<u64> {
    let at_s = match row.phase {
        Phase::Announced { at_s, .. } => at_s,
        _ => 0,
    };
    let stamps = [
        row.behind_since,
        row.minted_at,
        row.progress_at,
        row.wait_since,
        row.own_work_at,
        row.refused_at,
        row.limit_until,
        at_s,
    ];
    let spans = [
        STALLED_AFTER_S,
        MOVE_BUDGET_S,
        DIGEST_AFTER_S,
        HARNESS_TELL_S,
        TELL_S,
        LIMIT_MAX_S,
        LIMIT_GRACE_S,
        LONGEST_LIMIT_S + LIMIT_GRACE_S,
        upgrade::REASK_S,
        BACKSTOP_S,
    ];
    let mut out: Vec<u64> = stamps
        .iter()
        .filter(|st| **st != 0)
        .flat_map(|st| spans.iter().map(move |span| st.saturating_add(*span)))
        .collect();
    out.push(row.retry_at);
    out
}

#[test]
fn the_real_view_is_the_derived_machine() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let today = main_today();
    let states = reach(&today);
    let (mut quiet, mut faults, mut digests, mut stale_told, mut timed) = (0, 0, 0, 0, 0);
    for s in states.iter().filter(|s| s["phase"] <= 2) {
        for behind in BEHIND {
            let r = row(s, behind, NOW - 60);
            let proj = progress_projection(&r, NOW);
            let mut want: BTreeMap<&str, i64> =
                ["phase", "rearmed", "cause", "clock", "pressed", "now"]
                    .into_iter()
                    .map(|k| (k, s[k]))
                    .collect();
            want.insert("reset", if s["cause"] == 2 { s["reset"] } else { 0 });
            assert_eq!(proj, want, "{s:?}\n{r:?}");
            assert_eq!(r.word_in_force(NOW), s["now"] == 1, "{s:?}");
            assert_eq!(
                proj["cause"] == 0,
                r.overdue_cause(NOW) == OWN_WORK,
                "{s:?}"
            );
            assert!(r.interrupt(NOW).is_none(), "{s:?}: an interrupt for a wait");
            assert!(r.badge(NOW).is_none(), "{s:?}: the tab is marked");
            match r.attention(NOW) {
                Attention::Quiet => quiet += 1,
                Attention::Fault(n) => {
                    assert_eq!(n.kind, "limit-passed", "{s:?}: {n:?}");
                    assert!(s["cause"] == 2 && s["reset"] == 1, "{s:?}");
                    faults += 1;
                }
                Attention::Digest(n) => {
                    assert_eq!(n.kind, "own-work", "{s:?}: {n:?}");
                    assert!(s["cause"] == 0 && behind >= DIGEST_AFTER_S, "{s:?}");
                    digests += 1;
                }
                Attention::Interrupt(n) => panic!("{s:?}: {n:?}"),
            }
            if s["cause"] == 2 {
                assert_eq!(
                    matches!(r.attention(NOW), Attention::Fault(_)),
                    s["reset"] == 1,
                    "{s:?}"
                );
            }
            let horizon = NOW + 10 * 86_400;
            let rows = std::slice::from_ref(&r);
            let turned = turns(&r, NOW, horizon, &deadlines(&r));
            let seen = named(rows, NOW, horizon);
            for t in &turned {
                assert!(
                    seen.contains(t),
                    "{s:?}: the class turns at {t}, unnamed: {seen:?}"
                );
                timed += 1;
            }
        }
        let never = LIMIT_MAX_S + 3_600;
        let r = row(s, never, NOW - never);
        assert!(
            !matches!(r.attention(NOW), Attention::Quiet),
            "{s:?}: quiet with no end\n{r:?}"
        );
        assert!(r.interrupt(NOW).is_none(), "{s:?}");
        let day = BACKSTOP_S + 3_600;
        let r = row(s, day, NOW - day);
        if s["cause"] == 2 && s["reset"] == 0 {
            assert!(
                matches!(r.attention(NOW), Attention::Quiet),
                "{s:?}: a limit before its reset is a record for its whole span"
            );
        } else {
            assert!(
                !matches!(r.attention(NOW), Attention::Quiet),
                "{s:?}: a day without progress is told\n{r:?}"
            );
            stale_told += 1;
        }
    }
    assert!(
        quiet > 0 && faults > 0 && digests > 0 && stale_told > 0 && timed > 0,
        "{quiet} {faults} {digests} {stale_told} {timed}"
    );
}

fn quiet_row(s: &State, behind: u64, progress: u64) -> Row {
    let r = row(s, behind, progress);
    Row {
        quiet: true,
        limit_until: match (s["cause"], s["reset"]) {
            (2, 1) => NOW - LIMIT_GRACE_S - QUIET_MOVE_S - 60,
            _ => r.limit_until,
        },
        ..r
    }
}

#[test]
fn the_real_quiet_view_is_the_derived_machine_read_quietly() {
    let today = main_today();
    let states = reach(&today);
    let (mut quiet, mut faults, mut digests, mut timed, mut caught) = (0, 0, 0, 0, 0);
    for s in states.iter().filter(|s| s["phase"] <= 2) {
        for behind in BEHIND {
            let r = quiet_row(s, behind, NOW - 60);
            assert!(r.interrupt(NOW).is_none(), "{s:?}: an interrupt for a wait");
            assert!(r.badge(NOW).is_none(), "{s:?}: the tab is marked");
            match r.attention(NOW) {
                Attention::Quiet => quiet += 1,
                Attention::Fault(n) => {
                    assert_eq!(n.kind, "limit-passed", "{s:?}: {n:?}");
                    assert!(s["cause"] == 2 && s["reset"] == 1, "{s:?}");
                    faults += 1;
                }
                Attention::Digest(n) => {
                    assert_eq!(n.kind, "own-work", "{s:?}: {n:?}");
                    assert!(s["cause"] == 0 && behind >= DIGEST_AFTER_S, "{s:?}");
                    digests += 1;
                }
                Attention::Interrupt(n) => panic!("{s:?}: {n:?}"),
            }
            if s["cause"] == 2 {
                assert_eq!(
                    matches!(r.attention(NOW), Attention::Fault(_)),
                    s["reset"] == 1,
                    "{s:?}"
                );
            }
            let horizon = NOW + 10 * 86_400;
            let mut candidates = deadlines(&r);
            candidates.push(r.limit_until.saturating_add(LIMIT_GRACE_S + QUIET_MOVE_S));
            let seen = named(std::slice::from_ref(&r), NOW, horizon);
            for t in turns(&r, NOW, horizon, &candidates) {
                assert!(
                    seen.contains(&t),
                    "{s:?}: the quiet class turns at {t}, unnamed: {seen:?}"
                );
                timed += 1;
            }
        }
        let never = LIMIT_MAX_S + 3_600;
        let r = quiet_row(s, never, NOW - never);
        assert!(
            !matches!(r.attention(NOW), Attention::Quiet),
            "{s:?}: quiet with no end\n{r:?}"
        );
        assert!(r.interrupt(NOW).is_none(), "{s:?}");
        if s["cause"] == 2 && s["reset"] == 0 {
            let at_grace = Row {
                limit_until: NOW - LIMIT_GRACE_S - 60,
                ..quiet_row(s, BEHIND[0], NOW - 60)
            };
            assert!(
                matches!(at_grace.attention(NOW), Attention::Quiet),
                "{s:?}: the move's chance first"
            );
            let asking = Row {
                quiet: false,
                ..at_grace
            };
            assert!(
                matches!(asking.attention(NOW), Attention::Fault(_)),
                "{s:?}: the ask flow faults a spent limit at the grace"
            );
            caught += 1;
        }
    }
    assert!(
        quiet > 0 && faults > 0 && digests > 0 && timed > 0 && caught > 0,
        "{quiet} {faults} {digests} {timed} {caught}"
    );
}

#[test]
fn the_refused_notices_tell_is_an_instant_the_view_names() {
    let refused = Row {
        wait: "announce-refused:changed".to_string(),
        refused: 3,
        refused_at: NOW - 60,
        watch_at: NOW - 60 + TELL_S + WATCH_GAP / 2,
        ..pending(3_600)
    };
    let horizon = NOW + TELL_S + WATCH_GAP;
    let turned = turns(&refused, NOW, horizon, &deadlines(&refused));
    let tell = refused.refused_at + TELL_S;
    assert_eq!(
        turned,
        BTreeSet::from([tell]),
        "the class turns at the tell"
    );
    let seen = named(std::slice::from_ref(&refused), NOW, horizon);
    assert!(seen.contains(&tell), "named: {seen:?}");
    assert_eq!(next_change(std::slice::from_ref(&refused), NOW), Some(tell));
    let unwatched = Row {
        watch_at: 0,
        ..refused.clone()
    };
    let mut before = named(std::slice::from_ref(&unwatched), NOW, horizon);
    assert!(before.remove(&tell), "the arm names it");
    assert!(before.range(tell..).next().is_none(), "{before:?}");
    let uncounted = Row {
        refused: 0,
        refused_at: 0,
        ..refused.clone()
    };
    let by_age = uncounted.wait_since + HARNESS_TELL_S;
    let turned = turns(&uncounted, NOW, horizon, &deadlines(&uncounted));
    assert_eq!(turned, BTreeSet::from([by_age]), "{uncounted:?}");
    let seen = named(std::slice::from_ref(&uncounted), NOW, horizon);
    assert!(seen.contains(&by_age) && !seen.contains(&tell), "{seen:?}");
    let elsewhere = Row {
        wait: "background".to_string(),
        ..refused.clone()
    };
    let seen = named(std::slice::from_ref(&elsewhere), NOW, horizon);
    assert!(!seen.contains(&tell), "{elsewhere:?}: {seen:?}");
    assert!(
        turns(&elsewhere, NOW, horizon, &deadlines(&elsewhere)).is_empty(),
        "{elsewhere:?}"
    );
}
