// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorCreditsHold` (aterm-models-harness
//! `supervisor_credits_hold_model`): the REAL screen reader (`aterm_phase::read`,
//! over the measured 2.1.300 screen that says `Now using usage credits`, and
//! the same screen with the notice gone) and the REAL turn-end decider
//! (`decide_turn_end`, with the real `TurnEndState` it keeps) driven along
//! EVERY reachable state of the model, under `[harness] usage_credits` off
//! (the default since the owner's ruling of 2026-10-19) and on.
//!
//! Each model action is mirrored on the real side: a turn's end is the point
//! `observe`d with the turn's work, the reading's reset the one the loop keeps
//! from the notice credits started on (`Session::note_credits`), dropped once
//! a composer shows no notice past it (or past the bound with no placed
//! reset), and the episode's start the loop's first read of the notice;
//! `CreditsStart` keeps a reset `ResetAt` ticks out
//! less the grace (`reset_grace` = one tick), so the wait ends at the model's
//! reset; `CreditsStartUnread` keeps none; the hold's bound with none named
//! (`credits_bound`) is `Bound` ticks; `NoticeGone` removes only the drawn
//! no-reset notice, keeping its known episode until the bound; `Tick` moves
//! the clock a minute; `OwnAct` is the decider's continuation, recorded with
//! `acted`; `Tell` is
//! its escalation, recorded with `credits_told` as the loop does. At every
//! point the decider TYPES exactly where `OwnAct` is enabled, TELLS exactly
//! where `Tell` is, and otherwise waits until exactly the tick the reset —
//! with none named, the bound — makes `OwnAct` enabled.
//!
//! NEGATIVE CONTROLS: a decider run under the key on where the model has it
//! off, a loop that keeps no reset from the started notice, and a hold with no
//! bound (the policy before the review of 2026-10-19) disagree — each exactly
//! at the points the hold decides; the model's `Buggy` branch (`ActsOnCredits`)
//! breaks `NoOwnActOnCredits`, `StaleNoticeHolds` breaks `NoStaleHold`, and
//! `UnboundedHold` breaks `NoEndlessHold`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::policy::turn_end::{
    CREDITS_HOLD_TOLD, CreditsCarry, RULE_CONTINUE, TurnEndAction, TurnEndReading, TurnEndState,
    TurnEndTiming, decide_turn_end,
};
use aterm_models_harness::supervisor_credits_hold_model;
use aterm_phase::prompt::fixtures::{CREDITS_IN_USE_2_1_300, screen};
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);

/// A turn's work past the short streak's: a job.
const JOB: Duration = Duration::from_secs(12 * 60);

/// The grace a wait keeps past a reset: one tick.
const GRACE: Duration = MIN;

/// The screen at a turn's end, and the REAL reader's read of it, each made
/// once: the measured screen with `Now using usage credits` on the mode row,
/// or the same row with the notice gone.
fn read(notice: bool) -> &'static (Vec<String>, aterm_phase::Reading) {
    static READ: std::sync::OnceLock<[(Vec<String>, aterm_phase::Reading); 2]> =
        std::sync::OnceLock::new();
    let both = READ.get_or_init(|| {
        let on = screen(CREDITS_IN_USE_2_1_300);
        let mut off = on.clone();
        let mode = off
            .iter()
            .position(|r| r.contains("auto mode on"))
            .expect("the mode row");
        off[mode] = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string();
        [off, on].map(|rows| {
            let cursor = rows.iter().rposition(|r| r.starts_with('❯'));
            let read = aterm_phase::read(Some("claude"), &rows, cursor);
            (rows, read)
        })
    });
    &both[usize::from(notice)]
}

