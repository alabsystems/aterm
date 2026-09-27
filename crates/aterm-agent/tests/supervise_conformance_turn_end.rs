// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorTurnEnd` (aterm-spec `supervisor_turn_end_model`):
//! the REAL turn-end decider (`decide_turn_end`, with the real
//! `TurnEndState` it keeps) driven along EVERY reachable state of the model
//! — fully automatic (no cap, the owner's default) and under a cap the owner
//! wrote.
//!
//! Each model action is mirrored on the real side — a box or a wall on the
//! screen is the reading the decider is handed, a person at the keyboard is
//! the reading's `person` (a keystroke inside the grace), a draft left is
//! the composer holding text (with that keystroke's grace), a wall's wait
//! running out is the clock moved by the real retry ladder, a back-off
//! running out is the clock moved by EXACTLY the real ladder's wait for the
//! streak (so the growth — 2, 4, 8 … min — is the real one), a turn's yield
//! is the next point `observe`d with that much busy work, an hour passing is
//! the clock moved past the cap's window, and the model's `Continue`/`Retry`
//! and `SubmitDraft` are the real decider's own act, recorded with `acted`
//! — and at every state the real verdict is checked against the model: it
//! types a continuation exactly where `Continue` is enabled, a retry exactly
//! where `Retry` is, submits a draft exactly where `SubmitDraft` is, waits
//! where a wall's wait, a person or a back-off holds it, and escalates only
//! where the owner's cap is spent — and, at an act's point that showed no
//! busy read (`pending` 3), waits for its deadline and then acts or waits,
//! never nothing (the latch lane B2's review found: at every free point the
//! real verdict is something). The walk runs twice: once with that point a
//! reply that ended unseen, once with the act's `❯` row left unanswered —
//! an act the worker never took, judged at the same deadline as the same
//! short yield (escalated as "not taken" until 2026-09-24). Each state's verdict is
//! the loop's: the point folded in again with no work (`observe`), then
//! decided — as `turn_end_now` does at a deadline. The model's invariants
//! are checked at every state. NEGATIVE CONTROLS: at the state with a box
//! up — and with a person at the keyboard — and everything else free, the
//! real decider types nothing while the `Buggy = 1` model would; and the
//! `Buggy` model escalates a worker that "reports done", which the real
//! decider never does — so a green walk is not vacuous.
//!
//! Every walk runs twice more over what the turn ends on: a report (`Stage
//! done.`), whose `Continue` is `continue@v1`'s `keep going`, and a CHOICE
//! asked in prose (`Should I rewrite the parser or patch the lexer?`), whose
//! `Continue` is `answer@v1`'s `answer_text` (the owner directive of
//! 2026-09-25: the harness answers and goes on): the same model, so an
//! answered choice is exactly a continuation — the same switch, cap, back-off
//! and walls — and nothing else. Each walk requires its own rule on every
//! `Continue` it sees.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, RULE_ANSWER, RULE_API_RETRY, RULE_CONTINUE, TurnEndAction, TurnEndReading,
    TurnEndState, TurnEndTiming, decide_turn_end,
};
use aterm_phase::WallKind;
use aterm_spec::derive::{Model, supervisor_turn_end_model};

type State = BTreeMap<&'static str, i64>;

/// Work a turn did that counts, and work that does not.
const LONG: Duration = Duration::from_secs(3 * 60);
const SHORT: Duration = Duration::from_secs(10);
/// A person's keystroke well inside the grace.
const JUST_TYPED: Duration = Duration::from_secs(1);

fn cnst(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no const {name}"))
}

/// A turn that ends on a report, and one that ends on a choice.
const REPORT: &str = "Stage done.";
const CHOICE: &str = "Should I rewrite the parser or patch the lexer?";

thread_local! {
    /// What this walk's turns end on ([`REPORT`], [`CHOICE`]).
    static SAID: std::cell::Cell<&'static str> = const { std::cell::Cell::new(REPORT) };
}

/// The rule a `Continue` of the walk under way types: `continue@v1` after a
/// report, `answer@v1` after a choice.
fn continue_rule() -> &'static str {
    if SAID.with(std::cell::Cell::get) == CHOICE {
        RULE_ANSWER
    } else {
        RULE_CONTINUE
    }
}

/// The screen the model state stands for; `unanswered`: the act's `❯` row
/// is last on it, never taken.
fn reading(st: &State, worked: Option<Duration>, unanswered: bool) -> TurnEndReading {
    let phase = if st["pending"] == 1 || st["pending"] == 2 {
        Phase::Busy
    } else if st["box_up"] == 1 {
        Phase::Prompt
    } else {
        Phase::Idle
    };
    let wall = (st["wall"] > 0 && st["box_up"] == 0).then_some(WallKind::Overloaded);
    let pending_input = unanswered && phase == Phase::Idle;
    TurnEndReading {
        phase,
        authoritative: true,
        survey: false,
        wall,
        wall_message: if wall.is_some() {
            "API Error: 529 Overloaded.".to_string()
        } else {
            String::new()
        },
        reset_at: None,
        resumes_by_itself: false,
        composer: if st["draft"] == 1 && st["box_up"] == 0 {
            Composer::Typed
        } else {
            Composer::Empty
        },
        said_tail: Some(SAID.with(std::cell::Cell::get).to_string()),
        worked,
        rules: None,
        pending_input,
        interrupted: false,
        // The window's host: its walls here are retried, never restarted.
        restartable: true,
        upgrading: false,
        fresh: false,
        person: (st["person"] == 1).then_some(JUST_TYPED),
    }
}

