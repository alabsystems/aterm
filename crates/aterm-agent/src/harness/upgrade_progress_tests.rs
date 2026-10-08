// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! NO UPGRADE STUCK FOREVER, BOUND TO THE CODE (design record 2026-09-28,
//! §4.4, adapted to main on 2026-09-29 and to the owner's alert rules): the
//! derived `HarnessUpgradeProgress` machine against the REAL reducer and the
//! REAL classifier, over every state the committed machine reaches — the
//! machine main IS today, its gap latch clear since 2026-09-29: `keep` (the
//! watch recorded only a read that waits; closed by `upgrade::held_point` and
//! `upgrade::NEXT_POINT`), a negative control now. The other gap, a limit a
//! record at any age, is the CLASSIFIER's claim (`St::limit_until`,
//! `Row::limit_passed`), bound in T1-f. Four tests:
//!
//! * T1-a, THE REDUCER — THE ASK-AND-DRAIN FLOW's (`upgrade_ask = true`,
//!   opt-in since 2026-09-29), whose steps the machine's are: each state
//!   becomes the facts of a real look — at an
//!   idle point, at a settled break (`background_point`), and as the watch's
//!   dry read sees the screen (no break known) — and the real
//!   `upgrade::requested_step` (with `upgrade::break_step` at a break)
//!   decides: the machine's `StepAtIdle`/`StepAtBreak` is enabled exactly
//!   where the real step acts, and lands the phase the real step's kind
//!   does; the word the real watch records of the dry read
//!   (`upgrade_watch::look_word` of the act's word and what holds its point,
//!   `upgrade::held_point`) is exactly the class the machine's `Watch`
//!   records (after `WatchRearms` where the read re-arms: the watch does
//!   both in one pass). NEGATIVE CONTROLS: 0.94's break arm (every phase
//!   answered `background` at a break), the terminal stop (before
//!   `d633246d4`: a stopped round waits `failed` for good), and the `Buggy`
//!   member `RestartOverWork` — each caught where the real reducer does
//!   otherwise — and two looks that are not main's: the one main had until
//!   2026-09-29 (`RecordedWordStands`: an act read records nothing, the
//!   latch's machine), and a posture-blind neutral word (every act read
//!   `next-point`), which names aterm's own class at a person's box where the
//!   machine names the person.
//! * T1-a', THE QUIET UPGRADE'S READ (`upgrade_ask = false`, the default):
//!   its one step, the restart at a genuinely idle point, is not the
//!   machine's, so its truth is not the machine's `T`; over the same
//!   reachable states, the word the real watch records of the real quiet
//!   read is the neutral one exactly where nothing holds the restart, and
//!   elsewhere the class of what does. NEGATIVE CONTROLS: the ask flow's
//!   read, `RestartOverWork`, main before C9, the posture-blind word.
//! * T1-f, THE CLASSIFIER OVER THE MACHINE'S STATES: each state becomes a
//!   record ([`Row`]); its projection ([`progress_projection`]) is the state,
//!   and the owner-facing class the real [`Row::attention`] gives it is the
//!   owner's rule (2026-09-28: "don't continue to persist alerts to the user
//!   that are not actionable"): a fresh record is QUIET — a wait is a record,
//!   never a row or a mark — save for a limit whose named reset has passed
//!   (a fault line, [`Row::limit_passed`]) and the agent's own work held for
//!   a day (one digest line); a record with NO PROGRESS for a day is told
//!   (a fault or a digest line), a limit only past its longest; and every
//!   turn of the class by time alone falls at an instant [`next_change`]
//!   names — the weak fairness of the view. T1-f', the same over the QUIET
//!   flow's rows ([`Row::quiet`], the default): a limit past its named reset is
//!   told only after the quiet move's chance. NEGATIVE CONTROL: the ask flow's
//!   classification over the same quiet world tells a spent limit at the
//!   instant the quiet move may first go.
//! * THE REFUSED NOTICE'S INSTANT (the assessment's gap 3, closed): a fault
//!   line that turns at `refused_at + TELL_S` is an instant [`next_change`]
//!   names — found unnamed by the same check, which the view without that arm
//!   still fails.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_spec::derive::{Model, harness_upgrade_progress_model};
use aterm_spec::interp::{self, State};

