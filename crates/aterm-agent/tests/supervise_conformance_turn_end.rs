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
//! THE TASK'S FACE (the Autopilot design's §9.4.2): the projection also
//! holds the face the real teller told its host — the loop's discipline,
//! [`TurnEndState::face_moved`] asked after each point folded in and after
//! each live read of someone else's turn ([`TurnEndState::took`]) — which
//! must be the model's `told`, and the real [`TaskFace::of`] and
//! [`TurnEndState::other_turn`], which must be the model's face and `other`,
//! at every step. A continuation's reply that ends on the worker's own token
//! (`WorkedFinished`) is a done task; a live read of someone else's turn
//! (`OtherTurnRuns`) opens it again.
//!
//! Negative controls: with a box up, a person present or a draft the real
//! decider types nothing where the `Buggy = 1` model would; without the
//! loop's ledger the harness's own record reads as a task; a host turn not
//! registered backs off where the model continues; and three tellers each
//! fail the walk: one told on the journal's `DONE`/`LEFT` notes alone, one
//! told at points alone, and one that says landed for any done task. The
//! `Buggy` model that tells nothing at the live read is caught by
//! `TheHostIsToldTheTasksFace`; the model's other counterexamples are the
//! registry sweep's (`aterm-models/tests/non_vacuity.rs`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::harness::upgrade::transcript_tasked;
use aterm_agent::supervise::approvals::Typed;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, HostTurn, RULE_API_RETRY, RULE_CONTINUE, TurnEndAction, TurnEndReading, TurnEndState,
    TurnEndTiming, decide_turn_end,
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
/// A turn that ends on the worker's own token: the task is done.
const FINISHED_REPLY: &str = "Committed the parser and its tests.\nATERM-TASK-DONE";

thread_local! {
    /// What this walk's turns end on ([`REPORT`], [`CHOICE`]).
    static SAID: std::cell::Cell<&'static str> = const { std::cell::Cell::new(REPORT) };
    /// The question priority this walk's policy answers a choice by.
    static PRIORITY: std::cell::Cell<QuestionPriority> =
        const { std::cell::Cell::new(QuestionPriority::Recommended) };
}

/// The rule a `Continue` of the walk under way types: `continue@v1` after a
/// report, after a choice the priority's answer — `answer@v1` under
/// `recommended`, else `prose-<word>@v1`.
fn continue_rule() -> &'static str {
    if SAID.with(std::cell::Cell::get) == CHOICE {
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
/// (`supervise::approvals::typed_texts`): [`SUPERVISOR`] as it was recorded.
fn ours() -> Vec<Typed> {
    vec![Typed {
        at_ms: 1_000,
        text: SupervisorConfig::default().continue_text,
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
        said_tail: Some(if st["fin"] == 1 {
            FINISHED_REPLY.to_string()
        } else {
            SAID.with(std::cell::Cell::get).to_string()
        }),
        worked,
        rules: None,
        pending_input,
        interrupted: false,
        // The window's host: its walls here are retried, never restarted.
        restartable: true,
        resume: None,
        upgrading: false,
        taskless,
        finished_record: None,
        finished_at_check: false,
        check_record: None,
        left_record: false,
        left_work: None,
        person: (st["person"] == 1).then_some(JUST_TYPED),
        reach: Default::default(),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        notice_elsewhere: false,
        reset_clock: None,
        program: aterm_phase::Program::Claude,
        account: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        wall_remembered: false,
    }
}

/// HOW THE TASK'S FACE IS TOLD to the host: the loop's own discipline, and
/// three that miss a move (the negative controls).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Teller {
    /// The loop's: [`TurnEndState::face_moved`] after each point folded in
    /// and after each live read of someone else's turn.
    Loop,
    /// Told only where the journal says a task's end (`DONE`, `LEFT`): the
    /// task done where it was not.
    OnEndNotes,
    /// Told at points alone, never at a live read.
    AtPointsOnly,
    /// Landed for any done task.
    LandedForDone,
}

/// The real side of one walk: the decider's state, the loop's clock,
/// whether the act's `❯` row stands unanswered on the screen, the
/// conversation's record as the walk wrote it, and the face its host was last
/// told, by `teller`.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    unanswered: bool,
    record: String,
    told: Option<TaskFace>,
    teller: Teller,
    /// The task was done at the last point ([`Teller::OnEndNotes`]).
    noted: bool,
}

