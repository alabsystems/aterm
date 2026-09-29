// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorNetworkWall` (aterm-spec
//! `supervisor_network_wall_model`): the REAL turn-end decider
//! (`decide_turn_end`, with the real `TurnEndState` it keeps) driven along
//! EVERY reachable state of the model, so the committed answer to an API
//! error the network caused — the outage of 2026-09-27's `API Error: Can't
//! reach the API server … (ENOTFOUND)`, and a reply the connection cut off —
//! is the model's, state for state.
//!
//! Each model action is mirrored on the real side. The world (`NetFails`,
//! `NetReturns`) is invisible to the decider; the host's measure is the
//! reading's `reach` (`Unknown`, `Down`, `Up`); a wall appearing is the
//! point `observe`d with the vendor's own retries as its busy work (3 min,
//! short of the real `progress`); time at the wall is the real clock moved
//! one minute per tick, with the real ladders scaled to the model's
//! constants (`net_backoff` = `Rung1`, `Rung2` min, `down_hold` = `Hold`
//! min); the model's `Act` is the real decider's own act, recorded with
//! `acted`; the act meeting the wall again is the next point observed with
//! the vendor's retries (the same episode) or with ten minutes of work (a
//! new one, past `progress`); and `Taken` is an idle point with no wall
//! after real work, which ends the track.
//!
//! The walk asks the decider at EVERY state, as the loop does at every
//! change that can move its answer: the due its last wait named (a tick)
//! and each new run of the host's measure (the loop's `reach_edge`, which
//! since the review of 2026-09-27 decides again on a measure lost or newly
//! down, not only on an `Up`).
//!
//! At every state, the model's invariants hold, and the real verdict is
//! checked against the model: at a wall that shows, the real decider TYPES
//! exactly where `Act` is enabled — under the rule the model's branch
//! names (`api-back@v1` for an unreachable API measured up, `api-cutoff@v1`
//! for a reply cut off and not measured down, `api-retry@v1` otherwise) and
//! in the words `wall_retry_text` gives for it, the vendor's line quoted —
//! and otherwise WAITS until exactly the tick the model's `Act` would
//! become enabled with nothing else moving; while the worker works or the
//! act is in flight it does nothing. NEGATIVE CONTROLS: the `Buggy = 1`
//! model (the day's supervisor: at once at every appearance, whatever the
//! measure) disagrees with the real decider — among others at a measured
//! outage's appearance, where it types and the real decider waits the hold
//! — so a green walk is not vacuous; and so does `Carry = 1` (the hold's
//! clock carried across an episode's appearances), at the outage's next
//! appearance under a Down measure.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, Reach, TurnEndAction, TurnEndReading,
    TurnEndState, TurnEndTiming, decide_turn_end, wall_retry_text,
};
use aterm_phase::{ApiCause, WallKind};
use aterm_spec::derive::{Model, supervisor_network_wall_model};

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);
/// The vendor's own retries before it gives up (about 3 minutes in the
/// outage's journal): busy work short of the real `progress`.
const VENDOR_RETRIES: Duration = Duration::from_secs(3 * 60);
/// Real work between an act and the wall's next appearance: past `progress`.
const REAL_WORK: Duration = Duration::from_secs(10 * 60);

const ENOTFOUND: &str =
    "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
const ASLEEP: &str =
    "API Error: Your computer went to sleep mid-response. The response above may be incomplete.";

fn cnst(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no const {name}"))
}

fn minutes(m: &Model, name: &str) -> Duration {
    MIN * u32::try_from(cnst(m, name)).expect("a small constant")
}

/// The real policy's clocks, scaled so one model tick is one minute: the
/// short ladder's two rungs, the hold, and the defaults' `progress` (5 min,
/// between the vendor's retries and real work).
fn timing(m: &Model) -> TurnEndTiming {
    TurnEndTiming {
        net_backoff: vec![minutes(m, "Rung1"), minutes(m, "Rung2")],
        down_hold: minutes(m, "Hold"),
        ..TurnEndTiming::default()
    }
}

fn cause_of(st: &State) -> ApiCause {
    if st["cause"] == 1 {
        ApiCause::CutOff
    } else {
        ApiCause::Unreachable
    }
}