/// The REAL turn-end reading of a point: the reader's, its credits given the
/// reset the loop kept (`kept`) and its first read of the notice (`since`),
/// as `Session::turn_end_reading` gives it.
fn reading(notice: bool, known: bool, kept: Option<Instant>, since: Instant) -> TurnEndReading {
    let (rows, read) = read(notice);
    assert_eq!(
        read.credits.is_some(),
        notice,
        "the real reader reads the notice"
    );
    let mut r = TurnEndReading::of(read, rows, false, Some(JOB), None, None);
    r.credits = known.then(|| CreditsCarry {
        reset_at: kept,
        since: Some(since),
        kept: !notice,
        ..CreditsCarry::default()
    });
    r
}

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    /// The tick credits started on (the model's `clock` 0).
    start: Instant,
    /// The loop has read a credits notice in this episode, whether drawn now.
    known: bool,
    /// The reset the loop keeps from the notice credits started on.
    kept: Option<Instant>,
    /// The point's reading, as observed; `None` while a turn runs.
    point: Option<TurnEndReading>,
}

/// How a walk departs from the real loop, for the negative controls.
#[derive(Clone, Copy, Default)]
struct Run {
    /// The decider under the key on, whatever the model's dial says.
    key_on: bool,
    /// A loop that keeps no reset from the notice credits started on.
    forgets_reset: bool,
    /// A hold with none named that never lifts: no bound.
    unbounded: bool,
}

/// What a decision does at a point, as the model tells it apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Types,
    Tells,
    WaitsTo(Instant),
    Nothing,
    Other(String),
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Verdict::Types,
        TurnEndAction::Escalate { reason } if reason == CREDITS_HOLD_TOLD => Verdict::Tells,
        TurnEndAction::WaitUntil { until, .. } => Verdict::WaitsTo(*until),
        TurnEndAction::Nothing => Verdict::Nothing,
        other => Verdict::Other(format!("{other:?}")),
    }
}

/// Whether `action` is enabled at `st`.
fn enabled(m: &Model, action: &str, st: &State) -> bool {
    m.fire(action, &mut st.clone())
}

/// What the model says the decider does at the point `st`.
fn expected(m: &Model, st: &State, real: &Real) -> Verdict {
    if enabled(m, "OwnAct", st) {
        Verdict::Types
    } else if enabled(m, "Tell", st) {
        Verdict::Tells
    } else if st["named"] == 1 {
        Verdict::WaitsTo(real.start + MIN * u32::try_from(cnst(m, "ResetAt")).unwrap())
    } else if st["told"] == 1 {
        Verdict::WaitsTo(real.start + MIN * u32::try_from(cnst(m, "Bound")).unwrap())
    } else {
        Verdict::Nothing
    }
}

/// The policy the real decider runs under.
fn policy(m: &Model, run: Run) -> SupervisorConfig {
    SupervisorConfig {
        usage_credits: run.key_on || cnst(m, "UsageCredits") == 1,
        ..SupervisorConfig::default()
    }
}

/// Mirrors `action` (from `st` to `next`) on the real side.
fn mirror(m: &Model, action: &str, next: &State, r: &mut Real, cfg: &SupervisorConfig, run: Run) {
    let reset_at = MIN * u32::try_from(cnst(m, "ResetAt")).unwrap();
    match action {
        "CreditsStart" | "CreditsStartUnread" => {
            r.start = r.now;
            r.known = true;
            r.kept =
                (action == "CreditsStart" && !run.forgets_reset).then(|| r.now + reset_at - GRACE);
        }
        "Tick" => r.now += MIN,
        "NoticeGone" | "NoticeReturns" => {
            let notice = action == "NoticeReturns";
            if notice {
                // A new loop-carried notice; the policy keeps any earlier
                // episode's original start when that episode still stands.
                if !r.known {
                    r.start = r.now;
                }
                r.known = true;
            } else if r
                .kept
                .map_or(r.now >= r.start + r.st.timing.credits_bound, |reset| {
                    r.now >= reset
                })
            {
                r.known = false;
                r.kept = None;
            }
            if r.point.is_some() {
                let mut point = reading(notice, r.known, r.kept, r.start);
                point.worked = None;
                r.st.observe(&point, r.now);
                r.point = Some(point);
            }
        }
        "TurnEnds" | "AnotherWindow" => {
            let notice = next["notice"] == 1;
            // Independently mirror Session::note_credits' carrier expiry;
            // do not take the model's `known` field as the real answer.
            if !notice
                && r.kept
                    .map_or(r.now >= r.start + r.st.timing.credits_bound, |reset| {
                        r.now >= reset
                    })
            {
                r.known = false;
                r.kept = None;
            }
            // Another window past the one named, or past the bound: a new
            // episode, from this point.
            if action == "AnotherWindow" && next["clock"] == 0 {
                r.start = r.now;
            }
            let point = reading(notice, r.known, r.kept, r.start);
            r.st.observe(&point, r.now);
            r.point = Some(point);
        }
        "PersonTurn" => r.point = None,
        "OwnAct" | "Tell" => {
            let point = r.point.take().expect("a point");
            let a = decide_turn_end(&r.st, &point, cfg, r.now);
            if action == "Tell" {
                r.st.credits_told();
                r.point = Some(point);
            } else {
                r.st.acted(&a, &point, r.now);
            }
        }
        other => panic!("no mirror for {other}"),
    }
}

