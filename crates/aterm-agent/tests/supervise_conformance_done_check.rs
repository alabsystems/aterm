// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorDoneCheck` (aterm-spec
//! `supervisor_done_check_model`): the REAL turn-end decider
//! (`decide_turn_end`, with the real `TurnEndState` it keeps) driven along
//! EVERY reachable state of the model.
//!
//! Each model action is mirrored on the real side: someone else's turn is a
//! point `observe`d with real work and nothing of the policy's awaited (its
//! last words a done report, or progress, or a `529 Overloaded` wall); the
//! policy's `Continue`, `Check` and `WallAct` are the real decider's own act
//! at that point — decided past the point's back-off and the wall's wait, on
//! the real ladder's clock — recorded with `acted`; a yield is the next
//! point `observe`d with real work, its last words progress, or a done
//! report (the vendor's phrase after a continuation or a wall's act, the
//! `DONE` the check asks for after the check), or the wall again. At every
//! state the real verdict is checked against the model — it types the
//! continuation exactly where `Continue` is enabled, the check exactly where
//! `Check` is, the wall's retry exactly where `WallAct` is, and nothing on a
//! done task — and the real state's done reports are the model's `done`.
//! NEGATIVE CONTROLS: at a done report, and on a done task, the `Buggy = 1`
//! model continues where the real decider types the check, or nothing; and
//! at a wall someone else's turn ended on after the task was done, the
//! `Buggy = 1` model does nothing where the real decider retries (the review
//! of 2026-09-27) — so a green walk is not vacuous.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, DONE_CHECK, RULE_API_RETRY, RULE_CONTINUE, RULE_DONE_CHECK, TurnEndAction,
    TurnEndReading, TurnEndState, decide_turn_end,
};
use aterm_phase::wall::WallKind;
use aterm_spec::derive::{Model, supervisor_done_check_model};

type State = BTreeMap<&'static str, i64>;

/// Real work, long enough that no turn is short for its length.
const LONG: Duration = Duration::from_secs(4 * 60);
/// A turn's last words: progress, the vendor's done report, and the reply
/// the check asks for.
const PROGRESS: &str = "Stage 2 landed; the parser is next.";
const REPORT: &str = "Everything is already done; nothing left to do.";
const REPLY: &str = "DONE";

/// An authoritative idle point whose turn worked `worked`, ending on `said`
/// — or on a `529 Overloaded` wall.
fn point(said: &str, walled: bool, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        phase: Phase::Idle,
        authoritative: true,
        survey: false,
        wall: walled.then_some(WallKind::Overloaded),
        wall_message: if walled {
            "API Error: 529 Overloaded.".to_string()
        } else {
            String::new()
        },
        reset_at: None,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some(said.to_string()),
        worked,
        rules: None,
        pending_input: false,
        interrupted: false,
        restartable: true,
        resume: None,
        upgrading: false,
        taskless: false,
        person: None,
        login_back: false,
        reach: Default::default(),
        program: aterm_phase::Program::Claude,
        goal: None,
        model_field: None,
        limits: Default::default(),
        sandbox_fell: None,
        thread_model: None,
        upgrade_goal: false,
    }
}

/// What the real decider says at the point `r`, as a model word.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Continue,
    Check,
    WallAct,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Verdict::Continue,
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_DONE_CHECK => {
            assert_eq!(text, DONE_CHECK, "the check's words");
            Verdict::Check
        }
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_API_RETRY => Verdict::WallAct,
        TurnEndAction::Nothing => Verdict::Nothing,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

fn expected(m: &Model, st: &State) -> Verdict {
    if m.action_enabled("Continue", st) {
        Verdict::Continue
    } else if m.action_enabled("Check", st) {
        Verdict::Check
    } else if m.action_enabled("WallAct", st) {
        Verdict::WallAct
    } else {
        Verdict::Nothing
    }
}

