// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorCodexRateNudge` (aterm-spec
//! `supervisor_codex_rate_nudge_model`): the REAL approval decider
//! (`decide_screen` on the rate-limit nudge and the `/model` picker), the
//! REAL turn-end decider (`decide_turn_end`, with the real `TurnEndState` it
//! keeps) and the REAL goal stop (`TurnEndState::goal_stop`, what the loop's
//! busy read asks before its Esc — `turn_end_loop`'s `stop_codex_turn`,
//! itself driven over a scripted busy screen in the crate's
//! `turn_end_loop_tests`) driven along EVERY reachable state of the model, so
//! the committed answer to Codex's `Approaching rate limits` — switch only
//! near the limit, stop Codex's goal while switched, save the work once on
//! the cheaper model, put the thread back on its own model, hold until the
//! window resets — is the model's, state for state.
//!
//! The projection: `model` is the footer's first field (`GPT-6-Astra ultra`,
//! or the nudge's `GPT-6-Luna medium`); `goal` and `paused` its right side
//! (`Pursuing goal (…)`, `Goal paused (/goal resume)`); `near` Codex's own
//! usage reading (99% of the weekly window, or 1%); `sandbox` the fall the
//! loop reads off the rollouts; `shown` the nudge box (the HAND-BUILT
//! fixture on the measured list geometry); `turn` a busy reading, its end an
//! idle point with the turn's work; `person` a person's keystroke in the
//! turn (`human_ms`, the moment their turn began on the walk's clock) — at
//! a busy read, the LOOP'S evidence of it: the real `RunningTurn` the loop
//! keeps, driven along the walk as the loop drives it (its span begun at
//! the turn's first busy read, closed at every point, and at the nudge's
//! box — the box marks the end of the turn it covered — its keystrokes
//! floored at the switch's opening), read at the loop's first busy read of
//! the turn and again [`TURN`] into it, past the grace; never the model's
//! bit; `cov` a person's hand in the turn whose end the box covered (their
//! keystroke then, on the walk's clock); `ph` the real switch's phase (none,
//! owed, winding, restore then hold, holding, restore then free); `stops`
//! the real switch's stops; `over` the save's own turn run past the real
//! `WIND_DOWN_BOUND` on the walk's clock (its span the loop's). Every other
//! turn runs [`TURN`] on the walk's clock.
//!
//! Each model action is mirrored on the real side: a turn's end is the point
//! `observe`d (behind the box when the nudge shows it, observed once the box
//! is answered); `PressSwitch` is the real `open_switch` of the switch the
//! press made; `StopGoal` is the real goal stop's Esc on the running turn
//! (its interrupted point observed) or the real decider's `/goal pause` at a
//! free point; `StopWindDown` is the real goal stop's one Esc on the save's
//! own turn past its bound, its interrupted point observed (the marker
//! judged there); `Tell` the real stop's note; the turn-end policy's actions are
//! the real decider's own acts, recorded with `acted`; `Restore` is the real
//! `/model`, the approval decider's picks through the MEASURED 0.158.0
//! picker boxes, and the point showing the thread's own model again.
//!
//! At every state the invariants hold, the real switch's phase is the
//! model's, and: at the nudge, the real approval decider presses `Switch to
//! gpt-6-luna` exactly where `PressSwitch` is enabled, `Keep current model`
//! exactly where `PressKeep` is, and never the never-show-again; at a busy
//! read, the real goal stop sends its Esc exactly where `StopGoal` or
//! `StopWindDown` is enabled, tells exactly where `Tell` is, and stops
//! nothing else (the wind-down's own turn within its bound, a person's, a
//! switch a person took over); at a free
//! point, the real turn-end decider types the wind-down, `/model`, `/goal
//! pause`, the resume or a continuation exactly where `TypeWindDown`,
//! `Restore`, `StopGoal`, `Resume` or `Continue` is enabled, tells a person
//! exactly where `Tell` is, and otherwise types nothing. A HOST RESTART at
//! every open phase — the loop's ledger rows written, read back with
//! `open_wind_down` and seeded into a fresh state — decides the same and
//! tells the same (the notes said ride the rows), and so does one taken
//! WHILE the wind-down's or the goal's turn runs, judged at the point that
//! ends it, and one taken between the harness's Esc and the interrupted
//! point it makes (`esc=`), in every phase. NEGATIVE CONTROLS: every `Buggy
//! = 1` branch is enabled somewhere the real deciders answer OTHERWISE (a
//! keep where it switches, a stop where it lets the goal run on, no stop
//! where it stops a person's turn, no continuation where it types one, a
//! restore where it holds, a remembered switch where it forgets, an Esc
//! where a person's hand in the covered turn would spare the goal turn, an
//! Esc where the save's own turn would run on past its bound);
//! the grace-only reading of a person (round 2's defect) stops a person's
//! turn the model protects; round 3's reading — its span carried across
//! the nudge's box, no floor at the switch's opening, the grace latched —
//! spares a goal turn the model stops; and a restart that reads nothing
//! back types `keep going` where the model allows none.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::approvals::{Outcome, Row, open_wind_down, wind_switch_words};
use aterm_agent::supervise::codex_usage::LimitRead;
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    CodexSetting, Composer, GoalStop, PERSON_TURN_SLACK, RULE_CONTINUE, RULE_LIMIT_RESUME,
    RULE_MODEL_RESTORE, RULE_WIND_DOWN, RunningTurn, TurnEndAction, TurnEndReading, TurnEndState,
    TurnEndTiming, WIND_DOWN_BOUND, WindDown, WindEvent, WindPhase, decide_turn_end,
    wind_phase_word,
};
use aterm_agent::supervise::policy::{
    ApprovalCtx, Choice, Decision, NudgeCtx, RULE_MODEL_RESTORE_PICK, RULE_RATE_NUDGE_KEEP,
    RULE_RATE_NUDGE_SWITCH, decide_screen,
};
use aterm_phase::Program;
use aterm_phase::codex::CodexGoal;
use aterm_phase::codex::fixtures as cx;
use aterm_phase::prompt::fixtures::screen;
use aterm_spec::derive::{Model, supervisor_codex_rate_nudge_model};

