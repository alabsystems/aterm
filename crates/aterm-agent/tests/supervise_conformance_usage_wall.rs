// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 for `SupervisorUsageWall` (aterm-spec
//! `supervisor_usage_wall_model`): the REAL screen reader (`aterm_phase::read`,
//! over the measured 2026-09-28 weekly wall with and without the background
//! shell the tab counted) and the REAL turn-end decider (`decide_turn_end`,
//! with the real `TurnEndState` it keeps) driven along EVERY reachable state
//! of the model, so "a wall whose tokens are back does not stand for good" is
//! the code's, state for state.
//!
//! Each model action is mirrored on the real side. The world (`BackgroundStarts`,
//! `BackgroundEnds`, the account's `due` tick) is invisible to the decider
//! except through the SCREEN: `bg` decides whether the done row counts `1 shell
//! still running` and the footer `· 1 shell ·`, and the real reader must still
//! hand the decider the wall; a wall appearing is the point `observe`d with
//! the turn's work; time at the wall is the real clock moved a minute per tick
//! (`reset_grace` = `Grace`, `limit_backoff` = `Rung1`, `Rung2`); `Act` is the
//! decider's own act, recorded with `acted`; `Met` is the next point observed
//! with the wall on the screen again, `Taken` a point with none after real
//! work. At every state the model's invariant holds and the decider TYPES
//! exactly where `Act` is enabled (`usage-resume@v1`, `keep going`), and
//! otherwise WAITS until exactly the tick `Act` becomes enabled.
//!
//! The reset the decider is handed is the REAL placement's, kept on the real
//! side as the loop keeps it: a wall seen arriving is placed on its print
//! ([`place_reset`]); a wall FOUND sitting has no print to go by and is placed
//! by the session window's rule, its far bare clock in the past (`view = 1`);
//! and at `Met` the wall is placed on the print again exactly where the loop
//! reads it as one it watched — a turn seen run before it, or the act left
//! untaken by the REAL `TurnEndState::untaken_act` (Claude Code held the
//! line, no turn ran: the walk runs `Met` both ways).
//! NEGATIVE CONTROLS: the `Buggy = 1` model disagrees (a first act at once); the same walk with the
//! reader replaced by the precedence of 2026-09-28 (Busy beats a wall, so a
//! screen that counts a shell reads busy) disagrees at every `bg = 1` wall —
//! the stall the model's liveness calls `WallMasked`; and the loop of
//! 2026-10-01, which read a wall standing after an untaken act as no print,
//! disagrees after every unseen `Met` of a wall found sitting.
//!
//! THE LEFT WALL (`NoticeLeaves`, the review of 2026-10-14): the wall's notice
//! leaves an idle screen — here the measured wall with a person's `/model`
//! under it on a tab with no shell, which the real reader reads idle with no
//! wall — and the real decider waits on the reset the notice named and acts
//! there exactly where the model's `Act` is enabled at `wall == 6`. A person's
//! turn there that the account serves (`PersonTurnServed`), over before any
//! read saw it busy, is its prompt and reply drawn under the notice: read by
//! the real `TurnEndReading::of` and folded in by the real `observe`, it ends
//! the wait. At every transition the real state's own word on the wait
//! (`TurnEndState::waits_on_named_reset`) is the model's (`wall != 0`).
//! The account may lift the limit BEFORE the reset the notice named
//! (`LimitLiftedEarly`: the decider cannot see it, and a turn served then is
//! served before the reset). Another conversation `/resume`d there
//! (`ConversationResumed`) is ITS history on the screen — the measured 2.1.299
//! screen of a different limit (`resets 10:38am`), a person's turn served
//! under it — and the real decider, which ties a served turn to the notice of
//! the limit it waits on, waits on to the reset. So for one `/resume`d while
//! the wall SHOWS, its countdown armed (`ArmedConversationResumed`, W1 of
//! ATERM-KI-1015-1): the measured 2.1.300 screen of an older limit the vendor
//! continued by itself, whose word the real decider ties to that limit's
//! notice, not this one's.
//! NEGATIVE CONTROLS: the loop before 2026-10-14, which read a person's turn
//! no read saw busy as no turn, still waits after EVERY `PersonTurnServed`,
//! before the reset too — the model's `PersonTurnUnseen`; the loop of
//! 44ed0f339, which counted a turn served under ANY limit's notice, ends the
//! wait after every `ConversationResumed` — the model's
//! `OtherLimitsTurnRead`; and the loop of ef02ee212, which read the vendor's
//! continuation under ANY limit's notice as this limit's, ends it after every
//! `ArmedConversationResumed` — the model's `OtherLimitsContinuationRead`.
//!
//! THE OWN ACT'S TAKE WINDOW (`OwnActTyped`, `held`; round 16): the continue
//! policy's act at a turn's end is recorded as typed (`acted`), with the real
//! take window (30 s, under a tick); a wall that appears while it is open
//! answered it at once — read with no turn's work, no busy read — and the
//! real `observe`, which does not judge the act inside its window, tracks the
//! wall all the same: the real state waits on the reset its notice named
//! from that first reading, and through its notice leaving inside the
//! window; the decider waits out the window (the model's `Act` is off while
//! `held`) and then the reset. NEGATIVE CONTROL: the loop before round 16,
//! which folded a wall in only at a point, waits on nothing after a wall
//! that appears inside the window — the model's `TakeWindowDropsWall`.
//!
//! THE RETRY'S TAKE WINDOW (`Act` opens `held`; round 16's review): the
//! decider's own `usage-resume@v1` has the real window too. `MetInWindow` is
//! the wall answering it at once (the stub's immediate 429): read inside the
//! window with no turn's work — placed on the print, as the loop places a
//! wall that answered an act of its own untaken (the real
//! `TurnEndState::untaken_act`, inside the window or past it) — and its
//! notice may leave there (`NoticeLeaves`). The window's end, finding no
//! wall, judges the retry MET, never taken: the real state waits on, and the
//! decider waits the next rung, where the model's `Act` is enabled again.
//! NEGATIVE CONTROL: the loop of 1d54a69e9, which read that answer and still
//! judged the retry taken at the window's end (emulated by a busy read
//! there, the real `took`), stops waiting exactly after those windows — the
//! model's `RetryReadAsTaken`.
//!
//! THE NOTICE GONE BEFORE ANY READ (`WallAppearsLeft`, `MetLeftUnread`,
//! `unread`; round 16's residual (a), the lab's `tw16after` on 2.1.300): the
//! limit answers the act at once and a person's Esc on the vendor's countdown
//! comes before the loop's first read — the screen it reads is the wall with
//! the vendor's cancel (`⏺ Automatic continue cancelled · …`) under its
//! notice, no wall. Read as the loop reads it (the real
//! `TurnEndState::left_answer` over the real `aterm_phase::left_wall`, the
//! wall placed on the print as an answer is), the real state waits on the
//! reset from that reading, through the window, exactly where the model's
//! wall stands. NEGATIVE CONTROL: the loop of 1afcdade3, which read no limit
//! there, waits on nothing after every `WallAppearsLeft` and forgets the wall
//! at the end of every retry's window its answer left unread — the model's
//! `UnreadLeftDropped` and `UnreadAnswerReadAsTaken`.
//!
//! A PERSON'S TURN ANSWERED THE SAME WAY (`PersonWallLeft`, round 17's
//! residual (a)): the loop looked at a turn's end (an idle point folded in by
//! the real `observe`), and a person's prompt there meets the limit, their
//! Esc on the countdown before the loop's next read. The real `left_answer`
//! reads the cancel under the notice as the answer to a turn asked since that
//! look, and the real state waits on the reset exactly where the model's wall
//! stands; the loop of 15248f2b3 (`blind_to_a_persons_left`: it read a cancel's limit
//! only as an act's answer) waits on nothing after every `PersonWallLeft`. AN
//! OLDER CANCELLED LIMIT `/resume`d inside the own act's window
//! (`OlderLeftResumed`): read as the act's answer with nothing to date its
//! print (the bind is the hostless loop: an answer the screen cannot tell
//! from the notice already seen is no answer here), its bare clock already
//! past is placed by the real `place_reset` on the act's print and then kept
//! within a session window by the real `within_a_session_window` — passed:
//! the decider acts at the window's end, where the model's `Act` is enabled.
//! NEGATIVE CONTROL: the loop of 15248f2b3 (`uncapped`), which kept the
//! act's print's placement, waits a day there — the model's
//! `OlderLeftPlacedTomorrow`.
//!
//! USAGE CREDITS (`Credits`, accounts v2 S5): a turn the credits carry is the
//! measured `Now using usage credits` footer on a running turn, which the real reader
//! reads as no wall and the decider leaves alone, as the model's
//! `NoWallOnCredits` says. NEGATIVE CONTROL: a reader that takes the notice
//! for a spend wall disagrees at every credits state — the model's
//! `CreditsReadAsWall`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::SupervisorConfig;
use aterm_agent::supervise::limit::{ResetSpec, place_reset, reset_at, within_a_session_window};
use aterm_agent::supervise::phase::Phase;
use aterm_agent::supervise::policy::turn_end::{
    RULE_CONTINUE, RULE_LIMIT_RESUME, TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming,
    decide_turn_end,
};
use aterm_phase::prompt::fixtures::screen;
use aterm_spec::derive::{Model, supervisor_usage_wall_model};

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