use super::*;
use crate::harness::upgrade::{Facts, Step};
use crate::harness::upgrade_drive::watch::look_word;
use crate::harness::upgrade_drive::{WATCH_GAP, at_point};

/// The target the reducer is asked about.
const TARGET: &str = "2.1.284";

/// The committed machine at `Buggy = 0` with both gap latches clear (as
/// `aterm-spec`'s `derived_harness_upgrade_progress_gaps_are_closed_on_main`
/// builds it).
fn main_today() -> Model {
    interp::with_buggy(&harness_upgrade_progress_model(), 0)
}

/// Main as it was before a gap closed: the committed machine with that gap's
/// latch (`keep`) set from the start.
fn latched(name: &str) -> Model {
    let mut m = main_today();
    for v in &mut m.vars {
        if v.name == name {
            v.init = 1;
        }
    }
    m
}

/// Every state `m` reaches, in breadth-first order.
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

/// How long the session has been behind, the classifier's horizons between
/// them: an hour, an hour past [`STALLED_AFTER_S`] (the old six-hour mark,
/// which is a word in `--status` now and no alert), an hour past
/// [`MOVE_BUDGET_S`] (the day the agent's own work becomes one digest line).
const BEHIND: [u64; 3] = [3_600, STALLED_AFTER_S + 3_600, MOVE_BUDGET_S + 3_600];

/// The record's phase at a state: an announced round at its last ask (the
/// machine's one ask), its notice a minute old or past its window; a round
/// that gave up; the move under way.
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
    /// At a point the loop offers: at a settled break, the break's step
    /// (`background_point`).
    Point,
    /// The watch's dry read, off any point: it never knows a break.
    Read,
}

/// The facts a look at state `s` reads, as `upgrade_drive`'s `look` builds
/// them: a turn running (Claude busy, the busy footer, no idle read), a
/// settled break (Claude busy under its background work), an idle point (idle
/// and settled), a usage limit's wall, a box up; processes under the agent
/// (`work`); the READY's age and the stop's rest as the round's clock says.
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

/// The real step at state `s` for a look: `requested_step`, and at a
/// settled break's point the one rule both drivers hold a break to.
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

/// The dry run's word for the real `step` at a read with facts `f`, as the
/// visit says it: the act's own word, and after it what holds the point it
/// waits for ([`upgrade::held_point`], [`at_point`]) — the word the host's
/// watch reads.
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

/// The machine's class of a recorded wait word ([`wait_class`], as
/// [`progress_projection`] reads it).
fn class(word: &str) -> i64 {
    match wait_class(word) {
        WaitClass::OwnWork => 0,
        WaitClass::Person | WaitClass::GoalLeftPaused => 1,
        WaitClass::Limit => 2,
        _ => 3,
    }
}

/// The 0.94 break arm: at a break of the agent's own work every phase waited
/// `background` before the phase was looked at (no notice, give-up or re-arm).
/// Replayed over the real reducer.
fn break_arm_094(s: &State, look: Look) -> Step {
    let (step, f) = real_step(s, look);
    if f.background_point {
        Step::Wait("background")
    } else {
        step
    }
}

/// The terminal stop: a stopped round waited `failed` for good, never
/// re-armed. Replayed over the real reducer (owner, 2026-09-27).
fn terminal_stop(s: &State, look: Look) -> Step {
    let (step, _) = real_step(s, look);
    if matches!(phase_of(s), Phase::Failed(_)) {
        Step::Wait("failed")
    } else {
        step
    }
}

/// The phase a real act lands the round in, in the machine's terms.
fn lands(step: Step) -> Option<i64> {
    match step {
        Step::Announce => Some(1),
        Step::GiveUp => Some(2),
        Step::Terminate | Step::Fresh => Some(3),
        Step::Rearm => Some(0),
        Step::Wait(_) | Step::Void(_) => None,
    }
}