type State = BTreeMap<&'static str, i64>;

/// A turn's busy work: real work, past every short-turn rule — and past a
/// person's grace, so a keystroke that began one turn is out of the grace
/// by the next.
const TURN: Duration = Duration::from_secs(3 * 60);
/// The original window's reset, as Codex's records name it: days away, so
/// only `Far` (the reading dropping under its limit) ends a hold here.
const BACK: Duration = Duration::from_secs(3 * 24 * 3600);
/// The loop's clock as epoch seconds at the walk's start.
const BASE_UNIX: i64 = 1_790_000_000;
const TO: &str = "gpt-6-luna";
const MARKER: &str = "ATERM-SAVED-3f9a1c2e";

fn own() -> CodexSetting {
    CodexSetting {
        model: "GPT-6-Astra".to_string(),
        effort: Some("ultra".to_string()),
    }
}

fn cheaper() -> CodexSetting {
    CodexSetting {
        model: "GPT-6-Luna".to_string(),
        effort: Some("medium".to_string()),
    }
}

fn goal_of(st: &State) -> Option<CodexGoal> {
    if st["goal"] == 1 {
        Some(CodexGoal::Pursuing)
    } else if st["paused"] == 1 {
        Some(CodexGoal::Paused)
    } else {
        None
    }
}

fn limits(st: &State, back: Instant, now: Instant) -> LimitRead {
    if st["near"] == 1 {
        let ahead = i64::try_from(back.saturating_duration_since(now).as_secs()).unwrap_or(0);
        LimitRead::Near {
            used: 99,
            back_at: Some(BASE_UNIX + ahead),
        }
    } else {
        LimitRead::Far { used: 1 }
    }
}

/// The screen `st` stands for, as the turn-end policy reads it: `worked` the
/// turn that ended here (`None`: the same point read again), `said` the
/// agent's last words, `interrupted` the harness's own Esc's point, `person`
/// how long ago a person last typed (`human_ms`).
fn reading(
    st: &State,
    real: &Real,
    worked: Option<Duration>,
    said: &str,
    interrupted: bool,
    person: Option<Duration>,
) -> TurnEndReading {
    let phase = if st["turn"] == 1 {
        Phase::Busy
    } else if st["shown"] == 1 {
        Phase::Prompt
    } else {
        Phase::Idle
    };
    TurnEndReading {
        phase,
        authoritative: true,
        survey: false,
        wall: None,
        wall_message: String::new(),
        reset_at: None,
        resumes_by_itself: false,
        composer: if st["shown"] == 1 {
            Composer::Absent
        } else {
            Composer::Empty
        },
        said_tail: Some(said.to_string()),
        worked,
        rules: None,
        pending_input: false,
        interrupted,
        restartable: false,
        resume: None,
        upgrading: false,
        taskless: false,
        login_back: false,
        person,
        reach: Default::default(),
        program: Program::Codex,
        goal: goal_of(st),
        model_field: Some(if st["model"] == 0 { own() } else { cheaper() }),
        limits: limits(st, real.back, real.now),
        sandbox_fell: (st["sandbox"] == 1).then(|| "workspace-write".to_string()),
        thread_model: None,
        upgrade_goal: false,
    }
}

/// The real side of one walk.
#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    back: Instant,
    /// A turn ended behind the nudge: its point is observed once the box is
    /// answered (its work the loop's own span of it).
    owed: bool,
    /// The strict walk: a host restart is taken mid-turn at every turn end
    /// of an open switch ([`restart_mid_turn`]).
    strict: bool,
    /// The turn running now as the LOOP keeps it (`Session::running`).
    hand: RunningTurn,
    /// Round 3's reading of the same turn — the regression — kept beside it.
    round3: Round3,
    /// When a person last typed (`status human_ms=`), on the walk's clock.
    typed: Option<Instant>,
}

/// ROUND 3'S READING of a person at a busy read, the re-review's
/// regression: the span taken only at a point — never at the nudge's box —
/// no floor at the switch's opening, and a keystroke within the grace
/// latched for the whole turn.
#[derive(Clone, Copy, Default)]
struct Round3 {
    since: Option<Instant>,
    latched: bool,
}

