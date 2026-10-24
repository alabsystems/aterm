// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorLand` (aterm-models-harness `supervisor_land_model`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{HoldLeg, LandHeld, TaskFace};
use aterm_agent::harness::repo_work::RepoWork;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    Composer, LAND, LAND_ANSWER, LAND_NUDGE, RULE_API_RETRY, RULE_LAND, RULE_LAND_ANSWER,
    RULE_LAND_NUDGE, TurnEndAction, TurnEndReading, TurnEndState, WaitKind, clean_defense,
    decide_turn_end, open_work_strict, open_work_suffix, repo_work_suffix, says_done,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_models_harness::supervisor_land_model;
use aterm_phase::wall::WallKind;
use aterm_spec::derive::Model;

type State = BTreeMap<&'static str, i64>;

const LONG: Duration = Duration::from_secs(4 * 60);
const SHORT: Duration = Duration::from_secs(30);
const JOB: Duration = Duration::from_secs(12 * 60);
const KEYED: Duration = Duration::from_secs(20);

const PROGRESS: &str = "Stage 2 is on its branch; the parser is next.";
const QUESTION: &str = "Does the parser have to keep the old syntax?";
const TOKEN: &str = "Built and tested the parser.\nATERM-TASK-DONE";
const DEFENSE: &str = "Nothing left: the parser and its tests are on origin/main.\n\
                       - The suite passes on main and the worktree is removed.";
const NEEDS: &str = "Nothing left: the parser and its tests are on origin/main.\n\
                     NEEDS YOU: the release credential, to sign the build";
const OPEN_DEFENSE: &str = "Nothing left: the parser is pushed.\n- Its branch is not merged yet.";
const ANSWERED: &str = "Yes: the old syntax still parses.";

fn finding() -> RepoWork {
    RepoWork {
        repo: "/Users//a/Developer/parser".to_string(),
        changed: 0,
        untracked: 0,
        stashes: 0,
        unpushed: 0,
        unlanded: 1,
        main_ref: "origin/main".to_string(),
        paths: Vec::new(),
        state: Some(0x5eed),
    }
}

/// What an authoritative point under Land saw.
#[derive(Clone, Copy, Default)]
struct Seen {
    walled: bool,
    busy: bool,
    dirty: bool,
    keyed: bool,
}

fn point(said: &str, at: Seen, worked: Option<Duration>) -> TurnEndReading {
    TurnEndReading {
        phase: if at.busy { Phase::Busy } else { Phase::Idle },
        authoritative: true,
        survey: false,
        wall: at.walled.then_some(WallKind::Overloaded),
        wall_message: if at.walled {
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
        turn_said: None,
        opened_by: None,
        message_drawn: false,
        submits: None,
        inputs_before: None,
        screen_mark: None,
        worked,
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
        person: at.keyed.then_some(KEYED),
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
        autopilot: Autopilot::Land,
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
        repo: aterm_agent::harness::repo_work::RepoRead::of_found(at.dirty.then(finding)),
        fact: None,
    }
}

/// What the real decider says at a point, as a model word.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Instruction,
    Nudge,
    Answer,
    WallAct,
    Wait,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND => {
            assert!(text.starts_with(LAND), "the instruction's words: {text}");
            Verdict::Instruction
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND_NUDGE => {
            assert!(text.starts_with(LAND_NUDGE), "the nudge's words: {text}");
            Verdict::Nudge
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND_ANSWER => {
            assert!(
                text.starts_with(LAND_ANSWER),
                "the land answer's words: {text}"
            );
            Verdict::Answer
        }
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
        ("Instruction", Verdict::Instruction),
        ("Nudge", Verdict::Nudge),
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

fn nudge(st: &State, real: &TurnEndState) -> String {
    let mut text = LAND_NUDGE.to_string();
    if st["said"] == 3 {
        text.push(' ');
        text.push_str(&open_work_suffix(real.open_items()).expect("the defense's open work"));
    }
    if st["dirty"] == 1 {
        text.push(' ');
        text.push_str(&repo_work_suffix(&finding()));
    }
    text
}

/// HOW THE TASK'S FACE IS TOLD to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Teller {
    /// The loop's.
    Loop,
    /// Told only where the journal says a task's end (`DONE`, `LEFT`).
    OnEndNotes,
    /// Told at points alone, never at a live read.
    AtPointsOnly,
    /// Landed for any done task — a person's exchange among them.
    LandedForDone,
}

/// HOW THE HOLD IS TOLD to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldTeller {
    /// The loop's (`Session::tell_hold`).
    Loop,
    /// The face told, the hold never.
    Never,
    /// At points (and the start, and the acts' rows) alone.
    AtPointsOnly,
    /// At points and live reads, never at an act's ledger row.
    NotAtActs,
}

/// The real side of one walk.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    said: &'static str,
    at: Seen,
    told: Option<TaskFace>,
    teller: Teller,
    /// The task was done at the last point ([`Teller::OnEndNotes`]).
    noted: bool,
    /// The hold the host was last told (`IdleHost::land_held`).
    held: Option<LandHeld>,
    hold_teller: HoldTeller,
    /// The ledger's clock, unix ms: each act's row a minute after the last.
    row_ms: i64,
}

