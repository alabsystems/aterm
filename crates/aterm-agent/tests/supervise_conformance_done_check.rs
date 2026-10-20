// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorDoneCheck` (aterm-models-harness
//! `supervisor_done_check_model`): the real turn-end decider
//! (`decide_turn_end`, with its `TurnEndState`) driven along every reachable
//! state of the model.
//!
//! Someone else's turn is a point observed with real work (its last words a
//! done report, progress, a FINISHED report that leaves nothing, one that
//! names open work, or one over a repository the host's probe finds work no
//! remote has in, or a `529 Overloaded` wall); the policy's `Continue`,
//! `Check`, `Challenge`, `Answer` and `WallAct` are the decider's own acts on
//! the real ladder's clock, recorded with `acted`; a yield is the next point
//! observed — a short one where it asks — after a live turn read with a
//! person's (or the act's) message on it (`live_turn(Some(false))`). `Tasked`
//! is the loop's read of the task record (`set_managed`), `SeedDone` its read
//! of one that says `done` (`seed_task_done`), `BackoffDue` the clock moved
//! past the back-off the decider waits on. A turn that ends with the token
//! over a shell it left running is the busy screen the loop reads
//! (`finished_over_background`), the shell's end the idle screen it leaves,
//! and the agent's own wake at that end a live turn its background notice
//! opened (`live_turn(Some(true))`) and the idle screen it leaves. At every
//! state the decider types the continuation, the check, the challenge, the
//! answer and the wall's act exactly where the model enables them, waits
//! exactly where the model's `BackoffDue` is the only way on, and does
//! nothing on a done task; the record's done is taken on exactly where
//! `SeedDone` is enabled; its done reports, its short streak, its worker
//! flag and its owed challenge are the model's (`done`, `short`, `managed`,
//! `open`). Negative controls: the `Buggy =
//! 1` model continues at a done report where the real decider checks, does
//! nothing at a wall after the task was done where the real decider retries,
//! takes a recorded done on at a wall where the real decider refuses it and
//! retries, holds a worker's check for the back-off where the real decider
//! types it, answers a worker at once past its bound where the real decider
//! waits, leaves the task open over a running shell where the real decider
//! ends it, opens it again at the agent's own wake where the real decider
//! keeps it done, ends the task on an open FINISHED where the real decider
//! challenges, and ends it at the FINISHED that answers the challenge with
//! work still left (the once-a-job cap before the owner's rule of
//! 2026-10-19) where the real decider challenges again, at the second
//! token and the third.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::repo_work::RepoWork;
use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, DONE_CHECK, RULE_API_RETRY, RULE_CONTINUE, RULE_DONE_CHECK, RULE_OPEN_ITEMS,
    RULE_UNCOMMITTED, TurnEndAction, TurnEndReading, TurnEndState, WaitKind, decide_turn_end,
    is_answer_rule, open_items_text, uncommitted_text,
};
use aterm_models_harness::supervisor_done_check_model;
use aterm_phase::wall::WallKind;
use aterm_spec::derive::Model;

type State = BTreeMap<&'static str, i64>;

/// Real work, long enough that no turn is short for its length.
const LONG: Duration = Duration::from_secs(4 * 60);
/// A turn too short to be work: a yield that asks is one.
const SHORT: Duration = Duration::from_secs(30);
/// A turn's last words: progress, the vendor's done report, the reply the
/// check asks for, and a question.
const PROGRESS: &str = "Stage 2 landed; the parser is next.";
const REPORT: &str = "Everything is already done; nothing left to do.";
const REPLY: &str = "DONE";
const QUESTION: &str = "Does the parser have to keep the old syntax?";
/// The worker's own FINISHED, over a report that leaves nothing, over one
/// whose words still name open work, and over a shell the turn left running.
const FINISHED: &str = "Built, tested and landed the parser. Nothing left.\nATERM-TASK-DONE";
const FINISHED_OPEN: &str =
    "Landed the parser.\nStill open: the lexer's e2e suite (not run).\nATERM-TASK-DONE";