impl Round3 {
    fn busy(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }
    fn point(&mut self) {
        self.since = None;
        self.latched = false;
    }
    fn person(&mut self, typed: Option<Instant>, now: Instant) -> bool {
        let ago = typed.map(|t| now.saturating_duration_since(t));
        let busy_for = self
            .since
            .map(|s| now.saturating_duration_since(s))
            .unwrap_or_default();
        self.latched |= ago.is_some_and(|a| a < grace() || a <= busy_for + PERSON_TURN_SLACK);
        self.latched
    }
}

impl Real {
    /// How long ago a person last typed, now.
    fn person_ago(&self) -> Option<Duration> {
        self.typed.map(|t| self.now.saturating_duration_since(t))
    }

    /// The loop's point at `self.now`: the turn's work, as the loop's span
    /// measures it — and round 3's span closed with it.
    fn point(&mut self) -> Option<Duration> {
        self.round3.point();
        self.hand.point(self.now)
    }

    fn marker_line(&self) -> String {
        self.st.wind().map_or_else(
            || "Stage done.".to_string(),
            |w| format!("Pushed.\n{}", w.marker),
        )
    }
}

fn timing() -> TurnEndTiming {
    TurnEndTiming {
        // Every turn here is real work, and the first point is acted on at
        // once: the continue policy's back-off is not what is under test.
        min_work: Duration::ZERO,
        short_backoff: Duration::ZERO,
        ..TurnEndTiming::default()
    }
}

/// The real switch's phase, as the model's `ph`.
fn ph_of(st: &TurnEndState) -> i64 {
    match st.wind().map(|w| w.phase) {
        None => 0,
        Some(WindPhase::Owed) => 1,
        Some(WindPhase::Winding) => 2,
        Some(WindPhase::Restore { hold: true }) => 3,
        Some(WindPhase::Holding { .. }) => 4,
        Some(WindPhase::Restore { hold: false }) => 5,
    }
}

fn ctx(real: &Real, st: &State) -> ApprovalCtx {
    ApprovalCtx {
        nudge: NudgeCtx {
            enabled: true,
            open: real.st.switch_open(),
            from: Some(if st["model"] == 0 { own() } else { cheaper() }),
            limits: limits(st, real.back, real.now),
        },
        model_restore: real.st.restore_target(real.now),
        ..ApprovalCtx::new(
            PathBuf::from("/private/tmp/claude-502/scratch/pj"),
            Some(PathBuf::from("/Users/_owner")),
            502,
            None,
        )
    }
}

/// What the real approval decider does with the nudge at `st`.
fn nudge_press(real: &Real, st: &State) -> (&'static str, Choice) {
    let rows = screen(cx::RATE_NUDGE);
    match decide_screen(Some("codex"), &rows, &ctx(real, st)).expect("the nudge") {
        Decision::Approve {
            rule_id, choice, ..
        } => (rule_id, choice),
        other => panic!("the nudge not pressed at {st:?}: {other:?}"),
    }
}

/// `human_grace_s` as the loop reads it.
fn grace() -> Duration {
    Duration::from_secs(u64::from(SupervisorConfig::default().human_grace_s))
}

/// How a busy read reads a person's hand in the running turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hand {
    /// The loop's own: the real `RunningTurn`, floored at the switch's
    /// opening (`turn_end_loop`'s `person_in_this_turn`).
    Loop,
    /// Round 2's defect: the grace alone.
    GraceOnly,
    /// Round 3's regression ([`Round3`]).
    Round3,
}

/// What the real goal stop says over the loop's busy reads of the turn
/// running at `st` — its first, and one [`TURN`] into its span, past the
/// grace — a person's hand read the `hand` way, the turn's busy work the
/// loop's span of it: the first read that stops or tells decides (an Esc on
/// the save's own turn is `StopWindDown`).
fn stop_word(real: &Real, st: &State) -> &'static str {
    stop_word_as(real, st, Hand::Loop)
}

fn stop_word_as(real: &Real, st: &State, hand: Hand) -> &'static str {
    let mut running = real.hand;
    let mut round3 = real.round3;
    running.busy(real.now);
    round3.busy(real.now);
    let floor = real.st.wind().and_then(|w| w.opened_at);
    let winding = real
        .st
        .wind()
        .is_some_and(|w| w.phase == WindPhase::Winding);
    for at in [real.now + Duration::from_secs(1), real.now + TURN] {
        let person = match hand {
            Hand::Loop => running.person(real.typed, floor, None, grace(), at),
            Hand::GraceOnly => real
                .typed
                .is_some_and(|t| at.saturating_duration_since(t) < grace()),
            Hand::Round3 => round3.person(real.typed, at),
        };
        let busy = running.since().map(|s| at.saturating_duration_since(s));
        match real.st.goal_stop(busy, goal_of(st), person, at) {
            Some(GoalStop::Esc) if winding => return "StopWindDown",
            Some(GoalStop::Esc) => return "StopGoal",
            Some(GoalStop::Tell) => return "Tell",
            None => {}
        }
    }
    "none"
}