/// The walk's result: every state reached, those where the real decider
/// disagreed, and how often each action was mirrored.
struct Walked {
    states: BTreeSet<State>,
    off: Vec<State>,
    fired: BTreeMap<&'static str, usize>,
}

/// Every reachable state of `m`, the real side mirrored along the first path
/// to each; at every point the real decision is checked against the model's.
/// `strict`: a disagreement, or a broken invariant, fails at once.
fn walk(m: &Model, strict: bool, run: Run) -> Walked {
    let cfg = policy(m, run);
    let base = Instant::now() + Duration::from_secs(24 * 3600);
    let bound = if run.unbounded {
        Duration::from_secs(1000 * 3600)
    } else {
        MIN * u32::try_from(cnst(m, "Bound")).unwrap()
    };
    let init = Real {
        st: TurnEndState::new(TurnEndTiming {
            reset_grace: GRACE,
            reset_jitter: Duration::ZERO,
            credits_bound: bound,
            ..TurnEndTiming::default()
        }),
        now: base,
        start: base,
        known: false,
        kept: None,
        point: None,
    };
    let mut seen = BTreeSet::new();
    let mut off = Vec::new();
    let mut fired: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        if let Some(point) = &real.point {
            let got = verdict(&decide_turn_end(&real.st, point, &cfg, real.now));
            let want = expected(m, &st, &real);
            if got != want {
                assert!(
                    !strict,
                    "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
                );
                off.push(st.clone());
            }
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            mirror(m, action.name, &next, &mut r, &cfg, run);
            *fired.entry(action.name).or_default() += 1;
            queue.push_back((next, r));
        }
    }
    Walked {
        states: seen,
        off,
        fired,
    }
}

/// The model under the key's two values.
fn under_key(on: bool) -> Model {
    aterm_spec::interp::with_consts(
        &supervisor_credits_hold_model(),
        &[("UsageCredits", i64::from(on))],
    )
}

