// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance test for `SupervisorUsageWall`
//! (`supervisor_usage_wall_model`), driving `aterm_phase::read` and
//! `decide_turn_end` (`TurnEndState`) across every reachable model state:
//!
//! - **Wall detection & placement**: verifies walls with or without background
//!   shells (`bg`), print-based vs session-window placement (`place_reset`),
//!   and `Met` after a turn or untaken act (`TurnEndState::untaken_act`).
//! - **Left wall (`NoticeLeaves`)**: verifies waiting when the notice leaves an
//!   idle screen, early termination on `PersonTurnServed`, and immunity to
//!   unrelated limits in `ConversationResumed` / `ArmedConversationResumed`.
//! - **Take windows (`OwnActTyped`, `MetInWindow`)**: verifies walls appearing
//!   or answering within the 30 s take window (`held`) are tracked and not
//!   misread as taken when the window expires.
//! - **Cancelled notices (`WallAppearsLeft`, `MetLeftUnread`, `PersonWallLeft`,
//!   `OlderLeftResumed`)**: verifies `TurnEndState::left_answer` over
//!   `aterm_phase::left_wall` when `Esc` cancels the countdown before the first
//!   read, and bounds older resumed limits within the session window.
//! - **Usage credits (`Credits`)**: verifies `Now using usage credits` is not
//!   treated as a spend wall (`NoWallOnCredits`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::limit::{ResetSpec, place_reset, within_a_session_window};
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    FIRED_EARLY, RULE_CONTINUE, RULE_LIMIT_RESUME, TurnEndAction, TurnEndReading, TurnEndState,
    TurnEndTiming, decide_turn_end,
};
use aterm_models_harness::supervisor_usage_wall_model;
use aterm_phase::prompt::fixtures::screen;
use aterm_spec::derive::Model;

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

/// Base `SupervisorUsageWall` model (`Hidden = 0`, `Leaves = 1`, `OwnAct = 0`).
fn walled_model() -> Model {
    aterm_spec::interp::with_consts(
        &supervisor_usage_wall_model(),
        &[("Hidden", 0), ("OwnAct", 0), ("Credits", 0)],
    )
}

/// The model with usage credits carrying a turn (`Credits = 1`) beside the
/// walls of [`walled_model`] that show and stay.
fn credits_model() -> Model {
    aterm_spec::interp::with_consts(
        &supervisor_usage_wall_model(),
        &[
            ("Hidden", 0),
            ("OwnAct", 0),
            ("Credits", 1),
            ("Leaves", 0),
            ("Unread", 0),
        ],
    )
}

/// [`walled_model`] with the policy's own acts' take windows (`OwnAct = 1`:
/// the continue policy's act, and the wall's own retry): walked with the real
/// window ([`timing`]). A `Met` while the window is open is a turn that ran
/// into the wall again (its point judges the act at once); the wall answering
/// with no turn run inside the window is `MetInWindow`.
fn windowed_model() -> Model {
    aterm_spec::interp::with_consts(&walled_model(), &[("OwnAct", 1)])
}

/// [`windowed_model`] with every notice READ before it leaves (`Unread = 0`:
/// no `WallAppearsLeft`, no `MetLeftUnread`): the controls of the windows
/// themselves walk it, so each disagrees exactly where its own defect does.
fn read_windowed_model() -> Model {
    aterm_spec::interp::with_consts(&windowed_model(), &[("Unread", 0)])
}

/// The real policy's take window: an act's point with no busy read is
/// judged this long after it first showed.
const TAKE: Duration = Duration::from_secs(30);

const MIN: Duration = Duration::from_secs(60);
const LONG: Duration = Duration::from_secs(10 * 60);
const SHORT: Duration = Duration::from_secs(5);

const WALL: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
);
const LIVE: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-spinner.txt"
);

/// 2026-09-28 20:00 PDT, when the wall in every state is printed.
const PRINT: i64 = 1_790_650_800;
const PDT: i64 = -7 * 3600;

fn zone(_: &str) -> Option<i64> {
    Some(PDT)
}

/// Minutes after `PRINT` that `place_reset` places the weekly wall's bare clock.
fn placed_after_the_print(m: &Model, ticks: i64) -> Duration {
    let clock = 20 * 60 + ticks;
    let spec = ResetSpec::At {
        date: None,
        hour: u32::try_from(clock / 60).unwrap(),
        minute: u32::try_from(clock % 60).unwrap(),
        zone: Some("America/Los_Angeles".to_string()),
    };
    let at = place_reset(&spec, false, PRINT, Some(PRINT - 7 * 60), PDT, zone);
    assert!(at > PRINT, "{m:?}");
    Duration::from_secs(u64::try_from(at - PRINT).unwrap())
}