/// A turn-end act as one of the model's words.
fn word(a: &TurnEndAction, paused: bool) -> &'static str {
    match a {
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_WIND_DOWN => "TypeWindDown",
        TurnEndAction::TypeCommand {
            command, rule_id, ..
        } if *rule_id == RULE_WIND_DOWN && command == "/goal pause" => "StopGoal",
        TurnEndAction::TypeCommand {
            command, rule_id, ..
        } if *rule_id == RULE_MODEL_RESTORE && command == "/model" => "Restore",
        TurnEndAction::TypeCommand {
            command, rule_id, ..
        } if *rule_id == RULE_LIMIT_RESUME && command == "/goal resume" && paused => "Resume",
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_LIMIT_RESUME && !paused => "Resume",
        TurnEndAction::Type { rule_id, .. } if *rule_id == RULE_CONTINUE => "Continue",
        TurnEndAction::WaitUntil { .. } => "wait",
        TurnEndAction::Nothing => "nothing",
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

/// The turn-end policy's typed act the model enables at a free point `st`.
fn harness_act(m: &Model, st: &State) -> Option<&'static str> {
    ["TypeWindDown", "Restore", "StopGoal", "Resume", "Continue"]
        .into_iter()
        .find(|a| m.action_enabled(a, st))
}

/// Whether events carry the note that the goal keeps running.
fn told(events: &[WindEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, WindEvent::GoalNote(n) if n.contains("keeps running")))
}

/// The real decider at the point `st` shows, read again with no work — and
/// whether that look tells a person the goal keeps running.
fn decide_here(real: &Real, st: &State) -> (TurnEndAction, bool) {
    let cfg = SupervisorConfig::default();
    let mut at = real.st.clone();
    let _ = at.take_wind_events();
    let here = reading(st, real, None, "Stage done.", false, None);
    at.observe(&here, real.now);
    let a = decide_turn_end(&at, &here, &cfg, real.now);
    (a, told(&at.take_wind_events()))
}

/// The real `/model` restore: the picker's measured boxes, answered only
/// while the restore is in flight — the thread's own model by Enter, `More
/// reasoning…` by Enter, `Ultra` by `s` (this conversation).
fn picks(real: &Real, st: &State) {
    let c = ctx(real, st);
    assert_eq!(
        c.model_restore,
        Some(own()),
        "the restore in flight at {st:?}"
    );
    let press = |fixture: &str| match decide_screen(Some("codex"), &screen(fixture), &c) {
        Some(Decision::Approve {
            rule_id, choice, ..
        }) if rule_id == RULE_MODEL_RESTORE_PICK => choice,
        other => panic!("the picker not answered at {st:?}: {other:?}"),
    };
    assert!(
        matches!(press(cx::MODEL_PICK), Choice::Focus { steps: 0, label } if label == "GPT-6-Astra (current)")
    );
    assert!(
        matches!(press(cx::EFFORT_PICK), Choice::Focus { steps: 3, label } if label == "More reasoning…")
    );
    assert!(
        matches!(press(cx::ADVANCED_PICK), Choice::FocusKey { steps: 1, label, key: "s" } if label == "Ultra")
    );
}

