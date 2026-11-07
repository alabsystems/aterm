// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorOutageWait` (aterm-models-harness `supervisor_outage_wait_model`): the
//! real turn-end decider (`decide_turn_end`, with its `TurnEndState`) driven along every reachable
//! state of the model, so its answer at an outage wall (`ENOTFOUND`, the host measuring the route;
//! a 529 it never measures) is the model's, state for state: a wait that types nothing while the
//! route is measured down, one measured line on its edge up, and else the blind line at each rung
//! of the doubling ladder, the wait's deadline that rung. The words are held here: every act the
//! decider types at the wall is the line `cleared_text` says, and a decider typing main's retry
//! words instead is caught.
//!
//! Slice 12c (v6 §14.17.5.4): the walk keeps the host's real book of answered turns
//! (`harness::witness::Witnesses`), so a witness the model gives the wall's own route is another
//! session's answer on Claude Code's default route and one it gives another route only is an
//! answer on another endpoint, which the book never hands this session; and the model's rate
//! limit is Muse Code's 429 with the `retry-after` its words name (the Responses API's sentence),
//! `After` ticks: Claude Code's own 429 names no wait (the API gives it in a header Claude Code
//! does not draw), so it keeps the ladder.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::witness::{Base, RouteKey, Target, Witnesses};
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Cleared, Clearing, Composer, Outage, RULE_API_BACK, RULE_API_RETRY, RULE_OUTAGE_RESUME, Reach,
    TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming, WaitKind, cleared_text,
    decide_turn_end, wall_retry_text,
};
use aterm_models_harness::supervisor_outage_wait_model;
use aterm_phase::{ApiCause, WallKind};
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);
/// The vendor's own retries before it gives up (about 3 minutes): busy work short of the real `progress`.
const VENDOR_RETRIES: Duration = Duration::from_secs(3 * 60);
/// Real work after a resume: past `progress`, its tools run.
const REAL_WORK: Duration = Duration::from_secs(10 * 60);

const ENOTFOUND: &str =
    "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
const OVERLOADED: &str = "API Error: 529 Overloaded. This is a server-side issue, usually \
                          temporary — try again in a moment. If it persists, check \
                          https://status.claude.com.";
/// Muse Code's 429 as its screen draws it, the Responses API's sentence naming its
/// `retry-after` in seconds.
fn rate_limited(after: Duration) -> String {
    format!(
        "model failed: API error 429: Rate limit reached for requests per min. Please try again \
         in {}s. (rate_limit_exceeded)",
        after.as_secs()
    )
}

/// This session, and the session whose answers witness its route: Claude Code on its default route.
const ME: &str = "s-me";
const MATE: &str = "s-mate";

fn own_route() -> RouteKey {
    RouteKey {
        program: aterm_phase::Program::Claude,
        base: Base::anthropic(),
    }
}

/// Another route: Claude Code on another endpoint.
fn other_route() -> RouteKey {
    RouteKey {
        program: aterm_phase::Program::Claude,
        base: Base {
            target: Target {
                host: "gw.example.test".to_string(),
                port: 443,
                tls: true,
            },
            path: String::new(),
        },
    }
}

const GOAL_UNAVAILABLE: &str = "Goal still active · the API was unavailable or the connection \
                                dropped · retrying in 5 min (2/3) · send a message to retry now";
const GOAL_GAVE_UP: &str = "Goal paused after 3 automatic retries · the API was unavailable or \
                            the connection dropped · send a message to continue";

/// The real ladder, scaled so one model tick is one minute: `First` and `Cap` as the real
/// `outage_first` and `outage_cap`.
fn timing(m: &Model) -> TurnEndTiming {
    let minutes = |name| MIN * u32::try_from(cnst(m, name)).expect("a small constant");
    TurnEndTiming {
        outage_first: minutes("First"),
        outage_cap: minutes("Cap"),
        ..TurnEndTiming::default()
    }
}

/// The wall the model's `kind` stands for, as the reader reads its words.
fn wall_of(st: &State) -> WallKind {
    match st["kind"] {
        1 => WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        },
        3 => WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        },
        _ => WallKind::Overloaded,
    }
}

/// The outage the model's `kind` stands for.
fn outage_of(st: &State) -> Outage {
    match st["kind"] {
        1 => Outage::Connection,
        3 => Outage::ApiError,
        _ => Outage::Overload,
    }
}

