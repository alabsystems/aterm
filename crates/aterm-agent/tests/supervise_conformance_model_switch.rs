// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorModelSwitch` (aterm-spec `supervisor_model_switch_model`):
//! the real turn-end decider (`decide_turn_end`, with `TurnEndState`'s `observe`,
//! `acted` and `restarted`) driven along every reachable state of the model, so the
//! answer to a model limit (the fallback asked for until the host can make it, the
//! continuation first at the reset, a return only at a named reset that was ahead
//! when the switch was made) is the model's, state for state.
//!
//! The agent's own jobs (`JobsStart`, `JobsEnd`) are the host's word, invisible to the
//! decider, so the walk plays the host: a relaunch asked for while `jobs = 1` is
//! refused with `wait:changed` (the loop decides again and never calls `restarted`),
//! one asked for with `jobs = 0` is made (`restarted(.., true, ..)`, as
//! `restart_agent` records it). Time is the real clock moved a minute per tick with
//! grace and jitter at zero. At every state the model's invariants hold, and:
//!
//! * at a wall on the bucket's model (`bucket = 1`, `on = 0`) the decider types the
//!   continuation exactly where `Continue` is enabled, and asks the host for the
//!   fallback exactly where `AskFallback` is;
//! * on the fallback (`on = 1`) at an idle point it asks for the return
//!   (`Restart::ModelBack`) exactly where the switch owes one and its reset has come
//!   (the host permitting is the model's `Return`), and never otherwise;
//! * on the bucket's model with no wall it asks for nothing.
//!
//! Negative controls: the `Buggy = 1` model (a return guessed a tick after an
//! unscheduled switch) and a `ModelSwitch` seeded with a guessed half hour's
//! `back_at` are each caught by the same comparison.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, ModelSwitch, RULE_LIMIT_RESUME, Restart, TurnEndAction, TurnEndReading, TurnEndState,
    TurnEndTiming, decide_turn_end,
};
use aterm_models_harness::supervisor_model_switch_model;
use aterm_phase::WallKind;
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);
const REAL_WORK: Duration = Duration::from_secs(30 * 60);
const WALL: &str =
    "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";

/// The real policy's clocks, scaled so one model tick is one minute and the reset
/// is acted on at the reset.
fn timing() -> TurnEndTiming {
    TurnEndTiming {
        reset_grace: Duration::ZERO,
        reset_jitter: Duration::ZERO,
        window: MIN * 24 * 60,
        ..TurnEndTiming::default()
    }
}

fn base_reading() -> TurnEndReading {
    TurnEndReading {
        phase: Phase::Idle,
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some("Slice done.".to_string()),
        said_cut: false,
        turn_said: None,
        opened_by: None,
        worked: Some(REAL_WORK),
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        left_record: false,
        left_work: None,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        background: false,
        served_in_transcript: false,
        notice_elsewhere: false,
        reset_clock: None,
        person: None,
        reach: Default::default(),
        program: aterm_phase::Program::Claude,
        account: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        record_turn: None,
        credits: None,
        repo: None,
    }
}

/// The point the model state stands for: the wall, on the bucket's model,
/// with the reset the notice named (`named`) `ResetAt` ticks after it
/// appeared; else an idle point (the fallback at work, or the bucket's model
/// with its limit lifted).
fn reading(m: &Model, st: &State, appeared: Instant, worked: Option<Duration>) -> TurnEndReading {
    let walled = st["bucket"] == 1 && st["on"] == 0;
    if !walled {
        return TurnEndReading {
            worked,
            ..base_reading()
        };
    }
    let reset =
        (st["named"] == 1).then(|| appeared + MIN * u32::try_from(cnst(m, "ResetAt")).unwrap());
    TurnEndReading {
        wall: Some(WallKind::ModelBucket { consent: false }),
        wall_message: WALL.to_string(),
        reset_at: reset,
        worked,
        ..base_reading()
    }
}

#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    appeared: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The continuation typed at the reset.
    Continue,
    /// The fallback asked of the host.
    AskFallback,
    /// The return asked of the host.
    Return,
    /// Anything else: a wait, nothing, the point's own act.
    Other,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LIMIT_RESUME => Verdict::Continue,
        TurnEndAction::Restart {
            why: Restart::Model { .. },
            ..
        } => Verdict::AskFallback,
        TurnEndAction::Restart {
            why: Restart::ModelBack { .. },
            ..
        } => Verdict::Return,
        _ => Verdict::Other,
    }
}

fn expected(m: &Model, st: &State) -> Verdict {
    if st["bucket"] == 1 && st["on"] == 0 {
        if m.action_enabled("Continue", st) {
            Verdict::Continue
        } else if m.action_enabled("AskFallback", st) {
            Verdict::AskFallback
        } else {
            Verdict::Other
        }
    } else if st["on"] == 1 && st["back"] == 1 && st["clock"] >= cnst(m, "ResetAt") {
        // The switch owes its return and the reset has come; whether the host
        // can make it now is the model's `Return` guard (`jobs`), not the decider's.
        Verdict::Return
    } else {
        Verdict::Other
    }
}