/// The switch round-tripped through the ledger as a restarted loop reads it
/// (`open_wind_down`), seeded into a fresh state.
fn restarted(real: &Real, st: &State) -> TurnEndState {
    let w = real.st.wind().expect("an open switch");
    let dir = std::env::temp_dir().join(format!(
        "codex-nudge-restart-{}-{}",
        std::process::id(),
        st.values().map(|v| v.to_string()).collect::<String>()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    let path = dir.join("s-1.jsonl");
    let secs = |d: Duration| i64::try_from(d.as_secs()).expect("fits");
    let back_unix = BASE_UNIX + secs(real.back.saturating_duration_since(real.now));
    let since_unix = match w.phase {
        WindPhase::Holding { since } => {
            Some(BASE_UNIX - secs(real.now.saturating_duration_since(since)))
        }
        _ => None,
    };
    let esc_unix = w
        .stop_at
        .map(|at| BASE_UNIX - secs(real.now.saturating_duration_since(at)));
    let opened_unix = w
        .opened_at
        .map(|at| BASE_UNIX - secs(real.now.saturating_duration_since(at)));
    let words = wind_switch_words(
        w,
        wind_phase_word(w),
        Some(back_unix),
        since_unix,
        esc_unix,
        opened_unix,
        None,
    );
    let row = Row {
        rule_id: RULE_WIND_DOWN,
        outcome: Outcome::Typed,
        command: "the switch's last act",
        reason: &format!("the turn-end policy {words}"),
        box_seq: 1,
    }
    .to_json(1_790_000_000_000, None);
    std::fs::write(&path, row + "\n").expect("the ledger");
    let open = open_wind_down(&path, None).unwrap_or_else(|| panic!("read back at {st:?}"));
    let _ = std::fs::remove_dir_all(&dir);
    let clock = |u: i64| {
        let d = Duration::from_secs((BASE_UNIX - u).unsigned_abs());
        if u <= BASE_UNIX {
            real.now.checked_sub(d).expect("after the clock's start")
        } else {
            real.now + d
        }
    };
    let wind = open.into_wind(clock, real.now);
    assert_eq!(
        wind.back_at,
        Some(real.back),
        "the reset read back at {st:?}"
    );
    let mut fresh = TurnEndState::new(timing());
    fresh.seed_wind(wind, real.now);
    fresh
}

/// A HOST RESTART at the free point `st`: the switch's row as the loop
/// writes it, read back by a loop that starts after it and seeded into a
/// fresh state, decides the point as the live state does.
fn restarted_decides_the_same(real: &Real, st: &State, live: &TurnEndAction, tells: bool) {
    let fresh = restarted(real, st);
    assert_eq!(ph_of(&fresh), st["ph"], "the seeded phase at {st:?}");
    let (got, fresh_tells) = decide_here(
        &Real {
            st: fresh,
            ..real.clone()
        },
        st,
    );
    assert_eq!(
        word(&got, st["paused"] == 1),
        word(live, st["paused"] == 1),
        "a restarted loop decides otherwise at {st:?}: {got:?} vs {live:?}"
    );
    // The notes said ride the rows (`told=`): a restart raises none again.
    assert_eq!(
        fresh_tells, tells,
        "a restarted loop tells otherwise at {st:?}"
    );
}

/// A HOST RESTART WHILE A TURN RUNS (the review of 2026-09-28: the loop
/// restarted during the wind-down's own turn took the harness's save for a
/// person's message, dropped the hold and typed `keep going` at 95%): the
/// switch read back mid-turn, then the point that ends the turn (`r`)
/// judged by both the live state and the restarted one — the same phase,
/// the same next act.
fn restart_mid_turn(real: &Real, before: &State, after: &State, r: &TurnEndReading) {
    let mut fresh = restarted(real, before);
    let mut live = real.st.clone();
    live.observe(r, real.now);
    fresh.observe(r, real.now);
    assert_eq!(
        ph_of(&fresh),
        ph_of(&live),
        "a loop restarted mid-turn judges its end otherwise at {before:?} → {after:?}: {:?} \
         vs {:?}",
        fresh.wind(),
        live.wind()
    );
    if after["shown"] == 0 {
        let (a_live, _) = decide_here(
            &Real {
                st: live,
                ..real.clone()
            },
            after,
        );
        let (a_fresh, _) = decide_here(
            &Real {
                st: fresh,
                ..real.clone()
            },
            after,
        );
        assert_eq!(
            word(&a_fresh, after["paused"] == 1),
            word(&a_live, after["paused"] == 1),
            "a loop restarted mid-turn acts otherwise at {after:?}: {a_fresh:?} vs {a_live:?}"
        );
    }
}

/// Mirror one model action on the real side (the model has fired it). The
/// turn-end policy's actions are the real decider's own acts; one it does
/// not take (a `Buggy` branch) is refused (`false`): the walk does not
/// descend through it. Every turn that runs after it, the box not up, has
/// had the loop's first busy read ([`RunningTurn::busy`]).
fn mirror(action: &str, before: &State, after: &State, real: &mut Real) -> bool {
    if !mirror_act(action, before, after, real) {
        return false;
    }
    if after["turn"] == 1 && after["shown"] == 0 {
        real.hand.busy(real.now);
        real.round3.busy(real.now);
    }
    true
}

fn mirror_act(action: &str, before: &State, after: &State, real: &mut Real) -> bool {
    let cfg = SupervisorConfig::default();
    let now = real.now;
    match action {
        "GoalTurn" | "GoalEnds" | "GoalEscapes" | "Pend" | "Far" | "NearAgain" | "SandboxFalls" => {
        }
        // The save's own turn works on, past its bound.
        "WindDownRunsLong" => real.now += WIND_DOWN_BOUND,
        "StopWindDown" => {
            // The save's turn past its bound: the real stop's one Esc, then
            // its interrupted point — the marker judged there (none said).
            real.st.own_esc(now);
            let _ = real.st.take_wind_events();
            real.now += Duration::from_secs(1);
            let now = real.now;
            let worked = real.point();
            let r = reading(
                after,
                real,
                worked,
                "■ Conversation interrupted",
                true,
                real.person_ago(),
            );
            // A host restart between the Esc and its point: the Esc rides
            // the row (`esc=`), and the interrupt is the harness's own.
            if real.strict {
                restart_mid_turn(real, before, after, &r);
            }
            real.st.observe(&r, now);
            let unsaved = real.st.take_wind_events().iter().any(|e| {
                matches!(e, WindEvent::Note(n) if n.contains("did not confirm")
                    && n.contains("stopped its turn"))
            });
            assert!(unsaved, "the stopped save said unsaved at {before:?}");
        }
        // A person's message, their `/goal resume`: their keystroke begins
        // the turn.
        "PersonTurn" | "PersonResumes" => real.typed = Some(now),
        "TurnEnds" => {
            // The turn ran its work.
            real.now += TURN;
            let now = real.now;
            let said = if before["ph"] == 2 {
                real.marker_line()
            } else {
                "Stage done.".to_string()
            };
            if after["shown"] == 1 {
                // The box is up over the turn's end: a prompt, folded in
                // with nothing; its point is observed once the box goes.
                real.st
                    .observe(&reading(after, real, None, &said, false, None), now);
                real.owed = true;
                // The wind-down's own point is judged behind the box too.
                if before["ph"] == 2 {
                    let mut idle = after.clone();
                    idle.insert("shown", 0);
                    let worked = real.point();
                    let r = reading(&idle, real, worked, &said, false, real.person_ago());
                    if real.strict {
                        restart_mid_turn(real, before, &idle, &r);
                    }
                    real.st.observe(&r, now);
                    real.owed = false;
                }
            } else {
                let worked = real.point();
                let r = reading(after, real, worked, &said, false, real.person_ago());
                if real.strict && (before["ph"] == 1 || before["ph"] == 2) {
                    restart_mid_turn(real, before, after, &r);
                }
                real.st.observe(&r, now);
            }
        }
        "PressKeep" => {
            // The box's keep: the turn it covered ended under it.
            real.hand.boxed();
            if std::mem::take(&mut real.owed) && after["turn"] == 0 {
                let worked = real.point();
                real.st.observe(
                    &reading(after, real, worked, "Stage done.", false, real.person_ago()),
                    now,
                );
            }
        }
        "PressSwitch" => {
            // The switch opens as the loop opens it: the box's span closed,
            // a person's keystrokes floored at the opening.
            real.hand.boxed();
            real.st.open_switch(WindDown {
                opened_at: Some(now),
                ..WindDown::opened(own(), TO.to_string(), Some(real.back), MARKER.to_string())
            });
            // The turn whose end the box covered is judged now; a goal turn
            // running under the box ends at its own point.
            if std::mem::take(&mut real.owed) && after["turn"] == 0 {
                let worked = real.point();
                real.st.observe(
                    &reading(after, real, worked, "Stage done.", false, real.person_ago()),
                    now,
                );
            }
        }
        "StopGoal" if before["turn"] == 1 => {
            // The goal turn running while switched: the real stop's Esc,
            // then its interrupted point.
            real.st.own_esc(now);
            let _ = real.st.take_wind_events();
            real.owed = false;
            real.now += Duration::from_secs(1);
            let now = real.now;
            let worked = real.point();
            let r = reading(
                after,
                real,
                worked,
                "■ Conversation interrupted",
                true,
                real.person_ago(),
            );
            // A host restart between the Esc and its point: the Esc rides
            // the row (`esc=`), and the interrupt is the harness's own in
            // every phase.
            if real.strict {
                restart_mid_turn(real, before, after, &r);
            }
            real.st.observe(&r, now);
        }
        "Tell" if before["turn"] == 1 => real.st.goal_told(),
        "Tell" => {
            // At a free point: the point's own look tells.
            real.st.observe(
                &reading(before, real, None, "Stage done.", false, None),
                now,
            );
            assert!(
                told(&real.st.take_wind_events()) || real.st.wind().is_none(),
                "the stops spent, told at {before:?}"
            );
        }
        "Continue" | "TypeWindDown" | "Resume" | "StopGoal" => {
            let r = reading(before, real, None, "Stage done.", false, None);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, &cfg, now);
            if word(&a, before["paused"] == 1) != action {
                return false;
            }
            real.st.acted(&a, &r, now);
            if matches!(a, TurnEndAction::TypeCommand { .. }) {
                // A command needs no turn: its point is judged at once.
                real.now += Duration::from_secs(1);
                let now = real.now;
                real.st
                    .observe(&reading(after, real, None, "Stage done.", false, None), now);
            }
        }
        "Restore" => {
            let r = reading(before, real, None, "Stage done.", false, None);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, &cfg, now);
            if word(&a, false) != "Restore" {
                return false;
            }
            real.st.acted(&a, &r, now);
            picks(real, before);
            real.now += Duration::from_secs(1);
            let now = real.now;
            real.st
                .observe(&reading(after, real, None, "Stage done.", false, None), now);
        }
        _ => return false,
    }
    true
}

