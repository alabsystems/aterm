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
//! THE TASK (D1 of the live E2E of 2026-09-26): the walk WRITES the
//! conversation's record as it goes — a person's prompt and its answer for
//! `HumanWork`; for `HarnessTurn` the harness's own marked turn and a
//! `keep going` of the supervisor's own (unmarked, known from its loop's
//! ledger), each answered — and the reading's `taskless` is the REAL
//! `harness::upgrade::transcript_tasked` over it and that ledger, checked
//! against the model's `tasked` at every state: the harness's own turns
//! never make a task, and at a state with none the real decider does
//! nothing at all.
//!
//! THE HARNESS'S TURNS ARE NO SHORT TURNS OF THE WORKER'S (N1 of the live
//! E2E of 2026-09-26): a `HarnessTurn` is the turn the session's host typed
//! (`TurnEndState::host_typed`), answered short, in a session with a task
//! or without; the real streak and its wait are checked against the model's
//! at every state after it — unmoved — and the decider continues at once
//! where the worker's own work earned no back-off. A `HarnessTurnLong` is
//! the same turn answered with real work (a carry-on's answer is the
//! worker's own work, resumed): the real streak ends with the model's, and
//! a back-off the worker's short turn had started is over. NEGATIVE
//! CONTROLS: the same short answer read as someone else's turn (the host's
//! turn not registered, the regression) backs a free point off where the
//! model continues, and the `Buggy` model that counts it so is caught by
//! `NeverBackOffForTheHarness`; the `Buggy` model whose long answer leaves
//! the streak standing is caught by `NeverBackOffAfterRealWork`.
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

use aterm_agent::harness::upgrade::transcript_tasked;
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

/// A person's prompt, the harness's own turn, the supervisor's own
/// continuation (no mark: its words are the owner's), and an answer, as the
/// conversation's record holds them.
const PERSON: &str = r#"{"type":"user","message":{"role":"user","content":"Stage the parser."}}"#;
const HARNESS: &str = r#"{"type":"user","message":{"role":"user","content":"[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283."}}"#;
const SUPERVISOR: &str = r#"{"type":"user","message":{"role":"user","content":"keep going"}}"#;
const ANSWER: &str = r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","content":[{"type":"text","text":"ok"}]}}"#;

/// What the session's supervisor typed, as its ledger records it
/// (`supervise::approvals::typed_texts`).
fn ours() -> Vec<String> {
    vec![SupervisorConfig::default().continue_text]
}

/// The screen the model state stands for; `unanswered`: the act's `❯` row
/// is last on it, never taken; `taskless`: what the conversation's record
/// says (the real `TaskScan`'s word).
fn reading(
    st: &State,
    worked: Option<Duration>,
    unanswered: bool,
    taskless: bool,
) -> TurnEndReading {
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
        taskless,
        person: (st["person"] == 1).then_some(JUST_TYPED),
        login_back: false,
    }
}

/// The real side of one walk: the decider's state, the loop's clock,
/// whether the act's `❯` row stands unanswered on the screen, and the
/// conversation's record as the walk wrote it.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    unanswered: bool,
    record: String,
}