/// The screen the model state stands for: at a wall that shows, the
/// vendor's message as the reader reads it, with the host's measure; else a
/// worker at work (the act in flight, or its turn after the wall left).
fn reading(st: &State, worked: Option<Duration>, at: Instant) -> TurnEndReading {
    let showing = st["wall"] == 1;
    let cause = cause_of(st);
    TurnEndReading {
        phase: if showing { Phase::Idle } else { Phase::Busy },
        authoritative: true,
        survey: false,
        wall: showing.then_some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause,
        }),
        wall_message: match (showing, cause) {
            (false, _) => String::new(),
            (true, ApiCause::CutOff) => ASLEEP.to_string(),
            (true, _) => ENOTFOUND.to_string(),
        },
        reset_at: None,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: None,
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        resume: None,
        upgrading: false,
        taskless: false,
        login_back: false,
        person: None,
        reach: match st["verdict"] {
            1 => Reach::Down { since: at },
            2 => Reach::Up { since: at },
            _ => Reach::Unknown,
        },
        program: aterm_phase::Program::Claude,
        goal: None,
        model_field: None,
        limits: Default::default(),
        sandbox_fell: None,
        thread_model: None,
        upgrade_goal: false,
    }
}

/// An idle point with no wall after real work: the act was taken.
fn taken(at: Instant) -> TurnEndReading {
    let mut quiet = State::new();
    for v in ["wall", "cause", "verdict"] {
        quiet.insert(v, 0);
    }
    TurnEndReading {
        phase: Phase::Idle,
        worked: Some(REAL_WORK),
        said_tail: Some("Stage done.".to_string()),
        ..reading(&quiet, None, at)
    }
}

/// The real side of one walk: the decider's state and the loop's clock.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
}

/// What the decider says, as one of the model's words.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Typed, under this rule.
    Act(&'static str),
    /// Waited, this long.
    Wait(Duration),
    Nothing,
}

/// The rule the model's `Act` at `st` is taken under (the branch its
/// guard's disjunct names).
fn model_rule(st: &State) -> &'static str {
    match (st["cause"], st["verdict"]) {
        (0, 2) => RULE_API_BACK,
        (_, 1) => RULE_API_RETRY,
        (0, _) => RULE_API_RETRY,
        _ => RULE_API_CUTOFF,
    }
}

/// What the model says the decider does at `st`: at a wall that shows, the
/// act where `Act` is enabled, else a wait until the first tick it is —
/// found by moving `waited` alone, as time alone would.
fn expected(m: &Model, st: &State) -> Verdict {
    if st["wall"] != 1 {
        return Verdict::Nothing;
    }
    if m.action_enabled("Act", st) {
        return Verdict::Act(model_rule(st));
    }
    let hold = cnst(m, "Hold");
    let due = (st["waited"] + 1..=hold)
        .find(|&w| {
            let mut later = st.clone();
            later.insert("waited", w);
            m.action_enabled("Act", &later)
        })
        .unwrap_or_else(|| panic!("a wall no tick ever acts on: {st:?}"));
    Verdict::Wait(MIN * u32::try_from(due - st["waited"]).unwrap())
}

/// What the real decider says at `st`, with the act's words checked: the
/// vendor's line quoted, under the rule's own sentence.
fn verdict(a: &TurnEndAction, st: &State, now: Instant) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } => {
            let r = reading(st, None, now);
            assert_eq!(
                *text,
                wall_retry_text(rule_id, cause_of(st), &r.wall_message),
                "the words at {st:?}"
            );
            Verdict::Act(rule_id)
        }
        TurnEndAction::WaitUntil { until, .. } => {
            Verdict::Wait(until.saturating_duration_since(now))
        }
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for at {st:?}: {other:?}"),
    }
}

/// Mirror one model action on the real side (the model has fired it). A
/// buggy `Act` the real decider would not take is refused (`false`): the
/// walk does not descend through it.
fn mirror(
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
) -> bool {
    let now = real.now;
    match action {
        "NetFails" | "NetReturns" | "MeasureDown" | "MeasureUp" | "MeasureLost" | "MeasureLies"
        | "MeasureWrongDown" => {}
        "Unreachable" | "CutOff" | "MetUnreachable" | "MetCutOff" => {
            real.st
                .observe(&reading(after, Some(VENDOR_RETRIES), now), now);
        }
        "WorkedThenUnreachable" | "WorkedThenCutOff" => {
            real.st.observe(&reading(after, Some(REAL_WORK), now), now);
        }
        "Taken" => real.st.observe(&taken(now), now),
        "Tick" => real.now += MIN,
        "Act" => {
            let r = reading(before, None, now);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            if !matches!(a, TurnEndAction::Type { .. }) {
                return false;
            }
            real.st.acted(&a, &r, now);
        }
        other => panic!("an action this bind does not mirror: {other}"),
    }
    true
}