/// The real side of one walk: the decider's state, the loop's clock, when the host last
/// measured the route down and its edge up (a moment after whatever came before each), the host's
/// book of answered turns, and the `retry-after` the rate limit's words name.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    down_at: Instant,
    up_at: Instant,
    book: Witnesses,
    after: Duration,
}

/// The screen the model state stands for: at a wall that shows, the vendor's message as the
/// reader reads it, with the host's measure at a connection wall (an up stamped at its edge, so
/// whether it came after the last try is the real track's to say); else a worker at work.
fn reading(st: &State, worked: Option<Duration>, real: &Real) -> TurnEndReading {
    let showing = st["kind"] > 0 && st["turn"] == 0;
    let connection = st["kind"] == 1;
    TurnEndReading {
        phase: if showing { Phase::Idle } else { Phase::Busy },
        authoritative: true,
        survey: false,
        wall: showing.then(|| wall_of(st)),
        wall_message: match (showing, st["kind"]) {
            (false, _) => String::new(),
            (true, 1) => ENOTFOUND.to_string(),
            (true, 3) => rate_limited(real.after),
            (true, _) => OVERLOADED.to_string(),
        },
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: None,
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        said_lines: None,
        turn_lines: None,
        turn_breaks: Default::default(),
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        fault: None,
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
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        person: None,
        key: None,
        reach: match (connection, st["reach"]) {
            (true, 1) => Reach::Down {
                since: real.down_at,
            },
            (true, 2 | 3) => Reach::Up { since: real.up_at },
            _ => Reach::Unknown,
        },
        witness: real.book.witness(&own_route(), ME),
        program: if st["kind"] == 3 {
            aterm_phase::Program::Muse
        } else {
            aterm_phase::Program::Claude
        },
        seat: Default::default(),
        account: None,
        route: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        autopilot_by_person: false,
        login_superseded: false,
        login_notice: None,
        login_screen: None,
        login_interrupted: false,
        login_at_wall: None,
        login_method: None,
        login_shared: None,
        login_pin: aterm_agent::supervise::policy::login::LoginPin::Picker,
        login_moved: false,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        ran_tools: worked.is_some_and(|w| w >= REAL_WORK),
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::Clean,
        leftover: aterm_agent::harness::leftover::LeftRead::NotAsked,
        fact: None,
        wake: None,
    }
}

/// An idle point with no wall after real work: the resume was taken.
fn taken(real: &Real) -> TurnEndReading {
    let quiet = State::from([("kind", 0), ("turn", 0), ("reach", 0), ("witness", 0)]);
    TurnEndReading {
        phase: Phase::Idle,
        worked: Some(REAL_WORK),
        said_tail: Some("Stage done.".to_string()),
        ..reading(&quiet, None, real)
    }
}

/// What the decider says at an outage wall, as one of the model's words.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// The one line, measured or blind.
    Line(Clearing),
    /// Words other than the line under the outage's rule.
    Retry,
    /// A wait at the outage: whether a blind try ends it, and then the minutes to its rung.
    Wait {
        tries: bool,
        minutes: Option<u64>,
    },
    Other(String),
}

/// What the model says the decider does at a wall that shows: the measured line where
/// `ResumeMeasured` or `ResumeWitnessed` is enabled, the blind line where `TryBlind` is, else a
/// wait, typing nothing while the route is measured down and else until the rung `gap` ticks from
/// the wall's start.
fn expected(m: &Model, st: &State) -> Verdict {
    if m.action_enabled("ResumeMeasured", st) || m.action_enabled("ResumeWitnessed", st) {
        return Verdict::Line(Clearing::Measured);
    }
    if m.action_enabled("TryBlind", st) {
        return Verdict::Line(Clearing::Blind);
    }
    if st["kind"] == 1 && st["reach"] == 1 {
        return Verdict::Wait {
            tries: false,
            minutes: None,
        };
    }
    Verdict::Wait {
        tries: true,
        minutes: Some(u64::try_from(st["gap"] - st["waited"]).expect("a rung ahead")),
    }
}

