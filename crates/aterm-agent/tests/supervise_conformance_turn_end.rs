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
    Composer, DONE_CHECK, HostTurn, NO_WORK_ACTS, RULE_API_RETRY, RULE_CONTINUE, RULE_DONE_CHECK,
    TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming, decide_turn_end,
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
        opens_cut: false,
        turn_said: Some(crate::common::earlier(earlier)),
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
        rules: None,
        pending_input,
        interrupted: false,
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
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
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
        fact: None,
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
    // The same reply again, or the wall's tries spent at it: no act, and no wait for one, past a
    // person's grace.
    if held(m, st) && st["person"] == 0 {
        return Verdict::Nothing;
    }
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
            | "RetryHitsTheWall"
            | "HumanWork"
            | "HarnessTurn"
            | "HarnessTurnLong"
            | "WallAppears"
            | "ProgramTurn"
            | "ProgramTurnUnseen"
            | "ProgramTurnSame"
    ) {
        real.unanswered = false;
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
            | "RetryHitsTheWall"
            | "HumanWork"
            | "HarnessTurn"
            | "HarnessTurnLong"
            | "WallAppears"
            | "ProgramTurn"
            | "ProgramTurnUnseen"
    ) {
        real.earlier += 1;
    }
    match action {
        "HumanWork" => real.write(&format!("{PERSON}\n{ANSWER}\n")),
        "HarnessTurn" | "HarnessTurnLong" => {
            real.write(&format!("{HARNESS}\n{ANSWER}\n{SUPERVISOR}\n{ANSWER}\n"));
        }
        _ => {}
    }
    let (unanswered, earlier) = (real.unanswered, real.earlier);
    let taskless = real.taskless();
    let seen = |st: &State, worked| reading((st, earlier), worked, unanswered, taskless);
    match action {
        "BoxAppears" | "BoxLeaves" | "PersonTypes" | "GraceEnds" | "DraftLeft" => {}
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
        "OtherTurnRuns" => real.st.took(),
        "HarnessTurn" => {
            real.st.host_typed(real.now, HostTurn::Notice);
            real.st.observe(&seen(after, Some(SHORT)), real.now);
        }
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
        // The program's own turn is a short one no act awaited.
        "WorkedShort" | "WorkedShortSame" | "WorkedHitsTheWall" | "RetryHitsTheWall"
        | "ProgramTurn" | "ProgramTurnSame" => {
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
        "WorkedNeeds" => {
            let r = TurnEndReading {
                said_tail: Some(format!("{}\n{NEED}", SAID.with(std::cell::Cell::get))),
                ..seen(after, Some(LONG))
            };
            real.st.observe(&r, real.now);
        }
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

/// What the real decider says at `st`, the point read again as a loop reads
/// it before it decides.
fn decided(real: &Real, st: &State, cfg: &SupervisorConfig) -> TurnEndAction {
    let mut at = real.st.clone();
    let here = reading((st, real.earlier), None, real.unanswered, real.taskless());
    at.observe(&here, real.now);
    decide_turn_end(&at, &here, cfg, real.now)
}

/// Walk `m` against the real decider under `cfg` (`variant`: an unseen
/// reply is the act's row left unanswered); every verdict seen.
fn walk(m: &Model, cfg: &SupervisorConfig, variant: bool) -> BTreeMap<&'static str, usize> {
    let base = Instant::now() + Duration::from_secs(3600);
    let streak = cnst(m, "Streak");
    let take = cnst(m, "Take");

    // A state is walked again where the real streak or replies reached it otherwise.
    let mut seen: BTreeSet<(Vec<i64>, i64, i64, i64, i64)> = BTreeSet::new();
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
        );
        if !seen.insert(key) {
            continue;
        }
        for inv in &m.invariants {
            assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
        }
        let short = real.st.short_streak();
        assert_eq!(
            i64::from(short).min(streak),
            st["short"],
            "the streak at {st:?}"
        );
        assert_eq!(real.st.short_wait(), ladder(&real.st, short), "{st:?}");
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
        faces(&real, &st);
        let act = decided(&real, &st, cfg);
        let got = verdict(&act, &st);
        assert_eq!(
            got,
            expected(m, &st),
            "the real decider disagrees at {st:?}"
        );
        if let TurnEndAction::Type { text, .. } = &act
            && st["need"] == 1
            && got == Verdict::Continue
            && real.st.needs_ack_owed()
        {
            assert!(text.contains(NEEDS_ACK), "the need's ack at {st:?}: {text}");
        }
        if st["tasked"] == 1
            && st["other"] == 0
            && st["box_up"] == 0
            && !held(m, &st)
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
            faces(&r, &next);
            queue.push_back((next, r));
        }
    }
    acts
}

#[test]
fn tier1_the_real_decider_types_exactly_where_the_model_allows() {
    std::thread::scope(|scope| {
        for (said, priority) in [
            (REPORT, QuestionPriority::Recommended),
            (CHOICE, QuestionPriority::Recommended),
            (CHOICE, QuestionPriority::LongTerm),
            (CHOICE, QuestionPriority::ShortTerm),
        ] {
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
    assert!(!same.check_invariant("ANoWorkReplyGetsOneMoreActAtMost", &b));
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing
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

    // NO_WORK_ACTS no-work answers in a row, each in its own words, get no more acts.
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
        !past.check_invariant("ANoWorkRunGetsNoMoreActs", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &rotated, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing
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
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing
    );
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
        !lifted.check_invariant("ANoWorkRunGetsNoMoreActs", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &lifting, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing,
        "the real decider holds where the lifting model types"
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
        !lifted.check_invariant("ANoWorkRunGetsNoMoreActs", &b),
        "{b:?}"
    );
    let (st, real) = replay(&m, &unseen, base);
    assert!(!m.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing,
        "the real decider holds the unseen turns' run"
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
    assert_eq!(
        verdict(&decided(&real, &st, &policy()), &st),
        Verdict::Nothing
    );

    // A person's keys that submit no turn lift neither hold, though the letters above the composer changed (a scroll, a pane resized, a draft that grows the composer): no new prompt, the session's submits as they were, or one up by an Enter over the emptied composer or one that left their draft standing (a `\`+Enter newline, a menu's completion).
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
                Verdict::Nothing,
                "held after {path:?}"
            );
            assert!(m.fire(keyed, &mut st), "{st:?}");
            assert!(m.fire("GraceEnds", &mut st), "{st:?}");
            assert_eq!(expected(&m, &st), Verdict::Nothing, "{st:?}");
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
                Verdict::Nothing,
                "a person's {keyed} with {entered} more submits that submitted no turn lifts no hold \
                 after {path:?}"
            );
        }
    }
}
