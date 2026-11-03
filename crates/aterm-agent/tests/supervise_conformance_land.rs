// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `SupervisorLand` (aterm-models-harness `supervisor_land_model`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{HoldLeg, LandHeld, TaskFace};
use aterm_agent::harness::repo_work::{RepoRead, RepoWork};
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::land_check::listed_suffix;
use aterm_agent::supervise::policy::turn_end::{
    Composer, GaveUp, LAND, LAND_ANSWER, LAND_CHECK_HEAD, LAND_CONTRADICTED_HEAD, LAND_NUDGE,
    Outage, RULE_API_CUTOFF, RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_CHECK, RULE_LAND_NUDGE, Reach,
    TurnEndAction, TurnEndReading, TurnEndState, UNREACHABLE_HOLD, WaitKind, clean_defense,
    decide_turn_end, land_contradicted, open_work_strict, repo_work_suffix, says_done,
};
use aterm_agent::supervise::{Autopilot, SupervisorConfig};
use aterm_models_harness::supervisor_land_model;
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

/// Real work: it runs tools (a tool row on the screen, which at a wall is what tells work from
/// the program's own retries).
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
/// The land check's answers: its one landing word, a list, and a need alone.
const CHECKED: &str = aterm_agent::supervise::policy::land_check::LANDED;
const LIST: &str = "The parser's branch is not merged yet.";
const NEED_ONLY: &str = "NEEDS YOU: the release credential, to sign the build";
/// A managed worker's own failed conclusion.
const COULD_NOT_FINISH: &str =
    "Could not finish: the grammar the parser needs was deleted upstream.";

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
        ..RepoWork::default()
    }
}

/// What an authoritative point under Land saw: its turn's earlier words are [`crate::common::earlier`]'s `earlier`.
#[derive(Clone, Copy, Default)]
struct Seen {
    walled: bool,
    busy: bool,
    dirty: bool,
    keyed: bool,
    earlier: usize,
}

/// `reach`: what the host measured of the API's reach, as the loop reads it there.
fn point(said: &str, at: Seen, worked: Option<Duration>, reach: Reach) -> TurnEndReading {
    TurnEndReading {
        phase: if at.busy { Phase::Busy } else { Phase::Idle },
        authoritative: true,
        survey: false,
        wall: at.walled.then_some(OVERLONG),
        wall_message: if at.walled {
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
        turn_said: Some(crate::common::earlier(at.earlier)),
        said_lines: None,
        turn_lines: None,
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
        key: at.keyed.then_some(KEYED),
        login_back: false,
        auto_continue: None,
        since_limit: None,
        served_in_transcript: false,
        chosen_in_transcript: None,
        conversation: aterm_agent::supervise::policy::turn_end::Conversation::Hostless,
        notice_elsewhere: false,
        reset_clock: None,
        reach,
        witness: None,
        program: aterm_phase::Program::Claude,
        seat: Default::default(),
        account: None,
        autopilot: Autopilot::Land,
        login_label: None,
        autopilot_by_person: false,
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
        repo: aterm_agent::harness::repo_work::RepoRead::of_found(at.dirty.then(finding)),
        fact: None,
        wake: None,
    }
}

/// What the real decider says at a point, as a model word.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Instruction,
    Check,
    Nudge,
    Answer,
    WallAct,
    Wait,
    /// The measured outage holds the act ([`UNREACHABLE_HOLD`]).
    Held,
    Nothing,
}