/// The real side: the decider's state, the loop's clock, and the last words
/// of the point it stands at (and whether it shows the wall).
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    said: &'static str,
    walled: bool,
}

impl Real {
    /// Past the point's back-off (the real ladder's) and the wall's wait,
    /// the decision there.
    fn decide(&mut self) -> TurnEndAction {
        self.now += self.st.short_wait().unwrap_or_default();
        let r = point(self.said, self.walled, None);
        self.st.observe(&r, self.now);
        let cfg = SupervisorConfig::default();
        for _ in 0..4 {
            match decide_turn_end(&self.st, &r, &cfg, self.now) {
                TurnEndAction::WaitUntil { until, .. } => self.now = until,
                a => return a,
            }
        }
        panic!("the decider waits for ever at {:?}", self.said)
    }

    /// A turn that ended on `said` after real work — or on the wall.
    fn turn(&mut self, said: &'static str, walled: bool) {
        self.now += LONG;
        self.said = said;
        self.walled = walled;
        self.st.observe(&point(said, walled, Some(LONG)), self.now);
    }
}

fn mirror(action: &str, before: &State, real: &mut Real) {
    match action {
        "OtherTurnDone" => real.turn(REPORT, false),
        "OtherTurnWork" => real.turn(PROGRESS, false),
        "OtherTurnWall" | "YieldWall" => real.turn(PROGRESS, true),
        "Continue" | "Check" | "WallAct" => {
            let a = real.decide();
            assert_ne!(verdict(&a), Verdict::Nothing, "{action} at {before:?}");
            let r = point(real.said, real.walled, None);
            real.st.acted(&a, &r, real.now);
        }
        "YieldWork" => real.turn(PROGRESS, false),
        "YieldDone" => real.turn(
            if before["pending"] == 2 {
                REPLY
            } else {
                REPORT
            },
            false,
        ),
        other => panic!("an action this bind does not mirror: {other}"),
    }
}

#[test]
fn tier1_the_real_decider_checks_a_done_report_and_leaves_a_done_task() {
    let m = supervisor_done_check_model();
    let base = Instant::now() + Duration::from_secs(3600);
    // The walk starts at a point that ended someone's turn of real work.
    let mut init = Real {
        st: TurnEndState::default(),
        now: base,
        said: PROGRESS,
        walled: false,
    };
    init.st
        .observe(&point(PROGRESS, false, Some(LONG)), init.now);
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
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
    // done 0, 1 and 2 at a clean point, 0 and 1 at a wall, and a
    // continuation's, the check's and the wall's act's turn (from done 0 or 1).
    assert_eq!(seen.len(), 9, "the reachable space changed: {seen:?}");
    for word in ["Continue", "Check", "WallAct", "Nothing"] {
        assert!(
            verdicts.contains_key(word),
            "{word} never occurred: {verdicts:?}"
        );
    }

    // NEGATIVE CONTROLS: at a done report, and on a done task, the Buggy
    // model continues — the real decider types the check, then nothing.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut real = Real {
        st: TurnEndState::default(),
        now: base,
        said: PROGRESS,
        walled: false,
    };
    real.st
        .observe(&point(PROGRESS, false, Some(LONG)), real.now);
    let mut st = m.init_state();
    for action in ["OtherTurnDone", "Check", "YieldDone"] {
        if action == "OtherTurnDone" {
            assert!(buggy.action_enabled("Continue", &{
                let mut b = st.clone();
                assert!(buggy.fire(action, &mut b));
                b
            }));
        }
        let before = st.clone();
        assert!(m.fire(action, &mut st), "{action} at {st:?}");
        mirror(action, &before, &mut real);
        if action == "OtherTurnDone" {
            assert_eq!(verdict(&real.clone().decide()), Verdict::Check);
        }
    }
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
    let before = st.clone();
    assert!(m.fire("OtherTurnWall", &mut st));
    mirror("OtherTurnWall", &before, &mut real);
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::WallAct);
}