/// Walk `m` against the real decider: the reachable state count, every
/// verdict word seen, and each state where the two disagree. `strict`
/// fails at the first disagreement (the model under test); otherwise they
/// are collected (the negative control's).
fn walk(m: &Model, strict: bool) -> (usize, BTreeMap<&'static str, usize>, Vec<State>) {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(3600);
    let init = Real {
        st: TurnEndState::new(timing(m)),
        now: base,
    };
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut words = BTreeMap::<&str, usize>::new();
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
        // The loop's look at the point still showing: folded in again with
        // no work (which changes nothing), then decided.
        let mut at = real.st.clone();
        let here = reading(&st, None, real.now);
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, &cfg, real.now), &st, real.now);
        let want = expected(m, &st);
        if got != want {
            assert!(
                !strict,
                "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
            );
            disagree.push(st.clone());
        }
        *words
            .entry(match got {
                Verdict::Act(rule) => rule,
                Verdict::Wait(_) => "wait",
                Verdict::Nothing => "nothing",
            })
            .or_default() += 1;
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(action.name, &st, &next, &mut r, &cfg) {
                queue.push_back((next, r));
            }
        }
    }
    (seen.len(), words, disagree)
}

#[test]
fn tier1_the_real_decider_types_into_a_network_wall_exactly_where_the_model_allows() {
    let m = supervisor_network_wall_model();
    let (states, words, _) = walk(&m, true);
    assert_eq!(states, 4236, "the reachable space changed: {states}");
    for word in [
        RULE_API_BACK,
        RULE_API_CUTOFF,
        RULE_API_RETRY,
        "wait",
        "nothing",
    ] {
        assert!(
            words.get(word).copied().unwrap_or(0) > 0,
            "{word} never occurred: {words:?}"
        );
    }
}

/// NEGATIVE CONTROL: the day's supervisor (`Buggy = 1`, at once at every
/// appearance whatever the measure) is NOT the real decider. It disagrees
/// at a measured outage's appearance — it types, the real decider waits
/// the hold — and the walk finds that without being told where to look.
#[test]
fn the_buggy_model_disagrees_with_the_real_decider() {
    let m = supervisor_network_wall_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let (_, _, disagree) = walk(&buggy, false);
    assert!(!disagree.is_empty(), "the Buggy model agreed everywhere");
    assert!(
        disagree
            .iter()
            .any(|st| st["wall"] == 1 && st["verdict"] == 1 && st["waited"] == 0),
        "no disagreement at a measured outage's appearance: {} states",
        disagree.len()
    );

    // `Carry = 1` — the hold's clock carried across the episode's
    // appearances, `Act`'s guard untouched — is not the real decider either:
    // it measures the hold from each appearance (`since` restarted when the
    // wall comes back after its try), so the outage's re-appearance under a
    // Down measure waits the hold again where `Carry` types at once.
    let carry = aterm_spec::interp::with_consts(&m, &[("Carry", 1)]);
    let (_, _, carried) = walk(&carry, false);
    assert!(
        carried
            .iter()
            .any(|st| st["wall"] == 1 && st["verdict"] == 1 && st["dacts"] == 1),
        "Carry = 1 agreed at every re-appearance of a measured outage: {} states",
        carried.len()
    );

    // And the one state spelled out: the network down and measured so, the
    // wall just shown.
    let mut st = m.init_state();
    for a in ["NetFails", "MeasureDown", "Unreachable"] {
        assert!(m.fire(a, &mut st), "{a}");
    }
    assert!(buggy.action_enabled("Act", &st));
    assert!(!m.action_enabled("Act", &st));
    let now = Instant::now() + Duration::from_secs(3600);
    let mut real = TurnEndState::new(timing(&m));
    real.observe(&reading(&st, Some(VENDOR_RETRIES), now), now);
    assert_eq!(
        verdict(
            &decide_turn_end(
                &real,
                &reading(&st, None, now),
                &SupervisorConfig::default(),
                now
            ),
            &st,
            now
        ),
        Verdict::Wait(minutes(&m, "Hold")),
    );
}