fn verdict(a: &TurnEndAction) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND => {
            assert!(text.starts_with(LAND), "the instruction's words: {text}");
            Verdict::Instruction
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND_CHECK => {
            assert!(
                text.starts_with(LAND_CHECK_HEAD),
                "the land check's words: {text}"
            );
            Verdict::Check
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND_NUDGE => {
            assert!(
                text.starts_with(LAND_NUDGE) || text.starts_with(LAND_CONTRADICTED_HEAD),
                "the nudge's words, or its contradiction's: {text}"
            );
            Verdict::Nudge
        }
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LAND_ANSWER => {
            assert!(
                text.starts_with(LAND_ANSWER),
                "the land answer's words: {text}"
            );
            Verdict::Answer
        }
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

/// The acts a measured outage holds (`down` = 1, `capped` = 0: its cap not yet passed).
const HELD: [&str; 4] = ["Instruction", "Check", "Nudge", "Answer"];

fn expected(m: &Model, st: &State) -> Verdict {
    for (action, word) in [
        ("Instruction", Verdict::Instruction),
        ("Check", Verdict::Check),
        ("Nudge", Verdict::Nudge),
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

/// Land's nudge as the model's point says it: the agent's own list quoted where it answered the
/// check with one (never the word reader's open words), then the repository's facts; where the
/// reply standing is a `Nothing left:` defense over the repository's work, its contradiction
/// (slice 4c), git's finding leading.
fn nudge(st: &State) -> String {
    if matches!(st["said"], 2 | 3) && st["dirty"] == 1 {
        return land_contradicted(&RepoRead::Found(finding()), None, &[], None).expect("git's leg");
    }
    let mut text = LAND_NUDGE.to_string();
    if st["said"] == 5 {
        text.push(' ');
        text.push_str(&listed_suffix(LIST).expect("the list quoted"));
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
    /// Landed for any done task — the person's exchange that opens the session among them.
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
    /// Since when the host measured the API down, while it does.
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

    /// The decision at the point now, the wall's own wait run through (its ladder's clock,
    /// which the model does not count), never the measured outage's hold, which its `NetUp` or
    /// `OutageCapDue` ends.
    fn decide(&mut self) -> TurnEndAction {
        let r = point(
            self.said,
            Seen {
                keyed: false,
                ..self.at
            },
            None,
            self.reach(),
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

    /// The measured outage's cap passes (`OutageCapDue`): the clock moves to when the hold of
    /// the outage standing ends (`TurnEndTiming::outage_holds_until`), the reach still Down.
    fn outage_cap_passes(&mut self) {
        let since = self.down.expect("an outage stands");
        self.now = self.now.max(since + self.st.timing.outage_cap);
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

    /// A turn that ended as [`Self::ends`]'s, its earlier words the last's.
    fn yields_again(&mut self, said: &'static str, worked: Duration) {
        self.st.took();
        self.ended(said, self.at, worked);
    }

    /// A turn that ended on `said`, its earlier words its own.
    fn ends(&mut self, said: &'static str, at: Seen, worked: Duration) {
        let earlier = self.at.earlier + 1;
        self.ended(said, Seen { earlier, ..at }, worked);
    }

    fn ended(&mut self, said: &'static str, at: Seen, worked: Duration) {
        self.now += worked;
        self.said = said;
        self.at = Seen { busy: false, ..at };
        self.st
            .observe(&point(said, at, Some(worked), self.reach()), self.now);
        self.tell(false);
    }
}

fn face(st: &State) -> TaskFace {
    if st["other"] == 1 {
        TaskFace::Open
    } else if st["landed"] == 1 {
        TaskFace::Landed
    } else if st["failed"] == 1 {
        TaskFace::Failed
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
        2 => TaskFace::Failed,
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
    let done = st["landed"] == 1 || st["failed"] == 1;
    assert_eq!(
        real.gave_up().is_some(),
        st["failed"] == 1,
        "the failed conclusion at {st:?}"
    );
    assert_eq!(real.landed(), st["landed"] == 1, "the landing at {st:?}");
    // What a relaunch's owed carry-on reads (`IdlePoint::done_under_land`): it types nothing at
    // either end, so a task the model has concluded is never typed into by the host.
    assert_eq!(real.concluded(), done, "what a carry-on reads at {st:?}");
    // Every walk begins after a point, so no person's exchange here opens the session: Land
    // carries each one on (owner ruling 2026-10-27).
    assert!(
        !real.left_to_person(),
        "a person's exchange left to them at {st:?}"
    );
    assert_eq!(real.task_done(), done, "the task's end at {st:?}");
    assert_eq!(
        i64::from(real.done_reports()),
        if done {
            2
        } else {
            i64::from(matches!(st["said"], 1..=3))
        },
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
        "the reader's open work at {st:?}"
    );
    if st["said"] == 5 && st["walled"] == 0 {
        assert_eq!(real.land_listed(), Some(LIST), "the agent's list at {st:?}");
    }
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
        "NetDown" => real.down = Some(real.now),
        "NetUp" => real.down = None,
        "OutageCapDue" => real.outage_cap_passes(),
        "Instruction" | "Check" | "Nudge" | "Answer" | "WallAct" => {
            let a = real.decide();
            assert!(
                !matches!(
                    verdict(&a),
                    Verdict::Nothing | Verdict::Wait | Verdict::Held
                ),
                "{action} at {before:?}: {a:?}"
            );
            let r = point(real.said, real.at, None, real.reach());
            real.st.acted(&a, &r, real.now);
            if let TurnEndAction::Type { rule_id, .. } = &a {
                real.ledgered(rule_id);
            }
        }
        "BackoffDue" => real.now += real.st.short_wait().expect("a back-off runs"),
        "YieldWork" => real.yields(PROGRESS, plain, LONG),
        "YieldAsk" => real.yields(QUESTION, plain, SHORT),
        "YieldAskSame" => real.yields_again(QUESTION, SHORT),
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
        "YieldLanded" => real.yields(CHECKED, plain, SHORT),
        "YieldLandedDirty" => real.yields(
            CHECKED,
            Seen {
                dirty: true,
                ..plain
            },
            SHORT,
        ),
        "YieldList" => real.yields(LIST, plain, SHORT),
        "YieldNeedOnly" => real.yields(NEED_ONLY, plain, SHORT),
        // The word counts for a managed worker alone, and a manager changes the back-off's pacing,
        // which the model's other walks do not carry.
        "YieldCouldNotFinish" => {
            real.st.set_managed(true);
            real.yields(COULD_NOT_FINISH, plain, LONG);
            real.st.set_managed(false);
        }
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
        down: None,
        blind: false,
    };
    real.told = Some(real.st.face_start());
    if hold_teller != HoldTeller::Never {
        real.tell_hold();
    }
    real.st.observe(
        &point(PROGRESS, Seen::default(), Some(LONG), Reach::Unknown),
        real.now,
    );
    real.tell(false);
    real
}

/// A walk's real side before any point is judged: no turn has ended under the loop yet.
fn start_unjudged(base: Instant, teller: Teller) -> Real {
    let mut real = Real {
        st: TurnEndState::default(),
        now: base,
        said: PROGRESS,
        at: Seen::default(),
        told: None,
        teller,
        noted: false,
        held: None,
        hold_teller: HoldTeller::Loop,
        row_ms: 1_760_000_000_000,
        down: None,
        blind: false,
    };
    real.told = Some(real.st.face_start());
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
    let mut seen: BTreeSet<Vec<i64>> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), start(base, teller, hold_teller))]);
    let mut verdicts = BTreeMap::<String, usize>::new();
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &st)) {
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
            assert_eq!(
                crate::common::same(&real.st),
                st["same"],
                "the same reply at {st:?}"
            );
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
                assert_eq!(*text, nudge(&st), "the nudge at {st:?}");
            }
            if st["landed"] == 0
                && st["failed"] == 0
                && st["other"] == 0
                && st["rest"] == 0
                && st["same"] <= 1
            {
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
fn tier1_the_real_decider_lands_only_on_its_checks_landed_and_never_goes_quiet() {
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
        "Check",
        "Nudge",
        "Answer",
        "WallAct",
        "Wait",
        "Held",
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

    for teller in [Teller::OnEndNotes, Teller::AtPointsOnly] {
        let missed = walk_all(&m, teller, HoldTeller::Loop).expect_err("a face teller passed");
        assert!(missed.starts_with(&format!("{teller:?} told ")), "{missed}");
    }
    // A done task that is no landing is, under Land, the person's exchange that opens the session
    // alone (owner ruling 2026-10-27), which the model's walks, begun after a point, never reach:
    // a decider no point was judged in yet tells it theirs, and `LandedForDone` tells it landed.
    for (teller, told) in [
        (Teller::Loop, TaskFace::Theirs),
        (Teller::LandedForDone, TaskFace::Landed),
    ] {
        let mut real = start_unjudged(base, teller);
        real.ends(
            ANSWERED,
            Seen {
                keyed: true,
                ..Seen::default()
            },
            SHORT,
        );
        assert!(real.st.left_to_person(), "{teller:?}: the opening exchange");
        assert_eq!(
            real.told,
            Some(told),
            "{teller:?}: the opening exchange's face"
        );
    }
    let path = [
        "Instruction",
        "YieldDefense",
        "Check",
        "YieldLanded",
        "OtherTurnRuns",
    ];
    let mut right = m.init_state();
    let mut wrong = buggy.init_state();
    for action in path {
        assert!(m.fire(action, &mut right), "{action} at {right:?}");
        assert!(buggy.fire(action, &mut wrong), "{action} at {wrong:?}");
    }
    assert!(m.check_invariant("TheHostIsToldTheTasksFace", &right));
    assert!(!buggy.check_invariant("TheHostIsToldTheTasksFace", &wrong));

    let mut path = vec!["Instruction", "YieldWork"];
    for _ in 0..2 {
        path.extend(["Nudge", "YieldWork"]);
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
        path.extend(["Nudge", "YieldWork"]);
    }
    let (_, real) = walk(&m, &path, base);
    assert_eq!(real.st.land_episode(), (true, 9));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nudge);

    let path = ["Instruction", "YieldDefenseNeeds"];
    let (st, real) = walk(&m, &path, base);
    let mut b = st.clone();
    assert!(buggy.fire("AskHolds", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("ANeedNeverBlocks", &b));
    assert!(!buggy.action_enabled("Check", &b));
    assert!(!real.st.landed());
    let a = real.clone().decide();
    assert_eq!(verdict(&a), Verdict::Check, "a need never bars the check");
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
        assert!(!buggy.check_invariant("LandedOnlyByAudit", &b), "{yielded}");
        // A defense beside a need owes the check, which waits no back-off.
        let path: &[&str] = if yielded == "YieldDefenseNeeds" {
            &["Instruction", yielded]
        } else {
            &["Instruction", yielded, "BackoffDue"]
        };
        let (_, real) = walk(&m, path, base);
        assert!(!real.st.landed() && !real.st.task_done(), "{yielded}");
        let want = if yielded == "YieldDefenseNeeds" {
            Verdict::Check
        } else {
            Verdict::Nudge
        };
        assert_eq!(verdict(&real.clone().decide()), want, "{yielded}");
    }
    for path in [
        &["Instruction", "YieldDefense", "Check", "YieldLanded"][..],
        &[
            "Instruction",
            "YieldWall",
            "WallAct",
            "YieldDefense",
            "Check",
            "YieldLanded",
        ][..],
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
        &[
            "Instruction",
            "YieldWall",
            "WallAct",
            "YieldDefense",
            "Check",
            "YieldLanded",
        ],
        base,
    );
    let mut b = st.clone();
    assert!(buggy.fire("Nudge", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("NeverTypeIntoALandedTask", &b));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);

    let again = [
        "Instruction",
        "YieldAsk",
        "BackoffDue",
        "Answer",
        "YieldAskSame",
    ];
    let (st, real) = walk(&m, &again, base);
    let same = aterm_spec::interp::with_buggy(&m, 3);
    let mut b = st.clone();
    assert!(
        !m.action_enabled("BackoffDue", &st)
            && same.fire("BackoffDue", &mut b)
            && same.fire("Answer", &mut b),
        "{st:?}"
    );
    assert!(!same.check_invariant("ANoWorkReplyGetsOneMoreActAtMost", &b));
    assert_eq!(verdict(&real.clone().decide()), Verdict::Nothing);
    let (_, real) = walk(
        &m,
        &[&again[..4], &["YieldAsk", "BackoffDue"]].concat(),
        base,
    );
    assert_eq!(verdict(&real.clone().decide()), Verdict::Answer);

    // Buggy = 4 holds the untyped instruction back for a done claim's back-off (v6 §14.10).
    let late = aterm_spec::interp::with_buggy(&m, 4);
    let claims = [
        "OtherTurnWall",
        "WallAct",
        "YieldAsk",
        "Answer",
        "YieldClaim",
    ];
    let (st, real) = walk(&m, &claims, base);
    assert!(
        st["typed"] == 0
            && st["backoff"] == 1
            && m.action_enabled("Instruction", &st)
            && !m.action_enabled("BackoffDue", &st),
        "{st:?}"
    );
    assert!(real.st.short_wait().is_some(), "a back-off stands: {st:?}");
    assert_eq!(verdict(&real.clone().decide()), Verdict::Instruction);
    let mut b = st.clone();
    assert!(
        !late.action_enabled("Instruction", &b) && late.fire("BackoffDue", &mut b),
        "{st:?}"
    );
    assert!(!late.check_invariant("TheInstructionWaitsNoBackOff", &b));
    assert!(m.mutant_dials().contains(&4), "{:?}", m.mutant_dials());
    assert_eq!(
        aterm_spec::interp::violated_invariants(&late),
        BTreeSet::from(["TheInstructionWaitsNoBackOff"]),
    );
    let asks = ["OtherTurnWall", "WallAct", "YieldAsk", "Answer", "YieldAsk"];
    let (st, real) = walk(&m, &asks, base);
    assert!(
        st["typed"] == 0 && !m.action_enabled("Answer", &st) && m.action_enabled("BackoffDue", &st),
        "Land's answer keeps its back-off: {st:?}"
    );
    assert_eq!(verdict(&real.clone().decide()), Verdict::Wait);

    let (st, mut real) = walk(&m, &["OtherTurnWall"], base);
    let mut b = st.clone();
    assert!(buggy.fire("SeedLanded", &mut b), "{st:?}");
    assert!(!buggy.check_invariant("AWallIsNeverLeftOnALandedTask", &b));
    assert!(!real.st.seed_task_done());
    assert!(!real.st.task_done());
    assert_eq!(verdict(&real.clone().decide()), Verdict::WallAct);
}

/// A MEASURED OUTAGE HOLDS THE LAND ACT AND ITS RETURN OR ITS CAP TYPES IT: where the model's
/// `down` holds Instruction, Nudge or Answer, the real decider waits on the outage
/// ([`UNREACHABLE_HOLD`]), and once `NetUp` fires, or `OutageCapDue` with the reach still Down,
/// it types the act it held. NEGATIVE CONTROLS: the `Buggy = 5` model, which types into the
/// outage before its cap, breaks NoContinuationIsTypedIntoAMeasuredOutageBeforeItsCap (and
/// nothing else), and a decider whose readings ignore the Down types the very act the model
/// holds. Run against a decider with no `outage_hold`, this test and the full walk fail; against
/// one whose hold has no cap (run: the `outage_cap` passing changes nothing), the cap's leg here
/// and the full walk fail.
#[test]
fn tier1_a_measured_outage_holds_the_land_act_and_its_return_types_it() {
    let m = supervisor_land_model();
    let mistyped = aterm_spec::interp::with_buggy(&m, 5);
    assert!(m.mutant_dials().contains(&5), "{:?}", m.mutant_dials());
    assert_eq!(
        aterm_spec::interp::violated_invariants(&mistyped),
        BTreeSet::from(["NoContinuationIsTypedIntoAMeasuredOutageBeforeItsCap"]),
    );
    let base = Instant::now() + Duration::from_secs(3600);
    let worked = ["Instruction", "YieldWork", "NetDown"];
    let asked = ["Instruction", "YieldAsk", "BackoffDue", "NetDown"];
    for (path, act, word) in [
        (&["NetDown"][..], "Instruction", Verdict::Instruction),
        (&worked[..], "Nudge", Verdict::Nudge),
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
    let listed = ["Instruction", "YieldOpenDefense", "Check", "YieldList"];
    let mut st = stale.init_state();
    for action in listed {
        assert!(stale.fire(action, &mut st), "{action} at {st:?}");
    }
    assert!(!stale.check_invariant("TheHostIsToldWhatHoldsTheLanding", &st));
    assert!(stale.check_invariant("TheHostIsToldTheTasksFace", &st));
    let (_, real) = walk(&m, &listed, Instant::now());
    let held = real.held.expect("a hold told");
    assert!(held.read && held.holds(HoldLeg::Open), "{held:?}");
    assert!(
        held.open_words().any(|w| w == "not merged"),
        "the agent's own list: {:?}",
        held.open_words().collect::<Vec<_>>()
    );
    let (_, real) = walk(&m, &["Instruction", "YieldOpenDefense"], Instant::now());
    let held = real.held.expect("a hold told");
    assert!(
        held.read && !held.holds(HoldLeg::Open),
        "the reader's words hold nothing: {held:?}"
    );
}

/// THE LAND CHECK'S LAWS, each broken by its own dial (the mutants `non_vacuity` sweeps), and the
/// real decider kept off each mutant's step.
#[test]
fn tier1_each_check_mutant_breaks_its_law_and_the_real_decider_takes_no_such_step() {
    let m = supervisor_land_model();
    let base = Instant::now() + Duration::from_secs(3600);
    let dial = |n: i64| aterm_spec::interp::with_consts(&m, &[("Buggy", n)]);
    let fire_all = |model: &Model, path: &[&str]| {
        let mut st = model.init_state();
        for action in path {
            assert!(model.fire(action, &mut st), "{action} at {st:?}");
        }
        st
    };
    for (n, law) in [
        (6, "LandedOnlyByAudit"),
        (7, "OneCheckPerDefense"),
        (8, "NoCheckOverDirty"),
        (9, "ANeedRestTypesNothing"),
        (10, "ALandedKeepsItsNeed"),
    ] {
        assert!(m.mutant_dials().contains(&n), "{n}: {:?}", m.mutant_dials());
        assert!(
            aterm_spec::interp::violated_invariants(&dial(n)).contains(law),
            "Buggy={n} breaks {law}"
        );
    }
    // 6: the word reader's clean defense lands, unchecked.
    let b = fire_all(&dial(6), &["Instruction", "YieldDefense"]);
    assert!(b["landed"] == 1 && !dial(6).check_invariant("LandedOnlyByAudit", &b));
    let (_, real) = walk(&m, &["Instruction", "YieldDefense"], base);
    assert!(!real.st.landed());
    assert_eq!(verdict(&real.clone().decide()), Verdict::Check);
    // 7: a second check over the defense the first one audited.
    let listed = ["Instruction", "YieldDefense", "Check", "YieldList"];
    let mut b = fire_all(&dial(7), &listed);
    assert!(dial(7).fire("Check", &mut b), "{b:?}");
    assert!(!dial(7).check_invariant("OneCheckPerDefense", &b));
    assert!(!m.action_enabled("Check", &fire_all(&m, &listed)));
    let (_, real) = walk(&m, &listed, base);
    assert_eq!(
        verdict(&real.clone().decide()),
        Verdict::Wait,
        "the list's nudge waits"
    );
    // 8: the check over a dirty repository.
    let mut b = fire_all(&dial(8), &["Instruction", "YieldDefenseDirty"]);
    assert!(dial(8).fire("Check", &mut b), "{b:?}");
    assert!(!dial(8).check_invariant("NoCheckOverDirty", &b));
    let (_, real) = walk(&m, &["Instruction", "YieldDefenseDirty"], base);
    assert_ne!(verdict(&real.clone().decide()), Verdict::Check);
    // 9: an act typed while Land rests on a need.
    let rest = ["Instruction", "YieldDefense", "Check", "YieldNeedOnly"];
    let mut b = fire_all(&dial(9), &rest);
    assert!(dial(9).fire("BackoffDue", &mut b) && dial(9).fire("Nudge", &mut b));
    assert!(!dial(9).check_invariant("ANeedRestTypesNothing", &b));
    let (st, real) = walk(&m, &rest, base);
    assert_eq!(st["rest"], 1);
    assert!(real.st.land_resting());
    assert_eq!(
        verdict(&real.clone().decide()),
        Verdict::Nothing,
        "the rest types nothing"
    );
    // 10: a `LANDED` takes the need down though nothing retracted it: over a dirty repository,
    // where nothing lands, and over a clean one, where the need stands beside the landing.
    for (yields, lands) in [("YieldLandedDirty", false), ("YieldLanded", true)] {
        let path = ["Instruction", "YieldDefenseNeeds", "Check", yields];
        let b = fire_all(&dial(10), &path);
        assert!(b["need"] == 0 && !dial(10).check_invariant("ALandedKeepsItsNeed", &b));
        let (st, real) = walk(&m, &path, base);
        assert_eq!(st["need"], 1, "{yields}");
        assert_eq!(real.st.landed(), lands, "{yields}");
        assert!(
            !real.st.standing_needs().is_empty(),
            "{yields}: the need stands until the agent retracts it"
        );
    }
}

/// A MANAGED WORKER'S `Could not finish:` ENDS THE WORK AND NOTHING IS TYPED INTO IT UNTIL A TURN
/// RUNS. NEGATIVE CONTROLS: the `Buggy = 11` model, which nudges into it, breaks
/// `NeverTypeIntoAFailedTask` (and nothing else), and a session no manager waits on gets no such
/// word, so the same reply there is carried on from like any other. Run against a decider whose
/// `conclude` keeps the books or that reads the word for everyone, this test and the full walk fail.
#[test]
fn tier1_a_worker_that_could_not_finish_rests_failed_until_a_turn_runs() {
    let m = supervisor_land_model();
    let base = Instant::now() + Duration::from_secs(3600);
    let typed_in = aterm_spec::interp::with_buggy(&m, 11);
    assert!(m.mutant_dials().contains(&11), "{:?}", m.mutant_dials());
    assert_eq!(
        aterm_spec::interp::violated_invariants(&typed_in),
        BTreeSet::from(["NeverTypeIntoAFailedTask"]),
    );
    let verdicts = walk_all(&m, Teller::Loop, HoldTeller::Loop)
        .unwrap_or_else(|e| panic!("the face or the hold told: {e}"));
    assert!(verdicts.contains_key("Nothing"), "{verdicts:?}");

    // Each act's answer may be the word, the land check's among them.
    for path in [
        &["Instruction", "YieldCouldNotFinish"][..],
        &["Instruction", "YieldWork", "Nudge", "YieldCouldNotFinish"][..],
        &[
            "Instruction",
            "YieldDefense",
            "Check",
            "YieldCouldNotFinish",
        ][..],
    ] {
        let (st, mut real) = walk(&m, path, base);
        assert_eq!(st["failed"], 1, "{path:?}");
        assert_eq!(real.st.gave_up(), Some(GaveUp::CouldNotFinish), "{path:?}");
        assert!(real.st.task_done() && !real.st.landed(), "{path:?}");
        assert_eq!(real.st.land_held(), None, "nothing holds it: {path:?}");
        assert_eq!(
            real.st.face(),
            TaskFace::Failed,
            "its own face, no landing: {path:?}"
        );
        assert_eq!(verdict(&real.decide()), Verdict::Nothing, "{path:?}");
        for act in ["Instruction", "Check", "Nudge", "Answer", "WallAct"] {
            assert!(!m.action_enabled(act, &st), "{act} at {st:?}");
        }
        let mut wrote = st.clone();
        assert!(typed_in.fire("Nudge", &mut wrote), "{path:?}");
        assert!(!typed_in.check_invariant("NeverTypeIntoAFailedTask", &wrote));
    }

    // A turn of someone else's lifts it: the point is a report again.
    let (st, mut real) = walk(
        &m,
        &["Instruction", "YieldCouldNotFinish", "HumanWork"],
        base,
    );
    assert_eq!((st["failed"], real.st.gave_up()), (0, None));
    assert_eq!(verdict(&real.decide()), Verdict::Nudge);

    // No manager waits: the same reply concludes nothing and Land carries on.
    let (_, mut unmanaged) = walk(&m, &["Instruction"], base);
    unmanaged.yields(COULD_NOT_FINISH, Seen::default(), LONG);
    assert_eq!(unmanaged.st.gave_up(), None);
    assert!(!unmanaged.st.task_done());
    assert_eq!(verdict(&unmanaged.decide()), Verdict::Nudge);
}

/// THE GATES ON THE WORD. A loop's first point is judged before the record is read: the word waits
/// for [`TurnEndState::set_managed`]. Cruise never rests; a reply the person stopped or a wall cut
/// gave no such answer; a point with no turn since is the same reply, not a new word. Each case's
/// negative control is the row before it: the word with the gate open concludes.
#[test]
fn tier1_only_a_managed_land_worker_s_whole_reply_concludes() {
    let now = Instant::now() + Duration::from_secs(3600);
    let reading = |worked| point(COULD_NOT_FINISH, Seen::default(), worked, Reach::Unknown);
    // The first point, managed only after it.
    let mut st = TurnEndState::default();
    st.observe(&reading(Some(LONG)), now);
    assert_eq!(st.gave_up(), None);
    st.set_managed(true);
    assert_eq!(st.gave_up(), Some(GaveUp::CouldNotFinish));
    // Unmanaged for good: nothing, at this point or the next.
    let mut st = TurnEndState::default();
    st.observe(&reading(Some(LONG)), now);
    st.observe(&reading(None), now + SHORT);
    st.set_managed(true);
    assert_eq!(st.gave_up(), None, "the word was this point's alone");
    // Under Cruise, or a reply the person stopped, or one a wall cut.
    for (what, shaped) in [
        (
            "cruise",
            TurnEndReading {
                autopilot: Autopilot::Cruise,
                ..reading(Some(LONG))
            },
        ),
        (
            "interrupted",
            TurnEndReading {
                interrupted: true,
                ..reading(Some(LONG))
            },
        ),
        (
            "cut",
            TurnEndReading {
                said_cut: true,
                ..reading(Some(LONG))
            },
        ),
    ] {
        let mut st = TurnEndState::default();
        st.set_managed(true);
        st.observe(&shaped, now);
        assert_eq!(st.gave_up(), None, "{what}");
    }
    let mut st = TurnEndState::default();
    st.set_managed(true);
    st.observe(&reading(Some(LONG)), now);
    assert_eq!(st.gave_up(), Some(GaveUp::CouldNotFinish), "the gate open");
    // Cruise lifts one a loop before it left standing.
    st.observe(
        &TurnEndReading {
            autopilot: Autopilot::Cruise,
            ..reading(None)
        },
        now + SHORT,
    );
    assert_eq!(st.gave_up(), None, "Cruise never rests");
}

/// A RESTART TAKES THE RECORD'S CONCLUSION AT A POINT NO TURN RAN TO, AND ONLY THERE. NEGATIVE
/// CONTROL: a turn that ran at the first point opens the work again, so the seed is refused.
#[test]
fn tier1_a_failed_record_seeds_only_a_point_no_turn_ran_to() {
    let now = Instant::now() + Duration::from_secs(3600);
    let mut st = TurnEndState::default();
    st.observe(
        &point(COULD_NOT_FINISH, Seen::default(), None, Reach::Unknown),
        now,
    );
    assert!(st.seed_gave_up(GaveUp::Repeated));
    assert_eq!(st.gave_up(), Some(GaveUp::Repeated));
    assert!(st.task_done() && !st.landed());
    let mut st = TurnEndState::default();
    st.observe(
        &point(PROGRESS, Seen::default(), Some(LONG), Reach::Unknown),
        now,
    );
    assert!(!st.seed_gave_up(GaveUp::CouldNotFinish));
    assert_eq!(st.gave_up(), None);
}

/// THE SAME REPLY ENDS A MANAGED WORKER'S WORK ONCE LAND'S STOP TOOK THE POINT: the model's
/// `same == 2`, where the decider types nothing. NEGATIVE CONTROLS: one reply kept stops nothing,
/// and an unmanaged session rests without concluding.
#[test]
fn tier1_a_managed_worker_that_repeats_itself_concludes_at_lands_stop() {
    let m = supervisor_land_model();
    let base = Instant::now() + Duration::from_secs(3600);
    let held = [
        "Instruction",
        "YieldAsk",
        "BackoffDue",
        "Answer",
        "YieldAskSame",
    ];
    let (st, mut real) = walk(&m, &held, base);
    assert_eq!(st["same"], 2);
    assert_eq!(verdict(&real.decide()), Verdict::Nothing);
    let r = point(real.said, real.at, None, real.reach());

    let mut unmanaged = real.clone();
    unmanaged.st.conclude_repeated(&r, unmanaged.now);
    assert_eq!(unmanaged.st.gave_up(), None, "no manager waits on it");

    let mut managed = real.clone();
    managed.st.set_managed(true);
    managed.st.conclude_repeated(&r, managed.now);
    assert_eq!(managed.st.gave_up(), Some(GaveUp::Repeated));
    assert!(managed.st.task_done() && !managed.st.landed());
    assert_eq!(verdict(&managed.decide()), Verdict::Nothing);

    let (st, once) = walk(&m, &held[..2], base);
    assert_eq!(st["same"], 1);
    let mut once = once;
    once.st.set_managed(true);
    let r = point(once.said, once.at, None, once.reach());
    once.st.conclude_repeated(&r, once.now);
    assert_eq!(once.st.gave_up(), None, "one reply kept stops nothing");
}
