// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorTurnEnd` (aterm-spec `supervisor_turn_end_model`): the
//! real turn-end decider (`decide_turn_end`, with its `TurnEndState`) driven
//! along every reachable state of the model, fully automatic (no cap, the
//! default) and under a cap.
//!
//! Each model action is mirrored on the real side: a box or wall is the
//! reading handed to the decider, a person is a keystroke inside the grace, a
//! draft is composer text, waits run out by moving the clock through the real
//! retry and back-off ladders, and `Continue`/`Retry`/`SubmitDraft` are the
//! decider's own acts, recorded with `acted`. At every state the real verdict
//! types a continuation, a retry or a draft submit exactly where the model
//! enables it, waits where a wall, person or back-off holds, escalates only
//! where the cap is spent, and at an act's point with no busy read waits for
//! its deadline and then acts or waits, never nothing. The walk runs with that
//! point a reply that ended unseen and with the act's `❯` row unanswered.
//!
//! The task: the walk writes the conversation's record as it goes, and the
//! reading's `taskless` is the real `harness::upgrade::transcript_tasked`,
//! checked against the model's `tasked`: the harness's own turns never make a
//! task, and with none the decider does nothing. A `HarnessTurn` (the host's
//! typed turn, `TurnEndState::host_typed`) answered short never moves the
//! streak; a `HarnessTurnLong` ends it.
//!
//! Every walk runs over what the turn ends on: a report (`continue@v1`'s `keep
//! going`) and a choice asked in prose (`answer@v1`'s `answer_text`, and the
//! `prose-long-term@v1`/`prose-short-term@v1` texts per question priority):
//! the harness answers and goes on (owner, 2026-09-25).
//!
//! FOREVER CRUISE: a continuation's reply that claims done
//! (`WorkedClaimsDone`, the retired token on its last line) ends nothing —
//! the point's act is the defend-and-find-more prompt
//! (`continue-done-check@v1`) in the continuation's place, on the same gates
//! — and a reply that names what only a person can give (`WorkedNeeds`, a
//! `NEEDS YOU:` line) holds nothing: the act goes, carrying the need's ack.
//! The projection holds the real done claim, the needs standing and the live
//! read of someone else's turn ([`TurnEndState::other_turn`]) as the model's
//! `claim`, `need` and `other`; the real task is never done and its face
//! ([`TaskFace::of`]) open at every state (the landed and theirs faces are
//! `supervise_conformance_land.rs`'s).
//!
//! Negative controls: with a box up, a person present or a draft the real
//! decider types nothing where the `Buggy = 1` model would; the `Buggy`
//! model's task ends on a done claim, where the real decider defends it, and
//! a need holds its point, where the real decider types; without the loop's
//! ledger the harness's own record reads as a task; and a host turn not
//! registered backs off where the model continues. The model's other
//! counterexamples are the registry sweep's
//! (`aterm-models/tests/non_vacuity.rs`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::harness::upgrade::transcript_tasked;
use aterm_agent::supervise::approvals::Typed;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end::{
    Composer, DONE_CHECK, HostTurn, RULE_API_RETRY, RULE_CONTINUE, RULE_DONE_CHECK, TurnEndAction,
    TurnEndReading, TurnEndState, TurnEndTiming, decide_turn_end,
};
use aterm_agent::supervise::{QuestionPriority, SupervisorConfig};
use aterm_models_harness::supervisor_turn_end_model;
use aterm_phase::WallKind;
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

/// Work a turn did that counts, and work that does not.
const LONG: Duration = Duration::from_secs(3 * 60);
const SHORT: Duration = Duration::from_secs(10);
/// A person's keystroke well inside the grace.
const JUST_TYPED: Duration = Duration::from_secs(1);

/// Someone else's turn of real work: a job past `EXCHANGE_WORK`, never a
/// person's own exchange (which this model does not hold).
const JOB: Duration = Duration::from_secs(12 * 60);

/// A turn that ends on a report, and one that ends on a choice.
const REPORT: &str = "Stage done.";
const CHOICE: &str = "Should I rewrite the parser or patch the lexer?";
/// A turn that claims done on the retired token's line: a claim, never the
/// task's end.
const FINISHED_REPLY: &str = "Committed the parser and its tests.\nATERM-TASK-DONE";
/// The line a reply names a need on.
const NEED: &str = "NEEDS YOU: the release credential, to sign the build";