/// The real side of one walk: the decider's state, the loop's clock, and
/// whether the act's `❯` row stands unanswered on the screen.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    unanswered: bool,
}

/// What the real decider says at `st`, as one of the model's words.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Continue,
    Retry,
    Submit,
    Wait,
    Escalate,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == continue_rule() => Verdict::Continue,
        TurnEndAction::Accept { .. } => Verdict::Continue,
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_RETRY => Verdict::Retry,
        TurnEndAction::Submit { rule_id }
            if *rule_id == continue_rule() || *rule_id == RULE_API_RETRY =>
        {
            Verdict::Submit
        }
        TurnEndAction::WaitUntil { .. } => Verdict::Wait,
        TurnEndAction::Escalate { .. } => Verdict::Escalate,
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

/// What the model says the decider must do at `st`.
fn expected(m: &Model, st: &State) -> Verdict {
    // An unseen point: waited for, then — at its deadline — decided as the
    // point after `DeadlineUnseen`.
    if st["pending"] == 3 {
        if st["waited"] < cnst(m, "Take") {
            return Verdict::Wait;
        }
        let mut judged = st.clone();
        assert!(m.fire("DeadlineUnseen", &mut judged), "{st:?}");
        return expected(m, &judged);
    }
    if m.action_enabled("SubmitDraft", st) {
        return Verdict::Submit;
    }
    if m.action_enabled("Continue", st) {
        return Verdict::Continue;
    }
    if m.action_enabled("Retry", st) {
        return Verdict::Retry;
    }
    if st["pending"] > 0 || st["box_up"] == 1 {
        return Verdict::Nothing;
    }
    if m.action_enabled("Escalate", st) {
        return Verdict::Escalate;
    }
    // A free point where the model allows no act: a person, a wall's wait
    // or a back-off holds it.
    assert!(
        st["person"] == 1 || st["wall"] == 1 || st["backoff"] == 1,
        "a free point where nothing holds and nothing is done: {st:?}"
    );
    Verdict::Wait
}

/// The real policy's clocks, scaled so that each of the model's runs alone:
/// one retry wait for every retry (the real ladder's last entry repeats),
/// a cap window longer than any walk, and the back-off ladder the defaults
/// have, capped where the model's `Streak` saturates (2, 4, 8 min).
fn timing() -> TurnEndTiming {
    let min = |m: u64| Duration::from_secs(m * 60);
    TurnEndTiming {
        retry_backoff: vec![min(1)],
        window: min(24 * 60),
        short_backoff_max: min(8),
        ..TurnEndTiming::default()
    }
}