/// THE CREDITS HOLD, WALKED: every reachable state of the model, under the
/// key off and on, agrees with the real reader and decider — the hold types
/// nothing while credits carry the session under the key off, waits to the
/// named reset and then continues, tells once where none is named and waits
/// to its bound — and the model's laws hold at every state. Every action of
/// the model's own (not its defects') is mirrored.
#[test]
fn tier1_the_real_decider_holds_usage_credits_exactly_where_the_model_says() {
    for on in [false, true] {
        let m = under_key(on);
        let walked = walk(&m, true, Run::default());
        let own = [
            "CreditsStart",
            "CreditsStartUnread",
            "NoticeGone",
            "NoticeReturns",
            "Tick",
            "TurnEnds",
            "AnotherWindow",
            "PersonTurn",
            "OwnAct",
        ];
        for a in own {
            assert!(
                walked.fired.contains_key(a),
                "{a} never mirrored: {:?}",
                walked.fired
            );
        }
        assert_eq!(
            walked.fired.contains_key("Tell"),
            !on,
            "the owner is told only under the key off: {:?}",
            walked.fired
        );
        assert!(walked.states.len() > 10, "{}", walked.states.len());
        // These states are reached and checked against the actual policy,
        // not inferred from a drawn notice: known credits hold before their
        // bound even with no notice; no episode and expiry permit an act.
        let point = |s: &&State| s["point"] == 1 && s["notice"] == 0;
        assert!(
            walked
                .states
                .iter()
                .filter(point)
                .any(|s| { s["known"] == 0 && enabled(&m, "OwnAct", s) })
        );
        if !on {
            let bound = cnst(&m, "Bound");
            assert!(walked.states.iter().filter(point).any(|s| {
                s["known"] == 1
                    && s["named"] == 0
                    && s["clock"] == bound - 1
                    && !enabled(&m, "OwnAct", s)
            }));
            assert!(walked.states.iter().filter(point).any(|s| {
                s["known"] == 1
                    && s["named"] == 0
                    && s["clock"] == bound
                    && enabled(&m, "OwnAct", s)
            }));
        }
    }
}

/// NEGATIVE CONTROLS: a decider under the key on where the model has it off
/// disagrees only at a known episode's hold, including a vanished notice;
/// a loop that keeps no reset disagrees at the named reset (and can carry
/// that error into a later episode); a hold with no bound only where none
/// is — its wait, and the probe
/// at the bound.
/// The model's defect `ActsOnCredits` breaks `NoOwnActOnCredits`,
/// `StaleNoticeHolds` breaks `NoStaleHold`, and `UnboundedHold` breaks
/// `NoEndlessHold`.
#[test]
fn tier1_a_decider_that_ignores_the_hold_disagrees_where_the_model_holds() {
    let m = under_key(false);
    let ignores = walk(
        &m,
        false,
        Run {
            key_on: true,
            ..Run::default()
        },
    );
    assert!(
        !ignores.off.is_empty(),
        "the key-on decider agreed everywhere"
    );
    assert!(
        ignores.off.iter().all(|st| st["known"] == 1),
        "{:?}",
        ignores.off
    );
    assert!(ignores.off.iter().any(|st| st["notice"] == 0));
    let forgets = walk(
        &m,
        false,
        Run {
            forgets_reset: true,
            ..Run::default()
        },
    );
    assert!(
        !forgets.off.is_empty(),
        "the forgetful loop agreed everywhere"
    );
    assert!(
        forgets.off.iter().any(|st| {
            st["named"] == 1
                && st["clock"] == cnst(&m, "ResetAt")
                && st["known"] == 1
                && st["point"] == 1
        }),
        "{:?}",
        forgets.off
    );
    let unbounded = walk(
        &m,
        false,
        Run {
            unbounded: true,
            ..Run::default()
        },
    );
    assert!(
        !unbounded.off.is_empty(),
        "the unbounded hold agreed everywhere"
    );
    assert!(
        unbounded.off.iter().all(|st| st["named"] == 0),
        "{:?}",
        unbounded.off
    );
    assert!(
        unbounded
            .off
            .iter()
            .any(|st| st["clock"] >= cnst(&m, "Bound")),
        "the probe at the bound withheld: {:?}",
        unbounded.off
    );
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in ["CreditsStart", "TurnEnds", "ActsOnCredits"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoOwnActOnCredits", &st), "{st:?}");
    let mut st = buggy.init_state();
    for a in [
        "CreditsStart",
        "TurnEnds",
        "Tick",
        "Tick",
        "StaleNoticeHolds",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoStaleHold", &st), "{st:?}");
    let mut st = buggy.init_state();
    for a in [
        "CreditsStartUnread",
        "TurnEnds",
        "UnboundedHold",
        "Tick",
        "Tick",
        "Tick",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoEndlessHold", &st), "{st:?}");
}