const FINISHED_OVER_SHELL: &str = "Fixed the parser; the dev server runs.\nATERM-TASK-DONE";
/// What the agent says when it wakes itself at the shell's end.
const WOKE: &str = "The dev server stopped; the parser change still stands.";
/// The model's short streak: 4 stands for any past the worker's bound.
const SHORT_CAP: u32 = 4;

/// What the host's probe finds in the session's repository at a
/// `*DirtyFinished` point: a file not committed, a commit not pushed.
fn finding() -> RepoWork {
    RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 1,
        untracked: 0,
        stashes: 0,
        unpushed: 1,
        paths: vec!["src/lexer.rs".to_string()],
    }
}

/// An authoritative idle point whose turn worked `worked`, ending on `said`
/// — or on a `529 Overloaded` wall; `over_shell`, the BUSY screen of a turn
/// that ended while a shell it left runs on (no live turn drawn); `dirty`,
/// the host's probe finds the session's repository holds work no remote has
/// ([`finding`]).
fn point(
    said: &str,
    walled: bool,
    over_shell: bool,
    worked: Option<Duration>,
    dirty: bool,
) -> TurnEndReading {
    TurnEndReading {
        phase: if over_shell { Phase::Busy } else { Phase::Idle },
        authoritative: true,
        survey: false,
        wall: walled.then_some(WallKind::Overloaded),
        wall_message: if walled {
            "API Error: 529 Overloaded.".to_string()
        } else {
            String::new()
        },
        reset_at: None,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some(said.to_string()),
        worked,
        rules: None,
        pending_input: false,
        background: over_shell,
        interrupted: false,
        restartable: true,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        finished_at_check: false,
        check_record: None,
        left_record: false,
        left_work: None,
        person: None,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        notice_elsewhere: false,
        reset_clock: None,
        reach: Default::default(),
        program: aterm_phase::Program::Claude,
        account: None,
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
        login_label: None,
        login_superseded: false,
        login_notice: None,
        login_screen: None,
        login_interrupted: false,
        login_method: None,
        login_shared: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        record_turn: None,
        credits: None,
        repo: dirty.then(finding),
    }
}