/// Minutes from the act at which an older resumed limit (`7:30pm` at `8:00pm`)
/// is placed (`within_a_session_window` unless `uncapped`).
fn older_left_placed(uncapped: bool) -> i64 {
    let spec = ResetSpec::At {
        date: None,
        hour: 19,
        minute: 30,
        zone: Some("America/Los_Angeles".to_string()),
    };
    let at = place_reset(&spec, false, PRINT, Some(PRINT - 7 * 60), PDT, zone);
    assert_eq!(
        at - PRINT,
        23 * 3600 + 30 * 60,
        "tomorrow's on the act's print"
    );
    let kept = if uncapped {
        at
    } else {
        within_a_session_window(&spec, at, PRINT)
    };
    kept - PRINT
}

/// The bare clock of a wall FOUND sitting, 13 h after the print (9am read at
/// 20:00), placed as the loop places a wall with nothing to date it: the
/// session window's rule, 11 h in the PAST — how far before the wall's
/// appearance the decider is handed its reset.
fn misplaced_before_the_print() -> Duration {
    let nine = ResetSpec::At {
        date: None,
        hour: 9,
        minute: 0,
        zone: Some("America/Los_Angeles".to_string()),
    };
    let at = place_reset(&nine, false, PRINT, None, PDT, zone);
    assert!(at < PRINT, "the session rule places it in the past");
    Duration::from_secs(u64::try_from(PRINT - at).unwrap())
}

fn minutes(m: &Model, name: &str) -> Duration {
    MIN * u32::try_from(cnst(m, name)).expect("a small constant")
}

/// The real policy's clocks, scaled so one model tick is one minute; an act's
/// take window is no tick at all (a `Met` with no busy read is judged at once)
/// — but the real one, [`TAKE`], under a tick, where the model has the own
/// act's window (`OwnAct`).
fn timing(m: &Model) -> TurnEndTiming {
    TurnEndTiming {
        reset_grace: minutes(m, "Grace"),
        reset_jitter: Duration::ZERO,
        limit_backoff: vec![minutes(m, "Rung1"), minutes(m, "Rung2")],
        window: MIN * 24 * 60,
        take_within: if cnst(m, "OwnAct") == 1 {
            TAKE
        } else {
            Duration::ZERO
        },
        ..TurnEndTiming::default()
    }
}

/// The screen a state stands for: the wall (`bg`: the shell the tab counted
/// kept, else the done row and the footer without it), the wall whose notice
/// left the screen ([`left`]), or a worker at work.
#[derive(Clone, Copy)]
enum Screen {
    Walled,
    WalledShell,
    Resumed,
    ResumedContinued,
    Cancelled,
    Left,
    OnCredits,
    Live,
}

impl Screen {
    const ALL: [Self; 8] = [
        Self::Walled,
        Self::WalledShell,
        Self::Resumed,
        Self::ResumedContinued,
        Self::Cancelled,
        Self::Left,
        Self::OnCredits,
        Self::Live,
    ];

    fn of(st: &State) -> Self {
        match st["wall"] {
            1 if st["bg"] == 1 => Self::WalledShell,
            1 => Self::Walled,
            6 if st["other"] == 1 => Self::Resumed,
            6 if st["other"] == 2 => Self::ResumedContinued,
            6 if st["unread"] == 1 => Self::Cancelled,
            6 => Self::Left,
            _ if st["credits"] == 1 => Self::OnCredits,
            _ => Self::Live,
        }
    }

    fn rows(self) -> Vec<String> {
        match self {
            Self::Walled => walled(false),
            Self::WalledShell => walled(true),
            Self::Resumed => resumed(),
            Self::ResumedContinued => resumed_continued(),
            Self::Cancelled => cancelled(),
            Self::Left => left(),
            Self::OnCredits => on_credits(),
            Self::Live => screen(LIVE),
        }
    }

    /// The screen's rows and the REAL reader's read of them, each made once:
    /// both are pure, and a walk asks for them at every state and at every
    /// transition (the read was half of a walk's time).
    fn read(self) -> &'static (Vec<String>, aterm_phase::Reading) {
        static READ: std::sync::OnceLock<Vec<(Vec<String>, aterm_phase::Reading)>> =
            std::sync::OnceLock::new();
        let all = READ.get_or_init(|| {
            Self::ALL
                .iter()
                .map(|s| {
                    let rows = s.rows();
                    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
                    (rows, read)
                })
                .collect()
        });
        &all[self as usize]
    }
}

fn rows(st: &State) -> Vec<String> {
    Screen::of(st).rows()
}