/// T1-a, the reducer (see the module header).
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
        // The step at a point. The restart is its own action at an idle point
        // (`RestartAtIdle`: the READY heard, nothing under the agent); every
        // other act there is `StepAtIdle`'s; a break never restarts
        // (`break_step`).
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
            // Negative controls: each old rule refuses, somewhere, a step the
            // machine and the real reducer take.
            if today.action_enabled(action, s) {
                if lands(break_arm_094(s, Look::Point)).is_none() {
                    caught.insert("0.94's break arm");
                }
                if lands(terminal_stop(s, Look::Point)).is_none() {
                    caught.insert("the terminal stop");
                }
            }
        }
        // `RestartOverWork`: the restart over processes under the agent,
        // which the real reducer never takes.
        if buggy.action_enabled("RestartOverWork", s) {
            let (step, _) = real_step(s, Look::Point);
            assert_ne!(step, Step::Terminate, "{s:?}");
            caught.insert("RestartOverWork");
        }
        // The watch's read, at every posture: the dry read's word, and the
        // word the real watch records of it, the class the machine's `Watch`
        // records. Where the read re-arms, the watch re-arms and records the
        // new round's word in one pass: `WatchRearms`, then `Watch`.
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
        // The watch's re-arm: exactly where the real read re-arms.
        assert_eq!(
            today.action_enabled("WatchRearms", s),
            step == Step::Rearm,
            "{s:?}: {step:?}"
        );
        if !dry.starts_with("wait:") {
            acting += 1;
            // The gap is closed for a Claude Code record only: a Codex record's
            // act read records nothing and keeps the last word a look found.
            // Closing it for Codex changes this and the docs that say so.
            assert_eq!(
                look_word(&dry, false, || f.status.clone()),
                "",
                "{s:?}: a Codex record's `{dry}`"
            );
            // Negative control: a read that acts records nothing and the record
            // keeps its word (the latch's machine). Where that word is not what
            // holds the move now, it is caught.
            let old = s["cause"];
            let mut latched = with(s, "keep", 1);
            assert!(kept.fire("Watch", &mut latched), "{s:?}");
            assert_eq!(latched["cause"], old, "{s:?}: the latch is that look");
            if old != looked["cause"] {
                caught.insert("RecordedWordStands");
            }
            // NEGATIVE CONTROL — one neutral word for every act read, blind to
            // what holds its point: at a person's box it names aterm's own
            // class where the machine names the person (`NoStaleLook`).
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

    // The anchors are linked, name the one projection, and were entered.
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

/// The facts the watch's dry read reads at state `s` under the quiet upgrade
/// ([`Facts::quiet`]): T1-a's read ([`facts`], no break known) with the flow
/// set, the owner told long since (every state here is an hour behind or
/// more), and a limit's notice a grace past its named reset read as no limit.
fn quiet_facts(s: &State) -> Facts {
    let mut f = facts(s, Look::Read);
    f.quiet = true;
    f.unnoticed = BEHIND[0] < upgrade::QUIET_S;
    if s["posture"] == 3 && s["reset"] == 1 {
        f.limited = false;
    }
    f
}

/// The word the real watch records of the real reducer's read at `s` with
/// facts `f` ([`dry_word`], [`look_word`]).
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

/// WHAT HOLDS THE QUIET UPGRADE'S RESTART at state `s`, as classes of the
/// machine's `cause` — `None` where nothing does, the neutral word's: a
/// resting round's rest (its own); else, for the round the read would act on
/// (a rested one re-armed: the new pending round's), an idle point — or a
/// limit's notice a grace past its reset (C9) — with nothing under the agent
/// holds nothing, and with processes under it their own work; a turn and a
/// break, its own work; a limit before its reset, the limit; a person's box,
/// the person — and, with processes under the agent too, either, since both
/// hold the restart and the view tells both at the same instant. An
/// ANNOUNCED round past its window whose restart its own work holds GIVES UP
/// (round eight, #4: the quiet flow asks nothing again, and a round an older
/// aterm announced stood announced for good over work that never ended): the
/// release it owes goes at the next point a notice may go, which nothing
/// holds at an idle point.
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

/// Whether `word`, recorded at `s`, says what holds the quiet restart there
/// ([`quiet_truth`]).
fn quiet_bound(s: &State, word: &str) -> bool {
    match quiet_truth(s) {
        None => word == upgrade::NEXT_POINT,
        Some(classes) => word != upgrade::NEXT_POINT && classes.contains(&class(word)),
    }
}

/// T1-a', the quiet upgrade's read. The quiet flow's one step is the restart
/// at a genuinely idle point ([`upgrade::quiet_step`]), not the machine's; what
/// the flows share is the word the watch records, bound here over every
/// reachable state read with the quiet flow ([`quiet_facts`]): the neutral word
/// ([`upgrade::NEXT_POINT`]) exactly where nothing holds the restart, else the
/// class of what does ([`quiet_truth`]). Negative controls: the ask flow's
/// reducer, `RestartOverWork`, a notice past its reset still read as a limit,
/// and the posture-blind neutral word.
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
        // Negative control: the ask flow's read at the same state.
        let asking = Facts {
            quiet: false,
            ..f.clone()
        };
        if !quiet_bound(s, &read_word(s, &asking)) {
            caught.insert("the ask flow's read");
        }
        // Negative control: `RestartOverWork`, the processes unread.
        let blind_to_work = Facts {
            background: Vec::new(),
            ..f.clone()
        };
        if !quiet_bound(s, &read_word(s, &blind_to_work)) {
            caught.insert("RestartOverWork");
        }
        // Negative control: a notice past its reset is a limit.
        let before_c9 = Facts {
            limited: f.limited || s["posture"] == 3,
            ..f.clone()
        };
        if !quiet_bound(s, &read_word(s, &before_c9)) {
            caught.insert("main before C9");
        }
        // Negative control: one neutral word for every act read.
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

/// The record a state stands for: its phase, the class of its recorded
/// wait by a word of that class (its own work in the words each phase
/// records; a person's box; a limit; an act's empty word), how long it has
/// been `behind`, the round's clock through its stamps, a re-armed round's
/// `last_stop`. No holder and no processes named: the class is the word's.
/// `progress` is when the record last acted (its minting, notice or stop).
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
        // The limit's named reset as the transcript said it: passed (and its
        // grace) where the world's `reset` has, an hour ahead where not.
        limit_until: match (s["cause"], s["reset"]) {
            (2, 1) => NOW - LIMIT_GRACE_S - 60,
            (2, _) => NOW + 3_600,
            _ => 0,
        },
        ..pending(3_600)
    }
}