/// What the real decider says at the point `r`, as a model word.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Continue,
    Check,
    Challenge,
    Answer,
    WallAct,
    Wait,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Verdict::Continue,
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_DONE_CHECK => {
            assert_eq!(text, DONE_CHECK, "the check's words");
            Verdict::Check
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_OPEN_ITEMS => {
            assert_eq!(
                text,
                &open_items_text(&["still open"], None, false),
                "its words"
            );
            Verdict::Challenge
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_UNCOMMITTED => {
            assert_eq!(
                text,
                &uncommitted_text(&finding(), None, false),
                "its words"
            );
            Verdict::Challenge
        }
        TurnEndAction::Type { rule_id, .. } if is_answer_rule(rule_id) => Verdict::Answer,
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_RETRY => Verdict::WallAct,
        TurnEndAction::WaitUntil {
            why,
            wait: WaitKind::BackOff { .. },
            ..
        } if why.contains("back-off") => Verdict::Wait,
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

fn expected(m: &Model, st: &State) -> Verdict {
    for (action, word) in [
        ("Continue", Verdict::Continue),
        ("Check", Verdict::Check),
        ("Challenge", Verdict::Challenge),
        ("Answer", Verdict::Answer),
        ("WallAct", Verdict::WallAct),
    ] {
        if m.action_enabled(action, st) {
            return word;
        }
    }
    if m.action_enabled("BackoffDue", st) {
        Verdict::Wait
    } else {
        Verdict::Nothing
    }
}

/// The real side: the decider's state, the loop's clock, and the last words
/// of the point it stands at (whether it shows the wall, whether it is the
/// busy screen over a running shell, and whether the probe finds work in the
/// repository there).
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    said: &'static str,
    walled: bool,
    over_shell: bool,
    dirty: bool,
}

impl Real {
    /// The decision at the point now, the wall's own wait run through (its
    /// ladder's clock, which the model does not count) — never the short
    /// back-off's, which the model's `BackoffDue` moves.
    fn decide(&mut self) -> TurnEndAction {
        let r = point(self.said, self.walled, self.over_shell, None, self.dirty);
        self.st.observe(&r, self.now);
        let cfg = SupervisorConfig::default();
        for _ in 0..4 {
            match decide_turn_end(&self.st, &r, &cfg, self.now) {
                TurnEndAction::WaitUntil {
                    until,
                    wait: WaitKind::Retry,
                    ..
                } => self.now = until,
                a => return a,
            }
        }
        panic!("the decider waits for ever at {:?}", self.said)
    }

    /// A turn — a live one, read busy with a message on it — that ended on
    /// `said` after `worked`, or on the wall; the repository clean.
    fn turn(&mut self, said: &'static str, walled: bool, worked: Duration) {
        self.turn_over(said, walled, worked, false);
    }

    /// …or `dirty`: the probe finds work no remote has at its end.
    fn turn_over(&mut self, said: &'static str, walled: bool, worked: Duration, dirty: bool) {
        assert!(
            !self.st.live_turn(Some(false)),
            "a message's turn is no wake"
        );
        self.now += worked;
        self.said = said;
        self.walled = walled;
        self.over_shell = false;
        self.dirty = dirty;
        self.st
            .observe(&point(said, walled, false, Some(worked), dirty), self.now);
    }

    /// A turn that ended with the token while a shell it left runs on: the
    /// busy screen the loop reads.
    fn turn_over_shell(&mut self) {
        assert!(!self.st.live_turn(Some(false)));
        self.now += LONG;
        self.said = FINISHED_OVER_SHELL;
        self.walled = false;
        self.over_shell = true;
        self.dirty = false;
        assert!(
            self.st.finished_over_background(
                &point(FINISHED_OVER_SHELL, false, true, None, false),
                self.now
            ),
            "the token over the shell ends the task"
        );
    }

    /// The shell ends with no live turn since: the idle screen it leaves,
    /// the shell's run read as the busy reads measured it — the point the
    /// token made, never a new one (read as one, it was someone else's turn
    /// of the shell's length, and its done published again).
    fn shell_ends(&mut self) {
        let made = self.st.point_number();
        self.now += LONG;
        self.over_shell = false;
        self.st
            .observe(&point(self.said, false, false, Some(LONG), false), self.now);
        assert_eq!(self.st.point_number(), made, "the same point");
    }

    /// The agent wakes itself at the shell's end — its live turn opened by
    /// the background notice, read again with the notice scrolled away —
    /// and ends it at an idle screen: the point the token made still.
    fn agent_wakes(&mut self) {
        let made = self.st.point_number();
        assert!(self.st.live_turn(Some(true)), "the wake, said once");
        assert!(!self.st.live_turn(None), "the same wake, said already");
        assert!(self.st.done_over_background(), "the point stands");
        self.now += LONG;
        self.said = WOKE;
        self.over_shell = false;
        self.st
            .observe(&point(WOKE, false, false, Some(LONG), false), self.now);
        assert_eq!(self.st.point_number(), made, "the same point");
    }
}

fn mirror(action: &str, before: &State, real: &mut Real) {
    match action {
        "Tasked" => real.st.set_managed(true),
        "OtherTurnDone" => real.turn(REPORT, false, LONG),
        "OtherTurnWork" | "YieldWork" => real.turn(PROGRESS, false, LONG),
        "OtherTurnWall" | "YieldWall" => real.turn(PROGRESS, true, LONG),
        "OtherTurnFinished" | "YieldFinished" => real.turn(FINISHED, false, LONG),
        "OtherTurnOpenFinished" | "YieldOpenFinished" => real.turn(FINISHED_OPEN, false, LONG),
        "OtherTurnDirtyFinished" | "YieldDirtyFinished" => {
            real.turn_over(FINISHED, false, LONG, true);
        }
        "OtherTurnFinishedBg" | "YieldFinishedBg" => real.turn_over_shell(),
        "BackgroundEnds" => real.shell_ends(),
        "AgentWakes" => real.agent_wakes(),
        "SeedDone" => assert!(real.st.seed_task_done(), "taken on at {before:?}"),
        "Continue" | "Check" | "Challenge" | "Answer" | "WallAct" => {
            let a = real.decide();
            assert!(
                !matches!(verdict(&a), Verdict::Nothing | Verdict::Wait),
                "{action} at {before:?}: {a:?}"
            );
            let r = point(real.said, real.walled, false, None, real.dirty);
            real.st.acted(&a, &r, real.now);
        }
        "BackoffDue" => real.now += real.st.short_wait().expect("a back-off runs"),
        "YieldDone" => real.turn(
            if before["pending"] == 2 {
                REPLY
            } else {
                REPORT
            },
            false,
            LONG,
        ),
        "YieldAsk" => real.turn(QUESTION, false, SHORT),
        other => panic!("an action this bind does not mirror: {other}"),
    }
}

/// The real side at the walk's start: a point that ended someone's turn of
/// real work.
fn start(base: Instant) -> Real {
    let mut real = Real {
        st: TurnEndState::default(),
        now: base,
        said: PROGRESS,
        walled: false,
        over_shell: false,
        dirty: false,
    };
    real.st
        .observe(&point(PROGRESS, false, false, Some(LONG), false), real.now);
    real
}

/// `path` fired on the model `m` from its start, mirrored on the real side.
fn walk(m: &Model, path: &[&str], base: Instant) -> (State, Real) {
    let mut st = m.init_state();
    let mut real = start(base);
    for action in path {
        let before = st.clone();
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
        mirror(action, &before, &mut real);
    }
    (st, real)
}

#[test]
fn tier1_the_real_decider_checks_a_done_report_and_leaves_a_done_task() {
    let m = supervisor_done_check_model();
    let base = Instant::now() + Duration::from_secs(3600);
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), start(base))]);
    let mut verdicts = BTreeMap::<String, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        for inv in &m.invariants {
            assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
        }
        if st["pending"] == 0 {
            assert_eq!(
                i64::from(real.st.done_reports()),
                st["done"],
                "the done reports at {st:?}"
            );
            assert_eq!(real.st.task_done(), st["done"] == 2, "{st:?}");
            assert_eq!(real.st.managed(), st["managed"] == 1, "{st:?}");
            assert_eq!(
                i64::from(real.st.short_streak().min(SHORT_CAP)),
                st["short"],
                "the short streak at {st:?}"
            );
            assert_eq!(real.st.done_over_background(), st["bg"] == 1, "{st:?}");
            let owed = !real.st.open_items().is_empty() || real.st.repo_owed().is_some();
            assert_eq!(owed, st["open"] == 1, "the owed challenge at {st:?}");
            assert_eq!(
                real.st.clone().seed_task_done(),
                m.action_enabled("SeedDone", &st),
                "a recorded done taken on at {st:?}"
            );
            let got = verdict(&real.clone().decide());
            assert_eq!(
                got,
                expected(&m, &st),
                "the real decider disagrees at {st:?}"
            );
            *verdicts.entry(format!("{got:?}")).or_default() += 1;
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            mirror(action.name, &st, &mut r);
            queue.push_back((next, r));
        }
    }
    // Every free point the model has — done 0, 1 and 2, a wall, a question,
    // a FINISHED with work left owing its challenge, the streak and its back-off
    // running or over, a worker or not, the busy screen over a shell, the
    // agent's own wake — gives every word.
    for word in [
        "Continue",
        "Check",
        "Challenge",
        "Answer",
        "WallAct",
        "Wait",
        "Nothing",
    ] {
        assert!(
            verdicts.contains_key(word),
            "{word} never occurred: {verdicts:?}"
        );
    }
}

