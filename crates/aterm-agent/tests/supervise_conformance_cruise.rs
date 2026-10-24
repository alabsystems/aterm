// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorCruise` (aterm-models-harness `supervisor_cruise_model`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::repo_work::RepoWork;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, DONE_CHECK, RULE_API_RETRY, RULE_CONTINUE, RULE_DONE_CHECK, TurnEndAction,
    TurnEndReading, TurnEndState, WaitKind, decide_turn_end, is_answer_rule, open_work_suffix,
    repo_work_suffix,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_models_harness::supervisor_cruise_model;
use aterm_phase::wall::WallKind;
use aterm_spec::derive::Model;

type State = BTreeMap<&'static str, i64>;

/// Real work, long enough that no turn is short for its length; it runs tools (a tool row on the
/// screen, which at a wall is what tells work from the program's own retries).
const LONG: Duration = Duration::from_secs(4 * 60);
/// A turn too short to be work: a yield that asks is one.
const SHORT: Duration = Duration::from_secs(30);
/// A turn's last words: progress, the vendor's done report, the defense the
/// defend prompt asks for, and a question.
const PROGRESS: &str = "Stage 2 landed; the parser is next.";
const REPORT: &str = "Everything is already done; nothing left to do.";
const DEFENSE: &str = "Nothing left: the parser and its tests are on origin/main.\n\
                       - The suite passes on main.";
const QUESTION: &str = "Does the parser have to keep the old syntax?";
/// The retired token over a report that leaves nothing, over one whose words
/// still name open work, and over a shell the turn left running.
const FINISHED: &str = "Built, tested and landed the parser. Nothing left.\nATERM-TASK-DONE";
const FINISHED_OPEN: &str =
    "Landed the parser.\nStill open: the lexer's e2e suite (not run).\nATERM-TASK-DONE";
const FINISHED_OVER_SHELL: &str = "Fixed the parser; the dev server runs.\nATERM-TASK-DONE";
/// The model's short streak: 4 stands for any past the worker's bound.
const SHORT_CAP: u32 = 4;

/// What the host's probe finds in the session's repository at a
/// `*DirtyClaim` point: a file not committed, a commit not on the remote's
/// default branch.
fn finding() -> RepoWork {
    RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 1,
        untracked: 0,
        stashes: 0,
        unpushed: 1,
        unlanded: 1,
        main_ref: "origin/main".to_string(),
        paths: vec!["src/lexer.rs".to_string()],
        state: Some(0x5eed),
    }
}

/// An authoritative idle point under Cruise whose turn worked `worked`,
/// ending on `said` after [`crate::common::earlier`]'s `earlier` — or on a `529 Overloaded`
/// wall; `over_shell`, the BUSY screen of a turn that ended while a shell it
/// left runs on; `dirty`, the host's probe finds the session's repository
/// holds work no remote main has ([`finding`]).
fn point(
    (said, earlier): (&str, usize),
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
        pending_input: false,
        background: over_shell,
        interrupted: false,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        left_record: false,
        left_work: None,
        person: None,
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        reach: Default::default(),
        program: aterm_phase::Program::Claude,
        seat: Default::default(),
        account: None,
        autopilot: Autopilot::Cruise,
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
        ran_tools: worked.is_some_and(|w| w >= LONG),
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::of_found(dirty.then(finding)),
        fact: None,
    }
}