/// The REAL [`TurnEndReading::of`] of `read` over `screen`'s rows, made once
/// per thread for each set of inputs: it is pure, and a walk hands it the same
/// ones again and again (it was two thirds of a walk's time). A hit is the
/// same rows, the same `worked` and `reset_at`, and a read EQUAL to the one
/// it was made from.
fn turn_end_reading(
    screen: Screen,
    read: &aterm_phase::Reading,
    worked: Option<Duration>,
    reset_at: Option<Instant>,
) -> TurnEndReading {
    type Key = (usize, Option<Duration>, Option<Instant>);
    type Made = Vec<(aterm_phase::Reading, TurnEndReading)>;
    thread_local! {
        static MADE: std::cell::RefCell<std::collections::HashMap<Key, Made>> =
            std::cell::RefCell::default();
    }
    MADE.with_borrow_mut(|made| {
        let made = made.entry((screen as usize, worked, reset_at)).or_default();
        if let Some((_, t)) = made.iter().find(|(r, _)| r == read) {
            return t.clone();
        }
        let t = TurnEndReading::of(read, &screen.read().0, false, worked, reset_at, None);
        made.push((read.clone(), t.clone()));
        t
    })
}

/// A RUNNING TURN CARRIED BY USAGE CREDITS: the worker at work, and Claude
/// Code's `Now using usage credits` right-aligned on the mode row (as the
/// 2.1.300 lab drew it, `claude-2.1.300-credits-in-use.txt`).
fn on_credits() -> Vec<String> {
    let mut rows = screen(LIVE);
    let mode = rows
        .iter()
        .position(|r| r.contains("bypass permissions on"))
        .expect("the mode row");
    rows[mode] = format!("{:<115}Now using usage credits", rows[mode]);
    rows
}

/// THE WALL'S NOTICE CANCELLED BEFORE ANY READ: the measured wall on a tab
/// with no shell, the vendor's cancel drawn under its notice (and the notice's
/// `/goal` echo) as Esc on the countdown leaves it — the last thing said, no
/// wall for the reader.
fn cancelled() -> Vec<String> {
    with_rows_before_the_done_row(
        walled(false),
        &[
            "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
            "",
        ],
    )
}

/// ANOTHER CONVERSATION'S HISTORY, `/resume`d at the left wall: the measured
/// 2.1.299 screen of its own five-hour limit (`resets 10:38am`, lifted long
/// ago, Esc on its countdown) and a person's turn served under it.
fn resumed() -> Vec<String> {
    screen(include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.299-wall-session-cancelled-then-person-turn.txt"
    ))
}

/// ANOTHER CONVERSATION'S HISTORY, `/resume`d while the wall SHOWED, its
/// countdown armed: the measured 2.1.300 screen (W1 of ) —
/// its own older five-hour limit (`resets 8:48am`) and the vendor's
/// continuation of it (`⏺ Usage limit reset · continuing automatically`, the
/// reply under it), the countdown dropped.
fn resumed_continued() -> Vec<String> {
    screen(include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.300-resumed-older-limit-continued-at-armed-limit.txt"
    ))
}

/// The measured wall, with the shell the tab counted (`shell`) or without.
fn walled(shell: bool) -> Vec<String> {
    let mut rows = screen(WALL);
    if !shell {
        for r in &mut rows {
            *r = r
                .replace(" · 1 shell still running", "")
                .replace(" · 1 shell ·", " ·");
        }
    }
    rows
}

/// THE WALL'S NOTICE LEFT AN IDLE SCREEN: the measured wall on a tab with no
/// shell, a person's `/model` said under it (its `❯` row and its `⎿` result):
/// the notice is no longer the last thing said, and nothing runs.
fn left() -> Vec<String> {
    with_rows_before_the_done_row(
        walled(false),
        &["❯ /model", "  ⎿  Kept model as Fable 5.1", ""],
    )
}

/// The same screen after a person's turn the account served, over before any
/// read saw it busy: its prompt and the reply under the notice.
fn person_turned() -> Vec<String> {
    with_rows_before_the_done_row(
        left(),
        &[
            "❯ what is left to do?",
            "",
            "⏺ Nothing is left: the suite is green.",
            "",
        ],
    )
}

/// `rows` with `extra` said just above the turn's done row (`✻ Sautéed …`):
/// the way the rows of anything said after the wall are drawn.
fn with_rows_before_the_done_row(mut rows: Vec<String>, extra: &[&str]) -> Vec<String> {
    let at = rows
        .iter()
        .position(|r| r.starts_with("✻ Sautéed"))
        .expect("the wall's done row");
    for (i, row) in extra.iter().enumerate() {
        rows.insert(at + i, (*row).to_string());
    }
    rows
}