/// NEGATIVE CONTROLS: where the `Buggy = 1` model says what the policy
/// before it did, the real decider says otherwise.
#[test]
fn tier1_the_buggy_model_is_refuted_by_the_real_decider() {
    let m = supervisor_done_check_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let base = Instant::now() + Duration::from_secs(3600);

    // At a done report, and on a done task, the Buggy model continues — the
    // real decider types the check, then nothing.
    let (st, real) = walk(&m, &["OtherTurnDone"], base);
    assert!(buggy.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);
    let (st, real) = walk(&m, &["OtherTurnDone", "Check", "YieldDone"], base);
    assert!(real.st.task_done());
    assert!(
        buggy.action_enabled("Continue", &st),
        "the Buggy policy nudges a done task"
    );
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);
    // …and someone else's turn that ends on a wall there: the Buggy model
    // leaves the task done and does nothing; the real decider retries.
    let mut b = st.clone();
    assert!(buggy.fire("OtherTurnWall", &mut b));
    assert!(!buggy.action_enabled("WallAct", &b), "{b:?}");
    let (_, real) = walk(
        &m,
        &["OtherTurnDone", "Check", "YieldDone", "OtherTurnWall"],
        base,
    );
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::WallAct);

    // A WORKER'S check after a continuation's short done yield: the Buggy
    // model holds it for the back-off (only the wait is on), the real
    // decider types it at once — and waits only where nobody manages it.
    let path = ["Tasked", "Continue", "YieldDone"];
    let (st, real) = walk(&m, &path, base);
    assert!(!buggy.action_enabled("Check", &st) && buggy.action_enabled("BackoffDue", &st));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);
    let (_, real) = walk(&m, &path[1..], base);
    assert_eq!(verdict(&real.clone().decide()), Verdict::Wait);
    // …and its question after a short yield is answered at once — but a
    // worker that keeps asking is answered past its bound at the back-off's
    // pace: the Buggy model answers its fourth short turn in a row at once,
    // the real decider waits.
    let (_, real) = walk(&m, &["Tasked", "Continue", "YieldAsk"], base);
    assert_eq!(verdict(&real.clone().decide()), Verdict::Answer);
    let asks = [
        "Tasked", "Continue", "YieldAsk", "Answer", "YieldAsk", "Answer", "YieldAsk", "Answer",
        "YieldAsk",
    ];
    let (st, real) = walk(&m, &asks, base);
    assert_eq!(st["short"], 4);
    assert!(buggy.action_enabled("Answer", &st) && !m.action_enabled("Answer", &st));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Wait);

    // A RECORDED DONE AT A WALL: the Buggy model takes it on there, and
    // types nothing more; the real decider refuses it and retries.
    let (st, mut real) = walk(&m, &["OtherTurnWall"], base);
    assert!(buggy.action_enabled("SeedDone", &st), "{st:?}");
    assert!(!real.st.seed_task_done());
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::WallAct);

    // THE TOKEN OVER A RUNNING SHELL: the Buggy model reads nothing of the
    // busy screen and leaves the task open; the real decider ends it there,
    // and the shell's end leaves it done.
    for path in [
        &["OtherTurnFinishedBg"][..],
        &["Continue", "YieldFinishedBg"][..],
    ] {
        let mut b = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
        }
        assert_ne!(b["done"], 2, "{path:?}: {b:?}");
        let (st, real) = walk(&m, path, base);
        assert_eq!(st["done"], 2);
        assert!(
            real.st.task_done() && real.st.done_over_background(),
            "{path:?}"
        );
        let mut st = st;
        let mut real = real;
        let before = st.clone();
        assert!(m.fire("BackgroundEnds", &mut st));
        mirror("BackgroundEnds", &before, &mut real);
        assert!(real.st.task_done(), "{path:?}");
        assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);
    }

    // THE AGENT'S OWN WAKE at the shell's end: the Buggy model reads it as
    // someone else's turn, its report a first done one, and checks; the
    // real decider keeps the task done and types nothing. A person's message
    // at the same point is a turn of theirs, the task open again.
    let (st, real) = walk(&m, &["OtherTurnFinishedBg"], base);
    let mut b = st.clone();
    assert!(buggy.fire("AgentWakes", &mut b));
    assert_eq!(b["done"], 1, "{b:?}");
    let mut woke = real.clone();
    woke.agent_wakes();
    assert!(woke.st.task_done());
    assert_eq!(verdict(&woke.decide()), Verdict::Nothing);
    let mut asked = real;
    asked.turn(PROGRESS, false, LONG);
    assert!(!asked.st.task_done());
    assert_eq!(verdict(&asked.decide()), Verdict::Continue);

    // AN OPEN FINISHED: the Buggy model ends the task at once (the harness
    // before DONE-1); the real decider types the challenge — and so over a
    // repository with work no remote has.
    for finished in ["OtherTurnOpenFinished", "OtherTurnDirtyFinished"] {
        let (st, real) = walk(&m, &[finished], base);
        let mut b = st.clone();
        assert!(buggy.fire("FinishOpenAtOnce", &mut b), "{b:?}");
        assert!(!buggy.check_invariant("WorkLeftNeverEndsTheTask", &b));
        assert!(!real.st.task_done());
        assert_eq!(
            verdict(&real.clone().decide()),
            Verdict::Challenge,
            "{finished}"
        );
    }
    // …and at the FINISHED that answers it with work still left the Buggy
    // model (the once-a-job cap) ends the task, where the real decider
    // challenges again on the back-off's pace — at the second token and at
    // the third, never let go…
    let (st, _) = walk(&m, &["OtherTurnOpenFinished", "Challenge"], base);
    let mut b = st.clone();
    assert!(buggy.fire("EndAtTheChallengesAnswer", &mut b), "{b:?}");
    assert!(
        !buggy.check_invariant("WorkLeftNeverEndsTheTask", &b),
        "{b:?}"
    );
    for (finished, again) in [
        ("YieldOpenFinished", "OtherTurnOpenFinished"),
        ("YieldDirtyFinished", "OtherTurnDirtyFinished"),
    ] {
        let mut path = vec![again];
        for _ in 0..2 {
            path.extend(["Challenge", finished]);
            let (st, real) = walk(&m, &path, base);
            assert!(!real.st.task_done(), "{path:?}");
            assert_eq!(st["open"], 1, "{path:?}");
            assert_eq!(verdict(&real.clone().decide()), Verdict::Wait, "{path:?}");
            path.push("BackoffDue");
            let (_, real) = walk(&m, &path, base);
            assert_eq!(
                verdict(&real.clone().decide()),
                Verdict::Challenge,
                "{path:?}"
            );
        }
        // …and a worker's goes at once, every time.
        let mut path = vec!["Tasked", again];
        for _ in 0..2 {
            path.extend(["Challenge", finished]);
            let (_, real) = walk(&m, &path, base);
            assert_eq!(
                verdict(&real.clone().decide()),
                Verdict::Challenge,
                "{path:?}"
            );
        }
        // …until the FINISHED that answers it leaves nothing: the work
        // committed and pushed, the report naming nothing open.
        path.extend(["Challenge", "YieldFinished"]);
        let (_, real) = walk(&m, &path, base);
        assert!(
            real.st.task_done() && real.st.finished_by_worker(),
            "{path:?}"
        );
        assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);
    }
}