impl Real {
    /// No task, by the real scan over the record and the loop's ledger.
    fn taskless(&self) -> bool {
        !transcript_tasked(&self.record, &ours())
    }

    /// A point was folded in (`busy`: no — a live read of someone else's
    /// turn): the face told, by this walk's teller.
    fn tell(&mut self, busy: bool) {
        match self.teller {
            Teller::Loop => {
                if let Some(face) = self.st.face_moved() {
                    self.told = Some(face);
                }
            }
            Teller::AtPointsOnly => {
                if !busy && let Some(face) = self.st.face_moved() {
                    self.told = Some(face);
                }
            }
            Teller::OnEndNotes => {
                let done = self.st.task_done();
                if !busy && done && !self.noted {
                    self.told = Some(self.st.face());
                }
                if !busy {
                    self.noted = done;
                }
            }
            Teller::LandedForDone => {
                self.told = Some(if self.st.task_done() {
                    TaskFace::Landed
                } else {
                    self.st.face()
                });
            }
        }
    }
}

/// The model's face of `st` (`TheHostIsToldTheTasksFace`): open while
/// someone else's turn runs, else done where the worker's token ended it.
fn face(st: &State) -> TaskFace {
    if st["other"] == 0 && st["fin"] == 1 {
        TaskFace::Done
    } else {
        TaskFace::Open
    }
}

/// The face the model's `told` names.
fn told(st: &State) -> TaskFace {
    if st["told"] == 1 {
        TaskFace::Done
    } else {
        TaskFace::Open
    }
}

