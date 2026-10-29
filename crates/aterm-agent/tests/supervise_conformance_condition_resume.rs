// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorConditionResume` (aterm-models-harness `supervisor_condition_resume_model`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Cleared, Clearing, Composer, RULE_CONTINUE, RULE_LIMIT_RESUME, TurnEndAction, TurnEndReading,
    TurnEndState, TurnEndTiming, cleared_text, decide_turn_end,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_models_harness::supervisor_condition_resume_model;
use aterm_phase::wall::WallKind;
use aterm_spec::derive::Model;

type State = BTreeMap<&'static str, i64>;

const MIN: Duration = Duration::from_secs(60);
/// Real work: long enough that no turn is short, and it ran tools.
const LONG: Duration = Duration::from_secs(12 * 60);
/// How far ahead of the wall its notice names the reset.
const TO_RESET: Duration = Duration::from_secs(30 * 60);
/// A person's last key at the keyboard, inside the default grace (`human_grace_s`, 120 s).
const TYPING: Duration = Duration::from_secs(5);
const PROGRESS: &str = "Stage 2 of 5 is in; stage 3 is next.";
const WALL: &str = "You've hit your weekly limit · resets 4pm (America/Los_Angeles)";

/// A point under Cruise: the agent's own turn end saying [`PROGRESS`] after [`LONG`], or the
/// weekly wall that ended it (`reset_at`, the reset its notice names), the person's last key
/// [`TYPING`] ago where `person`.
fn point(reset_at: Option<Instant>, person: bool) -> TurnEndReading {
    TurnEndReading {
        phase: Phase::Idle,
        authoritative: true,
        survey: false,
        wall: reset_at.map(|_| WallKind::UsageWeekly),
        wall_message: if reset_at.is_some() {
            WALL.to_string()
        } else {
            String::new()
        },
        reset_at,
        jitter: Duration::ZERO,
        resumes_by_itself: false,
        composer: Composer::Empty,
        said_tail: Some(PROGRESS.to_string()),
        said_cut: false,
        opens_cut: false,
        turn_said: None,
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked: Some(LONG),
        rules: None,
        pending_input: false,
        background: false,
        interrupted: false,
        restartable: true,
        relaunch_off: false,
        resume: None,
        upgrading: false,
        taskless: false,
        finished_record: None,
        left_record: false,
        left_work: None,
        person: person.then_some(TYPING),
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
        login_moved: false,
        roster: None,
        wall_remembered: false,
        answered: None,
        worked_doubt: Duration::ZERO,
        ran_tools: reset_at.is_none(),
        tools_unread: false,
        record_turn: None,
        credits: None,
        repo: aterm_agent::harness::repo_work::RepoRead::of_found(None),
        fact: None,
        wake: None,
    }
}

/// What the real decider does at a point, as the model tells it apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// The one line past the clearing ([`cleared_text`]).
    Resumes,
    /// Cruise's own continuation at the agent's own turn end.
    ModePrompt,
    Waits,
    Other(String),
}

fn verdict(a: &TurnEndAction, line: &str) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LIMIT_RESUME && text == line => {
            Verdict::Resumes
        }
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => Verdict::ModePrompt,
        TurnEndAction::WaitUntil { .. } => Verdict::Waits,
        other => Verdict::Other(format!("{other:?}")),
    }
}

fn enabled(m: &Model, action: &str, st: &State) -> bool {
    m.fire(action, &mut st.clone())
}

/// The model's word at a point: its resume, the mode's prompt, or nothing typed (the condition
/// stands, or the person at the keyboard holds every act).
fn expected(m: &Model, st: &State) -> Verdict {
    if enabled(m, "Resume", st) {
        Verdict::Resumes
    } else if enabled(m, "ModePrompt", st) {
        Verdict::ModePrompt
    } else {
        Verdict::Waits
    }
}

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    /// The reset the wall's notice on the screen names, passed once it cleared; `None` once the
    /// agent's turn moved the screen past it.
    drawn: Option<Instant>,
    person: bool,
    /// The point's reading, as observed; `None` while a turn runs.
    point: Option<TurnEndReading>,
}

/// How a walk departs from the real loop, for the negative controls.
#[derive(Clone, Copy, Default)]
struct Run {
    /// A loop deaf to the person's keys: every reading says no one typed.
    deaf_to_person: bool,
    /// A loop blind to the wall: the wall's point reads as an ordinary turn end.
    blind_to_wall: bool,
    /// A checker that wants the mode's own prompt for the resume's words.
    wants_mode_words: bool,
}

impl Real {
    fn observe(&mut self, run: Run) {
        let drawn = self.drawn.filter(|_| !run.blind_to_wall);
        let r = point(drawn, self.person && !run.deaf_to_person);
        self.st.observe(&r, self.now);
        self.point = Some(r);
    }
}

fn mirror(action: &str, r: &mut Real, cfg: &SupervisorConfig, run: Run) {
    match action {
        "ConditionMeets" => {
            r.now += LONG;
            r.drawn = Some(r.now + TO_RESET);
            r.observe(run);
        }
        "Clears" => {
            let reset = r.drawn.expect("a standing wall");
            r.now = r.now.max(reset) + MIN;
            r.observe(run);
        }
        "PersonTypes" | "GraceLapses" => {
            r.person = action == "PersonTypes";
            if !r.person {
                r.now += 3 * MIN;
            }
            if r.point.is_some() {
                r.observe(run);
            }
        }
        "Resume" | "ModePrompt" => {
            let point = r.point.take().expect("a point");
            let a = decide_turn_end(&r.st, &point, cfg, r.now);
            r.st.acted(&a, &point, r.now);
            r.drawn = None;
        }
        "TurnEnds" => {
            r.now += LONG;
            r.observe(run);
        }
        other => panic!("no mirror for {other}"),
    }
}