fn mirror(
    m: &Model,
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
) -> bool {
    let now = real.now;
    match action {
        "JobsStart" | "JobsEnd" => {}
        // The limit lifts: on the bucket's model the worker goes on by itself
        // (the vendor's own resume, a person), so the wall's track ends; on
        // the fallback nothing on the screen changes.
        "BucketLifts" => {
            if before["on"] == 0 {
                let r = reading(m, after, real.appeared, Some(REAL_WORK));
                real.st.observe(&r, now);
            }
        }
        "Tick" => real.now += MIN,
        "Works" => {
            let r = reading(m, after, real.appeared, Some(REAL_WORK));
            real.st.observe(&r, now);
        }
        "BucketHitsNamed" | "BucketHitsUnnamed" => {
            real.appeared = now;
            let r = reading(m, after, now, Some(REAL_WORK));
            real.st.observe(&r, now);
        }
        "Continue" => {
            let r = reading(m, before, real.appeared, None);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            if verdict(&a) != Verdict::Continue {
                return false;
            }
            real.st.acted(&a, &r, now);
            // The wall answers again: the same appearance, seen once more.
            let again = reading(m, after, real.appeared, Some(Duration::from_secs(3)));
            real.st.observe(&again, now);
        }
        "AskFallback" => {
            let r = reading(m, before, real.appeared, None);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            let TurnEndAction::Restart { why, .. } = &a else {
                return false;
            };
            // The host's word: refused while the agent's own jobs run (the
            // loop decides again after its pause and records nothing), made
            // when they do not.
            if before["jobs"] == 0 {
                real.st.restarted(why, true, &r, now);
            }
        }
        "Return" => {
            let r = reading(m, before, real.appeared, None);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            let TurnEndAction::Restart { why, .. } = &a else {
                return false;
            };
            if !matches!(why, Restart::ModelBack { .. }) {
                return false;
            }
            real.st.restarted(why, true, &r, now);
            // Back on the bucket's model: a limit that still stands shows
            // again at once.
            if after["bucket"] == 1 {
                real.appeared = now;
                let again = reading(m, after, now, Some(Duration::from_secs(3)));
                real.st.observe(&again, now);
            }
        }
        // The model's defects: the real side does nothing, and the comparison at
        // the state reached is the negative control's disagreement.
        "GuessedReturn" | "GivesUp" | "ReturnBlocks" => {}
        other => panic!("an action this bind does not mirror: {other}"),
    }
    true
}

fn walk(m: &Model, strict: bool) -> (usize, Vec<State>) {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(3600);
    let init = Real {
        st: TurnEndState::new(timing()),
        now: base,
        appeared: base,
    };
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut disagree = Vec::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        // The decider is asked at every state, as the loop asks at each change
        // that can move its answer (a tick, a screen, a host's word).
        let mut at = real.st.clone();
        let here = reading(m, &st, real.appeared, None);
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, &cfg, real.now));
        let want = expected(m, &st);
        if got != want {
            assert!(
                !strict,
                "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
            );
            disagree.push(st.clone());
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(m, action.name, &st, &next, &mut r, &cfg) {
                queue.push_back((next, r));
            }
        }
    }
    (seen.len(), disagree)
}

#[test]
fn tier1_the_real_decider_switches_and_returns_exactly_where_the_model_allows() {
    let m = supervisor_model_switch_model();
    let (states, _) = walk(&m, true);
    assert!(states > 50, "{states}");
}

/// Negative controls: the `Buggy = 1` model (a return guessed at a switch nothing
/// scheduled one for) is not the real decider; and a switch seeded with a guessed
/// half hour's `back_at` makes the real decider return where the committed switch
/// owes none.
#[test]
fn the_guessed_return_and_the_old_half_hour_disagree_with_the_real_code() {
    let m = supervisor_model_switch_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let (_, off) = walk(&buggy, false);
    assert!(
        off.iter().any(|st| st["on"] == 0 && st["flips"] >= 2),
        "no disagreement after a guessed return: {} states",
        off.len()
    );

    // The guessed rule, on the real side: a switch with a guessed `back_at`.
    let cfg = SupervisorConfig::default();
    let t = Instant::now() + Duration::from_secs(3600);
    let mut st = TurnEndState::new(timing());
    st.seed_model(ModelSwitch {
        from: Some("fable".to_string()),
        to: "opus".to_string(),
        back_at: Some(t + 30 * MIN),
    });
    let idle = base_reading();
    st.observe(&idle, t + 31 * MIN);
    let a = decide_turn_end(&st, &idle, &cfg, t + 31 * MIN);
    assert_eq!(
        verdict(&a),
        Verdict::Return,
        "the guessed half hour returns: {a:?}"
    );
    // The committed switch of a limit that named no reset owes none.
    let mut committed = TurnEndState::new(timing());
    let wall = TurnEndReading {
        wall: Some(WallKind::ModelBucket { consent: false }),
        wall_message: WALL.to_string(),
        ..base_reading()
    };
    committed.observe(&wall, t);
    let a = decide_turn_end(&committed, &wall, &cfg, t);
    let TurnEndAction::Restart { why, .. } = &a else {
        panic!("the fallback is asked for: {a:?}");
    };
    committed.restarted(why, true, &wall, t);
    assert_eq!(committed.model_switch().map(|s| s.back_at), Some(None));
    let later = t + 24 * 60 * MIN;
    committed.observe(&idle, later);
    assert_ne!(
        verdict(&decide_turn_end(&committed, &idle, &cfg, later)),
        Verdict::Return
    );
}