impl Real {
    fn decide(&mut self) -> TurnEndAction {
        let r = point(
            self.said,
            Seen {
                keyed: false,
                ..self.at
            },
            None,
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
        let tells = match self.hold_teller {
            HoldTeller::Loop | HoldTeller::NotAtActs => true,
            HoldTeller::AtPointsOnly => !busy,
            HoldTeller::Never => false,
        };
        if tells {
            self.tell_hold();
        }
    }

    fn tell_hold(&mut self) {
        if let Some(held) = self.st.held_moved(true) {
            self.held = held;
        }
    }

    fn ledgered(&mut self, rule: &'static str) {
        self.row_ms += 60_000;
        self.st.ledgered(rule, self.row_ms);
        if matches!(
            self.hold_teller,
            HoldTeller::Loop | HoldTeller::AtPointsOnly
        ) {
            self.tell_hold();
        }
    }

    fn yields(&mut self, said: &'static str, at: Seen, worked: Duration) {
        self.st.took();
        self.ends(said, at, worked);
    }

    fn ends(&mut self, said: &'static str, at: Seen, worked: Duration) {
        self.now += worked;
        self.said = said;
        self.at = Seen { busy: false, ..at };
        self.st.observe(&point(said, at, Some(worked)), self.now);
        self.tell(false);
    }
}

fn face(st: &State) -> TaskFace {
    if st["other"] == 1 {
        TaskFace::Open
    } else if st["left"] == 1 {
        TaskFace::Theirs
    } else if st["landed"] == 1 {
        TaskFace::Landed
    } else {
        TaskFace::Open
    }
}

fn hold_of(held: Option<LandHeld>) -> (i64, i64) {
    match held {
        None => (0, 0),
        Some(h) => {
            assert!(!h.holds(HoldLeg::Cut), "a reply read whole cut: {h:?}");
            (if h.read { 2 } else { 1 }, i64::from(h.legs))
        }
    }
}

fn holds(real: &Real, st: &State) -> Result<(), String> {
    if hold_of(real.held) == (st["hold"], st["hlegs"]) {
        Ok(())
    } else {
        Err(format!(
            "{:?} told the hold {:?} where the model says hold={} hlegs={} at {st:?}",
            real.hold_teller, real.held, st["hold"], st["hlegs"]
        ))
    }
}

fn told(st: &State) -> TaskFace {
    match st["told"] {
        0 => TaskFace::Open,
        1 => TaskFace::Landed,
        _ => TaskFace::Theirs,
    }
}

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

fn projects(real: &TurnEndState, st: &State) {
    let done = st["landed"] == 1 || st["left"] == 1;
    assert_eq!(real.landed(), st["landed"] == 1, "the landing at {st:?}");
    assert_eq!(
        real.left_to_person(),
        st["left"] == 1,
        "the person's exchange at {st:?}"
    );
    assert_eq!(real.task_done(), done, "the task's end at {st:?}");
    assert_eq!(
        i64::from(real.done_reports()),
        if done { 2 } else { i64::from(st["said"] > 0) },
        "the done claim at {st:?}"
    );
    assert_eq!(
        real.land_episode().0,
        st["typed"] == 1,
        "the instruction typed at {st:?}"
    );
    assert_eq!(
        !real.standing_needs().is_empty(),
        st["need"] == 1,
        "the needs at {st:?}"
    );
    assert_eq!(
        !real.open_items().is_empty(),
        st["said"] == 3 && st["walled"] == 0 && !done,
        "the open work at {st:?}"
    );
    assert_eq!(
        real.repo_owed().is_some(),
        st["dirty"] == 1,
        "the repository's work at {st:?}"
    );
    assert_eq!(
        real.short_streak() > 0,
        st["short"] == 1,
        "the streak at {st:?}"
    );
}

fn mirror(action: &str, before: &State, real: &mut Real) {
    let plain = Seen::default();
    match action {
        "Instruction" | "Nudge" | "Answer" | "WallAct" => {
            let a = real.decide();
            assert!(
                !matches!(verdict(&a), Verdict::Nothing | Verdict::Wait),
                "{action} at {before:?}: {a:?}"
            );
            let r = point(real.said, real.at, None);
            real.st.acted(&a, &r, real.now);
            if let TurnEndAction::Type { rule_id, .. } = &a {
                real.ledgered(rule_id);
            }
        }
        "BackoffDue" => real.now += real.st.short_wait().expect("a back-off runs"),
        "YieldWork" => real.yields(PROGRESS, plain, LONG),
        "YieldAsk" => real.yields(QUESTION, plain, SHORT),
        "YieldWall" => real.yields(
            PROGRESS,
            Seen {
                walled: true,
                ..plain
            },
            LONG,
        ),
        "YieldClaim" => real.yields(TOKEN, plain, LONG),
        "YieldDefense" => real.yields(DEFENSE, plain, LONG),
        "YieldDefenseDirty" => real.yields(
            DEFENSE,
            Seen {
                dirty: true,
                ..plain
            },
            LONG,
        ),
        "YieldDefenseNeeds" => real.yields(NEEDS, plain, LONG),
        "YieldOpenDefense" => real.yields(OPEN_DEFENSE, plain, LONG),
        "OtherTurnRuns" => {
            real.st.took();
            real.at.busy = true;
            real.tell(true);
        }
        "HumanWork" => real.ends(PROGRESS, plain, JOB),
        "OtherTurnDefense" => real.ends(DEFENSE, plain, JOB),
        "OtherTurnWall" => real.ends(
            PROGRESS,
            Seen {
                walled: true,
                ..plain
            },
            JOB,
        ),
        "PersonExchange" => real.ends(
            ANSWERED,
            Seen {
                keyed: true,
                ..plain
            },
            SHORT,
        ),
        "SeedLanded" => {
            assert!(real.st.seed_task_done(), "taken on at {before:?}");
            real.tell(false);
        }
        other => panic!("an action this bind does not mirror: {other}"),
    }
}

fn start(base: Instant, teller: Teller, hold_teller: HoldTeller) -> Real {
    let mut real = Real {
        st: TurnEndState::default(),
        now: base,
        said: PROGRESS,
        at: Seen::default(),
        told: None,
        teller,
        noted: false,
        held: None,
        hold_teller,
        row_ms: 1_760_000_000_000,
    };
    real.told = Some(real.st.face_start());
    if hold_teller != HoldTeller::Never {
        real.tell_hold();
    }
    real.st
        .observe(&point(PROGRESS, Seen::default(), Some(LONG)), real.now);
    real.tell(false);
    real
}

fn walk(m: &Model, path: &[&str], base: Instant) -> (State, Real) {
    let mut st = m.init_state();
    let mut real = start(base, Teller::Loop, HoldTeller::Loop);
    for action in path {
        let before = st.clone();
        assert!(m.fire(action, &mut st), "{action} at {before:?}");
        mirror(action, &before, &mut real);
    }
    (st, real)
}

fn walk_all(
    m: &Model,
    teller: Teller,
    hold_teller: HoldTeller,
) -> Result<BTreeMap<String, usize>, String> {
    let base = Instant::now() + Duration::from_secs(3600);
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), start(base, teller, hold_teller))]);
    let mut verdicts = BTreeMap::<String, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        for inv in &m.invariants {
            assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
        }
        faces(&real, &st)?;
        holds(&real, &st)?;
        *verdicts
            .entry(format!("hold={} hlegs={}", st["hold"], st["hlegs"]))
            .or_default() += 1;
        if st["pending"] == 0 {
            projects(&real.st, &st);
            if m.action_enabled("SeedLanded", &st) {
                assert!(real.st.clone().seed_task_done(), "refused at {st:?}");
            }
            if st["walled"] == 1 {
                assert!(!real.st.clone().seed_task_done(), "taken at a wall: {st:?}");
            }
            let a = real.clone().decide();
            let got = verdict(&a);
            assert_eq!(
                got,
                expected(m, &st),
                "the real decider disagrees at {st:?}"
            );
            if let TurnEndAction::Type { text, .. } = &a
                && got == Verdict::Nudge
                && !real.st.needs_ack_owed()
            {
                assert_eq!(*text, nudge(&st, &real.st), "the nudge at {st:?}");
            }
            if st["landed"] == 0 && st["left"] == 0 && st["other"] == 0 {
                assert_ne!(got, Verdict::Nothing, "a silent point at {st:?}");
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
            faces(&r, &next)?;
            holds(&r, &next)?;
            queue.push_back((next, r));
        }
    }
    Ok(verdicts)
}