/// What the real decider says at `st`, with its words and its wait's reason checked against the
/// outage the wall is.
fn verdict(a: &TurnEndAction, st: &State, now: Instant) -> Verdict {
    let outage = outage_of(st);
    match a {
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_OUTAGE_RESUME => {
            [Clearing::Measured, Clearing::Blind]
                .into_iter()
                .find(|&c| text == cleared_text(Cleared::Outage(outage), c))
                .map_or(Verdict::Retry, Verdict::Line)
        }
        TurnEndAction::WaitUntil {
            until,
            why,
            wait: WaitKind::Outage { wall, tries },
        } if *wall == outage && *why == format!("waiting: {}", outage.words()) => Verdict::Wait {
            tries: *tries,
            // In whole minutes: a measure moves the clock a millisecond ([`mirror`]).
            minutes: tries.then(|| until.saturating_duration_since(now).as_secs().div_ceil(60)),
        },
        other => Verdict::Other(format!("{other:?}")),
    }
}

/// Mirror one model action on the real side (the model has fired it); false where the real
/// decider does not take the act the model took.
fn mirror(
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
) -> bool {
    match action {
        "ConnectionMeets" | "OverloadMeets" | "RateMeets" | "MetAgain" => {
            real.now += VENDOR_RETRIES;
            let r = reading(after, Some(VENDOR_RETRIES), real);
            real.st.observe(&r, real.now);
        }
        "MeasureDown" => {
            real.now += Duration::from_millis(1);
            real.down_at = real.now;
        }
        "EdgeUp" => {
            real.now += Duration::from_millis(1);
            real.up_at = real.now;
        }
        "MeasureLost" => real.now += Duration::from_millis(1),
        "SameRouteAnswers" => {
            real.now += Duration::from_millis(1);
            real.book.answered(own_route(), MATE, real.now);
        }
        "OtherRouteAnswers" => {
            real.now += Duration::from_millis(1);
            real.book.answered(other_route(), "s-far", real.now);
        }
        "Tick" => real.now += MIN,
        "ResumeMeasured" | "ResumeWitnessed" | "TryBlind" => {
            let r = reading(before, None, real);
            real.st.observe(&r, real.now);
            let a = decide_turn_end(&real.st, &r, cfg, real.now);
            if !matches!(a, TurnEndAction::Type { .. }) {
                return false;
            }
            real.st.acted(&a, &r, real.now);
        }
        "Works" => {
            real.now += REAL_WORK;
            let r = taken(real);
            real.st.observe(&r, real.now);
        }
        other => panic!("an action this bind does not mirror: {other}"),
    }
    true
}

struct Walked {
    /// Each verdict the real decider gave at a wall that shows, by kind.
    words: BTreeMap<String, usize>,
    /// Each state where the two disagree, with the real verdict and the model's.
    off: Vec<(State, Verdict, Verdict)>,
    states: usize,
}

/// Main's typed retry in place of the line `a` types: `api-back@v1`'s words where the line is
/// measured, `api-retry@v1`'s where it is blind, at the wall the model state `st` stands for.
fn mains_words(a: TurnEndAction, st: &State) -> TurnEndAction {
    let TurnEndAction::Type { text, .. } = &a else {
        return a;
    };
    let rule = if *text == cleared_text(Cleared::Outage(outage_of(st)), Clearing::Measured) {
        RULE_API_BACK
    } else {
        RULE_API_RETRY
    };
    TurnEndAction::Type {
        text: wall_retry_text(rule, wall_of(st), aterm_phase::Program::Claude),
        rule_id: rule,
    }
}

/// Walk `m` against the real decider at every wall that shows (`strict` fails at the first
/// disagreement; otherwise they are collected, the negative controls'). `mains` hands the checker
/// main's retry words wherever the decider types.
fn walk(m: &Model, strict: bool, mains: bool) -> Walked {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(3600);
    let init = Real {
        st: TurnEndState::new(timing(m)),
        now: base,
        down_at: base,
        up_at: base,
        book: Witnesses::default(),
        after: MIN * u32::try_from(cnst(m, "After")).expect("a small constant"),
    };
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut words = BTreeMap::<String, usize>::new();
    let mut off = Vec::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &st)) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        if st["kind"] > 0 && st["turn"] == 0 {
            let mut at = real.st.clone();
            let here = reading(&st, None, &real);
            at.observe(&here, real.now);
            let mut a = decide_turn_end(&at, &here, &cfg, real.now);
            if mains {
                a = mains_words(a, &st);
            }
            let got = verdict(&a, &st, real.now);
            let want = expected(m, &st);
            if got != want {
                assert!(
                    !strict,
                    "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
                );
                off.push((st.clone(), got.clone(), want));
            }
            let word = match got {
                Verdict::Wait { tries, .. } => format!("wait tries={tries}"),
                other => format!("{other:?}"),
            };
            *words
                .entry(format!("{word} at kind {}", st["kind"]))
                .or_default() += 1;
            *words.entry(word).or_default() += 1;
        }
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
    Walked {
        words,
        off,
        states: seen.len(),
    }
}