impl Real {
    /// No task, by the real scan over the record and the loop's ledger.
    fn taskless(&self) -> bool {
        !transcript_tasked(&self.record, &ours())
    }
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
    // No task: nothing ended here, and nothing is done.
    if st["tasked"] == 0 {
        return Verdict::Nothing;
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
        "WorkedShort"
            | "WorkedLong"
            | "RetryTaken"
            | "RetryHitsTheWall"
            | "HumanWork"
            | "HarnessTurn"
            | "HarnessTurnLong"
    ) {
        real.unanswered = false;
    }
    if action == "ReplyUnseen" {
        real.unanswered = variant;
    }
    // The record gains the turn: a person's prompt, or the harness's own.
    match action {
        "HumanWork" => real.record.push_str(&format!("{PERSON}\n{ANSWER}\n")),
        "HarnessTurn" | "HarnessTurnLong" => real
            .record
            .push_str(&format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n")),
        _ => {}
    }
    let unanswered = real.unanswered;
    let taskless = real.taskless();
    let seen = |st: &State, worked| reading(st, worked, unanswered, taskless);
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
        // The host typed it (its notice, its carry-on), and it was answered
        // short: the harness's own turn.
        "HarnessTurn" => {
            real.st.host_typed(real.now);
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        // …and answered with real work: the worker's own, resumed.
        "HarnessTurnLong" => {
            real.st.host_typed(real.now);
            real.st.observe(&seen(after, Some(LONG)), real.now);
        }
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
        record: String::new(),
    };
    // The walk starts at a point that ended a turn of real work, in a
    // session nobody has asked anything yet.
    let s0 = m.init_state();
    init.st
        .observe(&reading(&s0, Some(LONG), false, true), init.now);
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
        // The record's task is the model's: the harness's own turns are none.
        assert_eq!(real.taskless(), st["tasked"] == 0, "the task at {st:?}");
        let mut at = real.st.clone();
        let here = reading(&st, None, real.unanswered, real.taskless());
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, cfg, real.now));
        assert_eq!(
            got,
            expected(m, &st),
            "the real decider disagrees at {st:?}"
        );
        // No silent latch: at a free point of a session with a task the
        // real decider says something.
        if st["tasked"] == 1 && st["box_up"] == 0 && (st["pending"] == 0 || st["pending"] == 3) {
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
            // 184 before any turn of the harness's (160 with a task), and
            // after each of its two the same 184 again — a short answer
            // moves nothing but their count, a long one only what any turn
            // of real work moves — plus the 24 states of a session with no
            // task whose last turn answered was real work (a long answer to
            // the harness's).
            assert_eq!(states, 600, "the reachable space changed: {states}");
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
    real.observe(&reading(&m.init_state(), Some(LONG), false, false), base);
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let asked = |mut st: State| {
        st.insert("asked", 1);
        st.insert("tasked", 1);
        st
    };
    for var in ["box_up", "person", "draft"] {
        let mut held = asked(m.init_state());
        held.insert(var, 1);
        assert_ne!(
            verdict(&decide_turn_end(
                &real,
                &reading(&held, None, false, false),
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
    let mut done = asked(m.init_state());
    done.insert("short", 2);
    done.insert("backoff", 1);
    assert!(!m.action_enabled("Escalate", &done));
    assert!(buggy.action_enabled("Escalate", &done));

    // D1: after the harness's own turn — its record the harness's alone —
    // the real decider does nothing, however long the point stands, while
    // the Buggy model (a reader that takes the harness's turn for a task)
    // types into it and is caught.
    let record = format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n");
    assert!(
        !transcript_tasked(&record, &ours()),
        "the harness's own turns are no task"
    );
    // NEGATIVE CONTROL: without the loop's ledger, its `keep going` reads as
    // someone's — the ledger is what keeps the walk's record taskless.
    assert!(transcript_tasked(&record, &[]));
    let mut after = m.init_state();
    assert!(m.fire("HarnessTurn", &mut after));
    let mut real = TurnEndState::default();
    real.observe(&reading(&after, Some(SHORT), false, true), base);
    let later = base + Duration::from_secs(24 * 3600);
    assert_eq!(
        verdict(&decide_turn_end(
            &real,
            &reading(&after, None, false, true),
            &SupervisorConfig::default(),
            later
        )),
        Verdict::Nothing
    );
    let mut wrong = m.init_state();
    assert!(buggy.fire("HarnessTurn", &mut wrong));
    assert!(buggy.fire("BackoffDue", &mut wrong));
    assert!(buggy.fire("Continue", &mut wrong));
    assert!(!buggy.check_invariant("NeverTypeIntoATasklessSession", &wrong));
    // …and a person's prompt, answered, is one.
    assert!(transcript_tasked(
        &format!("{record}{PERSON}\n{ANSWER}\n"),
        &ours()
    ));

    // N1: after the worker's real work, the upgrade's two turns — its
    // notice's READY and its carry-on's reply, each short — leave the point
    // free: the model continues, and so does the real decider, the host's
    // turns registered as typed. NEGATIVE CONTROL: the same answers read as
    // someone else's turns back the point off, and the Buggy model that
    // counts them so is caught.
    let mut st = asked(m.init_state());
    for action in ["HarnessTurn", "HarnessTurn"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(m.action_enabled("Continue", &st), "{st:?}");
    let busy = |real: &mut TurnEndState, host: bool| {
        let mut now = base;
        real.observe(&reading(&st, Some(LONG), false, false), now);
        for _ in 0..2 {
            now += Duration::from_secs(30);
            if host {
                real.host_typed(now);
            }
            real.observe(&reading(&st, Some(SHORT), false, false), now);
        }
        verdict(&decide_turn_end(
            real,
            &reading(&st, None, false, false),
            &SupervisorConfig::default(),
            now,
        ))
    };
    assert_eq!(
        busy(&mut TurnEndState::new(timing()), true),
        Verdict::Continue
    );
    assert_eq!(busy(&mut TurnEndState::new(timing()), false), Verdict::Wait);
    let mut wrong = asked(m.init_state());
    assert!(buggy.fire("HarnessTurn", &mut wrong));
    assert!(!buggy.check_invariant("NeverBackOffForTheHarness", &wrong));

    // The answer that is real work: after the worker's short turn started a
    // back-off, the carry-on's long answer ends it — the model continues,
    // and so does the real decider; the Buggy model that leaves the streak
    // standing is caught.
    let mut st = asked(m.init_state());
    for action in ["Continue", "WorkedShort", "HarnessTurnLong"] {
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(m.action_enabled("Continue", &st), "{st:?}");
    let mut real = TurnEndState::new(timing());
    let mut now = base;
    real.observe(&reading(&st, Some(LONG), false, false), now);
    let r = reading(&st, None, false, false);
    let a = decide_turn_end(&real, &r, &SupervisorConfig::default(), now);
    real.acted(&a, &r, now);
    now += Duration::from_secs(30);
    real.observe(&reading(&st, Some(SHORT), false, false), now);
    assert_eq!(real.short_streak(), 1);
    now += Duration::from_secs(30);
    real.host_typed(now);
    real.observe(&reading(&st, Some(LONG), false, false), now);
    assert_eq!(
        verdict(&decide_turn_end(
            &real,
            &reading(&st, None, false, false),
            &SupervisorConfig::default(),
            now
        )),
        Verdict::Continue
    );
    let mut wrong = asked(m.init_state());
    for action in ["Continue", "WorkedShort", "HarnessTurnLong"] {
        assert!(buggy.fire(action, &mut wrong), "{action} at {wrong:?}");
    }
    assert!(!buggy.check_invariant("NeverBackOffAfterRealWork", &wrong));
}