thread_local! {
    /// What this walk's turns end on ([`REPORT`], [`CHOICE`]).
    static SAID: std::cell::Cell<&'static str> = const { std::cell::Cell::new(REPORT) };
    /// The question priority this walk's policy answers a choice by.
    static PRIORITY: std::cell::Cell<QuestionPriority> =
        const { std::cell::Cell::new(QuestionPriority::Recommended) };
}

/// The rule a `Continue` of the walk under way types at `st`: the defend
/// prompt where the point claims done; else `continue@v1` after a report,
/// after a choice the priority's answer — `answer@v1` under `recommended`,
/// else `prose-<word>@v1`.
fn continue_rule(st: &State) -> &'static str {
    if st["claim"] == 1 {
        RULE_DONE_CHECK
    } else if SAID.with(std::cell::Cell::get) == CHOICE {
        PRIORITY.with(std::cell::Cell::get).prose_rule_id()
    } else {
        RULE_CONTINUE
    }
}

/// The owner's policy for the walk under way: the defaults, answering a
/// choice by the walk's priority word.
fn policy() -> SupervisorConfig {
    SupervisorConfig {
        question_priority: PRIORITY.with(std::cell::Cell::get),
        ..SupervisorConfig::default()
    }
}

/// A person's prompt, the harness's own turn, the supervisor's own
/// continuation (no mark: its words are the owner's), and an answer, as the
/// conversation's record holds them.
const PERSON: &str = r#"{"type":"user","message":{"role":"user","content":"Stage the parser."}}"#;
const HARNESS: &str = r#"{"type":"user","message":{"role":"user","content":"[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283."}}"#;
const SUPERVISOR: &str = r#"{"type":"user","timestamp":"1970-01-01T00:00:01.000Z","message":{"role":"user","content":"keep going"}}"#;
const ANSWER: &str = r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","content":[{"type":"text","text":"ok"}]}}"#;