/// What the walk found.
#[derive(Default)]
struct Found {
    states: usize,
    words: BTreeMap<&'static str, usize>,
    /// Per Buggy branch: a state where it is enabled and the real deciders
    /// answer otherwise.
    buggy_refused: BTreeSet<&'static str>,
    disagree: Vec<State>,
    /// Busy reads of a person's turn the grace-only reading would stop.
    grace_only_stops: usize,
    /// Busy reads of a goal turn the model stops that round 3's reading
    /// ([`Round3`]) would spare.
    round3_spares: usize,
}

const BUGGY: &[&str] = &[
    "ContinueWhileSwitched",
    "SwitchKeepsGoal",
    "StopPersonsTurn",
    "CoveredHandSparesGoal",
    "WindDownRunsOn",
    "NudgeRewinds",
    "HoldBeforeRestore",
    "ResumeOnCheap",
    "ForgetOnRestart",
    "PressAtFar",
    "ContinueInSandbox",
    "WindDownInSandbox",
    "ContinueUnderGoal",
];

fn walk(m: &Model, strict: bool) -> Found {
    let base = Instant::now() + Duration::from_secs(3600);
    let init = Real {
        st: TurnEndState::new(timing()),
        now: base,
        back: base + BACK,
        owed: false,
        strict,
        hand: RunningTurn::default(),
        round3: Round3::default(),
        typed: None,
    };
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init, Vec::<&'static str>::new())]);
    let mut found = Found::default();
    while let Some((st, real, path)) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        let check = |ok: bool, st: &State, why: String, found: &mut Found| {
            if !ok {
                assert!(!strict, "{why} at {st:?}, reached by {path:?}");
                found.disagree.push(st.clone());
            }
        };
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        // The real switch's phase is the model's at every point it has
        // judged: not while a turn runs (a person's message moves the model
        // as it is sent, the real policy at its end), nor behind a box whose
        // point is not observed yet. Its stops are the model's always.
        if !real.owed && st["turn"] == 0 {
            check(
                ph_of(&real.st) == st["ph"],
                &st,
                format!("the real phase is {}", ph_of(&real.st)),
                &mut found,
            );
        }
        if st["ph"] != 0 {
            let stops = real.st.wind().map_or(-1, |w| i64::from(w.stops));
            check(
                stops == st["stops"] || real.st.wind().is_none(),
                &st,
                format!("the real stops are {stops}"),
                &mut found,
            );
        }
        if st["shown"] == 1 {
            let (rule, choice) = nudge_press(&real, &st);
            assert_ne!(
                choice,
                Choice::Digit(3),
                "never the never-show-again at {st:?}"
            );
            let pressed = if rule == RULE_RATE_NUDGE_SWITCH {
                "PressSwitch"
            } else {
                assert_eq!(rule, RULE_RATE_NUDGE_KEEP, "{st:?}");
                "PressKeep"
            };
            *found.words.entry(pressed).or_default() += 1;
            check(
                m.action_enabled(pressed, &st),
                &st,
                format!("the real nudge answer is {pressed}"),
                &mut found,
            );
            // Each buggy press refused where the real decider KEEPS.
            for b in ["NudgeRewinds", "PressAtFar"] {
                if m.action_enabled(b, &st) && pressed == "PressKeep" {
                    found.buggy_refused.insert(b);
                }
            }
        }
        if st["turn"] == 1 && st["shown"] == 0 {
            // A busy read: the real goal stop.
            let got = stop_word(&real, &st);
            *found.words.entry(got).or_default() += 1;
            let want = ["StopGoal", "StopWindDown", "Tell"]
                .into_iter()
                .find(|a| m.action_enabled(a, &st))
                .unwrap_or("none");
            check(
                got == want,
                &st,
                format!("the real goal stop is {got}, the model's {want}"),
                &mut found,
            );
            for b in ["SwitchKeepsGoal", "CoveredHandSparesGoal", "WindDownRunsOn"] {
                if m.action_enabled(b, &st) && got != "none" {
                    found.buggy_refused.insert(b);
                }
            }
            // A person's turn: the real stop sends nothing into it.
            if m.action_enabled("StopPersonsTurn", &st) && got == "none" {
                found.buggy_refused.insert("StopPersonsTurn");
            }
            // Round 2's defect, the grace alone read as a person's hand: it
            // stops a person's turn the model protects.
            if st["person"] == 1
                && !m.action_enabled("StopGoal", &st)
                && stop_word_as(&real, &st, Hand::GraceOnly) == "StopGoal"
            {
                found.grace_only_stops += 1;
            }
            // Round 3's regression: it spares a goal turn the model stops.
            if m.action_enabled("StopGoal", &st) && stop_word_as(&real, &st, Hand::Round3) == "none"
            {
                found.round3_spares += 1;
            }
        } else if st["turn"] == 0 && st["shown"] == 0 {
            let (a, tells) = decide_here(&real, &st);
            let got = word(&a, st["paused"] == 1);
            *found.words.entry(got).or_default() += 1;
            let want = harness_act(m, &st);
            let agree = match want {
                Some(w) => got == w,
                None => got == "nothing" || got == "wait",
            };
            check(
                agree,
                &st,
                format!("the real act is {got}, the model's {want:?}"),
                &mut found,
            );
            check(
                tells == m.action_enabled("Tell", &st),
                &st,
                format!("the real look tells: {tells}"),
                &mut found,
            );
            // Held before the reset: a wait for it, never nothing (a hold
            // that stops deciding would never resume).
            if st["ph"] == 4 && st["near"] == 1 && st["goal"] == 0 && st["sandbox"] == 0 {
                check(
                    got == "wait",
                    &st,
                    "the hold must wait".to_string(),
                    &mut found,
                );
            }
            // Each buggy act refused where the real decider answers
            // otherwise.
            let refused = [
                ("ContinueWhileSwitched", got != "Continue"),
                ("ContinueInSandbox", got != "Continue"),
                ("ContinueUnderGoal", got != "Continue"),
                ("ResumeOnCheap", got != "Continue" && got != "Resume"),
                ("HoldBeforeRestore", got != "wait"),
                ("WindDownInSandbox", got != "TypeWindDown"),
            ];
            for (b, differs) in refused {
                if m.action_enabled(b, &st) && differs {
                    found.buggy_refused.insert(b);
                }
            }
            // A restart that forgets the switch answers otherwise.
            if m.action_enabled("ForgetOnRestart", &st) && !real.owed {
                let forgot = Real {
                    st: TurnEndState::new(timing()),
                    ..real.clone()
                };
                let (f, _) = decide_here(&forgot, &st);
                if word(&f, st["paused"] == 1) != got {
                    found.buggy_refused.insert("ForgetOnRestart");
                }
            }
            if strict && (1..=5).contains(&st["ph"]) && st["ph"] != 2 && !real.owed {
                restarted_decides_the_same(&real, &st, &a, tells);
            }
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(action.name, &st, &next, &mut r) {
                let mut to = path.clone();
                to.push(action.name);
                queue.push_back((next, r, to));
            }
        }
    }
    found.states = seen.len();
    found
}

