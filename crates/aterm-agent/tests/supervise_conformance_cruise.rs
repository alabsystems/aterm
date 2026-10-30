// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorCruise` (aterm-models-harness `supervisor_cruise_model`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::repo_work::RepoWork;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::go_ahead::RULE_GO_AHEAD;
use aterm_agent::supervise::policy::turn_end::{
    Composer, DONE_CHECK, Outage, RULE_API_CUTOFF, RULE_CONTINUE, RULE_DONE_CHECK, Reach,
    TurnEndAction, TurnEndReading, TurnEndState, UNREACHABLE_HOLD, WaitKind, decide_turn_end,
    done_check_words, is_answer_rule, open_work_suffix, own_repo_work_suffix,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_models_harness::supervisor_cruise_model;
use aterm_phase::wall::WallKind;
use aterm_spec::derive::Model;

type State = BTreeMap<&'static str, i64>;

/// The wall the model's wall stands for: a reply past its output token maximum, a wall that keeps
/// its own act (`api-cutoff@v1`, once at once, then on the retry ladder) and its tries; an overload
/// is an outage, waited out ([`supervise_conformance_outage_wait`](crate::supervise_conformance_outage_wait)).
const OVERLONG: WallKind = WallKind::ApiError {
    code: None,
    retryable: true,
    cause: aterm_phase::ApiCause::Overlong,
};
const OVERLONG_MESSAGE: &str = "API Error: Claude's response exceeded the 32000 output token \
                                maximum. To configure this behavior, set the \
                                CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.";

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
/// ending on `said` after [`crate::common::earlier`]'s `earlier` — or on an over-long
/// reply's wall ([`OVERLONG`]); `over_shell`, the BUSY screen of a turn that ended while a shell it
/// left runs on; `dirty`, the host's probe finds the session's repository
/// holds work no remote main has ([`finding`]); `reach`, what the host measured of the API's
/// reach, as the loop reads it there.
fn point(
    (said, earlier): (&str, usize),
    walled: bool,
    over_shell: bool,
    worked: Option<Duration>,
    dirty: bool,
    reach: Reach,
) -> TurnEndReading {
    TurnEndReading {
        phase: if over_shell { Phase::Busy } else { Phase::Idle },
        authoritative: true,
        survey: false,
        wall: walled.then_some(OVERLONG),
        wall_message: if walled {
            OVERLONG_MESSAGE.to_string()
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
        opened_words: None,
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
        reach,
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
        login_moved: false,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        ran_tools: worked.is_some_and(|w| w >= LONG),
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::of_found(dirty.then(finding)),
        fact: None,
        wake: None,
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
    /// The measured outage holds the act ([`UNREACHABLE_HOLD`]).
    Held,
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
        // The owner's standing go-ahead answers a park as the model's Answer does.
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_GO_AHEAD => Verdict::Answer,
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_CUTOFF => Verdict::WallAct,
        TurnEndAction::WaitUntil {
            why,
            wait: WaitKind::BackOff { .. },
            ..
        } if why.contains("back-off") => Verdict::Wait,
        TurnEndAction::WaitUntil {
            why,
            wait:
                WaitKind::Outage {
                    wall: Outage::Connection,
                    tries: true,
                },
            ..
        } if why == UNREACHABLE_HOLD => Verdict::Held,
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

/// The defend prompt the model's `open` asks for at `st`, its suffix the
/// open work the real state read in the claim's words (`open` 1) or the
/// probe's finding (`open` 2), and no suffix with nothing left (`open` 0).
fn defend_prompt(st: &State, real: &TurnEndState) -> String {
    // The angle is the real state's count of the run's checks (v6 §14.5), which the model leaves
    // to the policy's own tests.
    let words = done_check_words(usize::from(real.checks()));
    match st["open"] {
        0 => words,
        1 => format!(
            "{words} {}",
            open_work_suffix(real.open_items()).expect("the claim's open work")
        ),
        _ => format!("{words} {}", own_repo_work_suffix(&finding())),
    }
}

/// The acts a measured outage holds (`down` = 1, `capped` = 0: its cap not yet passed).
const HELD: [&str; 3] = ["Continue", "Check", "Answer"];

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
        return Verdict::Wait;
    }
    let mut back = st.clone();
    back.insert("down", 2);
    if st["down"] == 1 && HELD.iter().any(|act| m.action_enabled(act, &back)) {
        Verdict::Held
    } else {
        Verdict::Nothing
    }
}

/// The real side: the decider's state, the loop's clock, and the last words
/// of the point it stands at (with the [`crate::common::earlier`] words before them, whether
/// it shows the wall, whether it is the busy screen over a running shell, and
/// whether the probe finds work in the repository there), and since when the host measured the
/// API down, while it does.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    said: &'static str,
    earlier: usize,
    walled: bool,
    over_shell: bool,
    dirty: bool,
    down: Option<Instant>,
    /// A decider that ignores the measured outage (the negative control): its readings never
    /// carry the Down.
    blind: bool,
}

impl Real {
    /// The reach a point's reading carries.
    fn reach(&self) -> Reach {
        match self.down {
            Some(since) if !self.blind => Reach::Down { since },
            _ => Reach::Unknown,
        }
    }

    /// The point now, as the loop reads it.
    fn reading(&self, worked: Option<Duration>) -> TurnEndReading {
        point(
            (self.said, self.earlier),
            self.walled,
            self.over_shell,
            worked,
            self.dirty,
            self.reach(),
        )
    }

    /// The decision at the point now, the wall's own wait run through (its
    /// ladder's clock, which the model does not count) — never the short
    /// back-off's, which the model's `BackoffDue` moves, nor the measured outage's hold, which
    /// its `NetUp` or `OutageCapDue` ends.
    fn decide(&mut self) -> TurnEndAction {
        let r = self.reading(None);
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
        self.st.observe(&self.reading(Some(worked)), self.now);
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
        self.st.observe(&self.reading(None), self.now);
        assert_eq!(
            self.st.point_number(),
            before,
            "no point at the busy screen"
        );
    }

    /// The shell ends: the idle screen it leaves, the turn's work as the busy
    /// reads measured it — the claim's point.
    /// The measured outage's cap passes (`OutageCapDue`): the clock moves to when the hold of
    /// the outage standing ends (`TurnEndTiming::outage_holds_until`), the reach still Down.
    fn outage_cap_passes(&mut self) {
        let since = self.down.expect("an outage stands");
        self.now = self.now.max(since + self.st.timing.outage_cap);
    }

    fn shell_ends(&mut self) {
        self.now += LONG;
        self.over_shell = false;
        self.st.observe(&self.reading(Some(LONG)), self.now);
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
        "NetDown" => real.down = Some(real.now),
        "NetUp" => real.down = None,
        "OutageCapDue" => real.outage_cap_passes(),
        "Continue" | "Check" | "Answer" | "WallAct" => {
            let a = real.decide();
            assert!(
                !matches!(
                    verdict(&a),
                    Verdict::Nothing | Verdict::Wait | Verdict::Held
                ),
                "{action} at {before:?}: {a:?}"
            );
            let r = real.reading(None);
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
        down: None,
        blind: false,
    };
    real.st.observe(&real.reading(Some(LONG)), real.now);
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
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), start(base))]);
    let mut verdicts = BTreeMap::<String, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(crate::common::key(&m, &st)) {
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
    for word in [
        "Continue", "Check", "Answer", "WallAct", "Wait", "Held", "Nothing",
    ] {
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

/// A MEASURED OUTAGE HOLDS THE ACT AND ITS RETURN OR ITS CAP TYPES IT: where the model's `down`
/// holds Continue, Check or Answer, the real decider waits on the outage ([`UNREACHABLE_HOLD`]),
/// and once `NetUp` fires, or `OutageCapDue` with the reach still Down, it types the act it held.
/// NEGATIVE CONTROLS: the `Buggy = 3` model, which types into the outage before its cap, breaks
/// NoContinuationIsTypedIntoAMeasuredOutageBeforeItsCap (and nothing else), and a decider whose
/// readings ignore the Down types the very act the model holds. Run against a decider with no
/// `outage_hold`, this test and the full walk fail; against one whose hold has no cap (run: the
/// `outage_cap` passing changes nothing), the cap's leg here and the full walk fail.
#[test]
fn tier1_a_measured_outage_holds_the_act_and_its_return_types_it() {
    let m = supervisor_cruise_model();
    let mistyped = aterm_spec::interp::with_buggy(&m, 3);
    assert!(m.mutant_dials().contains(&3), "{:?}", m.mutant_dials());
    assert_eq!(
        aterm_spec::interp::violated_invariants(&mistyped),
        BTreeSet::from(["NoContinuationIsTypedIntoAMeasuredOutageBeforeItsCap"]),
    );
    let base = Instant::now() + Duration::from_secs(3600);
    let asked = ["Tasked", "Continue", "YieldAsk", "NetDown"];
    for (path, act, word) in [
        (&["NetDown"][..], "Continue", Verdict::Continue),
        (&["OtherTurnClaim", "NetDown"][..], "Check", Verdict::Check),
        (&asked[..], "Answer", Verdict::Answer),
    ] {
        let (st, real) = walk(&m, path, base);
        assert!(!m.action_enabled(act, &st), "{path:?}");
        assert_eq!(expected(&m, &st), Verdict::Held, "{path:?}");
        assert_eq!(verdict(&real.clone().decide()), Verdict::Held, "{path:?}");
        let mut b = st.clone();
        assert!(mistyped.fire(act, &mut b), "{path:?}");
        assert!(
            !mistyped.check_invariant("NoContinuationIsTypedIntoAMeasuredOutageBeforeItsCap", &b),
            "{path:?}"
        );
        let blind = Real {
            blind: true,
            ..real.clone()
        };
        assert_eq!(
            verdict(&blind.clone().decide()),
            word,
            "a decider blind to the outage types into it: {path:?}"
        );
        let back = [path, &["NetUp"][..]].concat();
        let (st, real) = walk(&m, &back, base);
        assert!(m.action_enabled(act, &st), "{back:?}");
        assert_eq!(verdict(&real.clone().decide()), word, "{back:?}");
        let capped = [path, &["OutageCapDue"][..]].concat();
        let (st, real) = walk(&m, &capped, base);
        assert_eq!(st["down"], 1, "{capped:?}");
        assert!(m.action_enabled(act, &st), "{capped:?}");
        assert!(
            matches!(real.reach(), Reach::Down { .. }),
            "the reach still reads Down: {capped:?}"
        );
        assert_eq!(
            verdict(&real.clone().decide()),
            word,
            "the cap's end types the act: {capped:?}"
        );
    }
}

/// The words this bind hands the decider read as the over-long reply the model's wall stands for.
#[test]
fn the_walls_words_read_as_an_over_long_reply() {
    assert_eq!(aterm_phase::classify_wall(OVERLONG_MESSAGE), Some(OVERLONG));
}