/// What the session's supervisor typed, and when, as its ledger records it
/// (`supervise::approvals::typed_texts`): [`SUPERVISOR`] as it was recorded,
/// an owner's own continuation words.
fn ours() -> Vec<Typed> {
    vec![Typed {
        at_ms: 1_000,
        text: "keep going".to_string(),
        rule: aterm_agent::supervise::policy::turn_end::RULE_CONTINUE.to_string(),
    }]
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
    let phase = if st["pending"] == 1 || st["pending"] == 2 || st["other"] == 1 {
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
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: if st["draft"] == 1 && st["box_up"] == 0 {
            Composer::Typed
        } else {
            Composer::Empty
        },
        said_tail: Some(if st["claim"] == 1 {
            FINISHED_REPLY.to_string()
        } else {
            SAID.with(std::cell::Cell::get).to_string()
        }),
        said_cut: false,
        turn_said: None,
        opened_by: None,
        worked,
        rules: None,
        pending_input,
        interrupted: false,
        // The window's host: its walls here are retried, never restarted.
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless,
        finished_record: None,
        left_record: false,
        left_work: None,
        person: (st["person"] == 1).then_some(JUST_TYPED),
        reach: Default::default(),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        background: false,
        served_in_transcript: false,
        notice_elsewhere: false,
        reset_clock: None,
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

/// The real side of one walk: the decider's state, the loop's clock,
/// whether the act's `❯` row stands unanswered on the screen, the
/// conversation's record as the walk wrote it and the real scan's word on
/// it, and the person's prompts answered so far (each `HumanWork` closes a
/// new one on the screen).
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    unanswered: bool,
    record: String,
    /// No task, by the real scan over [`Self::record`] and the loop's ledger
    /// ([`Self::write`]): scanned again whenever the record changes, and
    /// only then — the scan is a function of the record alone.
    taskless: bool,
    prompts: u64,
}

impl Real {
    /// No task, by the real scan over the record and the loop's ledger.
    fn taskless(&self) -> bool {
        self.taskless
    }

    /// The record gains `rows`, and the real scan reads it again.
    fn write(&mut self, rows: &str) {
        self.record.push_str(rows);
        self.taskless = !transcript_tasked(&self.record, &ours());
    }
}

/// The real state against the model's at `st`: someone else's live turn,
/// the needs standing, the task never done and its face open (FOREVER
/// CRUISE).
fn faces(real: &Real, st: &State) {
    assert_eq!(
        real.st.other_turn(),
        st["other"] == 1,
        "someone else's turn at {st:?}"
    );
    assert_eq!(
        !real.st.standing_needs().is_empty(),
        st["need"] == 1,
        "the needs standing at {st:?}"
    );
    assert!(!real.st.task_done(), "the task ended at {st:?}");
    assert_eq!(
        TaskFace::of(&real.st, real.st.other_turn()),
        TaskFace::Open,
        "the face at {st:?}"
    );
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

fn verdict(a: &TurnEndAction, st: &State) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, text } if *rule_id == continue_rule(st) => {
            if st["claim"] == 1 {
                assert!(text.starts_with(DONE_CHECK), "the defend prompt at {st:?}");
            }
            Verdict::Continue
        }
        TurnEndAction::Accept { .. } => Verdict::Continue,
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_RETRY => Verdict::Retry,
        TurnEndAction::Submit { rule_id }
            if *rule_id == continue_rule(st) || *rule_id == RULE_API_RETRY =>
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
    // No task: nothing ended here. Nor while someone else's turn runs. (A
    // done claim ends nothing: FOREVER CRUISE.)
    if st["tasked"] == 0 || st["other"] == 1 {
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
            | "WorkedClaimsDone"
            | "WorkedNeeds"
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
        "HumanWork" => real.write(&format!("{PERSON}\n{ANSWER}\n")),
        "HarnessTurn" | "HarnessTurnLong" => {
            real.write(&format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n"));
        }
        _ => {}
    }
    let unanswered = real.unanswered;
    let taskless = real.taskless();
    let seen = |st: &State, worked| reading(st, worked, unanswered, taskless);
    match action {
        "BoxAppears" | "BoxLeaves" | "PersonTypes" | "GraceEnds" | "DraftLeft" => {}
        // A turn of someone else's that ended on the wall: the person's
        // keystroke the model's `person` stands for is read after it (a
        // person's own turn split by a wall is their exchange, which this
        // model does not hold).
        "WallAppears" => {
            let r = TurnEndReading {
                person: None,
                ..seen(after, Some(SHORT))
            };
            real.st.observe(&r, real.now);
        }
        "WallDue" => real.now += real.st.timing.retry_backoff[0],
        "HourPasses" => real.now += real.st.timing.window + Duration::from_secs(1),
        "BackoffDue" => {
            let wait = real
                .st
                .short_wait()
                .expect("a back-off runs after a short turn");
            real.now += wait;
        }
        // A person's prompt, answered: its `❯` row closed on the screen, a
        // prompt no point before closed — someone's prompt, which answers
        // a need standing (never Claude Code's own wake).
        "HumanWork" => {
            real.prompts += 1;
            let r = TurnEndReading {
                answered: Some(aterm_agent::supervise::policy::turn_end::ScreenAnswer {
                    prompt: real.prompts,
                    work: Some(JOB),
                }),
                ..seen(after, Some(JOB))
            };
            real.st.observe(&r, real.now);
        }
        // A live read of someone else's turn: no act of the policy's
        // awaited.
        "OtherTurnRuns" => real.st.took(),
        // The host typed it (its notice, its carry-on), and it was answered
        // short: the harness's own turn.
        "HarnessTurn" => {
            real.st.host_typed(real.now, HostTurn::Notice);
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        // …and answered with real work: the worker's own, resumed.
        "HarnessTurnLong" => {
            real.st.host_typed(real.now, HostTurn::CarryOn);
            real.st.observe(&seen(after, Some(LONG)), real.now);
        }
        "Continue" | "Retry" | "SubmitDraft" => {
            let r = seen(before, None);
            let cfg = policy();
            let a = decide_turn_end(&real.st, &r, &cfg, real.now);
            real.st.acted(&a, &r, real.now);
        }
        "WorkedShort" | "RetryHitsTheWall" => {
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        "WorkedLong" | "WorkedClaimsDone" | "RetryTaken" => {
            real.st.observe(&seen(after, Some(LONG)), real.now);
        }
        // Real work whose reply ends on a need's line.
        "WorkedNeeds" => {
            let r = TurnEndReading {
                said_tail: Some(format!("{}\n{NEED}", SAID.with(std::cell::Cell::get))),
                ..seen(after, Some(LONG))
            };
            real.st.observe(&r, real.now);
        }
        "ReplyUnseen" | "DeadlineUnseen" => {
            real.st.observe(&seen(after, None), real.now);
        }
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

/// The real side at a walk's start: a point that ended a turn of real
/// work, in a session nobody has asked anything yet.
fn start(m: &Model, base: Instant) -> Real {
    let mut real = Real {
        st: TurnEndState::new(timing()),
        now: base,
        unanswered: false,
        record: String::new(),
        taskless: !transcript_tasked("", &ours()),
        prompts: 0,
    };
    real.st
        .observe(&reading(&m.init_state(), Some(LONG), false, true), real.now);
    real
}

/// `path` fired on the model `m` from its start, mirrored on the real side.
fn replay(m: &Model, path: &[&str], base: Instant) -> (State, Real) {
    let take = cnst(m, "Take");
    let mut st = m.init_state();
    let mut real = start(m, base);
    for action in path {
        let before = st.clone();
        assert!(m.fire(action, &mut st), "{action} at {before:?}");
        mirror(action, &before, &st, &mut real, take, false);
    }
    (st, real)
}

/// What the real decider says at `st`, the point read again as a loop reads
/// it before it decides.
fn decided(real: &Real, st: &State, cfg: &SupervisorConfig) -> TurnEndAction {
    let mut at = real.st.clone();
    let here = reading(st, None, real.unanswered, real.taskless());
    at.observe(&here, real.now);
    decide_turn_end(&at, &here, cfg, real.now)
}

/// Walk `m` against the real decider under `cfg` (`variant`: an unseen
/// reply is the act's row left unanswered); every verdict seen.
fn walk(m: &Model, cfg: &SupervisorConfig, variant: bool) -> BTreeMap<&'static str, usize> {
    let base = Instant::now() + Duration::from_secs(3600);
    let streak = cnst(m, "Streak");
    let take = cnst(m, "Take");

    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), start(m, base))]);
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
        // The done claim the point's act answers, wherever a point stands.
        if st["pending"] == 0 && st["wall"] == 0 {
            assert_eq!(
                real.st.done_reports() == 1,
                st["claim"] == 1,
                "the done claim at {st:?}"
            );
        }
        faces(&real, &st);
        let act = decided(&real, &st, cfg);
        let got = verdict(&act, &st);
        assert_eq!(
            got,
            expected(m, &st),
            "the real decider disagrees at {st:?}"
        );
        // A new set of needs is acked by the act the point types — by its
        // own words, never a vendor's suggestion.
        if let TurnEndAction::Type { text, .. } = &act
            && st["need"] == 1
            && got == Verdict::Continue
            && real.st.needs_ack_owed()
        {
            assert!(text.contains(NEEDS_ACK), "the need's ack at {st:?}: {text}");
        }
        // No silent latch: at a free point of a session with a task the
        // real decider says something — at a done claim and a standing need
        // too (FOREVER CRUISE).
        if st["tasked"] == 1
            && st["other"] == 0
            && st["box_up"] == 0
            && (st["pending"] == 0 || st["pending"] == 3)
        {
            assert_ne!(got, Verdict::Nothing, "a silent point at {st:?}");
        }
        *acts
            .entry(match got {
                Verdict::Continue if st["claim"] == 1 => "defend",
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
            // Along EVERY transition, not only at a state's first visit: a
            // state the walk reached before by another path is checked here
            // on this one.
            faces(&r, &next);
            queue.push_back((next, r));
        }
    }
    acts
}