#[test]
fn tier1_the_real_decider_waits_out_an_outage_exactly_as_the_model_says() {
    let m = supervisor_outage_wait_model();
    let walked = walk(&m, true, false);
    for word in [
        "Line(Measured)",
        "Line(Blind)",
        "wait tries=false",
        "wait tries=true",
    ] {
        assert!(
            walked.words.get(word).copied().unwrap_or(0) > 0,
            "{word} never occurred: {:?}",
            walked.words
        );
    }
    // A witness clears an overload (nothing measures it), the rate limit is tried blind on its
    // own rung and waited out before it.
    for word in [
        "Line(Measured) at kind 2",
        "Line(Blind) at kind 3",
        "wait tries=true at kind 3",
    ] {
        assert!(
            walked.words.get(word).copied().unwrap_or(0) > 0,
            "{word} never occurred: {:?}",
            walked.words
        );
    }
    assert!(walked.states > 50, "{} states", walked.states);
}

/// Negative controls on the witness (v6 §14.17.5.4): main's ladder, which no witness clears
/// (`Buggy = 9`), tries blind at its rung over a witness the real decider resumes on at once; a
/// witness on another route (`Buggy = 6`) resumes where the real decider, whose book never hands
/// it that answer, waits; a second resume on a witness in one outage (`Buggy = 7`) types where
/// the real decider leaves it to the ladder; and a witness taken at a rate limit or into a route
/// measured down (`Buggy = 8`) types where the real decider waits.
#[test]
fn a_ladder_blind_to_witnesses_or_one_that_takes_any_disagrees_with_the_real_decider() {
    let m = supervisor_outage_wait_model();
    let mains = walk(&aterm_spec::interp::with_buggy(&m, 9), false, false);
    assert!(
        mains.off.iter().any(|(st, got, want)| st["witness"] == 1
            && *got == Verdict::Line(Clearing::Measured)
            && matches!(want, Verdict::Wait { tries: true, .. })),
        "main's ladder agreed at every witness: {:?}",
        mains.off
    );
    let crossed = walk(&aterm_spec::interp::with_buggy(&m, 6), false, false);
    assert!(
        crossed.off.iter().any(|(st, got, want)| st["witness"] == 2
            && *want == Verdict::Line(Clearing::Measured)
            && matches!(got, Verdict::Wait { tries: true, .. })),
        "another route's witness agreed everywhere: {:?}",
        crossed.off
    );
    let twice = walk(&aterm_spec::interp::with_buggy(&m, 7), false, false);
    assert!(
        twice.off.iter().any(|(st, got, want)| st["witnessed"] == 1
            && st["witness"] == 1
            && *want == Verdict::Line(Clearing::Measured)
            && *got != Verdict::Line(Clearing::Measured)),
        "a second witnessed resume agreed everywhere: {:?}",
        twice.off
    );
    let any = walk(&aterm_spec::interp::with_buggy(&m, 8), false, false);
    for (kind, reach, tries) in [(3, 0, true), (1, 1, false)] {
        assert!(
            any.off.iter().any(|(st, got, want)| st["kind"] == kind
                && st["reach"] == reach
                && st["witness"] == 1
                && *want == Verdict::Line(Clearing::Measured)
                && matches!(got, Verdict::Wait { tries: t, .. } if *t == tries)),
            "a witness taken at kind {kind}, reach {reach} agreed everywhere: {:?}",
            any.off
        );
    }
}