/// Where each of the machine's own actions is anchored in the shipping code
/// (`#[refines]`, this crate's `spec-anchors`, on through its dev edge on
/// itself): `(action, rust_method)`, sorted.
const ANCHORED: [(&str, &str); 12] = [
    ("Continue", "continue_policy"),
    ("PressKeep", "rate_nudge"),
    ("PressSwitch", "open_switch"),
    ("PressSwitch", "rate_nudge"),
    ("Restore", "wind_down_act"),
    ("Resume", "wind_down_act"),
    ("StopGoal", "goal_stop"),
    ("StopGoal", "wind_down_act"),
    ("StopWindDown", "goal_stop"),
    ("Tell", "goal_told"),
    ("TurnEnds", "observe"),
    ("TypeWindDown", "wind_down_act"),
];

#[test]
fn tier1_the_real_deciders_switch_stop_save_restore_and_hold_exactly_where_the_model_does() {
    use aterm_spec::xref;
    assert!(xref::reset_entered_anchors(), "the evidence window opens");
    let m = supervisor_codex_rate_nudge_model();
    let found = walk(&m, true);
    assert_eq!(
        found.states, 1428,
        "the reachable space changed: {}",
        found.states
    );
    for w in [
        "PressSwitch",
        "PressKeep",
        "StopGoal",
        "StopWindDown",
        "Tell",
        "TypeWindDown",
        "Restore",
        "Resume",
        "Continue",
        "wait",
        "nothing",
        "none",
    ] {
        assert!(
            found.words.get(w).copied().unwrap_or(0) > 0,
            "{w} never occurred: {:?}",
            found.words
        );
    }
    eprintln!(
        "codex rate nudge: {} states walked; {:?}",
        found.states, found.words
    );

    // The anchors are linked, name the one projection, and were ENTERED by
    // the walk; every other action of the machine — Codex's, the clock's, a
    // person's, the `Buggy = 1` members — is waived by name, none of them
    // bound.
    let mut anchored: Vec<(&str, &str)> = xref::refinements()
        .filter(|a| a.machine == "SupervisorCodexRateNudge")
        .map(|a| {
            assert_eq!(a.project, "supervise_conformance_codex_rate_nudge::ph_of");
            (a.action, a.rust_method)
        })
        .collect();
    anchored.sort_unstable();
    assert_eq!(anchored, ANCHORED);
    let entered = xref::entered_anchor_ids();
    for (action, method) in ANCHORED {
        let id = format!("SupervisorCodexRateNudge::{action} @ {method}");
        assert!(entered.contains(id.as_str()), "{id} entered: {entered:?}");
    }
    let bound: BTreeSet<&str> = ANCHORED.iter().map(|(a, _)| *a).collect();
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "SupervisorCodexRateNudge")
        .map(|w| w.action)
        .collect();
    assert!(
        bound.is_disjoint(&waived),
        "{:?}",
        bound.intersection(&waived)
    );
    let actions: BTreeSet<&str> = m.actions.iter().map(|a| a.name).collect();
    assert_eq!(
        bound.union(&waived).copied().collect::<BTreeSet<_>>(),
        actions,
        "every action bound or waived"
    );
    for b in BUGGY {
        assert!(waived.contains(b), "{b}: a Buggy member is waived");
    }
}