#[test]
fn tier1_the_real_decider_lands_only_on_a_clean_defense_and_never_goes_quiet() {
    assert!(clean_defense(Some(DEFENSE)));
    assert!(!clean_defense(Some(NEEDS)) && !clean_defense(Some(OPEN_DEFENSE)));
    assert!(!clean_defense(Some(TOKEN)) && says_done(Some(TOKEN)));
    assert!(open_work_strict(Some(TOKEN)).is_empty());
    assert!(!says_done(Some(PROGRESS)) && !says_done(Some(ANSWERED)));

    let m = supervisor_land_model();
    let verdicts = walk_all(&m, Teller::Loop, HoldTeller::Loop)
        .unwrap_or_else(|e| panic!("the face or the hold told: {e}"));
    for word in [
        "Instruction",
        "Nudge",
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

#[test]
fn tier1_the_buggy_model_and_three_tellers_are_refuted() {
    let m = supervisor_land_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let base = Instant::now() + Duration::from_secs(3600);

    for teller in [
        Teller::OnEndNotes,
        Teller::AtPointsOnly,
        Teller::LandedForDone,
    ] {
        let missed = walk_all(&m, teller, HoldTeller::Loop).expect_err("a face teller passed");
        assert!(missed.starts_with(&format!("{teller:?} told ")), "{missed}");
    }
    let path = ["Instruction", "YieldDefense", "OtherTurnRuns"];
    let mut right = m.init_state();
    let mut wrong = buggy.init_state();
    for action in path {
        assert!(m.fire(action, &mut right), "{action} at {right:?}");
        assert!(buggy.fire(action, &mut wrong), "{action} at {wrong:?}");
    }
    assert!(m.check_invariant("TheHostIsToldTheTasksFace", &right));
    assert!(!buggy.check_invariant("TheHostIsToldTheTasksFace", &wrong));

    let mut path = vec!["Instruction", "YieldOpenDefense", "BackoffDue"];
    for _ in 0..2 {
        path.extend(["Nudge", "YieldOpenDefense", "BackoffDue"]);
    }
    let mut b = buggy.init_state();
    for action in &path {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(buggy.fire("SpentAfterTwo", &mut b), "{b:?}");
    assert!(!buggy.check_invariant("LandNeverGoesQuiet", &b));
    assert!(!buggy.action_enabled("Nudge", &b));
    let (_, real) = walk(&m, &path, base);
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nudge);
    for _ in 0..7 {
        path.extend(["Nudge", "YieldOpenDefense", "BackoffDue"]);
    }
    let (_, real) = walk(&m, &path, base);
    assert_eq!(real.st.land_episode(), (true, 9));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nudge);

    let path = ["Instruction", "YieldDefenseNeeds", "BackoffDue"];
    let (st, real) = walk(&m, &path, base);
    let mut b = st.clone();
    assert!(buggy.fire("AskHolds", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("ANeedNeverBlocks", &b));
    assert!(!buggy.action_enabled("Nudge", &b));
    assert!(!real.st.landed());
    let a = real.clone().decide();
    assert_eq!(verdict(&a), Verdict::Nudge);
    assert!(
        matches!(&a, TurnEndAction::Type { text, .. }
            if text.contains(aterm_agent::supervise::policy::needs::NEEDS_ACK)),
        "{a:?}"
    );

    for yielded in ["YieldClaim", "YieldDefenseDirty", "YieldDefenseNeeds"] {
        let mut b = buggy.init_state();
        for action in ["Instruction", yielded] {
            assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
        }
        assert_eq!(b["landed"], 1, "{yielded}: {b:?}");
        assert!(
            !buggy.check_invariant("LandedOnlyByCleanDefense", &b),
            "{yielded}"
        );
        let (_, real) = walk(&m, &["Instruction", yielded, "BackoffDue"], base);
        assert!(!real.st.landed() && !real.st.task_done(), "{yielded}");
        assert_eq!(verdict(&real.clone().decide()), Verdict::Nudge, "{yielded}");
    }
    for path in [
        &["Instruction", "YieldDefense"][..],
        &["Instruction", "YieldWall", "WallAct", "YieldDefense"][..],
    ] {
        let (_, real) = walk(&m, path, base);
        assert!(real.st.landed(), "{path:?}");
        assert_eq!(
            verdict(&real.clone().decide()),
            Verdict::Nothing,
            "{path:?}"
        );
    }

    let (st, real) = walk(
        &m,
        &["Instruction", "YieldWall", "WallAct", "YieldDefense"],
        base,
    );
    let mut b = st.clone();
    assert!(buggy.fire("Nudge", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("NeverTypeIntoALandedTask", &b));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);

    let (st, mut real) = walk(&m, &["OtherTurnWall"], base);
    let mut b = st.clone();
    assert!(buggy.fire("SeedLanded", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("AWallIsNeverLeftOnALandedTask", &b));
    assert!(!real.st.seed_task_done());
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::WallAct);
}

#[test]
fn tier1_the_real_teller_tells_what_holds_the_landing() {
    let m = supervisor_land_model();
    let seen = walk_all(&m, Teller::Loop, HoldTeller::Loop)
        .unwrap_or_else(|e| panic!("the hold told: {e}"));
    for hold in [
        "hold=0 hlegs=0",
        "hold=1 hlegs=0",
        "hold=2 hlegs=8",
        "hold=2 hlegs=4",
        "hold=2 hlegs=2",
        "hold=2 hlegs=1",
    ] {
        assert!(seen.contains_key(hold), "{hold} never told: {seen:?}");
    }

    for teller in [
        HoldTeller::Never,
        HoldTeller::AtPointsOnly,
        HoldTeller::NotAtActs,
    ] {
        let missed = walk_all(&m, Teller::Loop, teller).expect_err("a hold teller passed");
        assert!(
            missed.starts_with(&format!("{teller:?} told the hold ")),
            "{teller:?}: {missed}"
        );
    }

    assert!(
        aterm_spec::interp::violated_invariants(&m).is_empty(),
        "the model proper breaks a law"
    );
    let stale = aterm_spec::interp::with_consts(&m, &[("StaleHold", 1)]);
    assert_eq!(
        aterm_spec::interp::violated_invariants(&stale),
        BTreeSet::from(["TheHostIsToldWhatHoldsTheLanding"]),
    );
    assert!(m.mutant_dials().contains(&2), "{:?}", m.mutant_dials());
    assert_eq!(
        aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_consts(
            &m,
            &[("Buggy", 2)]
        )),
        BTreeSet::from(["TheHostIsToldWhatHoldsTheLanding"]),
        "the sweep's own dial"
    );
    let mut st = stale.init_state();
    for action in ["Instruction", "YieldOpenDefense"] {
        assert!(stale.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(!stale.check_invariant("TheHostIsToldWhatHoldsTheLanding", &st));
    assert!(stale.check_invariant("TheHostIsToldTheTasksFace", &st));
    let (_, real) = walk(&m, &["Instruction", "YieldOpenDefense"], Instant::now());
    let held = real.held.expect("a hold told");
    assert!(held.read && held.holds(HoldLeg::Open), "{held:?}");
    assert!(
        held.open_words().any(|w| w == "not merged"),
        "the word Land read: {:?}",
        held.open_words().collect::<Vec<_>>()
    );
}