/// The model as this bind reads it: the wall's rows never leave the screen
/// behind the agent's shell (`Hidden = 0`). A wall that scrolls out of view is
/// remembered by the loop's `Session` (`wall_memory.rs`), which a reader and a
/// decider do not reach;
/// `tier1_the_real_loop_remembers_a_hidden_wall_exactly_where_the_model_does`
/// (`turn_end_loop_tests.rs`) binds that half, on the real `Session`. A notice
/// that leaves an IDLE screen (`Leaves = 1`) is the decider's own to wait out
/// (`turn_end.rs`'s `still_shown`), and is walked here. The own act's take
/// window is off (`OwnAct = 0`), and the act's window with it ([`timing`]):
/// [`windowed_model`] walks it.
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

/// The weekly wall says `resets <a bare clock>`, `ResetAt` ticks (minutes)
/// after its print, as the vendor prints one for a reset under a day off. The
/// reset the loop waits for is the REAL placement's ([`place_reset`], the print
/// known to the done row's minute and the slack): how many minutes after the
/// print the decider is handed — so the placement is inside the bind, and a
/// misplacement ([`reset_at`], the session window's five-hour rule) is a
/// disagreement the walk sees.
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

/// THE OLDER CANCELLED LIMIT a `/resume`d conversation ends on (`7:30pm`, its
/// reset half an hour before the print), read as the own act's answer with
/// nothing to date it: how far from the act the decider is handed its reset —
/// the real placement on the act's print ([`place_reset`]: tomorrow's,
/// 23 h 30 m out), kept within a session window by the real
/// [`within_a_session_window`] unless `uncapped` (the loop of 15248f2b3):
/// half an hour PASSED. Positive: ahead of the act.
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
fn rows(st: &State) -> Vec<String> {
    match st["wall"] {
        1 => walled(st["bg"] == 1),
        6 if st["other"] == 1 => resumed(),
        6 if st["other"] == 2 => resumed_continued(),
        6 if st["unread"] == 1 => cancelled(),
        6 => left(),
        _ if st["credits"] == 1 => on_credits(),
        _ => screen(LIVE),
    }
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
/// countdown armed: the measured 2.1.300 screen (W1 of ATERM-KI-1015-1) —
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

/// What the reader hands the decider at `st`, the wall's reset as the real
/// side placed it (`reset`), as `run` reads it. `masked`: the precedence of
/// 2026-09-28 — a screen that counts background work is busy, whatever the
/// turn ended on.
fn reading(
    st: &State,
    worked: Option<Duration>,
    reset: Instant,
    run: Run,
    real: &TurnEndState,
) -> TurnEndReading {
    let r = rows(st);
    let mut read = aterm_phase::read(Some("claude"), &r, Some(2));
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
    // The limit a cancel left that answered a turn — the act awaited, or a
    // person's asked since the last look — read in the screen's place as the
    // hostless loop reads it (`Session::turn_end_reading`): an answer the
    // screen cannot tell from the notice already seen is refused there with
    // no transcript. The loop of 1afcdade3 read no limit there, the loop of
    // 15248f2b3 none but an act's.
    if read.wall.is_none()
        && !run.blind_to_left
        && let Some(w) = real
            .left_answer(&r)
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
    let mut t = TurnEndReading::of(&read, &r, false, worked, reset_at, None);
    t.restartable = true;
    if run.masked && st["bg"] == 1 && st["wall"] == 1 {
        t.phase = Phase::Busy;
        t.wall = None;
        t.reset_at = None;
        t.wall_message = String::new();
    }
    // The loop of 44ed0f339: a turn served under ANY limit's notice is one
    // since the limit waited on (its witness, tied to no limit).
    if run.untied && t.since_limit.as_ref().is_some_and(|s| s.served) {
        t.served_in_transcript = true;
    }
    // The loop of ef02ee212: the vendor's word that it went on under ANY
    // limit's notice is this limit's (tied to none): read as if its notice
    // were the wall's own.
    if run.untied_continuation && t.auto_continue == Some(aterm_phase::AutoContinue::Fired) {
        let own = aterm_phase::read(Some("claude"), &walled(false), Some(2))
            .wall
            .expect("the measured wall");
        t.since_limit = Some(aterm_phase::SinceLimit {
            kind: own.kind,
            reset: own.reset,
            served: false,
            model_set: false,
            chosen_model: None,
        });
    }
    // The loop before round 16: inside the own act's take window a wall
    // was folded in only at the point that judged the act — read here as
    // no wall, which the policy now folds nowhere either.
    if run.blind_in_window && st["held"] == 1 {
        t.wall = None;
        t.wall_message = String::new();
        t.reset_at = None;
        t.reset_clock = None;
    }
    t
}

/// How the real side runs: `unseen`, a `Met` is the line held under the wall
/// with no turn run (no busy read), else a turn that ran into it again;
/// `blind_to_untaken`, the loop of 2026-10-01, which read a wall standing after
/// an untaken act as no print (the NEGATIVE CONTROL), else the loop as it is;
/// `blind_to_a_persons_turn`, the loop before 2026-10-14, which read a
/// person's turn no read saw busy as no turn (the NEGATIVE CONTROL of the left
/// wall), else the loop as it is; `untied`, the loop of 44ed0f339, which
/// counted a turn served under any limit's notice as one since the limit it
/// waits on (the NEGATIVE CONTROL of a resumed conversation);
/// `blind_in_window`, the loop before round 16, which folded a wall read
/// inside the own act's take window in only at the point that judged the act
/// (the NEGATIVE CONTROL of the window); `answer_read_as_taken`, the loop of
/// 1d54a69e9, which read the wall that answered a retry inside its window
/// but judged the retry taken at the window's end, its answer's notice gone
/// (the NEGATIVE CONTROL of the retry's window: emulated by a busy read at
/// that window's end, the real [`TurnEndState::took`], which is what that
/// judgement amounts to); `blind_to_left`, the loop of 1afcdade3, which read
/// no limit where a cancel left the notice that answered the act awaited
/// (the NEGATIVE CONTROL of the notice gone before any read);
/// `blind_to_a_persons_left`, the loop of 15248f2b3, which read a cancel's
/// limit as an answer to an act of its own only (the NEGATIVE CONTROL of a
/// person's turn answered so); `uncapped`, the same loop, which placed an
/// older cancelled limit read as an act's answer on that act's print (the
/// NEGATIVE CONTROL of a `/resume` inside the act's window);
/// `untied_continuation`, the loop of ef02ee212, which read the vendor's word
/// that it went on (`Fired`) under any limit's notice as this limit's (the
/// NEGATIVE CONTROL of a conversation resumed at an armed limit).
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
            assert_eq!(text, "keep going");
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
            // The retry's window ends on its answer, its notice gone:
            // judged taken by the loop of 1d54a69e9.
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
/// verdict there, or its wait after the transition into it), and each
/// action's transitions — how many fired, and the states BEFORE each one
/// after which the real wait disagreed.
struct Walked {
    states: BTreeSet<State>,
    off: Vec<State>,
    fired: BTreeMap<&'static str, usize>,
    off_after: BTreeMap<&'static str, Vec<State>>,
}

fn walk(m: &Model, strict: bool, run: Run) -> Walked {
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
    while let Some((st, real)) = queue.pop_front() {
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
        }
        for action in &m.actions {
            let mut next = st.clone();
            if !m.fire(action.name, &mut next) {
                continue;
            }
            let mut r = real.clone();
            if mirror(m, action.name, &st, &next, &mut r, &cfg, run) {
                *fired.entry(action.name).or_default() += 1;
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
        let walked = walk(&m, true, run);
        let states = &walked.states;
        let left = states.iter().filter(|st| st["wall"] == 6).count();
        let resumed = states.iter().filter(|st| st["other"] == 1).count();
        let armed_resumed = states.iter().filter(|st| st["other"] == 2).count();
        let early = states
            .iter()
            .filter(|st| st["wall"] == 6 && st["due"] < cnst(&m, "ResetAt"))
            .count();
        eprintln!(
            "usage wall Tier-1 (unseen {unseen}): {} states walked, {left} of them left walls \
             ({resumed} on a resumed conversation, {armed_resumed} on one resumed at the armed \
             wall, {early} lifted before the reset)",
            states.len()
        );
        assert!(
            states.len() > 100 && left > 20 && resumed > 5 && armed_resumed > 5 && early > 5,
            "{} {left} {resumed} {armed_resumed} {early}",
            states.len()
        );
    }
}

/// THE LOOP OF 2026-10-01 is not the model: a wall found sitting, its far bare
/// clock guessed passed, tried once and answered with the line held — no turn
/// run, no busy read — kept the guess, and the ladder typed into the standing
/// wall at every rung (24 lines until the reset). Read without
/// `untaken_act`, the walk disagrees exactly there: after an unseen `Met` of a
/// wall found sitting, where the model waits for the reset. With it (the
/// strict walk above), nowhere.
#[test]
fn the_loop_that_reads_no_print_in_an_untaken_act_types_into_a_standing_wall() {
    let m = walled_model();
    let run = Run {
        unseen: true,
        blind_to_untaken: true,
        ..Run::default()
    };
    let off = walk(&m, false, run).off;
    assert!(!off.is_empty(), "the blind loop agreed everywhere");
    // The wall showing, or its notice left: the same wait either way.
    assert!(
        off.iter().all(|st| (st["wall"] == 1 || st["wall"] == 6)
            && st["sitting"] == 1
            && st["view"] == 0
            && st["tries"] >= 1),
        "{off:?}"
    );
    // ... and the model's defect `MetKeepsTheGuess` is that loop: it types a
    // second act into the wall before the tokens are back.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in ["WallFoundSitting", "Act", "MetKeepsTheGuess", "Tick", "Act"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
    let mut st = m.init_state();
    for a in ["WallFoundSitting", "Act", "Met", "Tick"] {
        assert!(m.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!m.action_enabled("Act", &st), "{st:?}");
}

/// NEGATIVE CONTROLS: the reader of 2026-09-28 (a screen that counts a shell is
/// busy) disagrees at every wall that shows over background work; and the
/// `Buggy = 1` model (a first act at once, a reset placed in the past) is not
/// the real decider either.
#[test]
fn the_masked_reader_and_the_buggy_model_disagree_with_the_real_code() {
    let m = walled_model();
    let seen = |masked: bool| Run {
        masked,
        ..Run::default()
    };
    // The mask is the shell's: walked with no notice leaving the screen,
    // where a wall the masked reader never read would be left unread too.
    let unleft = aterm_spec::interp::with_consts(&m, &[("Leaves", 0)]);
    let masked = walk(&unleft, false, seen(true)).off;
    assert!(!masked.is_empty(), "the masked reader agreed everywhere");
    assert!(
        masked.iter().all(|st| st["bg"] == 1 && st["wall"] == 1),
        "{masked:?}"
    );
    // The placement control: the session window's rule, for the same wall a
    // day-less clock 13 h off, is in the PAST — the misplacement the model's
    // `view = 1` stands for, which the real placement makes only for a wall
    // found with nothing to date its print.
    let nine = ResetSpec::At {
        date: None,
        hour: 9,
        minute: 0,
        zone: Some("America/Los_Angeles".to_string()),
    };
    assert!(reset_at(&nine, PRINT, PDT, zone) < PRINT);
    assert_eq!(
        place_reset(&nine, false, PRINT, Some(PRINT - 7 * 60), PDT, zone),
        PRINT + 13 * 3600
    );
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let off = walk(&buggy, false, seen(false)).off;
    assert!(
        off.iter()
            .any(|st| st["wall"] == 1 && st["view"] == 1 && st["sitting"] == 0 && st["tries"] == 0),
        "no disagreement at a misplaced reset's first act: {} states",
        off.len()
    );
}

/// THE LOOP BEFORE 2026-10-14 is not the model: at a wall whose notice left the
/// screen it read a person's turn no read saw busy as no turn, and waited on
/// the reset over it — the model's `PersonTurnUnseen`. Walked with that reading
/// (the turn's own evidence dropped), the real state still waits after EVERY
/// `PersonTurnServed`, where the model's wall is over — some of them served
/// before the named reset (`LimitLiftedEarly`), the hold spanning the wait;
/// with the notice's leaving switched off, nowhere — the control is the left
/// wall's alone.
#[test]
fn the_loop_blind_to_a_persons_turn_waits_over_it() {
    let m = walled_model();
    let run = Run {
        blind_to_a_persons_turn: true,
        ..Run::default()
    };
    let walked = walk(&m, false, run);
    assert!(!walked.off.is_empty(), "the blind loop agreed everywhere");
    let served = walked.fired.get("PersonTurnServed").copied().unwrap_or(0);
    let held = walked
        .off_after
        .get("PersonTurnServed")
        .cloned()
        .unwrap_or_default();
    assert!(served > 0, "{:?}", walked.fired);
    assert_eq!(held.len(), served, "every served turn is waited over");
    assert!(
        held.iter().any(|st| st["clock"] < cnst(&m, "ResetAt")),
        "served before the reset: {held:?}"
    );
    let unleft = aterm_spec::interp::with_consts(&m, &[("Leaves", 0)]);
    let off = walk(&unleft, false, run).off;
    assert!(off.is_empty(), "{off:?}");
    // ... and the model's defect is that loop.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in [
        "WallAppears",
        "NoticeLeaves",
        "Tick",
        "Tick",
        "PersonTurnUnseen",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(
        !buggy.check_invariant("NoWaitOverAServedTurn", &st),
        "{st:?}"
    );
}

/// THE LOOP OF 44ED0F339 is not the model: it counted a turn served under ANY
/// limit's notice as one since the limit it waits on, so a person who
/// `/resume`d another conversation at the left wall — its history redrawn, an
/// older limit with a turn served under it — ended the wait, and the continue
/// policy typed into the standing limit (measured on 2.1.300 by the review of
/// 2026-10-14) — the model's `OtherLimitsTurnRead`. Walked with that reading,
/// the real state stops waiting after EVERY `ConversationResumed`, where the
/// model's limit stands; the loop as it is (the strict walk) waits on.
#[test]
fn the_loop_that_ties_no_turn_to_its_limit_ends_the_wait_on_a_resumed_conversation() {
    let m = walled_model();
    let run = Run {
        untied: true,
        ..Run::default()
    };
    let walked = walk(&m, false, run);
    let resumed = walked
        .fired
        .get("ConversationResumed")
        .copied()
        .unwrap_or(0);
    let ended = walked
        .off_after
        .get("ConversationResumed")
        .map_or(0, Vec::len);
    assert!(resumed > 0, "{:?}", walked.fired);
    assert_eq!(ended, resumed, "every resumed conversation ends the wait");
    // ... and the model's defect is that loop: it types into the standing
    // limit before its tokens are back.
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = buggy.init_state();
    for a in [
        "WallAppears",
        "NoticeLeaves",
        "ConversationResumed",
        "OtherLimitsTurnRead",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
}

/// THE LOOP OF EF02EE212 is not the model (W1 of ATERM-KI-1015-1, measured
/// on 2.1.300 on 2026-10-15): it read the vendor's word that it went on
/// (`Usage limit reset · continuing automatically`) as THIS limit's
/// continuation under ANY limit's notice, so a person who `/resume`d another
/// conversation while the wall showed, its countdown armed — that history an
/// older limit the vendor continued by itself — ended the wait, and the
/// continue policy typed into the standing limit (the stub's 429, 22 minutes
/// before the reset) — the model's `OtherLimitsContinuationRead`. Walked with
/// that reading, the real state stops waiting after EVERY
/// `ArmedConversationResumed`, where the model's limit stands; the loop as it
/// is (the strict walk) waits on, the vendor's word tied to this limit's own
/// notice by the reset it names.
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
    assert!(resumed > 0, "{:?}", walked.fired);
    assert_eq!(
        ended, resumed,
        "every resume at the armed wall ends the wait"
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

/// THE OWN ACT'S TAKE WINDOW (round 16; round 15's run on Claude Code 2.1.300,
/// 2026-10-14): the continue policy's act at a turn's end, answered at once by
/// a limit (no busy read), its notice leaving inside the window — the real
/// state tracks the limit from its first reading and the decider waits out
/// the window, then the reset the notice named, exactly where the model does.
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
        let held = |wall: i64, retried: bool| {
            states
                .iter()
                .filter(|st| st["held"] == 1 && st["wall"] == wall && (st["tries"] >= 1) == retried)
                .count()
        };
        eprintln!(
            "usage wall Tier-1 (the own acts' windows, unseen {unseen}): {} states walked; \
             the continue policy's act: {} with a wall read inside the window, {} with its \
             notice left there; the retry: {} answered inside its window, {} left there",
            states.len(),
            held(1, false),
            held(6, false),
            held(1, true),
            held(6, true)
        );
        let unread = |retried: bool| {
            states
                .iter()
                .filter(|st| st["unread"] == 1 && (st["tries"] >= 1) == retried)
                .count()
        };
        eprintln!(
            "  ... their notice cancelled before any read: {} after the continue policy's act, \
             {} after the retry",
            unread(false),
            unread(true)
        );
        assert!(
            held(1, false) > 2
                && held(6, false) > 2
                && held(1, true) > 2
                && held(6, true) > 2
                && unread(false) > 2
                && unread(true) > 2
                && walked.fired.contains_key("OwnActTyped")
                && walked.fired.contains_key("MetInWindow")
                && walked.fired.contains_key("WallAppearsLeft")
                && walked.fired.contains_key("MetLeftUnread")
                && walked.fired.contains_key("PersonWallLeft")
                && walked.fired.contains_key("OlderLeftResumed"),
            "{:?}",
            walked.fired
        );
    }
}

/// THE LOOP OF 1AFCDADE3 is not the model: where the limit answered the act at
/// once and a person's Esc on the vendor's countdown came before the loop's
/// first read, it read the cancel under the notice as no limit (round 16's
/// residual (a), the lab's `tw16after` on 2.1.300) — so nothing tracked a
/// limit that answered the continue policy's act, and a retry's answer
/// cancelled so read as the retry taken at its window's end. Walked with that
/// reading, the real state disagrees after EVERY `WallAppearsLeft` (it waits
/// on nothing where the model's wall stands), and after the window's end of
/// every retry whose answer was cancelled unread (the track dropped); with
/// the own acts' windows switched off, or every notice read before it leaves
/// (`Unread = 0`), nowhere — the control is the unread answer's alone (the
/// strict walk, the loop as it is, agrees everywhere).
#[test]
fn the_loop_blind_to_the_cancel_under_the_acts_answer_waits_on_nothing() {
    let run = Run {
        blind_to_left: true,
        ..Run::default()
    };
    let walked = walk(&windowed_model(), false, run);
    let left = walked.fired.get("WallAppearsLeft").copied().unwrap_or(0);
    let dropped = walked.off_after.get("WallAppearsLeft").map_or(0, Vec::len);
    assert!(left > 0, "{:?}", walked.fired);
    assert_eq!(dropped, left, "every unread answer waits on nothing");
    let ends = walked.off_after.get("Tick").cloned().unwrap_or_default();
    assert!(
        ends.iter()
            .any(|st| st["unread"] == 1 && st["tries"] >= 1 && st["held"] == 1),
        "a retry's window ending on its unread answer: {:?}",
        walked.off_after.keys()
    );
    // ... the person's turn too, where the own act is switched off: after
    // every `PersonWallLeft`.
    let walled = walk(&walled_model(), false, run);
    let person = walled.fired.get("PersonWallLeft").copied().unwrap_or(0);
    assert!(person > 0, "{:?}", walled.fired);
    assert_eq!(
        walled.off_after.get("PersonWallLeft").map_or(0, Vec::len),
        person,
        "every person's turn left unread waits on nothing"
    );
    let off = walk(&read_windowed_model(), false, run).off;
    assert!(off.is_empty(), "every notice read: {off:?}");
    // ... and the model's defects are that loop: it types into the standing
    // limit before its tokens are back, and forgets the wall a retry met.
    let buggy = aterm_spec::interp::with_buggy(&windowed_model(), 1);
    let mut st = buggy.init_state();
    for a in ["OwnActTyped", "WallAppearsLeft", "UnreadLeftDropped"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
    let mut st = buggy.init_state();
    for a in [
        "WallAppearsLate",
        "Tick",
        "Tick",
        "Tick",
        "Act",
        "MetLeftUnread",
        "UnreadAnswerReadAsTaken",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("WallRemembered", &st), "{st:?}");
}

/// THE LOOP OF 15248F2B3 is not the model (round 17's residual (a)): it read
/// the limit a cancel left as the answer to an act of its own only — so a
/// person's turn the limit answered at once, cancelled before any read, was
/// read as no limit and the continue policy's next act went into it — and it
/// placed an older cancelled limit a conversation `/resume`d inside the act's
/// window ended on, which it read as the act's answer, on that act's print:
/// a clock already past was tomorrow's. Walked with the first reading, the
/// real state disagrees after EVERY `PersonWallLeft`; with the second, the
/// decider disagrees exactly where an `OlderLeftResumed` wall stands past the
/// act's window (it waits a day where the model acts), and nowhere else.
#[test]
fn the_loop_that_reads_a_cancel_only_as_its_own_acts_answer_misses_a_person_and_waits_a_day() {
    let run = Run {
        blind_to_a_persons_left: true,
        ..Run::default()
    };
    let walked = walk(&windowed_model(), false, run);
    let person = walked.fired.get("PersonWallLeft").copied().unwrap_or(0);
    assert!(person > 0, "{:?}", walked.fired);
    assert_eq!(
        walked.off_after.get("PersonWallLeft").map_or(0, Vec::len),
        person,
        "every person's turn left unread waits on nothing"
    );
    let run = Run {
        uncapped: true,
        ..Run::default()
    };
    let walked = walk(&windowed_model(), false, run);
    assert!(
        !walked.off.is_empty(),
        "the uncapped loop agreed everywhere"
    );
    assert!(
        walked
            .off
            .iter()
            .all(|st| st["wall"] == 6 && st["sitting"] == 1 && st["view"] == 1 && st["held"] == 0),
        "only an older cancelled limit past the act's window: {:?}",
        walked.off
    );
    assert!(walked.off_after.is_empty(), "{:?}", walked.off_after);
    let off = walk(&read_windowed_model(), false, run).off;
    assert!(off.is_empty(), "every notice read: {off:?}");
    // ... and the model's defects are that loop.
    let buggy = aterm_spec::interp::with_buggy(&windowed_model(), 1);
    let mut st = buggy.init_state();
    for a in ["PersonWallLeft", "UnreadLeftDropped"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
    let mut st = buggy.init_state();
    for a in ["OwnActTyped", "OlderLeftPlacedTomorrow"] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoDayWait", &st), "{st:?}");
}

/// THE LOOP OF 1D54A69E9 is not the model: it read the wall that answered a
/// retry inside the retry's window, but the window's end — the answer's
/// notice gone (Esc on the vendor's new countdown) — judged the retry taken
/// and dropped the track, and the continue policy typed into the standing
/// limit (round 16's review). Walked with that judgement, the real side
/// disagrees after exactly those windows and nowhere else: every state it
/// disagrees at is one a retry was made at, and among them the left wall a
/// tick past the retry's window; with the windows switched off, nowhere.
#[test]
fn the_loop_that_reads_a_retrys_answer_as_its_taking_forgets_the_wall() {
    let run = Run {
        answer_read_as_taken: true,
        ..Run::default()
    };
    let walked = walk(&read_windowed_model(), false, run);
    assert!(
        !walked.off.is_empty(),
        "the loop of 1d54a69e9 agreed everywhere"
    );
    assert!(
        walked.off.iter().all(|st| st["tries"] >= 1),
        "only where a retry was made: {:?}",
        walked.off
    );
    let ends = walked.off_after.get("Tick").cloned().unwrap_or_default();
    assert!(
        !ends.is_empty()
            && ends
                .iter()
                .all(|st| st["held"] == 1 && st["wall"] == 6 && st["tries"] >= 1),
        "the retry's window ending on its answer's left notice: {ends:?}\n{:?}",
        walked.off_after
    );
    let off = walk(&walled_model(), false, run).off;
    assert!(off.is_empty(), "{off:?}");
    // ... and the model's defect is that loop: the wall forgotten.
    let buggy = aterm_spec::interp::with_buggy(&windowed_model(), 1);
    let mut st = buggy.init_state();
    for a in [
        "WallAppearsLate",
        "Tick",
        "Tick",
        "Tick",
        "Act",
        "MetInWindow",
        "NoticeLeaves",
        "RetryReadAsTaken",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("WallRemembered", &st), "{st:?}");
}

/// THE LOOP BEFORE ROUND 16 is not the model: it folded a wall read inside the
/// own act's take window in only at the point that judged the act, so a limit
/// that answered the act at once was waited on by nothing — and once its
/// notice left inside the window, the window's end read the act's short turn
/// and the continue policy typed into the standing limit. Walked with that
/// reading, the real state disagrees after transitions out of the window's
/// states alone (`held = 1`), the wall's appearance there among them; with the
/// own act switched off, nowhere — the control is the window's alone.
#[test]
fn the_loop_that_folds_a_wall_only_at_a_point_waits_on_nothing_in_the_window() {
    let run = Run {
        blind_in_window: true,
        ..Run::default()
    };
    let walked = walk(&read_windowed_model(), false, run);
    assert!(!walked.off.is_empty(), "the blind loop agreed everywhere");
    assert!(
        walked
            .off_after
            .values()
            .flatten()
            .all(|st| st["held"] == 1),
        "{:?}",
        walked.off_after
    );
    assert!(
        walked
            .off_after
            .get("WallAppears")
            .is_some_and(|before| !before.is_empty()),
        "{:?}",
        walked.off_after
    );
    let off = walk(&walled_model(), false, run).off;
    assert!(off.is_empty(), "{off:?}");
    // ... and the model's defect is that loop: it types into the standing
    // limit before its tokens are back.
    let buggy = aterm_spec::interp::with_buggy(&windowed_model(), 1);
    let mut st = buggy.init_state();
    for a in [
        "OwnActTyped",
        "WallAppears",
        "NoticeLeaves",
        "TakeWindowDropsWall",
    ] {
        assert!(buggy.fire(a, &mut st), "{a} at {st:?}");
    }
    assert!(!buggy.check_invariant("NoBlindAct", &st), "{st:?}");
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
