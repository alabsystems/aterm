// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorNetworkWall` (aterm-spec `supervisor_network_wall_model`): the real turn-end decider (`decide_turn_end`, with its `TurnEndState`) driven along every reachable state of the model, so the answer to an API error the network caused (`ENOTFOUND`, or a reply cut off) is the model's, state for state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, Reach, TurnEndAction, TurnEndReading,
    TurnEndState, TurnEndTiming, decide_turn_end, wall_retry_text,
};
use aterm_models_harness::supervisor_network_wall_model;
use aterm_phase::{ApiCause, WallKind};
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);
/// The vendor's own retries before it gives up (about 3 minutes): busy work short of the real `progress`.
const VENDOR_RETRIES: Duration = Duration::from_secs(3 * 60);
/// Real work between an act and the wall's next appearance: past `progress`, its tools run.
const REAL_WORK: Duration = Duration::from_secs(10 * 60);

const ENOTFOUND: &str =
    "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
const ASLEEP: &str =
    "API Error: Your computer went to sleep mid-response. The response above may be incomplete.";
const OVERLONG: &str = "API Error: Claude's response exceeded the 32000 output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.";

fn minutes(m: &Model, name: &str) -> Duration {
    MIN * u32::try_from(cnst(m, name)).expect("a small constant")
}

/// The real policy's clocks, scaled so one model tick is one minute: the short ladder's two rungs, the hold, and the defaults' `progress` (5 min, between the vendor's retries and real work).
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

/// The screen the model state stands for: at a wall that shows, the vendor's message as the reader reads it, with the host's measure (made `measured`); else a worker at work (the act in flight, or its turn after the wall left).
fn reading(st: &State, worked: Option<Duration>, measured: Instant) -> TurnEndReading {
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
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: None,
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
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
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        person: None,
        reach: match st["verdict"] {
            1 => Reach::Down { since: measured },
            2 => Reach::Up { since: measured },
            _ => Reach::Unknown,
        },
        program: aterm_phase::Program::Claude,
        seat: Default::default(),
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
        ran_tools: worked.is_some_and(|w| w >= REAL_WORK),
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::Clean,
        fact: None,
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

/// The real side of one walk: the decider's state, the loop's clock, and when the host's verdict
/// last changed (a moment after whatever came before it).
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    measured: Instant,
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

/// The rule the model's `Act` at `st` is taken under (the branch its guard's disjunct names).
fn model_rule(st: &State) -> &'static str {
    match (st["cause"], st["verdict"]) {
        (0, 2) => RULE_API_BACK,
        (_, 1) => RULE_API_RETRY,
        (0, _) => RULE_API_RETRY,
        _ => RULE_API_CUTOFF,
    }
}

/// The wall answered its tries the same (`Futile`: its cause's answers twice or more, its tries
/// spent), and the route was not measured back up since.
fn held(m: &Model, st: &State) -> bool {
    let futile = cnst(m, "Futile");
    let same = if st["cause"] == 0 { st["mu"] } else { st["mc"] };
    same >= 2 && (same >= futile || st["att"] >= futile) && !(st["up"] == 1 && st["verdict"] == 2)
}

/// What the model says the decider does at `st`: at a wall that shows, the act where `Act` is enabled, nothing where the wall answered its tries the same, else a wait until the first tick it is — found by moving `waited` alone, as time alone would.
fn expected(m: &Model, st: &State) -> Verdict {
    if st["wall"] != 1 {
        return Verdict::Nothing;
    }
    if m.action_enabled("Act", st) {
        return Verdict::Act(model_rule(st));
    }
    if held(m, st) {
        return Verdict::Nothing;
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

/// What the real decider says at `st`, with the act's words checked: the wall's kind named, under the rule's own sentence.
fn verdict(a: &TurnEndAction, st: &State, now: Instant) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } => {
            let r = reading(st, None, now);
            assert_eq!(
                *text,
                wall_retry_text(rule_id, r.wall.expect("a shown wall"), r.program),
                "the words at {st:?}"
            );
            Verdict::Act(rule_id)
        }
        // In whole seconds: a measure moves the clock a millisecond ([`Real::measured`]).
        TurnEndAction::WaitUntil { until, .. } => Verdict::Wait(Duration::from_secs(
            u64::try_from(
                until
                    .saturating_duration_since(now)
                    .as_millis()
                    .div_ceil(1000),
            )
            .unwrap_or(u64::MAX),
        )),
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for at {st:?}: {other:?}"),
    }
}

