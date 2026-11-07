// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance and negative-control suite for `SupervisorTurnEnd`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::harness::upgrade::transcript_tasked;
use aterm_agent::supervise::approvals::Typed;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::exception::FUTILE_AFTER_TRIES;
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end::{
    CommandEnter, Composer, DONE_CHECK, HostTurn, NO_WORK_ACTS, RULE_API_RETRY, RULE_CONTINUE,
    RULE_DONE_CHECK, TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming, WaitKind,
    decide_turn_end,
};
use aterm_agent::supervise::{PriorityWord, SupervisorConfig};
use aterm_models_harness::supervisor_turn_end_model;
use aterm_phase::WallKind;
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

/// The wall the model's wall stands for: a request the API refused, a wall that keeps its own act
/// (`api-retry@v1`, each try on the refused ladder) and its tries; an overload is an outage,
/// waited out ([`supervise_conformance_outage_wait`](crate::supervise_conformance_outage_wait)).
const REFUSED: WallKind = WallKind::ApiError {
    code: Some(400),
    retryable: false,
    cause: aterm_phase::ApiCause::Request,
};
const REFUSED_MESSAGE: &str = "API Error: 400 bad request";

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
    static PRIORITY: std::cell::Cell<PriorityWord> =
        const { std::cell::Cell::new(PriorityWord::Recommended) };
}