/// Every instant in `(from, horizon]` the view names for `rows`, one
/// [`next_change`] after another — when the host looks again by time alone.
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

/// The instants in `(from, horizon]` at which `row`'s owner-facing class
/// turns by time alone, among `candidates` (every stamp its deadlines are
/// counted from, plus each deadline): where [`Row::attention`]'s level one
/// second before differs from it at the instant.
fn turns(row: &Row, from: u64, horizon: u64, candidates: &[u64]) -> BTreeSet<u64> {
    candidates
        .iter()
        .copied()
        .filter(|t| *t > from && *t <= horizon)
        .filter(|t| row.attention(t - 1).level() != row.attention(*t).level())
        .collect()
}

/// A record's deadlines: the classifier's horizons counted from each stamp
/// it keeps, the round's window and rest, a refused notice's tell, a limit's
/// reset and grace.
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

/// T1-f, THE CLASSIFIER OVER THE MACHINE'S STATES (see the module header).
#[test]
fn the_real_view_is_the_derived_machine() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let today = main_today();
    let states = reach(&today);
    let (mut quiet, mut faults, mut digests, mut stale_told, mut timed) = (0, 0, 0, 0, 0);
    for s in states.iter().filter(|s| s["phase"] <= 2) {
        for behind in BEHIND {
            // A FRESH RECORD: it acted a minute ago.
            let r = row(s, behind, NOW - 60);
            // The projection IS the state.
            let proj = progress_projection(&r, NOW);
            let mut want: BTreeMap<&str, i64> = ["phase", "rearmed", "cause", "clock"]
                .into_iter()
                .map(|k| (k, s[k]))
                .collect();
            // The world's reset, as the record reads it: over a recorded limit.
            want.insert("reset", if s["cause"] == 2 { s["reset"] } else { 0 });
            assert_eq!(proj, want, "{s:?}\n{r:?}");
            // The class is the classifier's own reading of the record.
            assert_eq!(
                proj["cause"] == 0,
                r.overdue_cause(NOW) == OWN_WORK,
                "{s:?}"
            );
            // THE OWNER'S RULE, at every state: a wait is a record. Nothing
            // marks the tab or takes a row, whatever the age, and the only
            // things told of a record that acts are a limit that has ended
            // (aterm's own line in the log) and the agent's own work held
            // for a day (one digest line).
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
            // A LIMIT WHOSE RESET HAS PASSED is told, whatever the age or the
            // phase; one whose reset is ahead is a record (ruling 307).
            if s["cause"] == 2 {
                assert_eq!(
                    matches!(r.attention(NOW), Attention::Fault(_)),
                    s["reset"] == 1,
                    "{s:?}"
                );
            }
            // THE WEAK FAIRNESS OF THE VIEW: every turn of the class by time
            // alone is an instant [`next_change`] names, so the host looks then.
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
        // NO WAIT IS QUIET WITHOUT AN END: the same record with no progress
        // for the longest a quiet wait may stand is told — by a fault line in
        // the log or a digest line — unless it is a limit whose reset is
        // still ahead, which stands to [`LIMIT_MAX_S`] and no further.
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

/// The record a state stands for UNDER THE QUIET FLOW ([`Row::quiet`],
/// `upgrade_ask = false`, the default): [`row`], read with the flow set — and
/// the world's `reset` passed where the quiet flow's tell has: a limit past
/// the reset its row named, [`QUIET_MOVE_S`] after the instant the quiet move
/// may first go ([`Row::limit_passed`]).
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

/// T1-f', THE CLASSIFIER UNDER THE QUIET FLOW (the review of 2026-09-30): the
/// classifier reads the flow its rows are read under — an announced round
/// nothing asks again judged as pending, a limit past its named reset told
/// only after the quiet move's chance — and T1-f built every row under the ask
/// flow, so the default flow's classification was bound to nothing. Here every
/// state the machine reaches becomes a QUIET row ([`quiet_row`]) and is held to
/// the owner's rule as T1-f holds it: nothing marks the tab or takes a row;
/// the only things told of a record that acts are a limit whose reset and
/// the quiet move's chance have passed (a fault line) and the agent's own work
/// held for a day (one digest line); no wait is quiet without an end; and
/// every turn of the class by time alone, the quiet limit's own deadline
/// included, is an instant [`next_change`] names. NEGATIVE CONTROL, caught:
/// the ask flow's classification over a spent limit at the instant the quiet
/// move may first go, which faults it where the quiet flow still waits for the
/// move.
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
            // THE WEAK FAIRNESS OF THE VIEW, quietly: the quiet tell of a
            // limit is a deadline of its own.
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
        // NO WAIT IS QUIET WITHOUT AN END, quietly.
        let never = LIMIT_MAX_S + 3_600;
        let r = quiet_row(s, never, NOW - never);
        assert!(
            !matches!(r.attention(NOW), Attention::Quiet),
            "{s:?}: quiet with no end\n{r:?}"
        );
        assert!(r.interrupt(NOW).is_none(), "{s:?}");
        // NEGATIVE CONTROL: at the instant the ask flow tells a limit (the
        // grace past a named reset), the quiet world's reset has not passed
        // its move's chance.
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

/// THE REFUSED NOTICE'S INSTANT IS NAMED (the assessment of 2026-09-29, gap
/// 3, closed): a notice aterm's own fence refused is aterm's fault to fix
/// [`TELL_S`] after the first refusal ([`Row::refused_notice`]), before the
/// move is overdue — and [`next_change`] names `refused_at + TELL_S`, so the
/// view's host looks then. Until then it named no such instant: the class
/// came at the next one it did name (the record's watch, up to [`WATCH_GAP`]
/// later in a steady state, and hours later with no watch due). The check the
/// classifier bind makes of every state found it. NEGATIVE CONTROLS: the view
/// without that arm — the chain of instants it names less this one, which is
/// exactly what `next_change` named before, since each arm's instant is
/// independent of the others — leaves the turn unnamed, with no watch due for
/// hours; and the same row with no refusal counted, or a wait that is no
/// refused notice, names nothing of the kind and never turns.
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
    // Negative control: without the arm and with no watch due, no look is
    // named before the move is overdue, hours after the tell.
    let unwatched = Row {
        watch_at: 0,
        ..refused.clone()
    };
    let mut before = named(std::slice::from_ref(&unwatched), NOW, horizon);
    assert!(before.remove(&tell), "the arm names it");
    assert!(before.range(tell..).next().is_none(), "{before:?}");
    // NEGATIVE CONTROLS: no refusal counted — the wait is then aterm's own
    // stuck one from when it began ([`HARNESS_TELL_S`]), an instant the view
    // names too, but never `refused_at + TELL_S` — or no refused notice
    // waiting: no such instant, and no turn.
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