/// Mirror one model action on the real side (the model has fired it).
fn mirror(
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
) -> bool {
    if before["verdict"] != after["verdict"] {
        real.now += Duration::from_millis(1);
        real.measured = real.now;
    }
    let (now, measured) = (real.now, real.measured);
    match action {
        "NetFails" | "NetReturns" | "MeasureDown" | "MeasureUp" | "MeasureLost" | "MeasureLies"
        | "MeasureWrongDown" => {}
        "Unreachable" | "CutOff" | "MetUnreachable" | "MetCutOff" => {
            real.st
                .observe(&reading(after, Some(VENDOR_RETRIES), measured), now);
        }
        "WorkedThenUnreachable" | "WorkedThenCutOff" => {
            real.st
                .observe(&reading(after, Some(REAL_WORK), measured), now);
        }
        "Taken" => real.st.observe(&taken(now), now),
        "Tick" => real.now += MIN,
        "Act" => {
            let r = reading(before, None, measured);
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

/// Walk `m` against the real decider: every verdict word seen, and each state where the two disagree (`strict` fails at the first; otherwise they are collected, the negative control's).
fn walk(m: &Model, strict: bool) -> (BTreeMap<&'static str, usize>, Vec<State>) {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(3600);
    let init = Real {
        st: TurnEndState::new(timing(m)),
        now: base,
        measured: base,
    };
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut words = BTreeMap::<&str, usize>::new();
    let mut disagree = Vec::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &st)) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        let mut at = real.st.clone();
        let here = reading(&st, None, real.measured);
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
    (words, disagree)
}

#[test]
fn tier1_the_real_decider_types_into_a_network_wall_exactly_where_the_model_allows() {
    let m = supervisor_network_wall_model();
    let (words, _) = walk(&m, true);
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

/// Negative control: the `Buggy = 1` model (acting at once whatever the measure) types at a measured outage's appearance where the real decider waits the hold.
#[test]
fn the_buggy_model_disagrees_with_the_real_decider() {
    let m = supervisor_network_wall_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let (_, disagree) = walk(&buggy, false);
    assert!(!disagree.is_empty(), "the Buggy model agreed everywhere");
    assert!(
        disagree
            .iter()
            .any(|st| st["wall"] == 1 && st["verdict"] == 1 && st["waited"] == 0),
        "no disagreement at a measured outage's appearance: {} states",
        disagree.len()
    );

    let carry = aterm_spec::interp::with_consts(&m, &[("Carry", 1)]);
    let (_, carried) = walk(&carry, false);
    assert!(
        carried
            .iter()
            .any(|st| st["wall"] == 1 && st["verdict"] == 1 && st["dacts"] == 1),
        "Carry = 1 agreed at every re-appearance of a measured outage: {} states",
        carried.len()
    );
}

/// The words this bind hands the decider for each of the model's causes are the cause the reader reads them as — a reply cut off, a server never reached — and a reply past its output token maximum is a cause of its own, not a cut-off, and none of this model's.
#[test]
fn the_models_words_read_as_the_causes_it_stands_for() {
    let api = |cause| {
        Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause,
        })
    };
    assert_eq!(aterm_phase::classify_wall(ASLEEP), api(ApiCause::CutOff));
    assert_eq!(
        aterm_phase::classify_wall(ENOTFOUND),
        api(ApiCause::Unreachable)
    );
    assert_eq!(
        aterm_phase::classify_wall(OVERLONG),
        api(ApiCause::Overlong)
    );
}

/// Negative control: the `Buggy = 2` model (a wall tried after it answered its tries the same) types where the real decider holds.
#[test]
fn the_model_that_tries_past_the_bound_disagrees_with_the_real_decider() {
    let m = supervisor_network_wall_model();
    let more = aterm_spec::interp::with_buggy(&m, 2);
    let (_, disagree) = walk(&more, false);
    assert!(
        disagree.iter().any(|st| held(&m, st)),
        "no disagreement where the wall answered its tries the same: {} states",
        disagree.len()
    );
}