#[test]
fn tier1_the_real_decider_types_exactly_where_the_model_allows() {
    // Every walk on a thread of its own, which sets its own `SAID` and
    // `PRIORITY`: the twelve are independent, each the whole model's walk.
    std::thread::scope(|scope| {
        for (said, priority) in [
            (REPORT, QuestionPriority::Recommended),
            (CHOICE, QuestionPriority::Recommended),
            // A choice under every text word: the policy texts ride the same
            // switch, cap, back-off and walls as `answer_text`.
            (CHOICE, QuestionPriority::LongTerm),
            (CHOICE, QuestionPriority::ShortTerm),
        ] {
            // Fully automatic: no cap, and nothing is ever escalated, with an
            // unseen reply and with an act never taken.
            for variant in [false, true] {
                scope.spawn(move || {
                    SAID.with(|s| s.set(said));
                    PRIORITY.with(|p| p.set(priority));
                    let acts = walk(&supervisor_turn_end_model(), &policy(), variant);
                    for word in ["continue", "defend", "retry", "submit", "wait", "nothing"] {
                        assert!(
                            acts.get(word).copied().unwrap_or(0) > 0,
                            "{word} never occurred ({said}, {variant}): {acts:?}"
                        );
                    }
                    assert_eq!(acts.get("escalate"), None, "{said}, {variant}: {acts:?}");
                });
            }
            // A cap the owner wrote: escalated exactly where it is spent.
            scope.spawn(move || {
                SAID.with(|s| s.set(said));
                PRIORITY.with(|p| p.set(priority));
                let capped =
                    aterm_spec::interp::with_consts(&supervisor_turn_end_model(), &[("Budget", 2)]);
                let cfg = SupervisorConfig {
                    continue_per_hour: u32::try_from(cnst(&capped, "Budget")).unwrap(),
                    ..policy()
                };
                let acts = walk(&capped, &cfg, false);
                assert!(
                    acts.get("escalate").copied().unwrap_or(0) > 0,
                    "{said}: {acts:?}"
                );
            });
        }
    });
    let m = supervisor_turn_end_model();

    // Negative controls: a box up, a person at the keyboard or a draft, with
    // nothing else in the way — the real decider types nothing, the Buggy
    // model would.
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
            verdict(
                &decide_turn_end(
                    &real,
                    &reading(&held, None, false, false),
                    &SupervisorConfig::default(),
                    base
                ),
                &held
            ),
            Verdict::Continue,
            "{var}"
        );
        assert!(
            buggy.action_enabled("Continue", &held),
            "{var}: the Buggy model types over it — the walk would have caught a real one"
        );
    }

    // FOREVER CRUISE: at a continuation's done claim, past the back-off it
    // earned, the Buggy model has ended the task (nothing enabled, the
    // policy before it) — the real decider types the defend prompt, and its
    // task is not done.
    let path = ["HumanWork", "Continue", "WorkedClaimsDone", "BackoffDue"];
    let mut b = buggy.init_state();
    for action in path {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("CruiseNeverRests", &b), "{b:?}");
    assert!(!buggy.action_enabled("Continue", &b), "{b:?}");
    let (st, real) = replay(&m, &path, base);
    assert!(!real.st.task_done());
    let act = decided(&real, &st, &policy());
    assert!(
        matches!(&act, TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_DONE_CHECK),
        "{act:?}"
    );
    // …and at a standing need the Buggy model holds the point for the
    // person; the real decider types at once, the need's ack beside the
    // continuation, never an escalation.
    let path = ["HumanWork", "Continue", "WorkedNeeds"];
    let mut b = buggy.init_state();
    for action in path {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("ANeedNeverBlocks", &b), "{b:?}");
    assert!(!buggy.action_enabled("Continue", &b), "{b:?}");
    let (st, real) = replay(&m, &path, base);
    assert_eq!(real.st.standing_needs().len(), 1);
    let act = decided(&real, &st, &policy());
    assert!(
        matches!(&act, TurnEndAction::Type { rule_id, text }
            if *rule_id == RULE_CONTINUE && text.contains(NEEDS_ACK)),
        "{act:?}"
    );

    // Without the loop's ledger, its `keep going` reads as someone's turn:
    // the ledger is what keeps the harness's own record taskless.
    let record = format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n");
    assert!(!transcript_tasked(&record, &ours()));
    assert!(transcript_tasked(&record, &[]));

    // N1: after the worker's real work, the upgrade's two turns — its
    // notice's READY and its carry-on's reply, each short — leave the point
    // free when the host's turns are registered as typed; the same answers
    // read as someone else's turns back the point off.
    let st = asked(m.init_state());
    let busy = |host: bool| {
        let mut real = TurnEndState::new(timing());
        let mut now = base;
        real.observe(&reading(&st, Some(LONG), false, false), now);
        for _ in 0..2 {
            now += Duration::from_secs(30);
            if host {
                real.host_typed(now, HostTurn::Notice);
            }
            real.observe(&reading(&st, Some(SHORT), false, false), now);
        }
        verdict(
            &decide_turn_end(
                &real,
                &reading(&st, None, false, false),
                &SupervisorConfig::default(),
                now,
            ),
            &st,
        )
    };
    assert_eq!(busy(true), Verdict::Continue);
    assert_eq!(busy(false), Verdict::Wait);
}