/// NEGATIVE CONTROLS. Every `Buggy = 1` branch — the day's defects — is
/// enabled at some reachable state where the real deciders ANSWER OTHERWISE
/// (the switch at 1% is a keep, the goal left to run on is stopped, `keep
/// going` during the switch is the wind-down or nothing, the save into a
/// sandbox is nothing, the hold on the cheaper model is `/model`, a restart
/// that forgets decides otherwise than one that remembers, …): a green walk
/// is not vacuous. And a restart that reads NOTHING back — the incident's
/// press was ledgered as a row no reader saw — types `keep going` into the
/// owed wind-down, where the committed model allows no continuation.
#[test]
fn the_buggy_branches_and_a_forgetful_restart_are_not_the_real_deciders() {
    let m = supervisor_codex_rate_nudge_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let found = walk(&buggy, false);
    for b in BUGGY {
        assert!(
            found.buggy_refused.contains(b),
            "{b} is never answered otherwise by the real deciders: {:?}",
            found.buggy_refused
        );
    }
    // The grace-only person (round 2's defect) and round 3's regression
    // are caught by the walk.
    let strict = walk(&m, false);
    assert!(
        strict.grace_only_stops > 0,
        "the grace-only reading never stops a person's turn: the walk would not catch it"
    );
    assert!(
        strict.round3_spares > 0,
        "round 3's reading never spares a goal turn: the walk would not catch it"
    );

    // The forgetful restart, at the owed wind-down.
    let mut st = m.init_state();
    for a in [
        "GoalTurn",
        "Pend",
        "TurnEnds",
        "GoalTurn",
        "PressSwitch",
        "StopGoal",
    ] {
        assert!(m.fire(a, &mut st), "{a} at {st:?}");
    }
    assert_eq!(st["ph"], 1);
    assert!(!m.action_enabled("Continue", &st));
    let now = Instant::now() + Duration::from_secs(3600);
    let fresh = Real {
        st: TurnEndState::new(timing()),
        now,
        back: now + BACK,
        owed: false,
        strict: false,
        hand: RunningTurn::default(),
        round3: Round3::default(),
        typed: None,
    };
    let (a, _) = decide_here(&fresh, &st);
    assert_eq!(word(&a, false), "Continue", "{a:?}");
}