/// The task's face of `real` against the model's at `st`: someone else's
/// turn and the face itself must be the model's (a panic), and the face the
/// host was last told too — by the walk's teller, whose miss is the error
/// handed back.
fn faces(real: &Real, st: &State) -> Result<(), String> {
    assert_eq!(
        real.st.other_turn(),
        st["other"] == 1,
        "someone else's turn at {st:?}"
    );
    assert_eq!(
        TaskFace::of(&real.st, real.st.other_turn()),
        face(st),
        "the face at {st:?}"
    );
    if real.told == Some(told(st)) {
        Ok(())
    } else {
        Err(format!(
            "{:?} told {:?} where the model says {:?} at {st:?}",
            real.teller,
            real.told,
            told(st)
        ))
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
    // No task: nothing ended here, and nothing is done. Nor at a done task,
    // nor while someone else's turn runs.
    if st["tasked"] == 0 || st["fin"] == 1 || st["other"] == 1 {
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
            | "WorkedFinished"
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
            real.tell(false);
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
        "HumanWork" => {
            real.st.observe(&seen(after, Some(JOB)), real.now);
            real.tell(false);
        }
        // A live read of someone else's turn: no act of the policy's
        // awaited.
        "OtherTurnRuns" => {
            real.st.took();
            real.tell(true);
        }
        // The host typed it (its notice, its carry-on), and it was answered
        // short: the harness's own turn.
        "HarnessTurn" => {
            real.st.host_typed(real.now, HostTurn::Notice);
            real.st.observe(&seen(after, Some(SHORT)), real.now);
            real.tell(false);
        }
        // …and answered with real work: the worker's own, resumed.
        "HarnessTurnLong" => {
            real.st.host_typed(real.now, HostTurn::CarryOn);
            real.st.observe(&seen(after, Some(LONG)), real.now);
            real.tell(false);
        }
        "Continue" | "Retry" | "SubmitDraft" => {
            let r = seen(before, None);
            let cfg = policy();
            let a = decide_turn_end(&real.st, &r, &cfg, real.now);
            real.st.acted(&a, &r, real.now);
        }
        "WorkedShort" | "RetryHitsTheWall" => {
            real.st.observe(&seen(after, Some(SHORT)), real.now);
            real.tell(false);
        }
        "WorkedLong" | "WorkedFinished" | "RetryTaken" => {
            real.st.observe(&seen(after, Some(LONG)), real.now);
            real.tell(false);
        }
        "ReplyUnseen" | "DeadlineUnseen" => {
            real.st.observe(&seen(after, None), real.now);
            real.tell(false);
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

/// Walk `m` against the real decider under `cfg` (`variant`: an unseen
/// reply is the act's row left unanswered), the face told by the loop's
/// teller; every verdict seen.
fn walk(m: &Model, cfg: &SupervisorConfig, variant: bool) -> BTreeMap<&'static str, usize> {
    walk_telling(m, cfg, variant, Teller::Loop).unwrap_or_else(|e| panic!("the face told: {e}"))
}

/// [`walk`] with the face told by `teller`: the first state where the face
/// the host was told is not the model's, as an error.
fn walk_telling(
    m: &Model,
    cfg: &SupervisorConfig,
    variant: bool,
    teller: Teller,
) -> Result<BTreeMap<&'static str, usize>, String> {
    let base = Instant::now() + Duration::from_secs(3600);
    let mut init = Real {
        st: TurnEndState::new(timing()),
        now: base,
        unanswered: false,
        record: String::new(),
        told: None,
        teller,
        noted: false,
    };
    // The walk starts at a point that ended a turn of real work, in a
    // session nobody has asked anything yet — the loop's start told first.
    let s0 = m.init_state();
    init.told = Some(init.st.face_start());
    init.st
        .observe(&reading(&s0, Some(LONG), false, true), init.now);
    init.tell(false);
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
        // The task's face: the real one is the model's, and so is the face
        // the host was last told.
        faces(&real, &st)?;
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
        if st["tasked"] == 1
            && st["fin"] == 0
            && st["other"] == 0
            && st["box_up"] == 0
            && (st["pending"] == 0 || st["pending"] == 3)
        {
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
            // Along EVERY transition, not only at a state's first visit: a
            // state the walk reached before by another path (someone else's
            // turn ended with or without a live read of it) is checked here
            // on this one.
            faces(&r, &next)?;
            queue.push_back((next, r));
        }
    }
    Ok(acts)
}

#[test]
fn tier1_the_real_decider_types_exactly_where_the_model_allows() {
    let m = supervisor_turn_end_model();
    for (said, priority) in [
        (REPORT, QuestionPriority::Recommended),
        (CHOICE, QuestionPriority::Recommended),
        // A choice under every text word: the policy texts ride the same
        // switch, cap, back-off and walls as `answer_text`.
        (CHOICE, QuestionPriority::LongTerm),
        (CHOICE, QuestionPriority::ShortTerm),
    ] {
        SAID.with(|s| s.set(said));
        PRIORITY.with(|p| p.set(priority));
        // Fully automatic: no cap, and nothing is ever escalated, with an
        // unseen reply and with an act never taken.
        for variant in [false, true] {
            let acts = walk(&m, &policy(), variant);
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
            ..policy()
        };
        let acts = walk(&capped, &cfg, false);
        assert!(
            acts.get("escalate").copied().unwrap_or(0) > 0,
            "{said}: {acts:?}"
        );
    }
    SAID.with(|s| s.set(REPORT));
    PRIORITY.with(|p| p.set(QuestionPriority::Recommended));

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
            verdict(&decide_turn_end(
                &real,
                &reading(&held, None, false, false),
                &SupervisorConfig::default(),
                base
            )),
            Verdict::Continue,
            "{var}"
        );
        assert!(
            buggy.action_enabled("Continue", &held),
            "{var}: the Buggy model types over it — the walk would have caught a real one"
        );
    }

    // THE FACE TOLD (the Autopilot design's §9.4.2): a teller told on the
    // journal's `DONE`/`LEFT` notes alone, one told at points alone, and one
    // that says landed for any done task each fail the walk — the first two
    // at someone else's live read after a done task, the third at the done
    // task itself. And the `Buggy` model that tells nothing at that read is
    // caught by `TheHostIsToldTheTasksFace`, which the model proper holds
    // there.
    for teller in [
        Teller::OnEndNotes,
        Teller::AtPointsOnly,
        Teller::LandedForDone,
    ] {
        let failed = walk_telling(&m, &policy(), false, teller);
        assert!(failed.is_err(), "{teller:?} passed the walk");
    }
    let path = ["Continue", "WorkedFinished", "OtherTurnRuns"];
    let mut right = asked(m.init_state());
    let mut wrong = asked(buggy.init_state());
    for action in path {
        assert!(m.fire(action, &mut right), "{action} at {right:?}");
        assert!(buggy.fire(action, &mut wrong), "{action} at {wrong:?}");
    }
    assert!(m.check_invariant("TheHostIsToldTheTasksFace", &right));
    assert!(!buggy.check_invariant("TheHostIsToldTheTasksFace", &wrong));

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
        verdict(&decide_turn_end(
            &real,
            &reading(&st, None, false, false),
            &SupervisorConfig::default(),
            now,
        ))
    };
    assert_eq!(busy(true), Verdict::Continue);
    assert_eq!(busy(false), Verdict::Wait);
}