/// Negative control on the rate limit's own wait: a ladder deaf to the `retry-after` its words
/// name (`Buggy = 5`) tries on the first rung, where the real decider still waits for it.
#[test]
fn a_ladder_deaf_to_the_retry_after_disagrees_with_the_real_decider() {
    let m = supervisor_outage_wait_model();
    let deaf = walk(&aterm_spec::interp::with_buggy(&m, 5), false, false);
    assert!(
        deaf.off.iter().any(|(st, got, want)| st["kind"] == 3
            && st["waited"] < cnst(&m, "After")
            && *want == Verdict::Line(Clearing::Blind)
            && matches!(got, Verdict::Wait { tries: true, .. })),
        "a deaf ladder agreed everywhere: {:?}",
        deaf.off
    );
}

/// Negative controls on main's rule: the `Buggy = 1` model (today's typed retry, a try whatever
/// the route measures) disagrees with the real decider at a route measured down; and a checker
/// handed main's retry words wherever the decider types disagrees there and only there, so the
/// walk holds the words to the line.
#[test]
fn todays_typed_retry_disagrees_with_the_real_decider() {
    let m = supervisor_outage_wait_model();
    let typed = walk(&aterm_spec::interp::with_buggy(&m, 1), false, false);
    assert!(
        typed.off.iter().any(|(st, got, want)| st["kind"] == 1
            && st["reach"] == 1
            && *want == Verdict::Line(Clearing::Blind)
            && matches!(got, Verdict::Wait { tries: false, .. })),
        "no disagreement at a route measured down: {:?}",
        typed.off
    );
    let words = walk(&m, false, true);
    for clearing in [Clearing::Measured, Clearing::Blind] {
        assert!(
            words
                .off
                .iter()
                .any(|(_, got, want)| *want == Verdict::Line(clearing)
                    && matches!(got, Verdict::Other(_))),
            "main's words agreed with the {clearing:?} line: {:?}",
            words.off
        );
    }
    assert!(
        words
            .off
            .iter()
            .all(|(_, _, want)| matches!(want, Verdict::Line(_))),
        "{:?}",
        words.off
    );
}

/// Negative controls on the ladder and the clearing: a ladder that never doubles tries before the
/// real rung, one with no cap waits past it, and a second resume on one clearing types where the
/// real decider waits.
#[test]
fn a_ladder_with_no_doubling_or_no_cap_or_a_second_resume_disagrees_with_the_real_decider() {
    let m = supervisor_outage_wait_model();
    let flat = walk(&aterm_spec::interp::with_buggy(&m, 2), false, false);
    assert!(
        flat.off.iter().any(|(st, got, want)| st["gap"] < st["rung"]
            && *want == Verdict::Line(Clearing::Blind)
            && matches!(got, Verdict::Wait { tries: true, .. })),
        "no doubling agreed everywhere: {:?}",
        flat.off
    );
    let uncapped = walk(&aterm_spec::interp::with_buggy(&m, 3), false, false);
    assert!(
        uncapped
            .off
            .iter()
            .any(|(st, got, want)| st["gap"] > cnst(&m, "Cap")
                && matches!(want, Verdict::Wait { tries: true, .. })
                && *got == Verdict::Line(Clearing::Blind)),
        "no cap agreed everywhere: {:?}",
        uncapped.off
    );
    let twice = walk(&aterm_spec::interp::with_buggy(&m, 4), false, false);
    assert!(
        twice.off.iter().any(|(st, got, want)| st["reach"] == 3
            && st["resumes"] == 1
            && *want == Verdict::Line(Clearing::Measured)
            && *got != Verdict::Line(Clearing::Measured)),
        "a second resume agreed everywhere: {:?}",
        twice.off
    );
}