/// Construct the `TurnEndReading` for `st` under `run` (`masked` keeps a
/// screen with active background work `Busy` regardless of turn-end text).
fn reading(
    st: &State,
    worked: Option<Duration>,
    reset: Instant,
    run: Run,
    real: &TurnEndState,
) -> TurnEndReading {
    let (r, read) = Screen::of(st).read();
    let mut read = read.clone();
    // A reader that takes usage credits for a spend wall.
    if run.credits_as_wall && st["credits"] == 1 && read.credits.is_some() {
        let mut w = aterm_phase::Wall {
            kind: aterm_phase::WallKind::Spend,
            message: "Now using usage credits".to_string(),
            reset: None,
            row: 0,
            placement: aterm_phase::Placement::Footer,
            under_call: false,
        };
        w.row = r.len() - 1;
        read.phase = Phase::Limited {
            message: w.message.clone(),
            reset: None,
        };
        read.wall = Some(w);
    }
    // Resolve a cancelled limit notice via `left_answer` when `read.wall` is absent.
    if read.wall.is_none()
        && !run.blind_to_left
        && let Some(w) = real
            .left_answer(r)
            .filter(|a| {
                a.screen_tells && (!run.blind_to_a_persons_left || real.untaken_act().is_some())
            })
            .map(|a| a.wall)
    {
        read.phase = Phase::Limited {
            message: w.message.clone(),
            reset: w.reset.clone(),
        };
        read.wall = Some(w);
    }
    let reset_at = read.wall.is_some().then_some(reset);
    let mut t = turn_end_reading(Screen::of(st), &read, worked, reset_at);
    t.restartable = true;
    if run.masked && st["bg"] == 1 && st["wall"] == 1 {
        t.phase = Phase::Busy;
        t.wall = None;
        t.reset_at = None;
        t.wall_message = String::new();
    }
    if run.untied && t.since_limit.as_ref().is_some_and(|s| s.served) {
        t.served_in_transcript = true;
    }
    if run.untied_continuation && t.auto_continue == Some(aterm_phase::AutoContinue::Fired) {
        let own = Screen::Walled
            .read()
            .1
            .wall
            .clone()
            .expect("the measured wall");
        t.since_limit = Some(aterm_phase::SinceLimit {
            kind: own.kind,
            reset: own.reset,
            served: false,
            fired: true,
            model_set: false,
            chosen_model: None,
            generic: false,
        });
    }
    if run.blind_in_window && st["held"] == 1 {
        t.wall = None;
        t.wall_message = String::new();
        t.reset_at = None;
        t.reset_clock = None;
    }
    t
}

/// Configuration flags for the conformance walk (`unseen` selects whether `Met`
/// has an intervening turn; the remaining flags enable negative-control mutations).
#[derive(Clone, Copy, Default)]
struct Run {
    /// A reader that takes usage credits for a spend wall.
    credits_as_wall: bool,
    masked: bool,
    unseen: bool,
    blind_to_untaken: bool,
    blind_to_a_persons_turn: bool,
    untied: bool,
    untied_continuation: bool,
    blind_in_window: bool,
    answer_read_as_taken: bool,
    blind_to_left: bool,
    blind_to_a_persons_left: bool,
    uncapped: bool,
}