struct Walked {
    states: BTreeSet<State>,
    off: Vec<(State, Verdict, Verdict)>,
    fired: BTreeMap<&'static str, usize>,
}

fn walk(m: &Model, strict: bool, run: Run) -> Walked {
    let cfg = SupervisorConfig::default();
    let line = if run.wants_mode_words {
        "keep going"
    } else {
        cleared_text(Cleared::UsageLimit, Clearing::Measured)
    };
    let base = Instant::now() + Duration::from_secs(24 * 3600);
    let init = Real {
        st: TurnEndState::new(TurnEndTiming {
            reset_grace: Duration::ZERO,
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        }),
        now: base,
        drawn: None,
        person: false,
        point: None,
    };
    let mut seen = BTreeSet::new();
    let mut off = Vec::new();
    let mut fired: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        if let Some(point) = &real.point {
            let got = verdict(&decide_turn_end(&real.st, point, &cfg, real.now), line);
            let want = expected(m, &st);
            if got != want {
                assert!(
                    !strict,
                    "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
                );
                off.push((st.clone(), got, want));
            }
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            mirror(action.name, &mut r, &cfg, run);
            *fired.entry(action.name).or_default() += 1;
            queue.push_back((next, r));
        }
    }
    Walked {
        states: seen,
        off,
        fired,
    }
}

#[test]
fn tier1_the_real_decider_resumes_past_a_clearing_exactly_where_the_model_says() {
    let m = supervisor_condition_resume_model();
    let walked = walk(&m, true, Run::default());
    for a in [
        "ConditionMeets",
        "Clears",
        "PersonTypes",
        "GraceLapses",
        "Resume",
        "TurnEnds",
        "ModePrompt",
    ] {
        assert!(
            walked.fired.contains_key(a),
            "{a} never mirrored: {:?}",
            walked.fired
        );
    }
    assert!(walked.states.len() > 10, "{}", walked.states.len());
    let point = |s: &&State| s["working"] == 0;
    assert!(
        walked
            .states
            .iter()
            .filter(point)
            .any(|s| s["owed"] == 1 && s["person"] == 1 && !enabled(&m, "Resume", s)),
        "a clearing the person at the keyboard holds is reached"
    );
    assert!(
        walked
            .states
            .iter()
            .filter(point)
            .any(|s| s["standing"] == 1 && s["person"] == 0),
        "a standing wall with no one at the keyboard is reached"
    );
}

#[test]
fn tier1_a_loop_that_ignores_the_person_or_the_wall_disagrees_where_the_model_holds() {
    let m = supervisor_condition_resume_model();
    let deaf = walk(
        &m,
        false,
        Run {
            deaf_to_person: true,
            ..Run::default()
        },
    );
    assert!(!deaf.off.is_empty(), "the deaf loop agreed everywhere");
    assert!(
        deaf.off
            .iter()
            .all(|(s, _, want)| s["person"] == 1 && *want == Verdict::Waits),
        "{:?}",
        deaf.off
    );
    assert!(
        deaf.off
            .iter()
            .any(|(s, got, _)| s["owed"] == 1 && *got == Verdict::Resumes),
        "typed its resume over the person: {:?}",
        deaf.off
    );
    let blind = walk(
        &m,
        false,
        Run {
            blind_to_wall: true,
            ..Run::default()
        },
    );
    assert!(!blind.off.is_empty(), "the blind loop agreed everywhere");
    assert!(
        blind
            .off
            .iter()
            .any(|(s, got, _)| s["standing"] == 1 && *got == Verdict::ModePrompt),
        "typed into the standing wall: {:?}",
        blind.off
    );
    let mode_words = walk(
        &m,
        false,
        Run {
            wants_mode_words: true,
            ..Run::default()
        },
    );
    assert!(
        !mode_words.off.is_empty()
            && mode_words
                .off
                .iter()
                .all(|(s, _, want)| s["owed"] == 1 && *want == Verdict::Resumes),
        "the resume's words are held to the cleared line: {:?}",
        mode_words.off
    );
}

#[test]
fn each_buggy_resume_breaks_its_law() {
    let m = supervisor_condition_resume_model();
    for (buggy, path, law) in [
        (
            1,
            &[
                "TurnEnds",
                "ModePrompt",
                "ConditionMeets",
                "Clears",
                "Resume",
            ][..],
            "TheResumeIsTheClearedLine",
        ),
        (
            2,
            &["ConditionMeets", "Clears", "PersonTypes", "Resume"],
            "NeverTypedOverThePerson",
        ),
        (
            3,
            &["ConditionMeets", "Clears", "Resume", "TurnEnds", "Resume"],
            "OneResumePerClearing",
        ),
        (
            4,
            &["ConditionMeets", "Resume"],
            "NothingIntoAStandingCondition",
        ),
    ] {
        let bad = aterm_spec::interp::with_buggy(&m, buggy);
        let mut st = bad.init_state();
        for a in path {
            assert!(bad.fire(a, &mut st), "Buggy = {buggy}: {a} at {st:?}");
        }
        assert!(!bad.check_invariant(law, &st), "Buggy = {buggy}: {st:?}");
        let mut st = m.init_state();
        let held = path.iter().all(|a| m.fire(a, &mut st));
        assert!(
            !held || m.check_invariant(law, &st),
            "control: the model itself keeps {law}"
        );
    }
}