#[test]
fn each_buggy_dial_breaks_its_law_along_a_path_the_model_keeps_it_on() {
    let m = supervisor_outage_wait_model();
    let ticks = |n| std::iter::repeat_n("Tick", n);
    let rungs: Vec<&str> = ["OverloadMeets"]
        .into_iter()
        .chain(ticks(1))
        .chain(["TryBlind", "MetAgain"])
        .chain(ticks(2))
        .chain(["TryBlind", "MetAgain"])
        .chain(ticks(4))
        .chain(["TryBlind"])
        .collect();
    for (buggy, path, laws) in [
        (
            1,
            vec!["ConnectionMeets", "MeasureDown", "Tick", "TryBlind"],
            &["NothingTypedWhileMeasuredDown"][..],
        ),
        (
            1,
            vec!["ConnectionMeets", "EdgeUp", "Tick", "TryBlind"],
            &["TheLineSaysHowItCleared"],
        ),
        (
            2,
            vec![
                "OverloadMeets",
                "Tick",
                "TryBlind",
                "MetAgain",
                "Tick",
                "TryBlind",
            ],
            &["NoTryBeforeItsRung"],
        ),
        (3, rungs, &["TheGapStaysUnderItsCap"]),
        (
            4,
            vec![
                "ConnectionMeets",
                "EdgeUp",
                "ResumeMeasured",
                "MetAgain",
                "ResumeMeasured",
            ],
            &["OneResumePerMeasuredClearing", "TheLineSaysHowItCleared"],
        ),
        (
            5,
            vec!["RateMeets", "Tick", "TryBlind"],
            &["NoTryBeforeItsRetryAfter"],
        ),
        (
            6,
            vec!["OverloadMeets", "OtherRouteAnswers", "ResumeWitnessed"],
            &["NoWitnessOfAnotherRouteClears"],
        ),
        (
            7,
            vec![
                "OverloadMeets",
                "SameRouteAnswers",
                "ResumeWitnessed",
                "MetAgain",
                "SameRouteAnswers",
                "ResumeWitnessed",
            ],
            &["OneWitnessedResumePerOutage"],
        ),
        (
            8,
            vec!["RateMeets", "SameRouteAnswers", "ResumeWitnessed"],
            &["NoWitnessClearsARateLimit"],
        ),
        (
            8,
            vec![
                "ConnectionMeets",
                "MeasureDown",
                "SameRouteAnswers",
                "ResumeWitnessed",
            ],
            &["NothingTypedWhileMeasuredDown"],
        ),
        (
            9,
            vec!["OverloadMeets", "SameRouteAnswers", "Tick", "TryBlind"],
            &["TheLineSaysHowItCleared"],
        ),
    ] {
        let bad = aterm_spec::interp::with_buggy(&m, buggy);
        let mut st = bad.init_state();
        for a in &path {
            assert!(bad.fire(a, &mut st), "Buggy = {buggy}: {a} at {st:?}");
        }
        for law in laws {
            assert!(
                !bad.check_invariant(law, &st),
                "Buggy = {buggy}: {law} at {st:?}"
            );
        }
        let mut st = m.init_state();
        let mut kept = true;
        for a in &path {
            kept = kept && m.fire(a, &mut st);
            for law in laws {
                assert!(m.check_invariant(law, &st), "control: {law} at {st:?}");
            }
        }
        assert!(
            buggy != 3 || kept,
            "control: the base model climbs the same rungs to its cap"
        );
    }
}

/// The words this bind hands the decider read as the walls the model stands for: a server never
/// reached, and an overload. The `/goal` echo that the API was unavailable, read alone (retrying
/// or past its retries), is a generic API error, a wall the model's blind arm stands for as it
/// does the overload: Claude Code gives that reason an overload and a server error too, so no
/// measure of the route clears it unless the route was measured down at the wall, which makes
/// it the connection's outage (the model's measured arm), held by that Down only until the cap
/// has passed since it began.
#[test]
fn the_models_words_read_as_the_walls_they_stand_for() {
    assert_eq!(
        aterm_phase::classify_wall(ENOTFOUND),
        Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        })
    );
    for echo in [GOAL_UNAVAILABLE, GOAL_GAVE_UP] {
        assert_eq!(
            aterm_phase::classify_wall(echo),
            Some(WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::Server,
            }),
            "the /goal echo read alone: {echo}"
        );
    }
    assert_eq!(
        aterm_phase::classify_wall(OVERLOADED),
        Some(WallKind::Overloaded)
    );
    // The rate limit's words are Muse Code's own, as its reader reads them off its screen (a
    // measured capture, its wait made 120 s).
    let rate = rate_limited(Duration::from_secs(120));
    let rows = aterm_phase::prompt::fixtures::screen(
        &aterm_phase::muse::fixtures::RATE_LIMIT_FINAL.replace("in 5s.", "in 120s."),
    );
    let wall = aterm_phase::reader::read(Some("muse"), &rows, None)
        .wall
        .expect("Muse Code's rate limit is a wall");
    assert_eq!(
        wall.kind,
        WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        }
    );
    assert!(wall.message.starts_with(&rate), "{}", wall.message);
    assert_eq!(
        aterm_phase::retry_after(&wall.message),
        Some(Duration::from_secs(120))
    );
}
