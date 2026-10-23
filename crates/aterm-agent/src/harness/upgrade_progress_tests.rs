// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance tests binding `HarnessUpgradeProgress` to the upgrade reducer and classifier.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_harness::harness_upgrade_progress_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::*;
use crate::harness::upgrade::{Facts, Step};
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
    /// At a point the loop offers: at a settled break, the break's step (`background_point`).
    Point,
    /// The watch's dry read, off any point: it never knows a break.
    Read,
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
        &Request::None,
        &phase_of(s),
        &f,
        s["ready"] == 1,
        NOW,
        TARGET,
    );
    let step = if look == Look::Point && s["posture"] == 1 {
        upgrade::break_step(step)
    } else {
        step
    };
    (step, f)
}

fn dry_word(step: &Step, f: &Facts) -> String {
    let act = match step {
        Step::Wait(why) => return format!("wait:{why}"),
        Step::Announce => "would-announce".to_string(),
        Step::Terminate => "would-restart".to_string(),
        Step::Fresh => "would-restart:fresh".to_string(),
        Step::GiveUp => "gave-up".to_string(),
        Step::Rearm => format!("would-rearm:{}", upgrade::GAVE_UP),
        Step::Void(why) => format!("would-void:{why}"),
    };
    match upgrade::held_point(&Request::None, step, f, NOW, TARGET) {
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
        Step::Wait(_) | Step::Void(_) => None,
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
    let (mut idle_acts, mut break_acts, mut reads, mut acting) = (0, 0, 0, 0);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    for s in states.iter().filter(|s| s["phase"] <= 2) {
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
        let dry = dry_word(&step, &f);
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
        idle_acts > 0 && break_acts > 0 && reads > 0 && acting > 0,
        "{idle_acts} {break_acts} {reads} {acting}"
    );
    assert_eq!(
        caught,
        BTreeSet::from([
            "0.94's break arm",
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
            ("RestartAtIdle", "gate_restart"),
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
    ] {
        assert!(entered.contains(id), "{id} entered: {entered:?}");
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
        &Request::None,
        &phase_of(s),
        f,
        s["ready"] == 1,
        NOW,
        TARGET,
    );
    look_word(&dry_word(&step, f), true, || f.status.clone())
}

fn quiet_truth(s: &State) -> Option<BTreeSet<i64>> {
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
            &Request::None,
            &phase_of(s),
            &f,
            s["ready"] == 1,
            NOW,
            TARGET,
        );
        let dry = dry_word(&step, &f);
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
            let mut want: BTreeMap<&str, i64> = ["phase", "rearmed", "cause", "clock"]
                .into_iter()
                .map(|k| (k, s[k]))
                .collect();
            want.insert("reset", if s["cause"] == 2 { s["reset"] } else { 0 });
            assert_eq!(proj, want, "{s:?}\n{r:?}");
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