/// What the real decider says at the point `r`, as a model word.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Continue,
    Check,
    Answer,
    WallAct,
    Wait,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Verdict::Continue,
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_DONE_CHECK => {
            assert!(
                text.starts_with(DONE_CHECK),
                "the defend prompt's words: {text}"
            );
            Verdict::Check
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

/// The defend prompt the model's `open` asks for at `st`, its suffix the
/// open work the real state read in the claim's words (`open` 1) or the
/// probe's finding (`open` 2), and no suffix with nothing left (`open` 0).
fn defend_prompt(st: &State, real: &TurnEndState) -> String {
    match st["open"] {
        0 => DONE_CHECK.to_string(),
        1 => format!(
            "{DONE_CHECK} {}",
            open_work_suffix(real.open_items()).expect("the claim's open work")
        ),
        _ => format!("{DONE_CHECK} {}", repo_work_suffix(&finding())),
    }
}

fn expected(m: &Model, st: &State) -> Verdict {
    for (action, word) in [
        ("Continue", Verdict::Continue),
        ("Check", Verdict::Check),
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
/// of the point it stands at (with the [`crate::common::earlier`] words before them, whether
/// it shows the wall, whether it is the busy screen over a running shell, and
/// whether the probe finds work in the repository there).
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    said: &'static str,
    earlier: usize,
    walled: bool,
    over_shell: bool,
    dirty: bool,
}

impl Real {
    /// The decision at the point now, the wall's own wait run through (its
    /// ladder's clock, which the model does not count) — never the short
    /// back-off's, which the model's `BackoffDue` moves.
    fn decide(&mut self) -> TurnEndAction {
        let r = point(
            (self.said, self.earlier),
            self.walled,
            self.over_shell,
            None,
            self.dirty,
        );
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

    /// A turn — a live one, read busy — that ended on `said` after `worked`,
    /// or on the wall, its earlier words its own; `dirty`: the
    /// probe finds work no remote main has at its end.
    fn turn(&mut self, said: &'static str, walled: bool, worked: Duration, dirty: bool) {
        self.earlier += 1;
        self.said_again(said, walled, worked, dirty);
    }

    /// A turn that ended as [`Self::turn`]'s, its earlier words the last's.
    fn said_again(&mut self, said: &'static str, walled: bool, worked: Duration, dirty: bool) {
        self.st.took();
        self.now += worked;
        self.said = said;
        self.walled = walled;
        self.over_shell = false;
        self.dirty = dirty;
        self.st.observe(
            &point((said, self.earlier), walled, false, Some(worked), dirty),
            self.now,
        );
    }

    /// A turn that claimed done while a shell it left runs on: the busy
    /// screen the loop reads, no point.
    fn turn_over_shell(&mut self) {
        let before = self.st.point_number();
        self.st.took();
        self.earlier += 1;
        self.said = FINISHED_OVER_SHELL;
        self.walled = false;
        self.over_shell = true;
        self.dirty = false;
        self.st.observe(
            &point(
                (FINISHED_OVER_SHELL, self.earlier),
                false,
                true,
                None,
                false,
            ),
            self.now,
        );
        assert_eq!(
            self.st.point_number(),
            before,
            "no point at the busy screen"
        );
    }

    /// The shell ends: the idle screen it leaves, the turn's work as the busy
    /// reads measured it — the claim's point.
    fn shell_ends(&mut self) {
        self.now += LONG;
        self.over_shell = false;
        self.st.observe(
            &point((self.said, self.earlier), false, false, Some(LONG), false),
            self.now,
        );
    }
}

fn mirror(action: &str, before: &State, real: &mut Real) {
    match action {
        "Tasked" => real.st.set_managed(true),
        "OtherTurnWork" | "YieldWork" => real.turn(PROGRESS, false, LONG, false),
        "OtherTurnClaim" => real.turn(REPORT, false, LONG, false),
        "OtherTurnWall" | "YieldWall" => real.turn(PROGRESS, true, LONG, false),
        "OtherTurnOpenClaim" | "YieldOpenClaim" => real.turn(FINISHED_OPEN, false, LONG, false),
        "OtherTurnDirtyClaim" | "YieldDirtyClaim" => real.turn(FINISHED, false, LONG, true),
        "OtherTurnClaimBg" | "YieldClaimBg" => real.turn_over_shell(),
        "BackgroundEnds" => real.shell_ends(),
        "Continue" | "Check" | "Answer" | "WallAct" => {
            let a = real.decide();
            assert!(
                !matches!(verdict(&a), Verdict::Nothing | Verdict::Wait),
                "{action} at {before:?}: {a:?}"
            );
            let r = point(
                (real.said, real.earlier),
                real.walled,
                false,
                None,
                real.dirty,
            );
            real.st.acted(&a, &r, real.now);
        }
        "BackoffDue" => real.now += real.st.short_wait().expect("a back-off runs"),
        "YieldClaim" => real.turn(
            if before["pending"] == 2 {
                DEFENSE
            } else {
                REPORT
            },
            false,
            LONG,
            false,
        ),
        "YieldAsk" => real.turn(QUESTION, false, SHORT, false),
        "YieldAskSame" => real.said_again(QUESTION, false, SHORT, false),
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
        earlier: 0,
        walled: false,
        over_shell: false,
        dirty: false,
    };
    real.st.observe(
        &point((PROGRESS, 0), false, false, Some(LONG), false),
        real.now,
    );
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
fn tier1_the_real_decider_defends_every_claim_and_never_rests() {
    let m = supervisor_cruise_model();
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
        assert!(!real.st.task_done(), "the task ended at {st:?}");
        assert!(
            !real.st.clone().seed_task_done(),
            "a recorded done taken on at {st:?}"
        );
        if st["pending"] == 0 && st["bg"] == 0 {
            assert_eq!(
                i64::from(real.st.done_reports()),
                st["claim"],
                "the done claim at {st:?}"
            );
            assert_eq!(real.st.managed(), st["managed"] == 1, "{st:?}");
            assert_eq!(
                i64::from(real.st.short_streak().min(SHORT_CAP)),
                st["short"],
                "the short streak at {st:?}"
            );
            assert_eq!(
                !real.st.open_items().is_empty(),
                st["open"] == 1,
                "the open work at {st:?}"
            );
            assert_eq!(
                real.st.repo_owed().is_some(),
                st["open"] == 2,
                "the repository's work at {st:?}"
            );
            assert_eq!(
                crate::common::same(&real.st),
                st["same"],
                "the same reply at {st:?}"
            );
        }
        if st["pending"] == 0 {
            let a = real.clone().decide();
            let got = verdict(&a);
            assert_eq!(
                got,
                expected(&m, &st),
                "the real decider disagrees at {st:?}"
            );
            if let TurnEndAction::Type { text, .. } = &a
                && got == Verdict::Check
            {
                assert_eq!(*text, defend_prompt(&st, &real.st), "at {st:?}");
            }
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
    for word in ["Continue", "Check", "Answer", "WallAct", "Wait", "Nothing"] {
        assert!(
            verdicts.contains_key(word),
            "{word} never occurred: {verdicts:?}"
        );
    }
}

/// NEGATIVE CONTROLS: where the `Buggy = 1` model says what a policy before
/// it did, the real decider says otherwise.
#[test]
fn tier1_the_buggy_model_is_refuted_by_the_real_decider() {
    let m = supervisor_cruise_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let base = Instant::now() + Duration::from_secs(3600);

    let (st, real) = walk(&m, &["OtherTurnClaim"], base);
    assert!(buggy.action_enabled("Continue", &st), "{st:?}");
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);

    for claim in [
        "OtherTurnClaim",
        "OtherTurnDirtyClaim",
        "OtherTurnOpenClaim",
    ] {
        let path = [claim, "Check", "YieldClaim"];
        let mut b = buggy.init_state();
        for action in path {
            assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
        }
        assert!(
            !buggy.check_invariant("CruiseNeverRests", &b),
            "{claim}: {b:?}"
        );
        let (st, real) = walk(&m, &path, base);
        assert!(!real.st.task_done(), "{claim}");
        assert_eq!(verdict(&real.clone().decide()), Verdict::Wait, "{claim}");
        let mut path = path.to_vec();
        path.push("BackoffDue");
        let (st2, real) = walk(&m, &path, base);
        assert_eq!(st["claim"], 1);
        assert_eq!(verdict(&real.clone().decide()), Verdict::Check, "{claim}");
        assert!(m.action_enabled("Check", &st2));
    }

    let path = ["Tasked", "Continue", "YieldClaim"];
    let (st, real) = walk(&m, &path, base);
    assert!(!buggy.action_enabled("Check", &st) && buggy.action_enabled("BackoffDue", &st));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);
    let (_, real) = walk(&m, &path[1..], base);
    assert_eq!(verdict(&real.clone().decide()), Verdict::Wait);
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

    let again = ["Tasked", "Continue", "YieldAsk", "Answer", "YieldAskSame"];
    let (st, real) = walk(&m, &again, base);
    let same = aterm_spec::interp::with_buggy(&m, 2);
    let mut b = st.clone();
    assert!(
        !m.action_enabled("Answer", &st) && same.fire("Answer", &mut b),
        "{st:?}"
    );
    assert!(!same.check_invariant("ANoWorkReplyGetsOneMoreActAtMost", &b));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);
    let (_, real) = walk(&m, &[&again[..4], &["YieldAsk"]].concat(), base);
    assert_eq!(verdict(&real.clone().decide()), Verdict::Answer);

    let (st, mut real) = walk(&m, &["OtherTurnClaim"], base);
    let mut b = st.clone();
    assert!(buggy.fire("SeedOldDone", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("CruiseNeverRests", &b));
    assert!(!real.st.seed_task_done());
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);

    let (st, real) = walk(&m, &["OtherTurnClaimBg"], base);
    let mut b = st.clone();
    assert!(buggy.fire("FinishOverBackground", &mut b));
    assert!(!buggy.check_invariant("CruiseNeverRests", &b));
    let mut b = st.clone();
    assert!(buggy.fire("Check", &mut b));
    assert!(!buggy.check_invariant("TheBusyScreenGetsNoAct", &b));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);
    assert!(!real.st.task_done());
    let (_, real) = walk(&m, &["OtherTurnClaimBg", "BackgroundEnds"], base);
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);
}