/// The rule a `Continue` of the walk under way types at `st`: the defend
/// prompt where the point claims done; else `continue@v1` after a report or at
/// a turn the harness's own stop ended (`stopped`: its `Interrupted` row asks
/// nothing, `TurnEndReading::asked`), after a choice the priority's answer —
/// `answer@v1` under `recommended`, else `prose-<word>@v1`.
fn continue_rule(st: &State, stopped: bool) -> &'static str {
    if st["claim"] == 1 {
        RULE_DONE_CHECK
    } else if SAID.with(std::cell::Cell::get) == CHOICE && !stopped {
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

/// The screen the model state stands for, its last turn's earlier words
/// [`crate::common::earlier`]'s `earlier`; `unanswered`: the act's `❯` row is last on it,
/// never taken; `taskless`: what the conversation's record says (the real
/// `TaskScan`'s word).
fn reading(
    (st, earlier): (&State, usize),
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
    let wall = (st["wall"] > 0 && st["box_up"] == 0).then_some(REFUSED);
    let pending_input = unanswered && phase == Phase::Idle;
    TurnEndReading {
        phase,
        authoritative: true,
        survey: false,
        wall,
        wall_message: if wall.is_some() {
            REFUSED_MESSAGE.to_string()
        } else {
            String::new()
        },
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: if st["draft"] == 1 && st["box_up"] == 0 && st["command"] == 1 {
            Composer::Command(aterm_phase::DraftCommand::Slash, CommandEnter::Runs)
        } else if st["draft"] == 1 && st["box_up"] == 0 {
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
        opens_cut: false,
        turn_said: Some(crate::common::earlier(earlier)),
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
        pending_input,
        interrupted: false,
        fault: None,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless,
        finished_record: None,
        left_record: false,
        left_work: None,
        person: (st["person"] == 1).then_some(JUST_TYPED),
        key: (st["person"] == 1).then_some(JUST_TYPED),
        reach: Default::default(),
        witness: None,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        program: aterm_phase::Program::Claude,
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
        // A turn of real work ran a tool.
        ran_tools: worked.is_some_and(|w| w >= LONG),
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::Clean,
        leftover: aterm_agent::harness::leftover::LeftRead::NotAsked,
        fact: None,
        wake: None,
    }
}

/// The real side of one walk: the decider's state, the loop's clock,
/// whether the act's `❯` row stands unanswered on the screen, the last
/// turn's [`crate::common::earlier`] words, the conversation's record as the walk wrote it
/// and the real scan's word on it, and the person's prompts answered so far
/// (each `HumanWork` closes a new one on the screen).
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    unanswered: bool,
    earlier: usize,
    record: String,
    /// No task, by the real scan over [`Self::record`] and the loop's ledger
    /// ([`Self::write`]): scanned again whenever the record changes, and
    /// only then — the scan is a function of the record alone.
    taskless: bool,
    prompts: u64,
    /// The last reply named a need (`WorkedNeeds`): the same reply again says it again.
    needs_said: bool,
    /// The harness's own stop ended the last turn (`HarnessStopsTurn`, `HarnessStopsTurnLong`):
    /// the screen shows its `Interrupted` row until a turn ends, a live read sees someone's turn
    /// run or an act is typed below it.
    stopped: bool,
    path: Vec<String>,
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
/// the needs standing, the task never done and its face open.
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
    Recover,
    Retry,
    Submit,
    Wait,
    Escalate,
    Nothing,
}

fn verdict(a: &TurnEndAction, st: &State) -> Verdict {
    verdict_at(a, st, false)
}

/// [`verdict`] at a point the harness's own stop ended where `stopped` ([`Real::stopped`]).
fn verdict_at(a: &TurnEndAction, st: &State, stopped: bool) -> Verdict {
    match a {
        TurnEndAction::Type { text, .. }
            if text == aterm_agent::supervise::policy::turn_end::FORWARD_RECOVERY =>
        {
            Verdict::Recover
        }
        TurnEndAction::Type { rule_id, text } if *rule_id == continue_rule(st, stopped) => {
            if st["claim"] == 1 {
                assert!(text.starts_with(DONE_CHECK), "the defend prompt at {st:?}");
            }
            Verdict::Continue
        }
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_RETRY => Verdict::Retry,
        TurnEndAction::Submit { rule_id }
            if *rule_id == continue_rule(st, stopped)
                || *rule_id == RULE_API_RETRY
                || *rule_id == RULE_CONTINUE =>
        {
            Verdict::Submit
        }
        TurnEndAction::WaitUntil { .. } => Verdict::Wait,
        TurnEndAction::Escalate { .. } => Verdict::Escalate,
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

/// The same no-work reply came again, the wall answered its tries the same (`Futile`), or `NoWork` no-work answers came in a row (past an unspent wall's tries).
fn held(m: &Model, st: &State) -> bool {
    let (futile, no_work) = (cnst(m, "Futile"), cnst(m, "NoWork"));
    st["same"] == 2
        || st["tries"] == futile
        || (st["run"] >= no_work
            && (st["wall"] == 0 || st["tries"] >= futile || st["run"] >= no_work + futile))
}

/// The no-work answers in a row the real side counts, as far as the model's `run` counts them.
fn run(m: &Model, st: &TurnEndState) -> i64 {
    i64::try_from(st.replies().run)
        .unwrap_or(i64::MAX)
        .min(cnst(m, "NoWork") + cnst(m, "Futile"))
}

/// The wall's tries the real side counts: the answers to its own acts kept for its reply.
fn tries(st: &TurnEndState) -> i64 {
    st.replies()
        .last()
        .filter(|k| k.walled)
        .map_or(0, |k| i64::try_from(k.tries).unwrap_or(i64::MAX))
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
    if st["tasked"] == 0 || st["other"] == 1 {
        return Verdict::Nothing;
    }
    if m.action_enabled("SubmitDraft", st) {
        return Verdict::Submit;
    }
    if m.action_enabled("Recover", st) {
        return Verdict::Recover;
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
    // A person's command standing past their grace is their hold (§14.17.5.8): no act over it,
    // no Enter, and no wait for one, the same reply's quiet included; the point is decided again
    // when it goes.
    if command_holds(st) {
        return Verdict::Nothing;
    }
    // The same reply again, or the wall's tries spent at it: no act, and no wait for one, past a
    // person's grace.
    if held(m, st) && st["person"] == 0 {
        if st["wall"] == 0 {
            return Verdict::Wait;
        }
        return Verdict::Nothing;
    }
    assert!(
        st["person"] == 1 || st["wall"] == 1 || st["backoff"] == 1,
        "a free point where nothing holds and nothing is done: {st:?}"
    );
    Verdict::Wait
}

/// Whether a person's command stands in the composer, past their grace and no wait of the
/// policy's own due first (`CommandLeft`).
fn command_holds(st: &State) -> bool {
    st["draft"] == 1
        && st["command"] == 1
        && st["box_up"] == 0
        && st["person"] == 0
        && st["wall"] != 1
        && st["backoff"] == 0
}

/// The real policy's clocks, scaled so that each of the model's runs alone:
/// one retry wait for every retry (the real ladder's last entry repeats),
/// a cap window longer than any walk, and the back-off ladder the defaults
/// have, capped where the model's `Streak` saturates (2, 4, 8 min).
fn timing() -> TurnEndTiming {
    let min = |m: u64| Duration::from_secs(m * 60);
    TurnEndTiming {
        refused_backoff: vec![min(1)],
        window: min(24 * 60),
        short_backoff_max: min(8),
        ..TurnEndTiming::default()
    }
}

/// Mirror one model action on the real side (the model has fired it);
/// `variant`: an unseen reply is the act's `❯` row left unanswered.
fn mirror(action: &str, before: &State, after: &State, real: &mut Real, take: i64, variant: bool) {
    real.path.push(action.to_string());
    // A turn ended under the act's row: no row of the act's stands unanswered.
    if matches!(
        action,
        "WorkedShort"
            | "WorkedShortSame"
            | "WorkedHitsTheWall"
            | "WorkedLong"
            | "WorkedLongQuiet"
            | "WorkedLongQuietSame"
            | "WorkedClaimsDone"
            | "WorkedNeeds"
            | "RetryTaken"
            | "RetryAnsweredShort"
            | "RetryHitsTheWall"
            | "HumanWork"
            | "PersonTurnShort"
            | "HarnessTurn"
            | "HarnessTurnLong"
            | "WallAppears"
            | "ProgramTurn"
            | "ProgramTurnUnseen"
            | "ProgramTurnSame"
            | "HarnessStopsTurn"
            | "HarnessStopsTurnLong"
    ) {
        real.unanswered = false;
        // The harness's stop leaves its `Interrupted` row as the turn's end; any other turn's end
        // replaces it.
        real.stopped = matches!(action, "HarnessStopsTurn" | "HarnessStopsTurnLong");
    }
    if action == "ReplyUnseen" {
        real.unanswered = variant;
    }
    // A turn ended: its earlier words its own, but for the same reply again.
    if matches!(
        action,
        "WorkedShort"
            | "WorkedHitsTheWall"
            | "WorkedLong"
            | "WorkedLongQuiet"
            | "WorkedClaimsDone"
            | "WorkedNeeds"
            | "RetryTaken"
            | "RetryAnsweredShort"
            | "RetryHitsTheWall"
            | "HumanWork"
            | "PersonTurnShort"
            | "HarnessTurn"
            | "HarnessTurnLong"
            | "WallAppears"
            | "ProgramTurn"
            | "ProgramTurnUnseen"
            | "HarnessStopsTurn"
            | "HarnessStopsTurnLong"
    ) {
        real.earlier += 1;
        real.needs_said = action == "WorkedNeeds";
    }
    match action {
        "HumanWork" | "PersonTurnShort" => real.write(&format!("{PERSON}\n{ANSWER}\n")),
        "HarnessTurn" | "HarnessTurnLong" => {
            real.write(&format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n"));
        }
        _ => {}
    }
    let (unanswered, earlier) = (real.unanswered, real.earlier);
    let taskless = real.taskless();
    let (needs_said, stopped) = (real.needs_said, real.stopped);
    let seen = |st: &State, worked| {
        stood(
            said_need(
                reading((st, earlier), worked, unanswered, taskless),
                needs_said,
            ),
            stopped,
        )
    };
    match action {
        "BoxAppears" | "BoxLeaves" | "PersonTypes" | "GraceEnds" | "DraftLeft" | "CommandLeft"
        | "DraftCleared" => {}
        "WallAppears" => {
            let r = TurnEndReading {
                person: None,
                ..seen(after, Some(SHORT))
            };
            real.st.observe(&r, real.now);
        }
        "WallDue" => real.now += real.st.timing.refused_backoff[0],
        "HourPasses" => real.now += real.st.timing.window + Duration::from_secs(1),
        "BackoffDue" => {
            let wait = real
                .st
                .short_wait()
                .expect("a back-off runs after a short turn");
            real.now += wait;
        }
        "HumanWork" => {
            real.prompts += 1;
            let r = TurnEndReading {
                answered: Some(aterm_agent::supervise::policy::turn_end::ScreenAnswer {
                    prompt: real.prompts,
                    work: Some(JOB),
                    harness: false,
                }),
                ..seen(after, Some(JOB))
            };
            real.st.observe(&r, real.now);
        }
        // The person's own prompt, typed past the last point read and answered short: their hand
        // in the turn ([`JUST_TYPED`], the model's `person`), no act of the policy's awaited.
        // The hour their key holds the go-ahead for (`GO_AHEAD_HELD`, which keeps a same reply
        // from Cruise's recovery) is not this model's: as at every other action of the walk, the
        // key is read only where a point is decided, never booked into the state walked.
        "PersonTurnShort" => {
            real.now += SHORT + Duration::from_secs(5);
            real.prompts += 1;
            let r = TurnEndReading {
                answered: Some(aterm_agent::supervise::policy::turn_end::ScreenAnswer {
                    prompt: real.prompts,
                    work: Some(SHORT),
                    harness: false,
                }),
                key: None,
                ..seen(after, Some(SHORT))
            };
            real.st.observe(&r, real.now);
        }
        "OtherTurnRuns" => {
            real.st.took();
            real.stopped = false;
        }
        "HarnessTurn" => {
            real.st.host_typed(real.now, HostTurn::Notice);
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        "HarnessTurnLong" => {
            real.st.host_typed(real.now, HostTurn::CarryOn);
            real.st.observe(&seen(after, Some(LONG)), real.now);
        }
        "Continue" | "Recover" | "Retry" | "SubmitDraft" => {
            let r = seen(before, None);
            let cfg = policy();
            let mut a = decide_turn_end(&real.st, &r, &cfg, real.now);
            if before["backoff"] == 0
                && (before["same"] == 2 || before["run"] >= NO_WORK_ACTS as i64)
                && let TurnEndAction::WaitUntil {
                    until,
                    wait: WaitKind::Other,
                    ..
                } = a
            {
                real.now = until;
                a = decide_turn_end(&real.st, &r, &cfg, real.now);
            }
            real.st.acted(&a, &r, real.now);
            // Decided at the stopped point itself, the act is drawn below its row.
            real.stopped = false;
        }
        // The act's turn, the harness's own stop ended: the restart whose cut call the resume
        // restores (`TurnEndState::restored_cut`, as the loop marks it while its carry-on is owed),
        // the turn short, or past min_work with a tool run.
        "HarnessStopsTurn" => {
            real.st.restored_cut();
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        "HarnessStopsTurnLong" => {
            real.st.restored_cut();
            real.st.observe(&seen(after, Some(LONG)), real.now);
        }
        // The program's own turn is a short one no act awaited.
        // The wall's try answered short once the wall cleared: no act of the mode's, no count.
        "WorkedShort" | "WorkedShortSame" | "WorkedHitsTheWall" | "RetryHitsTheWall"
        | "RetryAnsweredShort" | "ProgramTurn" | "ProgramTurnSame" => {
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
        "WorkedLong" | "WorkedClaimsDone" | "RetryTaken" => {
            real.st.observe(&seen(after, Some(LONG)), real.now);
        }
        // Busy past min_work with no tool row: kept, so said again it is no work.
        "WorkedLongQuiet" | "WorkedLongQuietSame" => {
            let r = TurnEndReading {
                ran_tools: false,
                ..seen(after, Some(LONG))
            };
            real.st.observe(&r, real.now);
        }
        // Its words name the need (`needs_said`, set above).
        "WorkedNeeds" => real.st.observe(&seen(after, Some(LONG)), real.now),
        // The program's own turn no point saw busy: its new words alone show it ran.
        "ReplyUnseen" | "DeadlineUnseen" | "ProgramTurnUnseen" => {
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
        earlier: 0,
        record: String::new(),
        taskless: !transcript_tasked("", &ours()),
        prompts: 0,
        needs_said: false,
        stopped: false,
        path: Vec::new(),
    };
    real.st.observe(
        &reading((&m.init_state(), 0), Some(LONG), false, true),
        real.now,
    );
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

/// The screen `r` with the last reply's words naming the need when it did (`WorkedNeeds`).
fn said_need(r: TurnEndReading, needs_said: bool) -> TurnEndReading {
    if needs_said {
        TurnEndReading {
            said_tail: Some(format!("{}\n{NEED}", SAID.with(std::cell::Cell::get))),
            ..r
        }
    } else {
        r
    }
}

/// The screen `r` ending on the `Interrupted` row where the harness's own stop ended the last turn
/// ([`Real::stopped`]).
fn stood(r: TurnEndReading, stopped: bool) -> TurnEndReading {
    TurnEndReading {
        interrupted: stopped,
        ..r
    }
}

/// What the real decider says at `st`, the point read again as a loop reads
/// it before it decides.
fn decided(real: &Real, st: &State, cfg: &SupervisorConfig) -> TurnEndAction {
    let mut at = real.st.clone();
    let here = stood(
        said_need(
            reading((st, real.earlier), None, real.unanswered, real.taskless()),
            real.needs_said,
        ),
        real.stopped,
    );
    at.observe(&here, real.now);
    let act = decide_turn_end(&at, &here, cfg, real.now);
    if st["pending"] == 0
        && (st["same"] == 2 || st["run"] >= NO_WORK_ACTS as i64)
        && st["backoff"] == 0
        && (cfg.continue_per_hour == 0 || st["used"] < i64::from(cfg.continue_per_hour))
        && let TurnEndAction::WaitUntil {
            until,
            wait: WaitKind::Other,
            ..
        } = act
    {
        return decide_turn_end(&at, &here, cfg, until);
    }
    act
}

/// `act`, the real decider's at `st`, says the need's ack it owes.
fn says_the_ack(act: &TurnEndAction, got: &Verdict, st: &State, real: &Real) {
    if let TurnEndAction::Type { text, .. } = act
        && st["need"] == 1
        && *got == Verdict::Continue
        && real.st.needs_ack_owed()
    {
        assert!(text.contains(NEEDS_ACK), "the need's ack at {st:?}: {text}");
    }
}

/// What the real side must be at `st` however it got there, and what the real decider says.
fn holds_at(m: &Model, st: &State, real: &Real, cfg: &SupervisorConfig) -> Verdict {
    let short = real.st.short_streak();
    assert_eq!(
        i64::from(short).min(cnst(m, "Streak")),
        st["short"],
        "the streak at {st:?}"
    );
    assert_eq!(real.st.short_wait(), ladder(&real.st, short), "{st:?}");
    assert_eq!(
        real.st.wall_pass().spent,
        st["free"] > 0,
        "the wall's pass at {st:?}"
    );
    assert_eq!(real.taskless(), st["tasked"] == 0, "the task at {st:?}");
    if st["pending"] == 0 && st["wall"] == 0 {
        assert_eq!(
            real.st.done_reports() == 1,
            st["claim"] == 1,
            "the done claim at {st:?}"
        );
        assert_eq!(
            crate::common::same(&real.st),
            st["same"],
            "the same reply at {st:?}"
        );
    }
    if st["pending"] == 0 && st["wall"] > 0 {
        assert_eq!(tries(&real.st), st["tries"], "the wall's tries at {st:?}");
    }
    if st["pending"] == 0 {
        assert_eq!(run(m, &real.st), st["run"], "the no-work run at {st:?}");
    }
    faces(real, st);
    let act = decided(real, st, cfg);
    let got = verdict_at(&act, st, real.stopped);
    // DeadlineUnseen commits a late observation before recovery is compared. Probing
    // its held reply on a clone would consume that observation without updating the walk.
    if st["pending"] != 3 || !held(m, st) {
        assert_eq!(
            got,
            expected(m, st),
            "the real decider disagrees at {st:?}: {act:?}, path={:?}",
            real.path
        );
    }
    says_the_ack(&act, &got, st, real);
    if st["tasked"] == 1
        && st["other"] == 0
        && st["box_up"] == 0
        && !held(m, st)
        && !command_holds(st)
        && (st["pending"] == 0 || st["pending"] == 3)
    {
        assert_ne!(got, Verdict::Nothing, "a silent point at {st:?}");
    }
    got
}

/// What walks a state again: the model's key, then the real streak, same reply, wall's tries,
/// no-work run, wall's pass spent and the harness's stop ([`Real::stopped`]).
type WalkKey = (Vec<i64>, i64, i64, i64, i64, bool, bool);

/// Walk `m` against the real decider under `cfg` (`variant`: an unseen reply is the act's row left
/// unanswered); every verdict seen, and under `priority` how many decisions another question
/// priority made. A walk of a choice decides each state under every other priority as well: the
/// priority names the answer's words and rule, and never decides whether it goes. The model's own
/// laws at these states are the non-vacuity sweep's, at each configuration the model declares.
fn walk(m: &Model, cfg: &SupervisorConfig, variant: bool) -> BTreeMap<&'static str, usize> {
    let base = Instant::now() + Duration::from_secs(3600);
    let streak = cnst(m, "Streak");
    let take = cnst(m, "Take");
    let others: Vec<(PriorityWord, SupervisorConfig)> = PriorityWord::ALL
        .into_iter()
        .filter(|q| {
            SAID.with(std::cell::Cell::get) == CHOICE && *q != PRIORITY.with(std::cell::Cell::get)
        })
        .map(|q| {
            let other = SupervisorConfig {
                question_priority: q,
                ..cfg.clone()
            };
            (q, other)
        })
        .collect();

    // A state is walked again where the real streak, replies, wall's pass or the harness's stop
    // reached it otherwise.
    let mut seen: BTreeSet<WalkKey> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), start(m, base))]);
    let mut acts = BTreeMap::<&str, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        let short = i64::from(real.st.short_streak()).min(streak);
        let key = (
            crate::common::key(m, &st),
            short,
            crate::common::same(&real.st),
            tries(&real.st),
            run(m, &real.st),
            real.st.wall_pass().spent,
            real.stopped,
        );
        if !seen.insert(key) {
            continue;
        }
        let got = holds_at(m, &st, &real, cfg);
        let own = PRIORITY.with(std::cell::Cell::get);
        for (q, other) in &others {
            PRIORITY.with(|p| p.set(*q));
            let act = decided(&real, &st, other);
            assert_eq!(
                verdict_at(&act, &st, real.stopped),
                got,
                "{q:?} at {st:?}: {act:?}"
            );
            says_the_ack(&act, &got, &st, &real);
            *acts.entry("priority").or_default() += 1;
        }
        PRIORITY.with(|p| p.set(own));
        *acts
            .entry(match got {
                Verdict::Continue if st["claim"] == 1 => "defend",
                Verdict::Continue => "continue",
                Verdict::Recover => "recover",
                Verdict::Retry => "retry",
                Verdict::Submit => "submit",
                Verdict::Wait => "wait",
                Verdict::Escalate => "escalate",
                Verdict::Nothing => "nothing",
            })
            .or_default() += 1;
        let env = m.eval_env(&st);
        for action in &m.actions {
            let Some(next) = m.successors_in(action, &env, &st).into_iter().next() else {
                continue;
            };
            let mut r = real.clone();
            mirror(action.name, &st, &next, &mut r, take, variant);
            faces(&r, &next);
            queue.push_back((next, r));
        }
    }
    acts
}

/// `path` fired on `m` from its start and mirrored on the real side, which [`holds_at`] each state
/// as a walk does; the real decider's word at each.
fn walked_path(m: &Model, cfg: &SupervisorConfig, path: &[&str], variant: bool) -> Vec<Verdict> {
    let mut st = m.init_state();
    let mut real = start(m, Instant::now() + Duration::from_secs(3600));
    let mut said = vec![holds_at(m, &st, &real, cfg)];
    for action in path {
        let before = st.clone();
        assert!(m.fire(action, &mut st), "{action} at {before:?}");
        mirror(action, &before, &st, &mut real, cnst(m, "Take"), variant);
        said.push(holds_at(m, &st, &real, cfg));
    }
    said
}

/// The real decider at every state the model reaches, against what the model allows.
///
/// Three walks of the uncapped model and one of the capped: a report and a choice, an unseen reply
/// read as nothing or as the act's row left unanswered (a report only), the cap (a report only).
/// What they leave unwalked is implied or replayed here, by each question priority's own act:
///
/// - The priority enters the decider only as the prose answer's words and rule
///   (`turn_end::prose_words`, the prose arm's `prose_rule_id`), and the walk of a choice decides
///   each state under all three, the words `says_the_ack` and the rule [`verdict`] reads.
/// - It enters the state only through that rule, once the act is awaited. `acted` and `observe` read
///   it by exact rule ids no prose rule is among, so all three fall on one side, but for
///   `is_answer_rule`: through `is_continue_rule` in `observe`, into its streak, which the short path
///   below lives under each priority, and its `exchange`, false in every state here; and in
///   `in_limits_place`, which only a usage or model limit reaches and no state here does. `followed`
///   is no reader of the policy's.
/// - The cap is one `budgeted` over every act: a report's walk spends it, and the capped paths below
///   spend it on each priority's answer, one of them under a short streak.
/// - A choice's row left unanswered differs from a report's only where a decision names the choice,
///   which the unseen path below reaches at the deadline.
///
/// THE GAP: a defect that needs a choice and the row left unanswered, or a priority and the cap, at a
/// state off these paths; and a new reader of a prose rule off them that no decision shows.
/// NEGATIVE CONTROLS: `is_continue_rule` reading `rule == RULE_ANSWER` for `is_answer_rule` fails
/// `the streak at` under a long or short-term answer; the prose arm's rule as `RULE_ANSWER` fails
/// `an act the model has no word for`; no `budgeted` over the prose arm fails
/// `the real decider disagrees` where the capped path reaches its cap.
#[test]
fn tier1_the_real_decider_types_exactly_where_the_model_allows() {
    std::thread::scope(|scope| {
        for (said, variant) in [(REPORT, false), (REPORT, true), (CHOICE, false)] {
            scope.spawn(move || {
                SAID.with(|s| s.set(said));
                let acts = walk(&supervisor_turn_end_model(), &policy(), variant);
                for word in ["continue", "defend", "retry", "submit", "wait", "nothing"] {
                    assert!(
                        acts.get(word).copied().unwrap_or(0) > 0,
                        "{word} never occurred ({said}, {variant}): {acts:?}"
                    );
                }
                assert_eq!(acts.get("escalate"), None, "{said}, {variant}: {acts:?}");
                assert_eq!(
                    acts.contains_key("priority"),
                    said == CHOICE,
                    "{said}, {variant}: {acts:?}"
                );
            });
        }
        scope.spawn(|| {
            let capped =
                aterm_spec::interp::with_consts(&supervisor_turn_end_model(), &[("Budget", 2)]);
            let cfg = SupervisorConfig {
                continue_per_hour: u32::try_from(cnst(&capped, "Budget")).unwrap(),
                ..policy()
            };
            let acts = walk(&capped, &cfg, false);
            assert!(acts.get("escalate").copied().unwrap_or(0) > 0, "{acts:?}");
        });
        for q in PriorityWord::ALL {
            scope.spawn(move || {
                SAID.with(|s| s.set(CHOICE));
                PRIORITY.with(|p| p.set(q));
                let m = supervisor_turn_end_model();
                // Its answer counts into the short streak; the same again recovers.
                let said = walked_path(
                    &m,
                    &policy(),
                    &[
                        "HumanWork",
                        "Continue",
                        "WorkedShort",
                        "BackoffDue",
                        "Continue",
                        "WorkedShortSame",
                        "BackoffDue",
                    ],
                    false,
                );
                assert_eq!(
                    said,
                    [
                        Verdict::Nothing,
                        Verdict::Continue,
                        Verdict::Nothing,
                        Verdict::Wait,
                        Verdict::Continue,
                        Verdict::Nothing,
                        Verdict::Wait,
                        Verdict::Recover,
                    ],
                    "{q:?}"
                );
                // Its answer, unseen and its row left unanswered, is judged at the deadline.
                let said = walked_path(
                    &m,
                    &policy(),
                    &[
                        "HumanWork",
                        "Continue",
                        "ReplyUnseen",
                        "TimePassesUnseen",
                        "TimePassesUnseen",
                        "DeadlineUnseen",
                        "BackoffDue",
                    ],
                    true,
                );
                assert_eq!(said.last(), Some(&Verdict::Continue), "{q:?}: {said:?}");
                // Its answer spends the cap, and at the cap it goes no more.
                let capped = aterm_spec::interp::with_consts(&m, &[("Budget", 2)]);
                let cfg = SupervisorConfig {
                    continue_per_hour: 2,
                    ..policy()
                };
                let said = walked_path(
                    &capped,
                    &cfg,
                    &[
                        "HumanWork",
                        "Continue",
                        "WorkedLong",
                        "Continue",
                        "WorkedLong",
                    ],
                    false,
                );
                assert_eq!(
                    said[3],
                    Verdict::Continue,
                    "{q:?}: short of the cap: {said:?}"
                );
                assert_eq!(said[5], Verdict::Escalate, "{q:?}: at the cap: {said:?}");
                // The cap stands over an answer whatever the streak says.
                let said = walked_path(
                    &capped,
                    &cfg,
                    &[
                        "HumanWork",
                        "Continue",
                        "WorkedShort",
                        "BackoffDue",
                        "Continue",
                        "WorkedShort",
                        "BackoffDue",
                    ],
                    false,
                );
                assert_eq!(said.last(), Some(&Verdict::Escalate), "{q:?}: {said:?}");
            });
        }
    });
    let m = supervisor_turn_end_model();
    assert_eq!(
        cnst(&m, "Futile"),
        i64::try_from(FUTILE_AFTER_TRIES).unwrap(),
        "the model's bound is the real one"
    );
    assert_eq!(
        cnst(&m, "NoWork"),
        i64::try_from(NO_WORK_ACTS).unwrap(),
        "the model's run is the real one"
    );

    let base = Instant::now() + Duration::from_secs(3600);
    let mut real = TurnEndState::default();
    real.observe(
        &reading((&m.init_state(), 0), Some(LONG), false, false),
        base,
    );
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
                    &reading((&held, 0), None, false, false),
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

    let record = format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n");
    assert!(!transcript_tasked(&record, &ours()));
    assert!(transcript_tasked(&record, &[]));

    let st = asked(m.init_state());
    let busy = |host: bool| {
        let mut real = TurnEndState::new(timing());
        let mut now = base;
        real.observe(&reading((&st, 0), Some(LONG), false, false), now);
        // Two turns, each its own reply.
        for earlier in [1, 2] {
            now += Duration::from_secs(30);
            if host {
                real.host_typed(now, HostTurn::Notice);
            }
            real.observe(&reading((&st, earlier), Some(SHORT), false, false), now);
        }
        verdict(
            &decide_turn_end(
                &real,
                &reading((&st, 2), None, false, false),
                &SupervisorConfig::default(),
                now,
            ),
            &st,
        )
    };
    assert_eq!(busy(true), Verdict::Continue);
    assert_eq!(busy(false), Verdict::Wait);

    let again = [
        "HumanWork",
        "Continue",
        "WorkedShort",
        "BackoffDue",
        "Continue",
        "WorkedShortSame",
        "BackoffDue",
    ];
    let (st, real) = replay(&m, &again, base);
    let same = aterm_spec::interp::with_buggy(&m, 3);
    let mut b = st.clone();
    assert!(
        !m.action_enabled("Continue", &st) && same.fire("Continue", &mut b),
        "{st:?}"
    );
    assert!(!same.check_invariant("RepeatedRepliesUseRecovery", &b));
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Recover
    );
    let mut other = again;
    other[5] = "WorkedShort";
    let (st, real) = replay(&m, &other, base);
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Continue
    );

    // A wall's retry the worker answers with real work ends the short streak.
    let retried = [
        "HumanWork",
        "Continue",
        "WorkedShort",
        "BackoffDue",
        "WallAppears",
        "WallDue",
        "Retry",
        "RetryTaken",
    ];
    let kept = aterm_spec::interp::with_buggy(&m, 4);
    let mut b = kept.init_state();
    for action in retried {
        assert!(kept.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(
        !kept.check_invariant("NeverBackOffAfterRealWork", &b),
        "{b:?}"
    );
    assert!(!kept.action_enabled("Continue", &b), "{b:?}");
    let (st, real) = replay(&m, &retried, base);
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Continue
    );

    // A wall first met as a continuation's answer keeps all its tries: that answer is none of them.
    let mut met = vec!["HumanWork", "Continue", "WorkedHitsTheWall"];
    for _ in 1..FUTILE_AFTER_TRIES {
        met.extend(["WallDue", "Retry", "RetryHitsTheWall"]);
    }
    met.push("WallDue");
    let early = aterm_spec::interp::with_buggy(&m, 6);
    let mut b = early.init_state();
    for action in &met[..3] {
        assert!(early.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(
        !early.check_invariant("AWallsTriesAreItsOwnRetries", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &met, base);
    assert!(m.action_enabled("Retry", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Retry,
        "the last of its tries"
    );

    // A wall that answered its tries the same gets no more of them.
    let mut tried = vec!["HumanWork", "WallAppears"];
    for _ in 0..FUTILE_AFTER_TRIES {
        tried.extend(["WallDue", "Retry", "RetryHitsTheWall"]);
    }
    tried.push("WallDue");
    let more = aterm_spec::interp::with_buggy(&m, 5);
    let mut b = more.init_state();
    for action in &tried {
        assert!(more.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(more.fire("Retry", &mut b), "{b:?}");
    assert!(
        !more.check_invariant("AWallGetsItsTriesAndNoMore", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &tried, base);
    assert!(!m.action_enabled("Retry", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing
    );
    let mut fewer = tried.clone();
    fewer.truncate(fewer.len() - 3);
    let (st, real) = replay(&m, &fewer, base);
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Retry,
        "the control: a try short of the bound goes"
    );

    // NO_WORK_ACTS no-work answers switch from ordinary continuation to recovery.
    let mut rotated = vec!["HumanWork"];
    for _ in 0..NO_WORK_ACTS {
        rotated.extend(["Continue", "WorkedShort", "BackoffDue"]);
    }
    let past = aterm_spec::interp::with_buggy(&m, 7);
    let mut b = past.init_state();
    for action in &rotated {
        assert!(past.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(past.fire("Continue", &mut b), "{b:?}");
    assert!(
        !past.check_invariant("RoutineContinuationRespectsNoWorkBudget", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &rotated, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Recover
    );
    rotated.truncate(rotated.len() - 3);
    let (st, real) = replay(&m, &rotated, base);
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Continue,
        "the control: an answer short of the run goes"
    );

    // The program's own no-work turn counts as an act's answer: said again it holds, and it counts into the run.
    let own = [
        "HumanWork",
        "Continue",
        "WorkedShort",
        "BackoffDue",
        "ProgramTurnSame",
    ];
    let (st, real) = replay(&m, &own, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(verdict(&decided(&real, &st, &policy()), &st), Verdict::Wait);
    let mut lifting = vec!["HumanWork", "Continue", "WorkedShort", "BackoffDue"];
    for _ in 0..NO_WORK_ACTS {
        lifting.extend(["ProgramTurnSame", "BackoffDue"]);
    }
    let lifted = aterm_spec::interp::with_buggy(&m, 8);
    let mut b = lifted.init_state();
    for action in &lifting {
        assert!(lifted.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(lifted.fire("Continue", &mut b), "{b:?}");
    assert!(
        !lifted.check_invariant("RoutineContinuationRespectsNoWorkBudget", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &lifting, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Recover,
        "the real decider changes approach where the lifting model repeats"
    );
    // Its own turns no point saw busy, each in new words, count all the same: the run holds them.
    let mut unseen = vec!["HumanWork", "Continue", "WorkedShort", "BackoffDue"];
    unseen.extend(vec!["ProgramTurnUnseen"; NO_WORK_ACTS - 1]);
    let lifted = aterm_spec::interp::with_buggy(&m, 8);
    let mut b = lifted.init_state();
    for action in &unseen {
        assert!(lifted.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(lifted.fire("Continue", &mut b), "{b:?}");
    assert!(
        !lifted.check_invariant("RoutineContinuationRespectsNoWorkBudget", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &unseen, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Recover,
        "the real decider recovers the unseen turns' run"
    );
    unseen.pop();
    let (st, real) = replay(&m, &unseen, base);
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Continue,
        "the control: one unseen turn short of the run, the act goes"
    );
    let mut new_words = own;
    new_words[4] = "ProgramTurn";
    let (st, real) = replay(&m, &new_words, base);
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Wait,
        "the control: new words lift the same-reply hold, the back-off paces the act"
    );

    // A long answer that ran no tool, in new words, is kept and the next act goes at once; said again, it is no work and holds.
    let quiet = ["HumanWork", "Continue", "WorkedLongQuiet"];
    let (st, real) = replay(&m, &quiet, base);
    assert_eq!(crate::common::same(&real.st), 1, "kept");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Continue
    );
    let again = [
        "HumanWork",
        "Continue",
        "WorkedLongQuiet",
        "Continue",
        "WorkedLongQuietSame",
    ];
    let (st, real) = replay(&m, &again, base);
    assert_eq!(verdict(&decided(&real, &st, &policy()), &st), Verdict::Wait);

    // Keys that submit no turn do not reset recovery. After the typing grace expires,
    // an empty composer can recover; a draft remains untouched.
    let same = [
        "HumanWork",
        "Continue",
        "WorkedShort",
        "BackoffDue",
        "Continue",
        "WorkedShortSame",
        "BackoffDue",
    ];
    let mut run = vec!["HumanWork"];
    for _ in 0..NO_WORK_ACTS {
        run.extend(["Continue", "WorkedShort", "BackoffDue"]);
    }
    for path in [&same[..], &run[..]] {
        for (keyed, entered) in [("PersonTypes", 0), ("PersonTypes", 1), ("DraftLeft", 1)] {
            let (mut st, mut real) = replay(&m, path, base);
            let drawn = |real: &Real, st: &State, mark, submits, person| TurnEndReading {
                screen_mark: Some(mark),
                submits: Some(submits),
                person,
                ..reading((st, real.earlier), None, real.unanswered, real.taskless())
            };
            let held = drawn(&real, &st, 1, 5, None);
            real.st.observe(&held, real.now);
            assert_eq!(
                verdict(&decide_turn_end(&real.st, &held, &policy(), real.now), &st),
                Verdict::Recover,
                "recovery after {path:?}"
            );
            assert!(m.fire(keyed, &mut st), "{st:?}");
            assert!(m.fire("GraceEnds", &mut st), "{st:?}");
            assert_eq!(
                expected(&m, &st),
                if keyed == "DraftLeft" {
                    Verdict::Submit
                } else {
                    Verdict::Recover
                },
                "the abstract draft has no human provenance: {st:?}"
            );
            let ago = Duration::from_secs(300);
            real.now += 2 * ago;
            let keys = drawn(&real, &st, 2, 5 + entered, Some(ago));
            assert_eq!(
                keys.composer,
                if keyed == "DraftLeft" {
                    Composer::Typed
                } else {
                    Composer::Empty
                },
                "{st:?}"
            );
            real.st.observe(&keys, real.now);
            assert_eq!(
                verdict(&decide_turn_end(&real.st, &keys, &policy(), real.now), &st),
                if keyed == "DraftLeft" {
                    Verdict::Nothing
                } else {
                    Verdict::Recover
                },
                "a person's {keyed} with {entered} more submits that submitted no turn lifts no hold \
                 after {path:?}"
            );
        }
    }
}

/// The words this bind hands the decider read as the refused request the model's wall stands for.
#[test]
fn the_walls_words_read_as_a_refused_request() {
    assert_eq!(aterm_phase::classify_wall(REFUSED_MESSAGE), Some(REFUSED));
}

/// Negative control for §14.17.5.8, a person's command left standing: the old rule (`Buggy = 15`,
/// the draft step's Enter on any draft past the person's grace) sends it and breaks
/// `NeverSendsAPersonsCommand` on its path, where the real decider holds (`Nothing`) as the model
/// without it does; a prompt draft on the same path is sent by both.
#[test]
fn the_model_that_sends_a_persons_command_disagrees_with_the_real_decider() {
    let base = Instant::now() + Duration::from_secs(3600);
    let m = supervisor_turn_end_model();
    let cfg = policy();
    for (left, sent) in [("CommandLeft", false), ("DraftLeft", true)] {
        let path = ["HumanWork", left, "GraceEnds"];
        let (st, real) = replay(&m, &path, base);
        let act = verdict(&decided(&real, &st, &cfg), &st);
        assert_eq!(act, expected(&m, &st), "{left}: {st:?}");
        assert_eq!(act == Verdict::Submit, sent, "{left}: {st:?}");
        assert_eq!(m.action_enabled("SubmitDraft", &st), sent, "{left}: {st:?}");
        let old = aterm_spec::interp::with_buggy(&m, 15);
        let (mut st, _) = replay(&old, &path, base);
        assert!(old.fire("SubmitDraft", &mut st), "{left}: {st:?}");
        assert_eq!(
            old.check_invariant("NeverSendsAPersonsCommand", &st),
            sent,
            "{left}: {st:?}"
        );
    }
}

/// Negative control for §14.17.5's blocked reply: the old rule (`Buggy = 9`, an answer that names
/// a need and ran tools drops the reply) disagrees with the real decider, which keeps it so the same
/// answer again holds.
#[test]
fn the_model_that_drops_a_needs_answer_disagrees_with_the_real_decider() {
    let base = Instant::now() + Duration::from_secs(3600);
    let path = ["HumanWork", "Continue", "WorkedNeeds"];
    let m = supervisor_turn_end_model();
    let (st, real) = replay(&m, &path, base);
    assert_eq!(crate::common::same(&real.st), st["same"], "{st:?}");
    assert_eq!(st["same"], 1, "kept: {st:?}");
    let old = aterm_spec::interp::with_buggy(&m, 9);
    let (st, real) = replay(&old, &path, base);
    assert_ne!(crate::common::same(&real.st), st["same"], "{st:?}");
}

/// Negative controls for §14.17.5.7, the short-turn streak counting the agent's own short answers
/// and a wall's cut acts past its one pass per rung: the old rules, `Buggy = 10` (every act a wall
/// cut counted as a short turn) and `Buggy = 11` (a person's own short turn counted), each break
/// `NoPassedCutPersonsTurnOrHarnessStopCounts` on its path, and `Buggy = 12` (no cut act counted,
/// the pass never spent) breaks `AWallPassesOncePerRung` on a wall that cuts act after act, each
/// try answered short; each disagrees there with the real decider's streak, which the model
/// without them matches.
#[test]
fn the_models_that_count_a_cut_act_or_the_persons_turn_disagree_with_the_real_decider() {
    let base = Instant::now() + Duration::from_secs(3600);
    let m = supervisor_turn_end_model();
    for (buggy, law, path) in [
        (
            10,
            "NoPassedCutPersonsTurnOrHarnessStopCounts",
            &["HumanWork", "Continue", "WorkedHitsTheWall"][..],
        ),
        (
            11,
            "NoPassedCutPersonsTurnOrHarnessStopCounts",
            &["HumanWork", "Continue", "WorkedShort", "PersonTurnShort"][..],
        ),
        (
            12,
            "AWallPassesOncePerRung",
            &[
                "HumanWork",
                "Continue",
                "WorkedHitsTheWall",
                "WallDue",
                "Retry",
                "RetryAnsweredShort",
                "Continue",
                "WorkedHitsTheWall",
            ][..],
        ),
    ] {
        let (st, real) = replay(&m, path, base);
        assert_eq!(
            i64::from(real.st.short_streak()),
            st["short"],
            "{buggy}: {st:?}"
        );
        assert!(m.check_invariant(law, &st), "{buggy}: {st:?}");
        let old = aterm_spec::interp::with_buggy(&m, buggy);
        let (st, real) = replay(&old, path, base);
        assert!(!old.check_invariant(law, &st), "{buggy}: {st:?}");
        assert_ne!(
            i64::from(real.st.short_streak()),
            st["short"],
            "{buggy}: {st:?}"
        );
    }
}

/// Negative controls for the turn the harness's own stop ended (§14.17.5.7, the `harness_cut` of
/// `TurnEndState::observe`): the restart whose cut call a resume restores ended the act's turn, not
/// the agent, so it adds nothing to the short streak and its back-off stands on the rung the
/// agent's own short answers reached. The old rule, `Buggy = 13` (the stopped turn counted as one
/// more short answer, the rule before 20d80352d1), breaks
/// `NoPassedCutPersonsTurnOrHarnessStopCounts` on its path, and `Buggy = 14` (real work in the
/// stopped turn left uncounted, the streak's back-off re-armed past it) breaks
/// `NeverBackOffAfterRealWork`; each disagrees there with the real decider's streak, and
/// `Buggy = 14` with its verdict, which the model without them matches. CONTROL: the same
/// `Interrupted` row with no stop of the harness's marked is a person's Esc, and its short turn
/// counts: the real side keeps the stopped turn off the streak by the harness's own mark alone.
#[test]
fn the_models_that_count_the_turn_the_harness_stopped_disagree_with_the_real_decider() {
    let base = Instant::now() + Duration::from_secs(3600);
    let m = supervisor_turn_end_model();
    let lead = [
        "HumanWork",
        "Continue",
        "WorkedShort",
        "BackoffDue",
        "Continue",
    ];
    for (buggy, law, stop) in [
        (
            13,
            "NoPassedCutPersonsTurnOrHarnessStopCounts",
            "HarnessStopsTurn",
        ),
        (14, "NeverBackOffAfterRealWork", "HarnessStopsTurnLong"),
    ] {
        let path: Vec<&str> = lead.iter().copied().chain([stop]).collect();
        let (st, real) = replay(&m, &path, base);
        assert_eq!(
            i64::from(real.st.short_streak()),
            st["short"],
            "{stop}: {st:?}"
        );
        assert!(m.check_invariant(law, &st), "{stop}: {st:?}");
        assert_eq!(
            verdict_at(&decided(&real, &st, &policy()), &st, real.stopped),
            expected(&m, &st),
            "{stop}: {st:?}"
        );
        let old = aterm_spec::interp::with_buggy(&m, buggy);
        let (st, real) = replay(&old, &path, base);
        assert!(!old.check_invariant(law, &st), "{buggy}: {st:?}");
        assert_ne!(
            i64::from(real.st.short_streak()),
            st["short"],
            "{buggy}: {st:?}"
        );
        if buggy == 14 {
            assert_ne!(
                verdict_at(&decided(&real, &st, &policy()), &st, real.stopped),
                expected(&old, &st),
                "{buggy}: the real decider goes on at once after the work: {st:?}"
            );
        }
    }
    let (mut st, mut real) = replay(&m, &lead, base);
    assert!(m.fire("HarnessStopsTurn", &mut st), "{st:?}");
    real.earlier += 1;
    let unmarked = stood(
        reading((&st, real.earlier), Some(SHORT), false, real.taskless()),
        true,
    );
    real.st.observe(&unmarked, real.now);
    assert_eq!(
        i64::from(real.st.short_streak()),
        st["short"] + 1,
        "CONTROL: an unmarked Interrupted row's short turn counts: {st:?}"
    );
}