/// Mirror one model action on the real side (the model has fired it);
/// `variant`: an unseen reply is the act's `❯` row left unanswered.
fn mirror(action: &str, before: &State, after: &State, real: &mut Real, take: i64, variant: bool) {
    // The worker answered: no row of the act's stands unanswered.
    if matches!(
        action,
        "WorkedShort" | "WorkedLong" | "RetryTaken" | "RetryHitsTheWall" | "HumanWork"
    ) {
        real.unanswered = false;
    }
    if action == "ReplyUnseen" {
        real.unanswered = variant;
    }
    let unanswered = real.unanswered;
    let seen = |st: &State, worked| reading(st, worked, unanswered);
    match action {
        "BoxAppears" | "BoxLeaves" | "PersonTypes" | "GraceEnds" | "DraftLeft" => {}
        "WallAppears" => real.st.observe(&seen(after, Some(SHORT)), real.now),
        "WallDue" => real.now += real.st.timing.retry_backoff[0],
        "HourPasses" => real.now += real.st.timing.window + Duration::from_secs(1),
        "BackoffDue" => {
            let wait = real
                .st
                .short_wait()
                .expect("a back-off runs after a short turn");
            real.now += wait;
        }
        "HumanWork" => real.st.observe(&seen(after, Some(LONG)), real.now),
        "Continue" | "Retry" | "SubmitDraft" => {
            let r = seen(before, None);
            let cfg = SupervisorConfig::default();
            let a = decide_turn_end(&real.st, &r, &cfg, real.now);
            real.st.acted(&a, &r, real.now);
        }
        "WorkedShort" | "RetryHitsTheWall" => {
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        "WorkedLong" | "RetryTaken" => real.st.observe(&seen(after, Some(LONG)), real.now),
        "ReplyUnseen" | "DeadlineUnseen" => real.st.observe(&seen(after, None), real.now),
        "TimePassesUnseen" => {
            real.now += real.st.timing.take_within / u32::try_from(take).unwrap();
        }
        "Escalate" => {}
        other => panic!("an action this bind does not mirror: {other}"),
    }
}

/// The real ladder's wait after `short` short turns in a row: 2 min, then
/// doubled, capped at an hour.
fn ladder(st: &TurnEndState, short: u32) -> Option<Duration> {
    let t = &st.timing;
    let n = short.checked_sub(1)?;
    Some((t.short_backoff * 2u32.pow(n)).min(t.short_backoff_max))
}

/// Walk `m` against the real decider under `cfg` (`variant`: an unseen
/// reply is the act's row left unanswered); the reachable state count and
/// every verdict seen.
fn walk(
    m: &Model,
    cfg: &SupervisorConfig,
    variant: bool,
) -> (usize, BTreeMap<&'static str, usize>) {
    let base = Instant::now() + Duration::from_secs(3600);
    let mut init = Real {
        st: TurnEndState::new(timing()),
        now: base,
        unanswered: false,
    };
    // The walk starts at a point that ended a turn of real work.
    let s0 = m.init_state();
    init.st.observe(&reading(&s0, Some(LONG), false), init.now);
    let streak = cnst(m, "Streak");
    let take = cnst(m, "Take");

    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(s0, init)]);
    let mut acts = BTreeMap::<&str, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        for inv in &m.invariants {
            assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
        }
        // The real streak, as the model saturates it, and its real wait.
        let short = real.st.short_streak();
        assert_eq!(
            i64::from(short).min(streak),
            st["short"],
            "the streak at {st:?}"
        );
        assert_eq!(real.st.short_wait(), ladder(&real.st, short), "{st:?}");
        let mut at = real.st.clone();
        let here = reading(&st, None, real.unanswered);
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, cfg, real.now));
        assert_eq!(
            got,
            expected(m, &st),
            "the real decider disagrees at {st:?}"
        );
        // No silent latch: at a free point the real decider says something.
        if st["box_up"] == 0 && (st["pending"] == 0 || st["pending"] == 3) {
            assert_ne!(got, Verdict::Nothing, "a silent point at {st:?}");
        }
        *acts
            .entry(match got {
                Verdict::Continue => "continue",
                Verdict::Retry => "retry",
                Verdict::Submit => "submit",
                Verdict::Wait => "wait",
                Verdict::Escalate => "escalate",
                Verdict::Nothing => "nothing",
            })
            .or_default() += 1;
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            mirror(action.name, &st, &next, &mut r, take, variant);
            queue.push_back((next, r));
        }
    }
    (seen.len(), acts)
}

#[test]
fn tier1_the_real_decider_types_exactly_where_the_model_allows() {
    let m = supervisor_turn_end_model();
    for said in [REPORT, CHOICE] {
        SAID.with(|s| s.set(said));
        // FULLY AUTOMATIC: no cap, and nothing is ever escalated — with an
        // unseen reply, and with an act never taken.
        for variant in [false, true] {
            let (states, acts) = walk(&m, &SupervisorConfig::default(), variant);
            assert_eq!(states, 160, "the reachable space changed: {states}");
            for word in ["continue", "retry", "submit", "wait", "nothing"] {
                assert!(
                    acts.get(word).copied().unwrap_or(0) > 0,
                    "{word} never occurred ({said}, {variant}): {acts:?}"
                );
            }
            assert_eq!(acts.get("escalate"), None, "{said}, {variant}: {acts:?}");
        }
        // A cap the owner wrote: escalated exactly where it is spent.
        let capped = aterm_spec::interp::with_consts(&m, &[("Budget", 2)]);
        let cfg = SupervisorConfig {
            continue_per_hour: u32::try_from(cnst(&capped, "Budget")).unwrap(),
            ..SupervisorConfig::default()
        };
        let (_, acts) = walk(&capped, &cfg, false);
        assert!(
            acts.get("escalate").copied().unwrap_or(0) > 0,
            "{said}: {acts:?}"
        );
    }
    SAID.with(|s| s.set(REPORT));

    // NEGATIVE CONTROLS: a box up, or a person at the keyboard, with
    // nothing else in the way — the real decider types nothing, the Buggy
    // model would; and the Buggy model escalates "reports done".
    let base = Instant::now() + Duration::from_secs(3600);
    let mut real = TurnEndState::default();
    real.observe(&reading(&m.init_state(), Some(LONG), false), base);
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    for var in ["box_up", "person", "draft"] {
        let mut held = m.init_state();
        held.insert(var, 1);
        assert_ne!(
            verdict(&decide_turn_end(
                &real,
                &reading(&held, None, false),
                &SupervisorConfig::default(),
                base
            )),
            Verdict::Continue,
            "{var}"
        );
        assert!(!m.action_enabled("Continue", &held), "{var}");
        assert!(
            buggy.action_enabled("Continue", &held),
            "{var}: the Buggy model types over it — the walk would have caught a real one"
        );
    }
    let mut done = m.init_state();
    done.insert("short", 2);
    done.insert("backoff", 1);
    assert!(!m.action_enabled("Escalate", &done));
    assert!(buggy.action_enabled("Escalate", &done));
}