#[derive(Clone)]
struct Real {
    st: TurnEndState,
    now: Instant,
    appeared: Instant,
    /// The wall's reset as the loop placed it.
    reset: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Act,
    Wait(Duration),
    Nothing,
    /// The own act's take window: waited out before anything is decided.
    Take,
    /// An act of another rule than the wall's (the continue policy's).
    Typed(&'static str),
    /// The owner told: the credits hold's word where a reader takes the
    /// credits notice for a spend wall (`Run::credits_as_wall`).
    Told,
}

fn expected(m: &Model, st: &State) -> Verdict {
    if st["wall"] != 1 && st["wall"] != 6 {
        return Verdict::Nothing;
    }
    if st["held"] == 1 {
        return Verdict::Take;
    }
    if m.action_enabled("Act", st) {
        return Verdict::Act;
    }
    let cap = cnst(m, "Cap");
    let due = (1..=cap)
        .find(|&k| {
            let mut later = st.clone();
            later.insert("clock", (st["clock"] + k).min(cap));
            later.insert("waited", st["waited"] + k);
            m.action_enabled("Act", &later)
        })
        .unwrap_or_else(|| panic!("a wall no tick ever acts on: {st:?}"));
    Verdict::Wait(MIN * u32::try_from(due).unwrap())
}

fn verdict(a: &TurnEndAction, now: Instant) -> Verdict {
    match a {
        TurnEndAction::Type { text, rule_id } if *rule_id == RULE_LIMIT_RESUME => {
            assert_eq!(*text, SupervisorConfig::default().continue_text);
            Verdict::Act
        }
        TurnEndAction::Type { rule_id, .. } => Verdict::Typed(rule_id),
        TurnEndAction::WaitUntil { why, .. } if why.starts_with("the worker to take") => {
            Verdict::Take
        }
        TurnEndAction::WaitUntil { until, .. } => {
            Verdict::Wait(until.saturating_duration_since(now))
        }
        TurnEndAction::Nothing => Verdict::Nothing,
        TurnEndAction::Escalate { .. } => Verdict::Told,
        other => panic!("an act the model has no word for: {other:?}"),
    }
}

fn mirror(
    m: &Model,
    action: &str,
    before: &State,
    after: &State,
    real: &mut Real,
    cfg: &SupervisorConfig,
    run: Run,
) -> bool {
    let now = real.now;
    let on_the_print = |appeared: Instant| appeared + placed_after_the_print(m, cnst(m, "ResetAt"));
    match action {
        // The world the decider sees only through the screen: background
        // work, and the account's tokens back before the named reset.
        "BackgroundStarts" | "BackgroundEnds" | "LimitLiftedEarly" => {}
        // A turn the credits carry: the worker at work, nothing for the
        // decider; its end an ordinary point after real work.
        "CreditsServes" => {}
        "CreditsTurnEnds" => {
            let mut quiet = after.clone();
            quiet.insert("wall", 0);
            let mut t = reading(&quiet, Some(LONG), real.reset, run, &real.st);
            t.phase = Phase::Idle;
            t.said_tail = Some("Stage done.".to_string());
            real.st.observe(&t, now);
        }
        // The Buggy model's misplaced reset is a wall like any other on the
        // REAL side: the real loop places the reset where the notice says.
        // Inside the own act's take window the wall answered that act at
        // once: no turn ran, no busy read.
        "WallAppears" | "WallAppearsLate" | "WallMisplaced" => {
            real.appeared = now;
            real.reset = on_the_print(now);
            let worked = (before["held"] == 0).then_some(LONG);
            let r = reading(after, worked, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // The limit answers the own act at once and its notice is cancelled
        // before any read: the first reading is the cancel under the notice,
        // no turn's work.
        "WallAppearsLeft" => {
            real.appeared = now;
            real.reset = on_the_print(now);
            let r = reading(after, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // A person's prompt at a turn's end the loop looked at (the idle
        // point folded in first), met by the limit at once and cancelled
        // before the loop's next read: the first reading is the cancel under
        // the notice, no turn's work.
        "PersonWallLeft" => {
            let mut quiet = before.clone();
            quiet.insert("wall", 0);
            let mut end = reading(&quiet, Some(LONG), real.reset, run, &real.st);
            end.phase = Phase::Idle;
            end.said_tail = Some("Stage done.".to_string());
            real.st.observe(&end, now);
            real.appeared = now;
            real.reset = on_the_print(now);
            let r = reading(after, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // Another conversation's older cancelled limit inside the own act's
        // window: read as the act's answer, its past clock placed as the
        // hostless loop places a reset nothing dated.
        "OlderLeftResumed" => {
            real.appeared = now;
            let placed = older_left_placed(run.uncapped);
            let off = Duration::from_secs(placed.unsigned_abs());
            real.reset = if placed >= 0 {
                now + off
            } else {
                now.checked_sub(off).expect("an instant half an hour back")
            };
            let r = reading(after, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // The continue policy's act at a turn's end, recorded as the loop
        // records one it typed.
        "OwnActTyped" => {
            let typed = TurnEndAction::Type {
                text: "keep going".to_string(),
                rule_id: RULE_CONTINUE,
            };
            let r = reading(before, None, real.reset, run, &real.st);
            real.st.acted(&typed, &r, now);
        }
        // Found standing, nothing to date its print: the session rule's
        // placement, in the past.
        "WallFoundSitting" => {
            real.appeared = now;
            real.reset = now
                .checked_sub(misplaced_before_the_print())
                .expect("an instant 11 h back");
            let r = reading(after, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // A tick past an act's take window is the window's end: the loop's
        // wait on it runs out, and it reads the point again, which judges the
        // act (`turn_end_now`).
        "Tick" => {
            if run.answer_read_as_taken
                && before["held"] == 1
                && before["tries"] >= 1
                && before["wall"] == 6
            {
                real.st.took();
            }
            real.now += MIN;
            if before["held"] == 1 {
                let r = reading(after, None, real.reset, run, &real.st);
                real.st.observe(&r, real.now);
            }
        }
        "Act" => {
            let r = reading(before, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
            let a = decide_turn_end(&real.st, &r, cfg, now);
            if !matches!(a, TurnEndAction::Type { .. }) {
                return false;
            }
            real.st.acted(&a, &r, now);
        }
        // The wall answers the act: printed again after a turn, or standing
        // with the line held and no turn run. Read BEFORE the point is folded
        // in, as the loop reads it: a print the loop watched, placed where the
        // notice says.
        "Met" => {
            // A `Met` while the act's window is open is a turn that ran into
            // the wall (`MetInWindow` is the answer with none run).
            let worked = (!run.unseen || before["held"] == 1).then_some(SHORT);
            let answered = !run.blind_to_untaken && real.st.untaken_act().is_some();
            if worked.is_some() || answered {
                real.reset = on_the_print(real.appeared);
            }
            let r = reading(after, worked, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // The wall answers the retry at once, no turn run, inside its window:
        // read as the loop reads it — a wall standing where its own act went
        // untaken answered that act, a print placed where the notice says —
        // and folded in with no point yet.
        // ... and its notice cancelled before any read: the first reading is
        // the cancel under the answer's notice.
        "MetInWindow" | "MetLeftUnread" => {
            let answered = !run.blind_to_untaken && real.st.untaken_act().is_some();
            if answered {
                real.reset = on_the_print(real.appeared);
            }
            let r = reading(after, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        "Taken" => {
            let mut quiet = after.clone();
            quiet.insert("wall", 0);
            let mut t = reading(&quiet, Some(LONG), real.reset, run, &real.st);
            t.phase = Phase::Idle;
            t.said_tail = Some("Stage done.".to_string());
            real.st.observe(&t, now);
        }
        // The notice leaves the screen: the left screen is a point, no work
        // since. Another conversation resumed there: its history is the
        // point, no work since.
        "NoticeLeaves" | "ConversationResumed" | "ArmedConversationResumed" => {
            let r = reading(after, None, real.reset, run, &real.st);
            real.st.observe(&r, now);
        }
        // A person's turn the account served, no read having seen it busy:
        // its screen read by the real reader, and folded in.
        "PersonTurnServed" => {
            let r = person_turned();
            let read = aterm_phase::read(Some("claude"), &r, Some(2));
            let mut t = TurnEndReading::of(&read, &r, false, None, None, None);
            t.restartable = true;
            assert_eq!(
                (t.wall, &t.phase, t.since_limit.as_ref().map(|s| s.served)),
                (None, &Phase::Idle, Some(true)),
                "{t:#?}"
            );
            if run.blind_to_a_persons_turn {
                t.since_limit = None;
            }
            real.st.observe(&t, now);
        }
        // The defects: not the real code's moves.
        "WallMasked"
        | "GivesUp"
        | "MetKeepsTheGuess"
        | "MemoryLost"
        | "LeftReadAsTurnEnd"
        | "PersonTurnUnseen"
        | "OtherLimitsTurnRead"
        | "OtherLimitsContinuationRead"
        | "TakeWindowDropsWall"
        | "RetryReadAsTaken"
        | "UnreadLeftDropped"
        | "UnreadAnswerReadAsTaken"
        | "OlderLeftPlacedTomorrow"
        | "CreditsReadAsWall" => return false,
        other => panic!("an action this bind does not mirror: {other}"),
    }
    true
}

/// THE REAL STATE'S WORD ON THE WAIT is the model's: it waits on a reset a
/// notice named exactly while the model's wall stands — shown, acted on, or
/// left (`wall != 0`).
fn waits_as_modelled(real: &Real, st: &State) -> bool {
    real.st.waits_on_named_reset() == (st["wall"] != 0)
}

/// What a walk saw: every state, the states the real side disagreed at (its
/// verdict there, or its wait after the transition into it), and how many
/// transitions each action fired.
struct Walked {
    states: BTreeSet<State>,
    off: Vec<State>,
    fired: BTreeMap<&'static str, usize>,
    /// The states each action left the real wait disagreeing from.
    off_after: BTreeMap<&'static str, Vec<State>>,
    /// Per action, how many fired with the real reset more than
    /// [`FIRED_EARLY`] ahead: where the vendor's word that it went on is
    /// another day's, under any notice.
    early: BTreeMap<&'static str, usize>,
}

fn walk(m: &Model, strict: bool, run: Run) -> Walked {
    walk_until(m, strict, run, false)
}

/// [`walk`], ended at the first disagreement where `first_off`: a defect
/// needs one state to be caught, and the rest of its walk is the sound one's.
fn walk_until(m: &Model, strict: bool, run: Run, first_off: bool) -> Walked {
    let cfg = SupervisorConfig::default();
    let base = Instant::now() + Duration::from_secs(24 * 3600);
    let init = Real {
        st: TurnEndState::new(timing(m)),
        now: base,
        appeared: base,
        reset: base,
    };
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    let mut disagree = Vec::new();
    let mut fired: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut off_after: BTreeMap<&'static str, Vec<State>> = BTreeMap::new();
    let mut early: BTreeMap<&'static str, usize> = BTreeMap::new();
    while let Some((st, real)) = queue.pop_front() {
        if first_off && !disagree.is_empty() {
            break;
        }
        if !seen.insert(st.clone()) {
            continue;
        }
        if strict {
            for inv in &m.invariants {
                assert!(m.check_invariant(inv.name, &st), "{} at {st:?}", inv.name);
            }
        }
        let mut at = real.st.clone();
        let here = reading(&st, None, real.reset, run, &real.st);
        at.observe(&here, real.now);
        let got = verdict(&decide_turn_end(&at, &here, &cfg, real.now), real.now);
        let want = expected(m, &st);
        if got != want {
            assert!(
                !strict,
                "the real decider disagrees at {st:?}: {got:?}, not {want:?}"
            );
            disagree.push(st.clone());
            if first_off {
                break;
            }
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(m, action.name, &st, &next, &mut r, &cfg, run) {
                *fired.entry(action.name).or_default() += 1;
                if r.reset.saturating_duration_since(r.now) > FIRED_EARLY {
                    *early.entry(action.name).or_default() += 1;
                }
                // Checked at the transition, not at the state: the walk
                // keeps the real side of the first path to reach a state.
                if !run.masked && !waits_as_modelled(&r, &next) {
                    assert!(
                        !strict,
                        "the real state's wait disagrees after {} at {next:?}: {}",
                        action.name,
                        r.st.waits_on_named_reset()
                    );
                    disagree.push(next.clone());
                    off_after.entry(action.name).or_default().push(st.clone());
                }
                queue.push_back((next, r));
            }
        }
    }
    Walked {
        states: seen,
        off: disagree,
        fired,
        off_after,
        early,
    }
}

#[test]
fn tier1_the_real_reader_and_decider_continue_a_usage_wall_exactly_where_the_model_allows() {
    let m = walled_model();
    // A wall that answers an act after a turn ran into it, and one that
    // answers with the line held and no turn run.
    for unseen in [false, true] {
        let run = Run {
            unseen,
            ..Run::default()
        };
        let states = walk(&m, true, run).states;
        let left = states.iter().filter(|st| st["wall"] == 6).count();
        let resumed = states.iter().filter(|st| st["other"] == 1).count();
        let armed_resumed = states.iter().filter(|st| st["other"] == 2).count();
        let early = states
            .iter()
            .filter(|st| st["wall"] == 6 && st["due"] < cnst(&m, "ResetAt"))
            .count();
        assert!(
            states.len() > 100 && left > 20 && resumed > 5 && armed_resumed > 5 && early > 5,
            "{} {left} {resumed} {armed_resumed} {early}",
            states.len()
        );
    }
}

/// Verify `ArmedConversationResumed` does not end the current limit wait when
/// the resumed conversation's auto-continue belongs to an older limit notice.
#[test]
fn the_loop_that_ties_no_continuation_to_its_limit_ends_the_wait_on_a_resume_at_an_armed_wall() {
    let m = walled_model();
    let run = Run {
        untied_continuation: true,
        ..Run::default()
    };
    let walked = walk(&m, false, run);
    let resumed = walked
        .fired
        .get("ArmedConversationResumed")
        .copied()
        .unwrap_or(0);
    let ended = walked
        .off_after
        .get("ArmedConversationResumed")
        .map_or(0, Vec::len);
    let early = walked
        .early
        .get("ArmedConversationResumed")
        .copied()
        .unwrap_or(0);
    assert!(
        resumed > early && early > 0,
        "{:?} {:?}",
        walked.fired,
        walked.early
    );
    assert_eq!(
        ended,
        resumed - early,
        "every resume at the armed wall in its reset's last minute ends the wait"
    );
    // ... and the model's defect is that loop: it types into the standing
    // limit before its tokens are back.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in [
        "WallAppears",
        "ArmedConversationResumed",
        "OtherLimitsContinuationRead",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
}

/// Verify `windowed_model` conformance across `OwnActTyped` and retry take windows.
#[test]
fn tier1_a_limit_met_inside_the_own_acts_take_window_is_tracked_where_the_model_says() {
    let m = windowed_model();
    // A wall that answers after the window too: a turn that ran into it, or
    // the line held with no turn run.
    for unseen in [false, true] {
        let walked = walk(
            &m,
            true,
            Run {
                unseen,
                ..Run::default()
            },
        );
        let states = &walked.states;
        // The continue policy's act (`tries = 0`) and the retry, each with a
        // wall read inside its window, with its notice left there, and with
        // its notice cancelled before any read.
        let count = |pick: &dyn Fn(&State) -> bool, retried: bool| {
            states
                .iter()
                .filter(|st| pick(st) && (st["tries"] >= 1) == retried)
                .count()
        };
        for retried in [false, true] {
            assert!(
                count(&|st| st["held"] == 1 && st["wall"] == 1, retried) > 2
                    && count(&|st| st["held"] == 1 && st["wall"] == 6, retried) > 2
                    && count(&|st| st["unread"] == 1, retried) > 2,
                "retried {retried}, unseen {unseen}"
            );
        }
        for action in [
            "OwnActTyped",
            "MetInWindow",
            "WallAppearsLeft",
            "MetLeftUnread",
            "PersonWallLeft",
            "OlderLeftResumed",
        ] {
            assert!(
                walked.fired.contains_key(action),
                "{action} never fired: {:?}",
                walked.fired
            );
        }
    }
}

/// Verify the `Buggy = 1` model and each [`Run`] negative-control mutation
/// disagree with the real implementation.
#[test]
fn the_buggy_model_and_every_past_loop_disagree_with_the_real_code() {
    let walled = walled_model();
    // The mask is the shell's: walked with no notice leaving the screen,
    // where a wall the masked reader never read would be left unread too.
    let unleft = aterm_spec::interp::with_consts(&walled, &[("Leaves", 0)]);
    let buggy = aterm_spec::interp::with_buggy(&walled, 1);
    let windowed = windowed_model();
    let read_windowed = read_windowed_model();
    let on = Run::default();
    let cases: [(&str, &Model, Run); 10] = [
        ("the Buggy model", &buggy, on),
        (
            "2026-09-28: busy beats a wall",
            &unleft,
            Run { masked: true, ..on },
        ),
        (
            "2026-10-01: no print in an untaken act",
            &walled,
            Run {
                unseen: true,
                blind_to_untaken: true,
                ..on
            },
        ),
        (
            "before 2026-10-14: blind to a person's turn",
            &walled,
            Run {
                blind_to_a_persons_turn: true,
                ..on
            },
        ),
        (
            "44ed0f339: a served turn tied to no limit",
            &walled,
            Run { untied: true, ..on },
        ),
        (
            "1afcdade3: no limit under a cancel",
            &windowed,
            Run {
                blind_to_left: true,
                ..on
            },
        ),
        (
            "15248f2b3: a cancel's limit only as an act's answer",
            &windowed,
            Run {
                blind_to_a_persons_left: true,
                ..on
            },
        ),
        (
            "15248f2b3: an older cancelled limit on the act's print",
            &windowed,
            Run {
                uncapped: true,
                ..on
            },
        ),
        (
            "1d54a69e9: a retry's answer judged its taking",
            &read_windowed,
            Run {
                answer_read_as_taken: true,
                ..on
            },
        ),
        (
            "before round 16: a wall folded in only at a point",
            &read_windowed,
            Run {
                blind_in_window: true,
                ..on
            },
        ),
    ];
    std::thread::scope(|scope| {
        for (name, m, run) in cases {
            scope.spawn(move || {
                assert!(
                    !walk_until(m, false, run, true).off.is_empty(),
                    "{name}: agreed with the real code everywhere"
                );
            });
        }
    });
}

/// USAGE CREDITS, WALKED: every reachable state of the model with credits
/// carrying a turn agrees with the real reader and decider — at a credits
/// state the reader reads credits in use and no wall, and nothing is
/// decided — and NEGATIVE CONTROL: a reader that takes the notice for a spend
/// wall disagrees at every credits state, and nowhere else — the model's
/// `CreditsReadAsWall`, caught by `NoWallOnCredits`.
#[test]
fn tier1_usage_credits_are_no_wall_where_the_model_says() {
    let m = credits_model();
    let walked = walk(&m, true, Run::default());
    let on = walked
        .states
        .iter()
        .filter(|st| st["credits"] == 1)
        .collect::<Vec<_>>();
    assert!(!on.is_empty(), "{:?}", walked.fired);
    for st in &on {
        let read = aterm_phase::read(Some("claude"), &rows(st), Some(2));
        assert!(read.credits.is_some() && read.wall.is_none(), "{st:?}");
    }
    let off = walk(
        &m,
        false,
        Run {
            credits_as_wall: true,
            ..Run::default()
        },
    )
    .off;
    assert!(
        !off.is_empty(),
        "the credits-as-wall reader agreed everywhere"
    );
    assert!(off.iter().all(|st| st["credits"] == 1), "{off:?}");
    assert_eq!(
        off.iter().collect::<BTreeSet<_>>().len(),
        on.len(),
        "at every credits state"
    );
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in ["CreditsServes", "CreditsReadAsWall"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoWallOnCredits", &st), "{st:?}");
}
