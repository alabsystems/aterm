// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What is the worker doing RIGHT NOW, from one screen read? The busy signals
//! were measured on Claude Code 2.1.267: the spinner row (`✶ Deliberating…`,
//! with or without its `(4s · …)` suffix — the suffix is absent for the first
//! second), `Waiting for N dynamic workflow` / `N background agent`, a done row
//! or footer that says a background shell is still running (a turn can END
//! while a shell keeps the work going; that is still busy for a supervisor),
//! `esc to interrupt`, `Still working` and `ctrl+b to run in background`. The
//! footer does not always say `esc to interrupt` while a turn runs: two of the
//! four misclassifications a real session produced were a static `✢ Thinking…`
//! over `? for shortcuts` and an auto-mode footer that said only `⏵⏵ auto mode
//! on (shift+tab to cycle)` — though saved auto-mode screens show it too
//! (`⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt`). That is why the
//! status row is the primary signal, not the footer.
//!
//! The spinner row is read by its SHAPE, never by a list of glyphs. Measured
//! 2026-09-20, again on 2.1.267: `✶ Bloviating…` and `✳ Tempering… (15s ·
//! thinking with xhigh effort)` ran above the frame, and a row whose glyph the
//! reader could not name — or a row the release had INDENTED — read IDLE the
//! moment the footer dropped `esc to interrupt`. A list of glyphs is a list of
//! the releases we happened to look at, and reading a running turn as idle is
//! the one misreading a supervisor must never make: it types into a worker
//! mid-turn. So the shape decides — one non-ASCII glyph that is none of the
//! transcript's own markers, ONE space, a capitalised verb, an ellipsis, an
//! optional parenthesised timing — and the measured list survives only to keep
//! the loose rows recognised (`✻ Cooked for 4s · done 2:41 PM`, which has no
//! ellipsis; `*`, which is ASCII).
//!
//! A signal counts only in the LIVE ZONE. Claude Code draws its composer
//! between two full-width rules (the top one may carry a right-aligned label,
//! `──── sandboxed ─`); the rows under the bottom rule are the FOOTER. The
//! STATUS ROW is the lowest row above the top rule that starts with a spinner
//! glyph — a spinner, `Waiting for …`, or the done row a finished turn leaves
//! — in column 0, or at any indent when the row has the full spinner shape,
//! found before any row only the transcript has: the worker's `⏺` message or
//! tool call, or output under the `⎿` gutter. What
//! sits between the status row and the top rule never hides it, whatever it
//! is: a `⎿  Tip:` row, a todo list, a right-aligned hint of any length, the
//! rating survey (`● How is Claude doing this session?`, or the long-context,
//! plugin or memory card: one parked block, [`crate::survey::survey_block`]),
//! a banner, a queued message. A status row ABOVE a transcript row is history: measured, a worker
//! whose turn ended at 11:30 PM, with `/model` run after it and an empty
//! composer, still showed `✻ Waiting for 1 dynamic workflow to finish` fifteen
//! rows up, and a whole-screen scan read it busy. Without the composer frame
//! (not Claude Code, or a full-screen box) every row is scanned, as before.
//!
//! ONE EXCEPTION, THE WAIT CLAUDE CODE CARRIES: a `Waiting for N … to
//! finish` row with no status row under it stands for the turns Claude Code
//! started itself after it, until a later done row or a person's prompt
//! replaces it. Claude Code draws a turn's `turn_duration` row only when its
//! pending counts changed or a person's prompt came between (2.1.295 `Ad`,
//! 2.1.300 `Nd`, read from the binaries): a turn a `<task-notification>`, a
//! monitor's expiry or a `/loop` wakeup started, which ends with the same
//! work still pending, draws NO row, so its `⏺` rows sit between the
//! waiting row and the frame. Read as history, that end was an idle point,
//! and 47 of the 133 acts of the trust session's journal (2026-09-24 to
//! 10-15) typed `keep going` at one, every 30 minutes for days, each answered
//! "No change since my last update". So when the walk finds no status row,
//! it goes on up over the transcript (a parked card stepped over) and stops
//! at a person's `❯` row, at what ends the live zone or at the first glyph
//! row: a waiting row there is SOFT background work ([`busy_signal`]'s last
//! rule), which a question, a limit or another wall the notification turn
//! ended on still outranks; a done row or anything else is history, as
//! above. The measured `/model` screen keeps its reading: its `❯ /model` row
//! stops the walk. [`status_row`] stays `None` there. The waiting row is read
//! as its LINE (below), and it is the one busy reading a fresher word may
//! overrule ([`carried_wait_alone`]): a build that hid the done row of a
//! finished wait would leave it standing for good, so aterm-agent's loop
//! reads the turn ended where the session's transcript says it closed since
//! with nothing pending.
//!
//! THE STATUS ROW IS READ AS ITS LINE. In a pane narrower than it, Ink wraps
//! the row onto the rows under it at its text column (measured on 2.1.299 at
//! 26 and 34 columns: `✻ Worked for 0s · done` over `  10:36 AM`), and
//! Claude Code's flex row can put the rest of a cut verb in column 0 under
//! the glyph. Read a row at a time, a spinner cut to `✶ Choreog` over
//! `  raphing…` read IDLE (KI S2R-F1, measured at 12 columns) and a done row
//! cut before `· 1 shell still running` read no shell. So every reader of
//! the status row reads its line ([`status_block`]'s `line`, [`crate::wrap`]).
//!
//! A background MONITOR alone is soft ([`Busy::soft`]): a `persistent` one
//! runs for as long as the worker lives and wakes it by itself, so a worker
//! whose turn ended `· 1 monitor still running` (measured) is waiting on the
//! monitor's next event — and could be asking a question, or be at its usage
//! limit, all the same. Anything the turn ended on outranks it: a question,
//! a limit notice, or another wall (an API error, a login, a full context).
//!
//! The live zone also carries how much of the worker's context is left before
//! Claude Code auto-compacts it ([`context_left`]): `1% until auto-compact`,
//! right-aligned on the row above the top rule (measured 2026-09-13, three
//! hours into a turn, before the compaction that took the row away).

use std::borrow::Cow;
use std::fmt;

use crate::prompt::parse_prompt;
use crate::wall::{Placement, Wall, WallKind, classify_wall};

/// The worker's phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// A turn (or a background shell, workflow, agent or monitor) is running.
    Busy,
    /// An approval box is showing ([`parse_prompt`] is `Some`).
    Prompt,
    /// Not busy, no prompt, and the worker's last turn ended on a usage or
    /// rate limit notice ([`limit_notice`]). It sits at an idle composer, and
    /// an instruction sent to it fails until the limit resets or its model is
    /// switched — idle to the eye, a wall to a supervisor.
    Limited {
        /// The notice as Claude Code says it (`You've reached your Fable
        /// limit. …`), its continuation rows joined.
        message: String,
        /// When it resets, if the notice says (`7:30pm (America/Los_Angeles)`).
        reset: Option<String>,
    },
    /// Not busy, no prompt, the last thing said does not end in `?`.
    Idle,
    /// Not busy, no prompt, and the last thing the worker said
    /// ([`last_said_row`]) ends with `?` — it is waiting on an answer.
    Question,
}

impl Phase {
    /// The lowercase word the CLI prints.
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Busy => "busy",
            Phase::Prompt => "prompt",
            Phase::Limited { .. } => "limited",
            Phase::Idle => "idle",
            Phase::Question => "question",
        }
    }
}

/// The glyphs Claude Code's spinner cycles through, plus the static ones it
/// leaves on a row mid-thought — the ones we have MEASURED. It is not the
/// busy decision (see [`is_spinner_glyph`]): a release that adds a glyph must
/// not read as idle. It keeps the LOOSE status rows recognised, the ones the
/// shape rule cannot claim because they have no ellipsis (`✻ Cooked for 4s
/// · done 2:41 PM`) or no non-ASCII glyph (`*`).
const SPINNERS: &[char] = &['✢', '✽', '✻', '✶', '✳', '✧', '✦', '⚡', '·', '*'];

/// Glyphs that lead a row the spinner never leads: the transcript's own
/// markers (`⏺`/`●` the worker's message or tool call, `⎺` the gutter under
/// it), the composer's caret, the footer's mode arrows, a workflow's progress
/// ring. Without this list the shape rule below would read `⏺ Bloviating…`
/// — the worker SAYING what it is about to do — as a live spinner, and a
/// finished turn would never read idle again.
const NOT_SPINNER: &[char] = &['⏺', '●', '⎿', '❯', '⏵', '◯'];

/// One glyph that can lead Claude Code's spinner row: one we measured, or ANY
/// non-ASCII glyph that is not punctuation of the layout ([`NOT_SPINNER`]) and
/// not a letter or digit: a spinner we cannot name must not read IDLE (see
/// the module docs).
fn is_spinner_glyph(g: char) -> bool {
    SPINNERS.contains(&g)
        || (!g.is_ascii()
            && !NOT_SPINNER.contains(&g)
            && !is_box_drawing(g)
            && !g.is_alphanumeric())
}

/// A box-drawing character (U+2500–U+257F: `│`, `─`, `╌`, `╭` …) — the
/// layout's own strokes, never a spinner. Without this a box's `│`-led row
/// whose words run into an ellipsis read as a spinner row, and the box's top
/// was taken to be under it: Claude Code 2.1.281+'s rm breaker note `│
/// Dangerous rm operation on possibly-empty variable path: ${…}/tmp (use a
/// literal path: …)` (the vendor's own `${…}` placeholder, jF() in the
/// binary) parsed as a box titled `│ removes /tmp)`, kind `other`.
fn is_box_drawing(g: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&g)
}

/// The SHAPE of Claude Code's spinner row, whatever glyph it is cycling:
/// `^\s*<glyph> <Capitalised…>( \(<timing>\))?` — one spinner glyph, ONE
/// space (the `⎺  ` gutter's two are what that one space rules out), a
/// capitalised verb, and text that runs into an ellipsis before any
/// parenthesised timing. Indentation is allowed: the row is not always in
/// column 0. Tight enough that the transcript cannot wear it, and the only
/// thing [`SPINNERS`] is not asked about.
fn is_spinner_shape(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    let mut cs = t.chars();
    if !cs.next().is_some_and(is_spinner_glyph) {
        return false;
    }
    let Some(text) = cs.as_str().strip_prefix(' ') else {
        return false;
    };
    if !text.starts_with(char::is_uppercase) {
        return false;
    }
    let head = text.split(" (").next().unwrap_or(text);
    head.contains('…') || head.contains("...")
}

/// Classify one screen. A prompt wins over busy: a worker blocked on an
/// approval box cannot proceed however many background shells its footer
/// counts, and a supervisor that waited for those would wait forever. A
/// running turn (a spinner, `esc to interrupt`) wins over a limit notice (a
/// worker still running is not at the wall yet) — but not when the only thing
/// holding the screen busy is work the agent left running in the background
/// (a shell, a workflow): the wall the turn ended on stands then, behind it
/// (`background_wall`, below). A limit notice wins over a question, and a busy
/// that is only a background monitor ([`Busy::soft`]) loses to both and to any
/// other wall the turn ended on: it can outlive any budget.
pub fn worker_phase(rows: &[String]) -> Phase {
    if parse_prompt(rows).is_some() {
        return Phase::Prompt;
    }
    let busy = busy_signal(rows);
    if let Some(b) = busy.filter(|b| !b.soft) {
        // The turn ended on a wall and only work the agent
        // left running in the background holds the screen busy.
        return match background_wall(rows, b) {
            Some(w) if w.kind.reads_limited() => Phase::Limited {
                message: w.message,
                reset: w.reset,
            },
            Some(_) => Phase::Idle,
            None => Phase::Busy,
        };
    }
    if let Some((message, reset)) = limit_notice(rows) {
        return Phase::Limited { message, reset };
    }
    if last_said_row(rows).is_some_and(|r| crate::text::trim_end(r).ends_with('?')) {
        return Phase::Question;
    }
    // A background MONITOR alone is soft busy: it lost to the limit notice and
    // the question above, and loses to any other wall the turn ENDED on as a
    // shell's work does — an API error, a login, a full context stands, behind
    // it, as a wall the loop answers (the review of the merged main: it read
    // BUSY, and the retry ladder, the login escalation and the `/compact`
    // never ran).
    if let Some(b) = busy.filter(|b| b.soft)
        && let Some(w) = background_wall(rows, b)
    {
        return if w.kind.reads_limited() {
            Phase::Limited {
                message: w.message,
                reset: w.reset,
            }
        } else {
            Phase::Idle
        };
    }
    if busy.is_some() {
        Phase::Busy
    } else {
        Phase::Idle
    }
}

/// Where a busy signal was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The status row above the composer's top rule.
    Status,
    /// A row between the status row and the top rule (a right-aligned hint).
    Hint,
    /// A row under the composer's bottom rule.
    Footer,
    /// No composer frame on the screen: any row, as before the live zone.
    Screen,
}

/// One busy verdict: where it was read and which rule fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub zone: Zone,
    pub rule: &'static str,
    /// Only a background monitor is running: a question, a limit notice or
    /// any other wall the turn ended on outranks it in [`worker_phase`].
    pub soft: bool,
}

impl Busy {
    fn hard(zone: Zone, rule: &'static str) -> Self {
        Self {
            zone,
            rule,
            soft: false,
        }
    }
    fn soft(zone: Zone, rule: &'static str) -> Self {
        Self {
            zone,
            rule,
            soft: true,
        }
    }
}

impl fmt::Display for Busy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let zone = match self.zone {
            Zone::Status => "status row",
            Zone::Hint => "hint",
            Zone::Footer => "footer",
            Zone::Screen => "whole screen, no composer frame",
        };
        write!(f, "{zone}: {}", self.rule)
    }
}

/// The busy signal on the screen, read from the live zone when the composer
/// frame is there. The hard rules first: the status row (a spinner — a word or
/// a todo's words running into an ellipsis — `Waiting for N dynamic workflow`
/// / `N background agent` / `N …`, the turn ended while work it started runs
/// on — or a done row that still counts `N shell(s) still running`), a
/// `Still working` hint under it, then the footer (`esc to interrupt`, `Still
/// working`, `ctrl+b to run in background`, `· N shell(s) ·`, or a workflow's
/// `◯ <name> … agents done` progress line). Then the soft ones: a monitor still
/// running, on the status row (`· 1 monitor still running`) or in the footer
/// (`· N monitor(s) ·`), and then work of any other kind left running, on a
/// done row ([`left_running`]) or counted in the footer ([`counts_work`]).
/// Last, with no status row at all, the wait Claude Code carries over the
/// turns it started itself ([`carried_wait`]: module docs), soft. The status
/// row is read as its LINE, its wrapped rows joined ([`status_block`]).
/// With no frame, the first row [`busy_reason`] names, anywhere.
pub fn busy_signal(rows: &[String]) -> Option<Busy> {
    let Some(frame) = composer_frame(rows) else {
        return rows
            .iter()
            .find_map(|r| busy_reason(r))
            .map(|rule| Busy::hard(Zone::Screen, rule));
    };
    let block = status_block(rows, frame.top);
    let line = block.line(rows);
    let status = line.as_deref();
    let under = &rows[block.from..frame.top];
    let footer = &rows[frame.bottom + 1..];
    status
        .and_then(status_busy)
        .map(|rule| Busy::hard(Zone::Status, rule))
        .or_else(|| {
            under
                .iter()
                .any(|r| r.contains("Still working"))
                .then(|| Busy::hard(Zone::Hint, "Still working"))
        })
        .or_else(|| {
            footer
                .iter()
                .find_map(|r| footer_busy(r))
                .map(|rule| Busy::hard(Zone::Footer, rule))
        })
        .or_else(|| {
            status
                .filter(|r| still_running(r, "monitor"))
                .map(|_| Busy::soft(Zone::Status, "a monitor still running"))
        })
        .or_else(|| {
            footer
                .iter()
                .any(|r| footer_count(r, "monitor"))
                .then(|| Busy::soft(Zone::Footer, "a monitor running"))
        })
        .or_else(|| {
            status
                .filter(|r| is_done_row(r) && left_running(r))
                .map(|_| Busy::soft(Zone::Status, "background work still running"))
        })
        .or_else(|| {
            footer
                .iter()
                .any(|r| counts_work(r))
                .then(|| Busy::soft(Zone::Footer, "background work running"))
        })
        .or_else(|| (block.status.is_none() && carried_wait(rows, frame.top)).then_some(CARRIED))
}

/// [`busy_signal`]'s reading of the wait Claude Code carries ([`carried_wait`]).
const CARRIED: Busy = Busy {
    zone: Zone::Status,
    rule: "background work still running",
    soft: true,
};

/// THE WAIT CLAUDE CODE CARRIES (module docs): with no status row above the
/// frame whose top rule is row `top`, the first glyph row the walk up over
/// the transcript meets ([`glyph_above`]) is a `Waiting for N … to finish`
/// row, read as its LINE (module docs: a narrow pane wraps it, `✻ Waiting
/// for` over `  1 dynamic workflow…`). Claude Code drew no done row for the
/// turns it started itself since (their pending work was the same), so that
/// row still says what the session waits on.
fn carried_wait(rows: &[String], top: usize) -> bool {
    const WAITING: [&str; 3] = [
        "waiting for a dynamic workflow",
        "waiting for a background agent",
        "waiting for background work",
    ];
    let width = crate::question::screen_width(rows);
    glyph_above(rows, top)
        .map(|i| {
            let end = status_line_end(rows, i, top, width).unwrap_or(i);
            status_text(&rows[i..=end])
        })
        .and_then(|line| status_busy(&line))
        .is_some_and(|rule| WAITING.contains(&rule))
}

/// THE SCREEN READS BUSY BY NOTHING BUT THE WAIT CLAUDE CODE CARRIES: no
/// status row drawn, no hint or footer that says anything busy, and the
/// first glyph row over the transcript a waiting row ([`carried_wait`],
/// module docs). The one busy reading a fresher word may overrule: Claude
/// Code's renderer decides which done rows it draws, and a build that hid
/// the one a finished wait's last turn wrote would leave the waiting row
/// standing for good, where the session's own transcript says the turn
/// closed with nothing pending (aterm-agent's loop, BG-1). `false` without
/// the frame.
#[must_use]
pub fn carried_wait_alone(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| status_block(rows, f.top).status.is_none())
        && busy_signal(rows) == Some(CARRIED)
}

/// Walking up from the top rule `top` over every row (a parked card stepped
/// over, as [`status_block`] steps over it), the first glyph row
/// ([`is_glyph_row`]) met; `None` when a person's `❯` row in column 0 (their
/// prompt, which makes Claude Code draw the next turn's row again), a row
/// that ends the live zone ([`ends_live_zone`]) or the screen's top comes
/// first.
fn glyph_above(rows: &[String], top: usize) -> Option<usize> {
    let parked = crate::survey::parked_span(rows);
    for i in (0..top).rev() {
        if parked.is_some_and(|(first, last)| (first..=last).contains(&i)) {
            continue;
        }
        let row = &rows[i];
        if is_glyph_row(row) {
            return Some(i);
        }
        if row.starts_with('❯') || ends_live_zone(row) {
            return None;
        }
    }
    None
}

/// NO STATUS ROW ON THE SCREEN AT ALL: Claude Code's composer frame is drawn,
/// the walk up from its top rule finds no status row, and the walk on over
/// the transcript ([`glyph_above`]) meets no glyph row before it stops — no
/// done row, no waiting row: the one a turn ended on scrolled off, or
/// Claude Code draws none (`showTurnDuration: false`). Where the screen
/// says nothing of how the last turn ended, the session's own transcript
/// may (aterm-agent's loop, BG-1). `false` without the frame.
#[must_use]
pub fn status_unseen(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        status_block(rows, f.top).status.is_none() && glyph_above(rows, f.top).is_none()
    })
}

/// A done row that says work is `still running` UNDER ANY LABEL: Claude Code
/// writes one label for whatever background tasks the turn left running
/// (2.1.286: `1 shell`, `1 monitor`, `1 Artifact comment monitor`, `1 MCP
/// task`, `2 background tasks`, `◇ 1 cloud session`, …). The shell and the
/// monitor keep their own rules; any other kind — one aterm has no word for —
/// is the agent's background work too, soft as a monitor is: FAIL CLOSED, so
/// a quiet restart never ends work the screen says is running.
fn left_running(row: &str) -> bool {
    row.split(" · ").skip(1).any(|item| {
        crate::text::trim_end(item)
            .strip_suffix(" still running")
            .is_some_and(|label| !crate::text::trim(label).is_empty())
    })
}

/// A footer row whose pill COUNTS background work under any label
/// ([`left_running`]'s, the same task kinds): an item that is a count, then
/// words that end in a kind of work — `1 MCP task`, `1 Artifact comment
/// monitor`, `◇ 1 cloud session`, `1 shell, 1 monitor` — whatever qualifies
/// it. A count of anything else (`10 files changed`) is no work.
fn counts_work(row: &str) -> bool {
    const WORK: [&str; 7] = [
        "shell", "monitor", "task", "agent", "workflow", "session", "team",
    ];
    row.split("   ")
        .flat_map(|part| part.split(" · "))
        .flat_map(|item| item.split(", "))
        .any(|item| {
            let item = crate::text::trim(item);
            let item = item.strip_prefix("◇ ").unwrap_or(item);
            let digits = item.chars().take_while(char::is_ascii_digit).count();
            digits > 0
                && item[digits..]
                    .strip_prefix(' ')
                    .and_then(|label| label.rsplit(' ').next())
                    .is_some_and(|noun| WORK.contains(&noun.strip_suffix('s').unwrap_or(noun)))
        })
}

/// The rules of [`busy_signal`] that are the agent's OWN BACKGROUND WORK
/// with its turn over — a dynamic workflow, a background agent or other
/// background task it waits for, a shell or a monitor it left running, a
/// workflow's progress line — never a live turn's (a spinner, `esc to
/// interrupt`, `Still working`, a foreground command's `ctrl+b to run in
/// background`).
const BACKGROUND_RULES: &[&str] = &[
    "waiting for a dynamic workflow",
    "waiting for a background agent",
    "waiting for background work",
    "a shell still running",
    "a shell running",
    "a workflow running",
    "a monitor still running",
    "a monitor running",
    "background work still running",
    "background work running",
];

/// The wall the turn ENDED on, seen through the agent's own
/// background work: `busy` is one of [`BACKGROUND_RULES`], no live turn is
/// drawn ANYWHERE ([`no_live_turn`]) and none has just begun
/// ([`turn_just_begun`]).
///
/// A notice in the FOOTER or a BANNER
/// row is drawn while a turn is still WRAPPING UP as well (`⚠ Usage limit
/// reached · limit resets 9:50pm · clau.de/wrap-up` over `✻ Waiting for 1
/// background agent and 1 dynamic workflow to finish`, measured 41 minutes
/// before that turn ended): the agent is not at its wall yet, so such a notice
/// counts only once the status row is a DONE row ([`turn_over`]). The
/// vendor's `⎿` block and `⏺` error row are the turn's own last words, and
/// need no done row (a wall found sitting after `claude --resume` has none).
fn background_wall(rows: &[String], busy: Busy) -> Option<Wall> {
    if !BACKGROUND_RULES.contains(&busy.rule) || !no_live_turn(rows) || turn_just_begun(rows) {
        return None;
    }
    let wall = notice(rows, &classify_wall)?;
    if matches!(wall.placement, Placement::Footer | Placement::Banner) && !turn_over(rows) {
        return None;
    }
    Some(wall)
}

/// The status row above the composer is a DONE row (`✻ Churned for 4h 18m 23s
/// · done 9:22 PM · 1 shell still running`): the turn is over, whatever the
/// agent left running.
fn turn_over(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        status_block(rows, f.top)
            .line(rows)
            .is_some_and(|line| is_done_row(&line))
    })
}

/// A message was submitted under the status row (`❯ …` between the status
/// row and the composer): drawn a frame before the spinner of the turn it
/// began, so until that spinner comes the waiting row is still the status row
/// ([`background_wait`]'s guard; the wall's reading needs it too).
fn turn_just_begun(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        let block = status_block(rows, f.top);
        rows[block.from..f.top].iter().any(|r| r.starts_with('❯'))
    })
}

/// WHAT KEEPS THIS SCREEN BUSY IS THE AGENT'S OWN BACKGROUND
/// WORK — a shell, a monitor, a workflow, a background agent, hard or soft —
/// AND NOTHING ELSE: the composer frame is drawn, no live turn is drawn
/// anywhere ([`no_live_turn`]) and none has just begun ([`turn_just_begun`]).
/// Whatever the last thing said is: a screen scrolled back through the
/// transcript, a local command's output (`❯ /model` and its `⎿` result) under a
/// wall, hides the wall from [`worker_phase`], and this is what the loop that
/// waited on that wall asks to keep waiting (`/model` and `/usage` were the
/// owner's first act at the last two walls). It says nothing of walls: a
/// screen that shows one reads it.
#[must_use]
pub fn background_only(rows: &[String]) -> bool {
    composer_frame(rows).is_some()
        && busy_signal(rows).is_some_and(|b| BACKGROUND_RULES.contains(&b.rule))
        && no_live_turn(rows)
        && !turn_just_begun(rows)
}

/// HOW MANY BACKGROUND SHELLS Claude Code's screen counts: the `N shell(s)
/// still running` item of the status row over the composer (`✻ Cooked for 9m
/// 27s · done 11:07 AM · 1 shell still running`), or a footer row's `N
/// shell(s)` pill item (`⏵⏵ auto mode on · 2 shells · ← for agents`) — the
/// largest count either names, the items [`still_running`] and
/// [`footer_count`] read. `Some(0)` for a composer frame that counts none,
/// `None` without one (no Claude Code screen to read: a box covers it, or it
/// is another program's). A count of another kind of work (`1 monitor`) is
/// not a shell.
#[must_use]
pub fn shells_running(rows: &[String]) -> Option<u32> {
    let frame = composer_frame(rows)?;
    let status = status_block(rows, frame.top)
        .status
        .map(|i| rows[i].as_str());
    let footer = &rows[frame.bottom + 1..];
    let counted = status
        .into_iter()
        .chain(footer.iter().map(String::as_str))
        .flat_map(|row| row.split("   "))
        .flat_map(|part| part.split(" · "))
        .flat_map(|item| item.split(", "))
        .filter_map(shell_count)
        .max();
    Some(counted.unwrap_or(0))
}

/// One footer or status item's shell count: `N shell`, `N shells`, either
/// with ` still running`, led by at most a `◇ `.
fn shell_count(item: &str) -> Option<u32> {
    let item = crate::text::trim(item);
    let item = item.strip_prefix("◇ ").unwrap_or(item);
    let item = item.strip_suffix(" still running").unwrap_or(item);
    let (count, noun) = item.split_once(' ')?;
    if !matches!(noun, "shell" | "shells") || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    count.parse().ok()
}

/// No signal of a LIVE turn on the screen: not on the status row, not in a
/// hint row, not in ANY footer row (a spinner, `esc to interrupt`, `Still
/// working`, `ctrl+b to run in background`, the vendor's retry row). Without
/// a composer frame, no row of the screen.
fn no_live_turn(rows: &[String]) -> bool {
    let live = |rule: &str| !BACKGROUND_RULES.contains(&rule);
    let Some(frame) = composer_frame(rows) else {
        return !rows.iter().any(|r| busy_reason(r).is_some_and(live));
    };
    let block = status_block(rows, frame.top);
    let status = block
        .line(rows)
        .as_deref()
        .and_then(status_busy)
        .is_some_and(live);
    let hint = rows[block.from..frame.top]
        .iter()
        .any(|r| r.contains("Still working"));
    let footer = rows[frame.bottom + 1..]
        .iter()
        .any(|r| footer_busy(r).is_some_and(live));
    !(status || hint || footer)
}

/// WHAT KEEPS THIS SCREEN BUSY IS THE AGENT'S OWN BACKGROUND WORK, AND
/// NOTHING ELSE: the composer frame is drawn, and the busy signal it carries
/// ([`busy_signal`], read from the live zone) is one of [`BACKGROUND_RULES`]
/// — the turn ended (`✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still
/// running`, [`fixtures`]' `wait_bg4.out`) or the agent waits on work it
/// started (`✻ Waiting for 1 dynamic workflow to finish`, `wait_bg.out`).
/// `Some(the rule)`, `None` for a live turn (`wait_bg2.out`: a spinner over
/// the same workflow's progress line), an idle screen, or one with no frame.
/// A natural break for a host's word that interrupts the orchestration (the
/// live upgrade's notice, and its re-ask after half an hour); never one where
/// anything may be ended.
///
/// Nor one where a turn has just BEGUN: a message submitted under the status
/// row is drawn there (`❯ …`) a frame before the spinner of the turn it
/// began, and until that spinner comes the waiting row is still the status
/// row. Read as a break, the notice's last look before typing passed it and
/// the notice landed in the new turn (measured from `wait_bg2.out` with its
/// spinner not yet drawn).
#[must_use]
pub fn background_wait(rows: &[String]) -> Option<&'static str> {
    composer_frame(rows)?;
    if turn_just_begun(rows) {
        return None;
    }
    let busy = busy_signal(rows)?;
    BACKGROUND_RULES.contains(&busy.rule).then_some(busy.rule)
}

/// The [`BACKGROUND_RULES`] of a SHELL or a MONITOR the turn left running.
const LEFT_RUNNING_RULES: &[&str] = &[
    "a shell still running",
    "a shell running",
    "a monitor still running",
    "a monitor running",
];

/// THE TURN IS OVER, AND WHAT RUNS ON IS A SHELL OR A MONITOR IT LEFT: the
/// status row over the composer is a DONE row ([`turn_over`]: `✻ Worked for
/// 9m 2s · done 3:15 PM · 1 shell still running`) and what keeps the screen
/// busy ([`background_wait`]) is a shell or a monitor. Never a turn still
/// waiting on work it started (`✻ Waiting for 1 background agent to
/// finish`, `✻ Waiting for 1 dynamic workflow to finish`): that turn goes on
/// by itself when the work ends, and its last words are not its end. The
/// worker's own end of its task read here stands over what it left running
/// (the supervisor's done over background work).
#[must_use]
pub fn turn_over_background(rows: &[String]) -> bool {
    turn_over(rows) && background_wait(rows).is_some_and(|rule| LEFT_RUNNING_RULES.contains(&rule))
}

/// WHO OPENED THE TURN DRAWN LAST, read up from the composer's top rule: the
/// newest row that opens a turn is a person's message (`❯` in column 0, then
/// whitespace; a message queued under the status row included) —
/// `Some(false)` — or one of Claude Code's OWN WAKES, after which it goes on
/// by itself with no message of anyone's (`wait_bg6.out`, `wait_bg12.out`):
/// a background command's end (`⏺ Background command "…" completed (exit
/// code 0)`, `… failed with exit code 144`) or a monitor's event (`⏺ Monitor
/// event: …`) — `Some(true)`. `None`: neither is on the screen, or there is
/// no frame to read up from.
#[must_use]
pub fn opened_by_own_wake(rows: &[String]) -> Option<bool> {
    let top = composer_frame(rows)?.top;
    rows[..top].iter().rev().find_map(|row| {
        if row
            .strip_prefix('❯')
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        {
            return Some(false);
        }
        let text = row
            .strip_prefix('⏺')
            .or_else(|| row.strip_prefix('●'))?
            .trim_start();
        (text.starts_with("Background command \"") || text.starts_with("Monitor event:"))
            .then_some(true)
    })
}

/// Which busy signal `row` carries, if any (named for the CLI's diagnostics).
/// This is the whole-screen rule, used when the screen has no composer frame.
pub fn busy_reason(row: &str) -> Option<&'static str> {
    if row.contains("esc to interrupt") {
        Some("esc to interrupt")
    } else if is_spinner_row(row) {
        Some("spinner row")
    } else if row.contains("Still working") {
        Some("Still working")
    } else if row.contains("ctrl+b to run in background") {
        Some("ctrl+b to run in background")
    } else if number_then(row, "Waiting for ", " dynamic workflow") {
        Some("waiting for a dynamic workflow")
    } else if still_running(row, "shell")
        || number_then(row, "· ", " shell ·")
        || number_then(row, "· ", " shells ·")
    {
        Some("a shell still running")
    } else {
        None
    }
}

/// The status row's hard rule. A done row (`✻ Cooked for 4s · done 2:41 PM`)
/// is a status row that says nothing busy.
fn status_busy(row: &str) -> Option<&'static str> {
    if is_activity_row(row) {
        Some("spinner")
    } else if number_then(row, "Waiting for ", " dynamic workflow") {
        Some("waiting for a dynamic workflow")
    } else if number_then(row, "Waiting for ", " background agent") {
        Some("waiting for a background agent")
    } else if row
        .split_once("Waiting for ")
        .is_some_and(|(_, n)| n.starts_with(|c: char| c.is_ascii_digit()))
    {
        Some("waiting for background work")
    } else if still_running(row, "shell") {
        Some("a shell still running")
    } else {
        None
    }
}

/// A footer row's hard rule.
fn footer_busy(row: &str) -> Option<&'static str> {
    if row.contains("esc to interrupt") {
        Some("esc to interrupt")
    } else if row.contains("Still working") {
        Some("Still working")
    } else if row.contains("ctrl+b to run in background") {
        Some("ctrl+b to run in background")
    } else if footer_count(row, "shell") {
        Some("a shell running")
    } else if is_workflow_progress(row) {
        Some("a workflow running")
    } else {
        None
    }
}

/// `N <noun> still running` / `N <noun>s still running` somewhere in `row`.
fn still_running(row: &str, noun: &str) -> bool {
    number_then(row, "", &format!(" {noun} still running"))
        || number_then(row, "", &format!(" {noun}s still running"))
}

/// `N <noun>` or `N <noun>s` as one footer item: led by `· ` (or the start of
/// the row), ended by ` ·`, a wide gap, the row's end or ` still running`
/// (`⏵⏵ auto mode on · 1 shell · ← for agents`).
fn footer_count(row: &str, noun: &str) -> bool {
    let t = crate::text::trim_end(row);
    t.match_indices(noun).any(|(at, _)| {
        let Some(before) = t[..at].strip_suffix(' ') else {
            return false;
        };
        let digits = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .count();
        let lead = &before[..before.len() - digits];
        let after = &t[at + noun.len()..];
        let after = after.strip_prefix('s').unwrap_or(after);
        digits > 0
            && (lead.ends_with("· ") || crate::text::trim(lead).is_empty())
            && (after.is_empty()
                || after.starts_with(" ·")
                || after.starts_with("  ")
                || after.starts_with(" still running"))
    })
}

/// A workflow's progress line under the footer (`◯ <name>  <description…>
/// 1/4 agents done · 20m 59s · ↓ 428.2k tokens`).
fn is_workflow_progress(row: &str) -> bool {
    crate::text::trim_start(row).starts_with('◯')
        && (row.contains("agents done") || row.contains("agent done"))
}

/// `^\s*<glyph>\s+\S+…` — a spinner glyph ([`is_spinner_glyph`], never the
/// list alone, so an unnamed glyph is still a spinner), whitespace, then a
/// word that runs into an ellipsis (`…` or `...`). The one-word rule stays:
/// this is the WHOLE-SCREEN scan, where several words ending in an ellipsis
/// are anyone's prose; the status row reads those through
/// [`is_activity_row`].
pub fn is_spinner_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    let mut cs = t.chars();
    let Some(g) = cs.next() else {
        return false;
    };
    if !is_spinner_glyph(g) {
        return false;
    }
    let rest = cs.as_str();
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let word = rest.split_whitespace().next().unwrap_or("");
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c != '…' && c != '.')
        && (chars.as_str().contains('…') || chars.as_str().contains("..."))
}

/// A status row naming an activity in progress: the glyph, then text that
/// runs into an ellipsis before any `(` suffix — one spinner word
/// (`✶ Deliberating…`) or the several words of the todo in progress
/// (`✻ Running the test suite… (45s · ↑ 2.1k tokens)`), which
/// [`is_spinner_row`]'s one-word rule does not see. Read on the status row
/// only; the whole-screen scan keeps the one-word rule.
fn is_activity_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    let mut cs = t.chars();
    if !cs.next().is_some_and(is_spinner_glyph) {
        return false;
    }
    let rest = cs.as_str();
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let text = crate::text::trim_start(rest);
    let head = text.split(" (").next().unwrap_or(text);
    (head.chars().next().is_some_and(|c| c != '…' && c != '.')
        && (head.contains('…') || head.contains("...")))
        || is_retry_status(text)
}

/// The text of the vendor's API-retry status row, which no ellipsis ends:
/// the row that stands in for the spinner while Claude Code retries its own
/// request, so the worker is busy. Read as the TAIL of its line, in the
/// vendor's own shapes whole (2.1.300's `uyn`, read from the binary; the
/// first measured on 2.1.283, 2026-09-27), so the words in front may be cut
/// (`…`) at a narrow width:
/// * `<error> · Retrying in <wait>[ (<reset>)] · attempt <k>/<n>` — `Can't
///   reach the API server — check your internet or DNS (ENOTFOUND) ·
///   Retrying in 1s · attempt 5/10`, `Usage limit reached · Retrying in 14s
///   (9:50pm) · attempt 3/10` (the reset when the error carries limits);
/// * `<banner> · will retry in <wait> · check your network` (a stalled
///   stream);
/// * `<banner> · next try in <wait> · attempt <k>[/<n>] · esc to interrupt`
///   (the auto-mode check unavailable, a low-priority wait);
/// * `<banner> · retrying, waiting up to <wait> · attempt <k>/<n>`, or a last
///   item `retrying once, waiting up to <wait>` (no response from the API);
/// * and any status line whose LAST ` · ` item is `esc to interrupt`.
///
/// `<wait>` is words of a number and its unit (`14s`, `1m 30s`, `2d`). The
/// retry clause stands in any item past the first, and every item after it
/// is the vendor's own (`attempt <k>[/<n>]`, `check your network`, `esc to
/// interrupt`), the last one maybe cut by the pane (`· attempt 1/3…`, `·
/// att…`): a pane that cuts the row's tail leaves it the row. A done row
/// (`Cogitated for 0s · done 11:02 PM`), the worker's words, a clause that is
/// not the vendor's (`Retrying in soon`, `attempt soon`, `attempt one of
/// 300`) are not one.
///
/// CLAUDE CODE'S OWN RETRY OF A USAGE LIMIT is one too: under a
/// `CLAUDE_CODE_RETRY_WATCHDOG` a person set by hand (aterm never sets it)
/// the vendor draws no limit notice, keeps the request and ticks `✻ Session
/// limit reached · Retrying in 2m 28s (8:48am) · attempt 1/300` until the
/// reset, then sends it again by itself (measured on 2.1.300 and 2.1.301 in
/// the lab: the five-hour and the weekly limit, `Retrying in 5h (Oct 16 at
/// 9:49am)`, at 110, 80 and 50 columns, a turn's first request and its
/// second). Its wait names the reset after the span, in parentheses. That is
/// a turn in progress, as any retry of the vendor's is: nothing is typed
/// into it, the served retry goes on as the turn, and Esc on it ends the
/// turn as Esc ends any. With a statusLine configured the footer draws no
/// `esc to interrupt` meanwhile, so this row is the screen's only word that
/// the turn is not over; read as a done row, the point was idle and the
/// supervisor typed into a session that was retrying.
///
/// WHERE INK PUT THE WORDS AROUND THE CLAUSE is the line reader's to undo,
/// never this rule's: under about 44 columns the vendor's glyph box wraps
/// away from its text, and where the cut head and the tail do not fit beside
/// each other they are laid out as two text columns side by side, their rows
/// interleaved (MEASURED on 2.1.301 at 18 to 38 columns, `API     ·
/// Retrying in 1s · attempt` over `error  1/10`; on 2.1.300 at 50, the
/// watchdog's weekly retry), which [`StatusBlock::line`] reads column by
/// column ([`bare_line`]) back into the shapes above.
fn is_retry_status(text: &str) -> bool {
    let items: Vec<&str> = text.split(" · ").map(crate::text::trim_end).collect();
    let [_, .., last] = items.as_slice() else {
        return false;
    };
    if *last == "esc to interrupt" {
        return true;
    }
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    // `14s`, `2m`, `1m 30s`, `2d`: each word a number and its unit.
    let wait = |s: &str| {
        !s.is_empty()
            && s.split(' ')
                .all(|w| w.strip_suffix(['s', 'm', 'h', 'd']).is_some_and(number))
    };
    // `14s (9:50pm)`, `2m 28s (8:48am)`, `5h (Oct 16 at 9:49am)`: the wait,
    // then the reset the limit names.
    let wait_reset = |s: &str| {
        wait(s)
            || s.split_once(" (").is_some_and(|(w, reset)| {
                wait(w)
                    && reset.strip_suffix(')').is_some_and(|r| {
                        !crate::text::trim(r).is_empty() && !r.contains(['(', ')'])
                    })
            })
    };
    const CLAUSES: [&str; 5] = [
        "Retrying in ",
        "will retry in ",
        "next try in ",
        "retrying, waiting up to ",
        "retrying once, waiting up to ",
    ];
    let Some(at) = items.iter().skip(1).position(|item| {
        CLAUSES
            .iter()
            .any(|c| item.strip_prefix(c).is_some_and(wait_reset))
    }) else {
        return false;
    };
    // `attempt 3/10`, `attempt 2`: the vendor's count of its tries.
    let count = |a: &str| match a.split_once('/') {
        Some((k, n)) => number(k) && number(n),
        None => number(a),
    };
    const WORDS: [&str; 2] = ["check your network", "esc to interrupt"];
    let tail =
        |item: &str| item.strip_prefix("attempt ").is_some_and(count) || WORDS.contains(&item);
    // The last item cut by the pane: the start of one of the vendor's
    // (`attempt 1/3…`, `att…`).
    let cut = |item: &str| {
        item.strip_suffix('…')
            .map(crate::text::trim_end)
            .is_some_and(|head| {
                !head.is_empty()
                    && (WORDS.iter().any(|w| w.starts_with(head))
                        || "attempt ".starts_with(head)
                        || head.strip_prefix("attempt ").is_some_and(|a| {
                            let (k, n) = a.split_once('/').unwrap_or((a, ""));
                            number(k) && n.chars().all(|c| c.is_ascii_digit())
                        }))
            })
    };
    match items[at + 2..].split_last() {
        None => true,
        Some((&end, between)) => between.iter().all(|&i| tail(i)) && (tail(end) || cut(end)),
    }
}

/// `<prefix><digits><suffix>` somewhere in `row`.
fn number_then(row: &str, prefix: &str, suffix: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = crate::text::find(&row[from..], suffix) {
        let end = from + rel;
        let before = &row[..end];
        let digits = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .count();
        if digits > 0 && before[..before.len() - digits].ends_with(prefix) {
            return true;
        }
        from = end + suffix.len();
    }
    false
}

/// Claude Code's composer frame: the top rule (directly above the composer's
/// caret row) and the bottom rule under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) top: usize,
    pub(crate) bottom: usize,
}

pub(crate) fn composer_frame(rows: &[String]) -> Option<Frame> {
    frame_at(rows, composer_index(rows)?)
}

/// The frame around caret row `caret`: the rule right above it and the first
/// rule under it.
fn frame_at(rows: &[String], caret: usize) -> Option<Frame> {
    let top = caret.checked_sub(1).filter(|&t| is_rule(&rows[t]))?;
    let bottom = (caret + 1..rows.len()).find(|&i| is_rule(&rows[i]))?;
    Some(Frame { top, bottom })
}

/// The composer's frame on a screen ([`composer_rules`]) and how much of the
/// screen the search for it READ — for a caller that keeps a reading of the
/// frame and the rows under its bottom rule from one rebuild of a pane's rows
/// to the next, and knows which rows changed between them (aterm's footer
/// and lights: `footer::mode_row_under`, `footer::pill_row_under`).
///
/// What the search reads, from [`composer_index`] and [`frame_at`]: every
/// row from the screen's last up to the caret row it stops at, row by row;
/// the row above a `❯` row it tests (its rule, for an option-like caret);
/// and, for a `❯` row not in column 0, the rows [`is_list_entry`] walks —
/// up to the rule over the list, and on up the screen to the rule over
/// that. So with a caret row found, the frame is a function of the rows from
/// the one above the caret down, and of nothing over them — unless a row
/// there opens on a `❯` past column 0 (the slash menu's highlighted entry, a
/// draft that holds one), whose walk may go anywhere: then, as with no caret
/// row at all, of every row. [`Self::from`] is that first row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameReach {
    frame: Option<Frame>,
    /// The first row the search read: no row above it can move the frame.
    from: usize,
}

impl FrameReach {
    /// The frame on `rows` and what its search read. Never panics, on any
    /// rows.
    #[must_use]
    pub fn of(rows: &[String]) -> Self {
        let Some(caret) = composer_index(rows) else {
            return Self {
                frame: None,
                from: 0,
            };
        };
        let walked = rows[caret..]
            .iter()
            .any(|r| !r.starts_with('❯') && crate::text::trim_start(r).starts_with('❯'));
        Self {
            frame: frame_at(rows, caret),
            from: if walked { 0 } else { caret.saturating_sub(1) },
        }
    }

    /// [`Self::of`], falling back to agy's or Muse Code's composer rules
    /// ([`crate::agy::composer_rules`], [`crate::muse::composer_rules`]) when
    /// Claude Code's composer is not on `rows`.
    #[must_use]
    pub fn of_or_agy(rows: &[String]) -> Self {
        let reach = Self::of(rows);
        if reach.frame.is_some() {
            return reach;
        }
        if let Some((top, bottom)) = crate::agy::composer_rules(rows) {
            return Self {
                frame: Some(Frame { top, bottom }),
                from: top,
            };
        }
        if let Some((top, bottom)) = crate::muse::composer_rules(rows) {
            // Muse's open-frame test also reads the previous nonblank row
            // above its top rule for a drawer's key hints.
            return Self {
                frame: Some(Frame { top, bottom }),
                from: 0,
            };
        }
        reach
    }

    /// The frame's rules, `(top, bottom)` — [`composer_rules`] of the rows
    /// it was found on.
    #[must_use]
    pub fn rules(&self) -> Option<(usize, usize)> {
        self.frame.map(|f| (f.top, f.bottom))
    }

    /// The first row the search read ([`FrameReach`]).
    #[must_use]
    pub fn from(&self) -> usize {
        self.from
    }

    /// Whether the search reads the rows as it read them before the rows
    /// `changed` changed (the count of rows the same): none of them is a row
    /// it read, so the frame and this reach stand without a search.
    #[must_use]
    pub fn unmoved(&self, changed: &[usize]) -> bool {
        changed.iter().all(|&j| j < self.from)
    }

    /// Whether a reading of the frame and of the rows UNDER its bottom rule
    /// alone — the footer's rule and mode row, the lights' pill — reads the
    /// rows as it read them before the rows `changed` changed, `now` being
    /// the frame found on them since ([`Self::of`], or this reach where it is
    /// [`Self::unmoved`]): the same frame, and no changed row under its bottom
    /// rule. With no frame then and now, every such reading is the same
    /// none. A changed row in the composer itself (a keystroke's echo) or
    /// over it moves no such reading while the frame stands.
    #[must_use]
    pub fn holds_under(&self, now: &FrameReach, changed: &[usize]) -> bool {
        now.frame == self.frame
            && self
                .frame
                .is_none_or(|f| changed.iter().all(|&j| j <= f.bottom))
    }
}

/// Whether the screen shows Claude Code's composer between its two rules.
/// Without it the worker is something else (a build, a script, a REPL) or a
/// box covers the screen, and the whole-screen rules apply.
pub fn has_composer_frame(rows: &[String]) -> bool {
    composer_frame(rows).is_some()
}

/// Claude Code's prompt box in SHELL MODE: `!` typed into the empty box
/// turns its caret from `❯` into `!`, between the same two rules (measured on
/// 2.1.283, 2026-09-26, every printable key typed alone into the empty box:
/// `!` is the one that changes the caret — `!\u{a0}Try "…"` under the
/// placeholder, `!\u{a0}ls` with a command typed — the `SHELL_MODE*`
/// fixtures). It is the REPL, drawn and taking keys, so its `idle` is
/// evidence ([`crate::ClaudeReader`]'s `phase_authoritative`); it is NOT
/// [`composer_frame`], whose `❯` caret row is where the draft, the footer,
/// the lights and the turn-end policy read a prompt.
pub(crate) fn has_shell_mode_frame(rows: &[String]) -> bool {
    (1..rows.len()).rev().any(|i| {
        let caret = rows[i]
            .strip_prefix('!')
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\u{a0}']));
        caret && is_rule(&rows[i - 1]) && rows[i + 1..].iter().any(|r| is_rule(r))
    })
}

/// Whether row `row` of `rows` is INSIDE one of Claude Code's prompt boxes,
/// from its top rule down to its bottom rule: the `❯` composer, or shell
/// mode's `!` box ([`has_shell_mode_frame`]), its caret in column 0 right
/// under the top rule. The box the row is in is the nearest one that starts
/// at or above it; a box with no bottom rule yet holds nothing.
///
/// This is where Claude Code keeps the TERMINAL'S CURSOR while its REPL takes
/// keys, so the box that holds the cursor is the REPL that is running.
/// Measured on 2.1.283 (2026-09-27, `subscribe … screen,cursor` of a private
/// headless aterm at 150x50, the fullscreen and the inline renderer, a new
/// tab and the same tab): from the first frame that draws the box whole the
/// cursor sits on its caret row — at column 2 at rest, after a draft's last
/// character (on the draft's second row when it wraps), in shell mode, with
/// the `/` and `@` menus and the shortcuts open, after one Ctrl-C. It is
/// never in a box an EARLIER run left on the screen: the inline renderer
/// relaunched in the same tab draws under the new launch line, and until its
/// REPL is up the cursor sits under that line, on the rows its folder-trust
/// dialog took, or on the row the new bottom rule is drawn on — below the
/// old box (all 19 frames of three relaunches).
#[must_use]
pub fn prompt_box_holds(rows: &[String], row: usize) -> bool {
    let caret_row = |r: &str| {
        r.starts_with('❯')
            || r.strip_prefix('!')
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\u{a0}']))
    };
    let last = row.saturating_add(1).min(rows.len().saturating_sub(1));
    (1..=last)
        .rev()
        .find(|&i| caret_row(&rows[i]) && is_rule(&rows[i - 1]))
        .and_then(|caret| (caret + 1..rows.len()).find(|&i| is_rule(&rows[i])))
        .is_some_and(|bottom| row <= bottom)
}

/// The index of the composer's BOTTOM rule, when the frame is on the screen —
/// for aterm's footer (`aterm_agent::harness::footer`), which writes its facts
/// INTO that rule (on the glass only) and reads the mode row under it, and
/// must not look for either anywhere else.
pub fn composer_bottom(rows: &[String]) -> Option<usize> {
    composer_frame(rows).map(|f| f.bottom)
}

/// Both composer rules, `(top, bottom)`, when the frame is on the screen —
/// for aterm's lights (`aterm_agent::harness::lights`), which read the tags
/// Claude Code draws INTO the top rule (`──── <tag> ↯ ─`).
pub fn composer_rules(rows: &[String]) -> Option<(usize, usize)> {
    composer_frame(rows).map(|f| (f.top, f.bottom))
}

/// The index of the composer's top rule, when the frame is on the screen —
/// for the prompt parser, which must know where the live zone begins.
pub(crate) fn composer_top(rows: &[String]) -> Option<usize> {
    composer_frame(rows).map(|f| f.top)
}

/// A full-width composer rule: `─` from column 0 to the last column, with at
/// most a label inside it (`──── sandboxed ─`) and none of a table's joints.
pub fn is_rule(row: &str) -> bool {
    // A rule opens on `─` (and the trim below cuts only its end): any other
    // first char rejects the row before the char-wise trims and counts.
    if !row.as_bytes().starts_with("─".as_bytes()) {
        return false;
    }
    let t = crate::text::trim_end(row);
    t.ends_with('─') && rule_run(t)
}

/// Whether `t` holds ten `─` or more and none of the box's joins and sides
/// (`┬ ┴ ┼ ├ ┤ ┌ ┐ └ ┘ │`). Every one of them is U+25xx, encoded
/// `E2 94 xx`, and `E2` only ever opens a char, so one pass over the bytes
/// counts and refuses them exactly as a char-wise count and search would.
fn rule_run(t: &str) -> bool {
    let b = t.as_bytes();
    let mut dashes = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0xE2 && i + 2 < b.len() && b[i + 1] == 0x94 {
            match b[i + 2] {
                0x80 => dashes += 1,
                // ┬ ┴ ┼ ├ ┤ ┌ ┐ └ ┘ │
                0xAC | 0xB4 | 0xBC | 0x9C | 0xA4 | 0x8C | 0x90 | 0x94 | 0x98 | 0x82 => {
                    return false;
                }
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    dashes >= 10
}

/// The width in chars of a PLAIN rule — ten `─` or more and nothing else,
/// trailing blanks aside — or `None`: a rule carrying a label (`── Voice
/// input … ──`, which [`is_rule`] takes) is not one.
pub(crate) fn plain_rule_width(row: &str) -> Option<usize> {
    let t = crate::text::trim_end(row).as_bytes();
    let dash = "─".as_bytes();
    (t.len() >= 10 * dash.len() && t.chunks(dash.len()).all(|c| c == dash))
        .then_some(t.len() / dash.len())
}

/// The live zone above the top rule, found by walking up from it: the status
/// row, if one comes before the transcript, the last of the rows its line was
/// wrapped onto ([`status_line_end`]), and where the rows under it (or under
/// the transcript row the walk stopped at) begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusBlock {
    pub(crate) status: Option<usize>,
    /// The status line's last row: [`Self::status`] itself when its line fits
    /// its row (meaningless without a status row).
    pub(crate) end: usize,
    pub(crate) from: usize,
}

impl StatusBlock {
    /// The status row's text, its line read across the rows Ink wrapped it
    /// onto, joined with one space ([`crate::wrap::rejoined`]): `✻ Choreogr
    /// aphing…` (a word wider than the row broken where the row ends, read in
    /// two halves), `✻ Baked for 0s · done 7:06 AM` — a glyph alone on its
    /// row read over the text columns Ink laid out under it ([`status_text`]).
    pub(crate) fn line<'a>(&self, rows: &'a [String]) -> Option<Cow<'a, str>> {
        let status = self.status?;
        Some(if self.end > status {
            Cow::Owned(status_text(&rows[status..=self.end]))
        } else {
            Cow::Borrowed(rows[status].as_str())
        })
    }
}

pub(crate) fn status_block(rows: &[String], top: usize) -> StatusBlock {
    // A rating card (or the drafts card) parked above the composer is ONE block the walk
    // steps over, whatever its rows say: read as a transcript row, an unread card would end
    // the walk and hide the status row under it.
    let parked = crate::survey::parked_span(rows);
    let width = crate::question::screen_width(rows);
    for i in (0..top).rev() {
        if parked.is_some_and(|(first, last)| (first..=last).contains(&i)) {
            continue;
        }
        if let Some(end) = status_line_end(rows, i, top, width) {
            return StatusBlock {
                status: Some(i),
                end,
                from: end + 1,
            };
        }
        if is_transcript_row(&rows[i]) || ends_live_zone(&rows[i]) {
            return StatusBlock {
                status: None,
                end: i,
                from: i + 1,
            };
        }
    }
    StatusBlock {
        status: None,
        end: 0,
        from: 0,
    }
}

/// THE STATUS ROW'S LINE, wrapped in a narrow pane (ATERM-KI-1015-1 S2R-F1;
/// MEASURED on 2.1.300 at 10 columns, the fixtures
/// `claude-2.1.300-spinner-*-10col.txt`): when row `i` opens the status line
/// — a glyph row ([`is_glyph_row`]), or its glyph alone on its row, the line
/// under it — its last row, short of row `until`, in a pane `width` cells
/// wide. Ink wraps the spinner's line at the row's own column (`✻ Choreogr`
/// over `aphing…`; `·` over `Precipitat` over `ing…`, the glyph alone where
/// the word did not fit beside it; `✻` over `Calculatin` over `g` over `…`,
/// broken between the spans of its shimmer too: [`crate::wrap::run_end`])
/// and a done row's text at its text column, two in (`✻ Baked for 0s · done
/// 7:06` over `  AM`, by the rule of a row too full for the next word,
/// [`crate::wrap::continues`]; never under a row that already reads busy
/// alone, but for the rest of a parenthesis it left open, `✶ Deliberating…
/// (4s · esc to` over `  interrupt)` at 30 columns); never into a row that
/// opens a block of its own (a `⎿` tip or
/// todo, a queued `❯` message, a transcript row, another glyph row, the
/// survey). Read a row at a time the spinner held
/// no `…` and the footer's `esc to interrupt` is cut off at that width: a
/// running turn read idle, and `keep going` was typed into it.
///
/// A join never turns a DONE row ([`is_done_shape`]) into an activity: a
/// word with an ellipsis under a full done row is not its verb (`Thinking
/// about it…` in column 0, `  more to say…` at column 2). Nor does a join
/// ever LOSE a busy reading the row has on its own ([`status_busy`]; the
/// cross-lane review's screen F2: a row read by its tail, `· esc to
/// interrupt` or the vendor's retry clause, took the proxy hint Claude Code
/// draws under it at column 2 as its last item, and the turn still retrying
/// read idle). A row in the glyph's own column joins only a row that has no
/// status shape of its own ([`has_status_shape`]) to a line that has one, so
/// a stray row there cannot turn a done row busy either.
///
/// A GLYPH INK WRAPPED AWAY FROM ITS TEXT (the cross-lane review's screen F1,
/// MEASURED on 2.1.301: the vendor's glyph box is 2 cells wide with
/// `flexWrap`, and where the text does not fit beside it the text moves
/// under it — the retry row at 34, 38 and 42 columns, `✻` over `API error ·
/// Retrying in 1s · attempt 2/10`; the spinner at 10, `✻` over `Infusing…`)
/// opens the line where a row at its column under it does, and the line is
/// every row under it to the first blank, a row against the right edge among
/// them (the block's last row fills an 18-column pane, `      attempt 1/10`:
/// the decision review's RR-LC-1), read column by column where Ink laid it
/// out as two ([`bare_line`]). Read past, the walk took the previous turn's
/// done row for the live one and a retrying turn read idle.
fn status_line_end(rows: &[String], i: usize, until: usize, width: usize) -> Option<usize> {
    let row = rows[i].as_str();
    let t = crate::text::trim(row);
    let mut cs = t.chars();
    if !cs.next().is_some_and(is_spinner_glyph) {
        return None;
    }
    let until = (i + 1..until)
        .find(|&j| opens_block(&rows[j]))
        .unwrap_or(until);
    let lead = leading_spaces(row);
    let text = crate::text::trim(cs.as_str());
    // The spinner's line at its own column, broken between the spans of its
    // shimmer as well as where a row is full ([`crate::wrap::run_end`]).
    let run = crate::wrap::run_end(rows, i, lead, until);
    if text.is_empty() && !is_glyph_row(row) {
        // The glyph alone: its text's rows, to the first blank.
        let opens = run > i && is_glyph_row(&crate::wrap::rejoined(&rows[i..=run]));
        let end = (i + 1..until)
            .take_while(|&k| !crate::text::trim(&rows[k]).is_empty())
            .last()
            .unwrap_or(i);
        return opens.then_some(end);
    }
    if !is_glyph_row(row) {
        return None;
    }
    let keeps = |end: usize| {
        let joined = crate::wrap::rejoined(&rows[i..=end]);
        !(is_done_shape(row) && is_activity_row(&joined))
            && !(status_busy(row).is_some() && status_busy(&joined).is_none())
    };
    // A done row's text at its text column, where a row is full. A row that
    // already reads busy alone (its verb run into its `…`, a retry status)
    // takes nothing at column two: its line wraps at its own column, and the
    // row there under a full retry row is the vendor's own notice (2.1.283's
    // `  Fast mode unavailable: …`; the review of round 20, 2026-10-15: joined,
    // the retry read as a done row, and idle mid-retry) — unless the row's own
    // parenthesis is still open: the spinner's `(4s · esc to` cut at the
    // pane's edge, the row under it (`  interrupt)`) the rest of that
    // parenthesis, never a notice (read alone, the narrow pane's hint was
    // lost: `harness::upgrade_drive`'s `busy_footer`).
    let done_end = if is_activity_row(row) && !paren_open(text) {
        i
    } else {
        crate::wrap::line_end(rows, i, text, lead + 2, width, until)
    };
    if done_end > i {
        return Some(if keeps(done_end) { done_end } else { i });
    }
    let joins = run > i
        && !has_status_shape(row)
        && has_status_shape(&crate::wrap::rejoined(&rows[i..=run]))
        && keeps(run);
    Some(if joins { run } else { i })
}

/// Whether `text` opens a parenthesis it does not close: a status row's
/// `(4s · esc to` cut by the pane's edge ([`status_line_end`]).
fn paren_open(text: &str) -> bool {
    text.matches('(').count() > text.matches(')').count()
}

/// A row that opens a block of its own under the status row, never the rest of
/// its line ([`status_line_end`]): a `⎿` row (a tip, a todo list), a queued
/// `❯` message, a `⏺`/`●` row (a transcript row, the survey's question),
/// another glyph row, a rule.
fn opens_block(row: &str) -> bool {
    crate::text::trim_start(row).starts_with(['⎿', '❯', '!', '●', '⏺'])
        || is_glyph_row(row)
        || is_transcript_row(row)
        || ends_live_zone(row)
}

/// The text of a status line's rows `block` ([`StatusBlock::line`]): a glyph
/// alone on its first row read by [`bare_line`], any other line its rows
/// joined ([`crate::wrap::rejoined`]).
fn status_text(block: &[String]) -> String {
    let mut cs = crate::text::trim(&block[0]).chars();
    if cs.next().is_some() && cs.next().is_none() {
        bare_line(block)
    } else {
        crate::wrap::rejoined(block)
    }
}

/// THE LINE OF A BARE GLYPH'S BLOCK (`block[0]` the glyph's row,
/// [`status_line_end`]): where its cut head and its tail do not fit beside
/// each other, Ink lays the vendor's retry row out as TWO text columns side
/// by side — the cut error on the left, the dim tail from its ` · ` on — and
/// their rows interleave (`API    · Retrying in` / `error 4s · attempt` /
/// `      1/10` at 20 columns, MEASURED on 2.1.301; the decision review's
/// RR-LC-1; and the watchdog's retry of the weekly limit at 50 columns,
/// MEASURED on 2.1.300: `Weekly    · Retrying in 5h (Oct 16 at 9:49am) ·`
/// over `li…      attempt 1/300`, read `✻ Weekly li… · Retrying in 5h (Oct
/// 16 at 9:49am) · attempt 1/300`). Joined row by row that reads `Retrying
/// in error 4s`, no wait, and the live retry read idle. The columns are told
/// by the first row under the glyph: its first ` · ` is where the tail's
/// column opens, at column `col`. Where EVERY row under the glyph is blank at
/// `col - 1` and has text past it, the line is the glyph, the left parts
/// joined, then the right parts joined. A head that fills its column leaves
/// no such gap — the stalled stream's uncut `Waiting for API response` at 34
/// to 41 columns, `Waiting for API · will retry in 2m` over `response       ·
/// check your network` (the 2.1.301 `aSn` laid out as the measured blocks
/// are) — and is split there too, but only where the split reads as the
/// vendor's activity or retry row and the rows joined have no status shape:
/// it only ever adds a busy reading. Any other block is
/// [`crate::wrap::rejoined`], as before.
fn bare_line(block: &[String]) -> String {
    let joined = || crate::wrap::rejoined(block);
    let under: Vec<Vec<char>> = block[1..].iter().map(|r| r.chars().collect()).collect();
    let Some(col) = block
        .get(1)
        .and_then(|first| first.find(" · ").map(|at| first[..at].chars().count()))
    else {
        return joined();
    };
    if col == 0 || under.len() < 2 {
        return joined();
    }
    let gapped = under
        .iter()
        .all(|cs| cs.len() > col && cs[col - 1].is_whitespace());
    let part = |cs: &[char]| crate::text::trim(&cs.iter().collect::<String>()).to_string();
    let side = |right: bool| {
        under
            .iter()
            .map(|cs| {
                let at = col.min(cs.len());
                part(if right { &cs[at..] } else { &cs[..at] })
            })
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let split = format!(
        "{} {} {}",
        crate::text::trim(&block[0]),
        side(false),
        side(true)
    );
    if gapped {
        return split;
    }
    let joined = joined();
    if is_activity_row(&split) && !has_status_shape(&joined) {
        split
    } else {
        joined
    }
}

/// A status line with a shape of its own: a spinner's activity or the
/// vendor's retry row ([`is_activity_row`]), a `Waiting for N …` row, or a
/// done row ([`is_done_shape`]).
fn has_status_shape(line: &str) -> bool {
    is_activity_row(line)
        || line
            .split_once("Waiting for ")
            .is_some_and(|(_, n)| n.starts_with(|c: char| c.is_ascii_digit()))
        || is_done_shape(line)
}

/// A DONE row's shape: past the glyph, a word, ` for `, the turn's time (words
/// of a number and its unit, `9m 27s`), then the row's end or ` ·` (`✻
/// Crunched for 9m 27s · done 11:07 AM · 1 shell still running`, `✻ Worked
/// for 0s ·` cut there by a narrow pane).
fn is_done_shape(line: &str) -> bool {
    let t = crate::text::trim(line);
    let mut cs = t.chars();
    if !cs.next().is_some_and(is_spinner_glyph) {
        return false;
    }
    let Some((verb, after)) = crate::text::trim_start(cs.as_str()).split_once(" for ") else {
        return false;
    };
    if verb.is_empty() || verb.contains(' ') {
        return false;
    }
    let time = after.split(" ·").next().unwrap_or(after);
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    !time.is_empty()
        && time
            .split(' ')
            .all(|w| w.strip_suffix(['s', 'm', 'h', 'd']).is_some_and(number))
}

/// A row no live zone reaches past, walking up from the composer's top rule:
/// Claude Code's launch card (anchor `launch.banner`), which opens a
/// process's output, or another composer rule — the frame of a process that
/// drew before this one. Neither is ever between a live status row and its
/// own frame, and everything above either was drawn by something else: the
/// live run of 2026-09-27 (Claude Code 2.1.283, `/exit` then a fresh `claude`
/// with no `clear`, [`crate::prompt::fixtures::EXIT_RELAUNCH_STALE_FOOTER`])
/// walked through the new card, the shell's lines and the dead process's
/// whole frame, took its `/exit` spinner for the live status row, and read
/// the new idle session `busy` until its first turn.
fn ends_live_zone(row: &str) -> bool {
    is_rule(row) || is_launch_card_row(row)
}

/// The launch card's own logo row (` ▐▛███▜▌   Claude Code v2.1.283`): the
/// banner text on a row that STARTS with the card's `▐` glyph. The banner text
/// alone is not enough — a person's queued `❯` message or the worker's todo
/// list can name "Claude Code v…" between a live status row and its frame,
/// and stopping there would hide a running turn's spinner (the review of
/// 2026-09-27: a spinner above `❯ what changed in Claude Code v2.1.284?` read
/// Idle).
fn is_launch_card_row(row: &str) -> bool {
    crate::text::trim_start(row).starts_with('\u{2590}')
        && row.contains(const { crate::anchors::anchor_text("launch.banner") })
}

/// A row that can be the status row: a spinner glyph in column 0, then
/// whitespace (`✶ Deliberating…`, `· Mustering… (38s)`, `✻ Cooked for 4s ·
/// done 2:41 PM`) — or, wherever it sits on the row, the full spinner
/// [shape](is_spinner_shape), so that an INDENTED live row is still the
/// status row and not something the walk steps over on its way to the
/// transcript. A glyph alone on its row is none here: it opens the status row
/// only over the text a narrow pane wrapped away from it
/// ([`status_line_end`], read by the status walk).
pub fn is_glyph_row(row: &str) -> bool {
    let mut cs = row.chars();
    (cs.next().is_some_and(|g| SPINNERS.contains(&g)) && cs.next().is_some_and(char::is_whitespace))
        || is_spinner_shape(row)
}

/// A row only the transcript has: the worker's message or tool call in column
/// 0 (`⏺`; `●` where the platform draws that — never the session survey), or
/// output under the `⎿` gutter that is not a tip or a todo item.
pub(crate) fn is_transcript_row(row: &str) -> bool {
    if is_survey(row) {
        return false;
    }
    if row.starts_with(['⏺', '●']) {
        return true;
    }
    let t = crate::text::trim_start(row);
    t.starts_with('⎿') && !is_tip_or_todo(t)
}

/// A `⎿  Tip: …` row, or a row of the todo list Claude Code can hang under
/// its spinner (`⎿  ☐ Write the tests`, `     ☒ Read the parser`, `     … +3
/// pending`). `t` is trimmed at the start.
fn is_tip_or_todo(t: &str) -> bool {
    let under_gutter = t.strip_prefix('⎿').map(str::trim_start);
    under_gutter.is_some_and(|g| g.starts_with("Tip:")) || {
        let item = under_gutter.unwrap_or(t);
        item.starts_with(['☐', '☒', '☑', '◻', '◼', '□', '■']) || item.starts_with("… +")
    }
}

/// Claude Code's session survey, which it parks above the composer between
/// turns and during them: `● How is Claude doing this session? (optional)`
/// and its options row `1: Bad    2: Fine   3: Good   0: Dismiss`.
pub fn is_survey(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    (t.starts_with('●') && t.contains("How is Claude doing this session"))
        || (t.starts_with("1: Bad") && t.contains("0: Dismiss"))
}

/// Whether Claude Code's session survey is OPEN — parked above the composer,
/// where the next digit typed into it is taken as a RATING (`1` bad, `2`
/// fine, `3` good) or, `0`, dismisses it. Open is the question row, `●` in
/// column 0 (`● How is Claude doing this session? (optional)`), right over
/// its options row (`1: Bad    2: Fine   3: Good   0: Dismiss`), with nothing
/// between the options and the composer's top rule but what Claude Code
/// parks there itself: blank rows, a hint or banner against the right edge
/// (`✔ Update installed · Restart to update`, measured), a `⎿  Tip:` row.
/// A survey quoted in the transcript is not open — the worker's message
/// about it, a tool's output under the `⎿` gutter, the worker's words or a
/// done row between it and the rule — and neither is a question row without
/// its options. It is parked during a turn too, so a live spinner above it
/// changes nothing; nor does a box on the screen (the box is what the worker
/// waits on, and a supervisor answers it first). Without the composer frame,
/// never.
pub fn survey_open(rows: &[String]) -> bool {
    crate::survey::survey_block(rows).is_some()
}

/// How much of the worker's context is left before Claude Code auto-compacts
/// it, in percent (0 to 100), from its indicator in the live zone: `1% until
/// auto-compact` (Claude Code 2.1.267/2.1.268, measured 2026-09-13), or
/// `Context left until auto-compact: 7%` as other versions spell it. The
/// indicator counts only where Claude Code parks it: under the status row
/// (under the last transcript row when there is none) and above the
/// composer's top rule, against the right edge — ending within three columns
/// of the composer's rules and starting past the transcript's columns, at
/// column 6 or later (in a 55-column pane the long spelling starts at 18) —
/// and the whole row, trimmed, the indicator and nothing else. With no status
/// row the zone begins under the last transcript row, so the rest of that
/// row's block (the worker's message, a tool's output) is in it; the edge is
/// what tells the indicator from a copy there. So a worker that quotes it —
/// in its `⏺` message, on a row of that message, in a tool's output under
/// the `⎿` gutter (a peer's indicator in `aterm ctl text` output ends where
/// the peer's edge is, plus the gutter, short of this screen's) — does not
/// count, and neither does a copy above the status row (history) or one flush
/// left. The one copy it cannot tell from the indicator is a row of the last
/// block that ends against this very edge, alone on its row, with no status
/// row under the block. It sits beside whatever else is parked there (a
/// spinner and its `⎿  Tip:`, the session survey, a hint). Without the
/// composer frame, `None`. On a framed screen `None` says only that no
/// indicator is shown: a worker with room left, or one that has just
/// compacted (measured: the row was gone after the compaction).
pub fn context_left(rows: &[String]) -> Option<u8> {
    let frame = composer_frame(rows)?;
    context_above(rows, frame, rule_width(rows, frame))
}

/// The context left as a screen SHOWS it, for a display of the reading (aterm's footer):
/// [`context_left`]'s indicator above the composer, or the one Claude Code 2.1.305 draws at
/// the right end of the row under the composer's bottom rule, after its mode and hints —
/// `  ⏸ manual mode on · ? for shortcuts      0% until auto-compact`, ending at the rule's
/// right edge (measured 2026-10-20 in the real-Claude lab at 80 and 144 columns, idle and
/// busy, where [`context_left`] reads `None`). That row is Claude Code's own, never the
/// transcript's; it counts only against the right edge, and only its last run after two or
/// more blanks, the indicator and nothing else (`context_reading`). The supervisor's watch
/// keeps [`context_left`]'s zone alone; a reading's displays read this
/// ([`crate::reader::ScreenReader::context_shown`]). `None` says only that no indicator is
/// read: whether it has gone is the footer's to judge
/// (`aterm_agent::harness::footer::context_gone`), since Claude Code's other notices take
/// that slot for a while.
pub fn context_left_shown(rows: &[String]) -> Option<u8> {
    let frame = composer_frame(rows)?;
    let width = rule_width(rows, frame);
    rows.get(frame.bottom + 1)
        .map(|row| crate::text::trim_end(row))
        .filter(|row| row.chars().count() + 3 >= width)
        .and_then(|row| row.rsplit("  ").next())
        .and_then(|run| context_reading(crate::text::trim(run)))
        .or_else(|| context_above(rows, frame, width))
}

/// The cells of the composer's bottom rule in `frame`, trimmed.
fn rule_width(rows: &[String], frame: Frame) -> usize {
    crate::text::trim_end(&rows[frame.bottom]).chars().count()
}

/// [`context_left`]'s reading in `frame`, its rule `width` cells.
fn context_above(rows: &[String], frame: Frame, width: usize) -> Option<u8> {
    let from = status_block(rows, frame.top).from;
    rows[from..frame.top]
        .iter()
        .rev()
        .filter(|row| is_against_right_edge(row, width))
        .find_map(|row| context_reading(crate::text::trim(row)))
}

/// The percentage an indicator reads, `t` its row trimmed: `<n>% until
/// auto-compact` or `Context left until auto-compact: <n>%`, `<n>` digits
/// only, 0 to 100 — anything before, after or between them and it is not the
/// indicator.
fn context_reading(t: &str) -> Option<u8> {
    let digits = t.strip_suffix("% until auto-compact").or_else(|| {
        t.strip_prefix("Context left until auto-compact: ")
            .and_then(|rest| rest.strip_suffix('%'))
    })?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u8>().ok().filter(|&n| n <= 100)
}

/// The status row: walking up from the composer's top rule, the first row
/// that starts with a spinner glyph in column 0, before any transcript row
/// (`None` when a transcript row comes first, and without a composer frame).
/// Its FIRST row, as drawn: in a pane narrower than its line, [`status_line`]
/// is the whole of it.
pub fn status_row(rows: &[String]) -> Option<&str> {
    let frame = composer_frame(rows)?;
    status_block(rows, frame.top)
        .status
        .map(|i| rows[i].as_str())
}

/// The status row's LINE ([`status_row`]), read across the rows Ink wrapped
/// it onto in a pane narrower than it (`✻ Choreogr` over `aphing…` at 10
/// columns, `✻ Baked for 0s · done 7:06` over `  AM` at 26, `✻ Worked for 9m
/// 2s · done 3:15 PM · 1 shell still` over `  running`): every reader of what
/// the status row says reads this (`status_line_end`).
#[must_use]
pub fn status_line(rows: &[String]) -> Option<Cow<'_, str>> {
    let frame = composer_frame(rows)?;
    status_block(rows, frame.top).line(rows)
}

/// The first row of the LIVE ZONE, by position: every row a running turn
/// draws its busy signal on is at or under it. With the composer frame it is
/// where [`busy_signal`] starts reading — the status row, else the row under
/// the transcript row, launch card or other composer rule the walk up from
/// the top rule stopped at — and a box up with it starts it no lower than
/// the box's own first row; with a box and no frame, the box's first row
/// ([`crate::prompt::prompt_box_first_row`]: the box replaces the frame, and
/// the turn it blocks draws its footer again where the box was); with
/// neither, `0`, the whole screen, as the reading itself. Never above the
/// row under an EXITED process's resume hint (anchor `exit.resume`, alone on
/// its row): the frame over it is dead however the reading takes it — the
/// only frame on the screen while a fresh `claude` has drawn nothing yet.
///
/// For a wait on the busy guard (`await gone esc.to.interrupt rows <this>
/// …`): a busy row above the live zone is not the turn's, and a wait armed
/// over it never latched. The live run of 2026-09-27 (Claude Code 2.1.283,
/// `/exit` then a fresh `claude` with no `clear`,
/// [`crate::prompt::fixtures::EXIT_RELAUNCH_STALE_FOOTER_QUESTION2`]): the
/// DEAD process's `esc to interrupt` footer stood above the new launch card,
/// the hosted loop's whole-screen `await gone` ran out its 20 s step on
/// every wait, and a question dialog's first key came 26-40 s after it was
/// drawn, each later tab ~20 s.
#[must_use]
pub fn busy_zone_start(rows: &[String]) -> usize {
    let boxed = crate::prompt::prompt_box_first_row(rows);
    let start = match composer_frame(rows) {
        Some(frame) => {
            let block = status_block(rows, frame.top);
            let start = block.status.unwrap_or(block.from);
            boxed.map_or(start, |b| b.min(start))
        }
        None => boxed.unwrap_or(0),
    };
    let notice = const { crate::anchors::anchor_text("exit.resume") };
    let exited = rows
        .iter()
        .rposition(|r| crate::text::trim_end(r) == notice)
        .map_or(0, |i| i + 1);
    start.max(exited)
}

/// The composer row: the last row whose first glyph is `❯` and that is not an
/// option row (`❯ 1. Yes`) — unless it is the caret in column 0 right under a
/// composer rule: a draft that starts with a number (`❯ 1. Keep the harness`,
/// an answer to a numbered question typed but not sent) is still the composer.
/// Nor is it an entry of the list Claude Code opens under the composer
/// ([`is_list_entry`]).
pub fn composer_index(rows: &[String]) -> Option<usize> {
    // A plain loop: every reading finds the composer many times over, and a
    // debug build's iterator adapters cost more than the test of a row.
    let mut i = rows.len();
    while i > 0 {
        i -= 1;
        // Past its spaces a caret row opens on `❯` (`E2 9D AF`), or on bytes
        // that can open more whitespace (`crate::text`): any other ends the
        // trim on a char that is not the caret. Most rows end here, unparsed.
        let b = rows[i].as_bytes();
        let mut k = 0;
        while k < b.len() && b[k] == b' ' {
            k += 1;
        }
        let may_be_caret = match b.get(k) {
            Some(0xE2) => matches!(b.get(k + 1), Some(0x9D | 0x80 | 0x81)),
            Some(0xC2 | 0xE1 | 0xE3 | b'\t'..=b'\r') => true,
            _ => false,
        };
        if !may_be_caret {
            continue;
        }
        let t = crate::text::trim_start(&rows[i]);
        if t.starts_with('❯')
            && (!is_option_like(t) || (rows[i].starts_with('❯') && i > 0 && is_rule(&rows[i - 1])))
            && !is_list_entry(rows, i)
        {
            return Some(i);
        }
    }
    None
}

/// Whether the indented `❯` row `i` is an entry of a list drawn UNDER the
/// composer's bottom rule, and no caret. Claude Code 2.1.291 and 2.1.292 mark
/// the highlighted entry of their slash menu with the Select's pointer (2.1.287
/// drew the entries alone): `  ❯ /compact   Free up context …` over
/// `    /autocompact …`, every entry indented, the pointer on whichever one is
/// highlighted. That row is the last `❯` row of the screen, so it took the
/// composer's place and the frame (the rule above it, a rule below it) was
/// gone: no composer, no wall, `agent=unknown`, and a `/compact` the loop
/// typed was never seen as written. The caret is in column 0; a list under
/// the frame is the run of indented rows that starts right under its bottom
/// rule, a rule that closes a caret row in column 0.
fn is_list_entry(rows: &[String], i: usize) -> bool {
    if rows[i].starts_with('❯') {
        return false;
    }
    let mut first = i;
    while first > 0 && !is_rule(&rows[first - 1]) && leading_spaces(&rows[first - 1]) > 0 {
        first -= 1;
    }
    let Some(bottom) = first.checked_sub(1).filter(|&b| is_rule(&rows[b])) else {
        return false;
    };
    (0..bottom)
        .rev()
        .find(|&j| is_rule(&rows[j]))
        .is_some_and(|top| top + 1 < bottom && rows[top + 1].starts_with('❯'))
}

/// One entry of Claude Code's completion menu ([`completion_menu`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// Its row: an index into the rows read.
    pub row: usize,
    /// The column its first glyph is drawn at (past the pointer on the pointed row).
    pub col: usize,
    /// Its first word (`/clear`; `+` for a file of the `@` menu).
    pub word: String,
}

/// THE COMPLETION MENU Claude Code opens under the composer's bottom rule, the one whose
/// Enter runs an entry: its entries, top to bottom, and the one the Select's pointer
/// marks (2.1.290 and later: `  ❯ /compact   Free up context …`). The menu is the run of
/// indented rows right under the bottom rule; an entry is a row of it drawn at the first
/// one's column, and a row further in is an entry's description wrapped onto a row of
/// its own (`/code-review`'s, at the description column). While it is open the vendor's
/// Enter runs the SELECTED suggestion (2.1.300 bundle, the typeahead's Enter handler
/// `Cr`: `SG({suggestion: Hs[selected], shouldExecute: …})`, with no screen reader's
/// navigation on), not the draft the composer shows: a person's `/cl` left standing is
/// `/clear`. [`CompletionMenu::runs`] says which entries that can be.
///
/// THE `@` MENU of files (`+`) and agents (`*`) opens with NO entry selected, and Enter
/// then sends the draft as shown; an entry a person's arrow selected is completed
/// instead (`read @no` became `read @novel.txt`, `and @RE` became `and @"Explore
/// (agent)"`), and nothing is sent (MEASURED on 2.1.302 and 2.1.268). A build that
/// draws the pointer leaves its room before every entry ([`POINTER_ROOM`]: the entries
/// at column 4, the pointer at 2), so an unpointed `@` menu there has none selected and
/// is `None`: Enter sends. A build with no pointer (2.1.289 and before; its entries at
/// column 2) draws the selection in colour only, so its `@` menu is returned
/// ([`CompletionMenu::mentions`]) and read by its cells ([`CompletionMenu::runs`]).
///
/// `None` as well when the rows under the bottom rule are no such run (the footer, the
/// shortcuts, nothing).
#[must_use]
pub fn completion_menu(rows: &[String]) -> Option<CompletionMenu> {
    let frame = composer_frame(rows)?;
    let mut entries = Vec::new();
    let mut pointed = None;
    let mut at = None;
    for (row, r) in rows.iter().enumerate().skip(frame.bottom + 1) {
        let lead = leading_spaces(r);
        if lead == 0 || crate::text::trim(r).is_empty() {
            break;
        }
        let t = crate::text::trim_start(r);
        let (col, text, marked) = match t.strip_prefix('❯') {
            Some(rest) => {
                let text = crate::text::trim_start(rest);
                (
                    lead + 1 + rest.chars().count() - text.chars().count(),
                    text,
                    true,
                )
            }
            None => (lead, t, false),
        };
        if *at.get_or_insert(col) != col {
            continue;
        }
        let Some(word) = text.split_whitespace().next() else {
            continue;
        };
        if marked {
            pointed = Some(entries.len());
        }
        entries.push(MenuEntry {
            row,
            col,
            word: word.to_string(),
        });
    }
    // Unpointed, the slash menu (its first entry opens `/`, or `@` as an older `@` menu's
    // did) has its top entry selected as it opens (MEASURED on 2.1.268: Enter ran it).
    let first = entries.first();
    let mentions = first.is_some_and(|e: &MenuEntry| e.word.starts_with(['+', '*']));
    let menu = pointed.is_some()
        || first.is_some_and(|e| e.word.starts_with(['/', '@']))
        || first.is_some_and(|e| mentions && e.col < POINTER_ROOM);
    menu.then_some(CompletionMenu {
        entries,
        pointed,
        mentions,
    })
}

/// The column a build that draws the completion menu's pointer starts every entry at:
/// `  ❯ ` before the pointed one, four spaces before the rest (2.1.290 and later). A
/// build with no pointer starts them at column 2 (2.1.289 and before).
pub const POINTER_ROOM: usize = 4;

/// One thing Enter may do over Claude Code's completion menu ([`CompletionMenu::runs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEnter<'a> {
    /// Run this slash-menu entry, by its first word (`/clear`).
    Run(&'a str),
    /// Complete this `@`-menu entry into the draft (`+` for a file, `*` for an agent):
    /// nothing is sent.
    Complete(&'a str),
    /// Send the draft as the composer shows it: the `@` menu with no entry selected.
    Draft,
}

/// Claude Code's completion menu as [`completion_menu`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMenu {
    /// The entries shown, top to bottom.
    pub entries: Vec<MenuEntry>,
    /// The entry the Select's pointer marks: an index into [`Self::entries`].
    pub pointed: Option<usize>,
    /// The `@` menu of files (`+`) and agents (`*`): Enter completes its selected entry,
    /// and with none selected (as it opens) sends the draft ([`completion_menu`]).
    pub mentions: bool,
}

impl CompletionMenu {
    /// Whether the selected entry is drawn IN COLOUR ONLY, so that the rows alone do not
    /// say which it is: more than one entry, none pointed (2.1.289 and before draw no
    /// pointer). Claude Code draws the selected entry in its theme's suggestion colour
    /// and the others dimmed (MEASURED on 2.1.268: the first glyph `/` in `b1b9f9`, the
    /// others' in `999999`). The selection is not always the top one: a person's arrow
    /// moves it, and the vendor keeps it as the draft changes (MEASURED: `/copy`, `down`,
    /// the draft erased and `/co` typed showed its third entry selected). An `@` menu of
    /// one entry on such a build draws its selection in colour only too, but that colour
    /// has no other entry's to be told against ([`Self::runs`]).
    #[must_use]
    pub fn colour_only(&self) -> bool {
        self.pointed.is_none() && self.entries.len() > 1
    }

    /// WHAT ENTER MAY DO: run the pointed or selected entry (the slash menu), complete it
    /// (the `@` menu, [`Self::mentions`]), or send the draft (the `@` menu with none
    /// selected). A menu that shows its selection in colour only ([`Self::colour_only`])
    /// is read by `fg`, the colour of each entry's first glyph in entry order (`None` for
    /// one not read).
    ///
    /// The slash menu has one entry selected: the one drawn unlike all the others, where
    /// those others are at least two and all alike. Unread, or with no such entry — two
    /// entries in two colours (nothing on the screen says which colour is the
    /// selection's), all in one — any entry shown may be the one Enter runs, and every
    /// one is returned.
    ///
    /// The `@` menu draws every entry alike with none selected (MEASURED on 2.1.268: each
    /// first glyph `999999`, files and agents alike) and the selected one unlike the rest
    /// (`b1b9f9`): entries all alike send the draft, and any unlike another means one is
    /// selected, whichever it is, so Enter completes an entry. Unread, or one entry alone
    /// (nothing to tell its colour against), either may be so, and both are returned.
    #[must_use]
    pub fn runs(&self, fg: Option<&[Option<String>]>) -> Vec<MenuEnter<'_>> {
        let one = |k: usize| {
            let word = self.entries[k].word.as_str();
            if self.mentions {
                MenuEnter::Complete(word)
            } else {
                MenuEnter::Run(word)
            }
        };
        let every = || (0..self.entries.len()).map(one);
        if let Some(k) = self.pointed {
            return vec![one(k)];
        }
        let fg = fg.filter(|fg| self.colour_only() && fg.len() == self.entries.len());
        if self.mentions {
            return match fg.and_then(all_alike) {
                Some(true) => vec![MenuEnter::Draft],
                Some(false) => every().collect(),
                None => std::iter::once(MenuEnter::Draft).chain(every()).collect(),
            };
        }
        let selected = if self.colour_only() {
            fg.and_then(odd_one_out)
        } else {
            Some(0)
        };
        match selected {
            Some(k) => vec![one(k)],
            None => every().collect(),
        }
    }
}

/// Whether every colour of `fg` is the same; `None` when any is unread.
fn all_alike(fg: &[Option<String>]) -> Option<bool> {
    let fg: Vec<String> = fg
        .iter()
        .map(|c| c.as_deref().map(str::to_ascii_lowercase))
        .collect::<Option<_>>()?;
    Some(fg.windows(2).all(|w| w[0] == w[1]))
}

/// The one of `fg` unlike all the others, which are at least two and all alike; `None`
/// when any is unread.
fn odd_one_out(fg: &[Option<String>]) -> Option<usize> {
    let fg: Vec<String> = fg
        .iter()
        .map(|c| c.as_deref().map(str::to_ascii_lowercase))
        .collect::<Option<_>>()?;
    if fg.len() < 3 {
        return None;
    }
    // The others' colour: the one the first two share, else the third's.
    let rest = if fg[0] == fg[1] { &fg[0] } else { &fg[2] };
    let mut odd = (0..fg.len()).filter(|&k| fg[k] != *rest);
    let k = odd.next()?;
    odd.next().is_none().then_some(k)
}

fn is_option_like(t: &str) -> bool {
    // `t.trim_start_matches('❯').trim_start()`, on bytes.
    let mut k = 0;
    while t.as_bytes()[k..].starts_with("❯".as_bytes()) {
        k += '❯'.len_utf8();
    }
    let after = crate::text::trim_start(&t[k..]).as_bytes();
    let digits = after.iter().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && after.get(digits) == Some(&b'.')
}

/// The text typed (or suggested) in the composer, without the caret.
pub fn composer_text(rows: &[String]) -> Option<String> {
    let i = composer_index(rows)?;
    Some(
        rows[i]
            .trim_start()
            .trim_start_matches('❯')
            .trim()
            .to_string(),
    )
}

/// Whether the cursor sits INSIDE the composer's typed text: on the caret row
/// (`caret`, [`composer_draft`]'s index), anywhere but column 2. A placeholder
/// suggestion keeps the cursor at column 2 of the caret row, and so does a
/// typed draft whose caret was moved home (`←`, `Home`, `ctrl-a`), so the
/// cursor can only PROVE typed text, never a placeholder: at column 2, or on
/// any other row (a grid displaced by a net-zero re-grid parts the cursor
/// from the composer: the owner's tab of 2026-09-28, the cursor one row below
/// the prompt), it says nothing and the caret row's `cell` decides. `cursor`
/// is `(row, col)` in the coordinates of the rows [`composer_draft`] read.
///
/// The rule the upgrade's composer read (`aterm-agent`
/// `harness/upgrade.rs`, `composer_is_empty`) uses. The supervisor's read
/// (`turn_end_loop`, `Session::composer_draft`) is stricter — a cursor off
/// column 2 on any row makes it a draft — and stays its own policy at a wall;
/// the caret-row test is the part they share, so whichever change touches that
/// read next imports this function rather than write the caret-row rule a
/// third time.
#[must_use]
pub fn cursor_inside_text(cursor: Option<(usize, usize)>, caret: usize) -> bool {
    matches!(cursor, Some((row, col)) if row == caret && col != 2)
}

/// The composer's rows: the caret row's index, then the text of each row
/// from it down to the bottom rule — the caret row's without the caret — as
/// a draft that wraps or breaks onto more than one row fills them (the caret
/// row alone with no rule under it). `None` without a caret row.
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let caret = composer_index(rows)?;
    let end = (caret + 1..rows.len())
        .find(|&i| is_rule(&rows[i]))
        .unwrap_or(caret + 1);
    let mut lines = vec![composer_text(rows)?];
    lines.extend(
        rows[caret + 1..end]
            .iter()
            .map(|r| crate::text::trim(r).to_string()),
    );
    Some((caret, lines))
}

/// Whether the composer's text is Claude Code's DIM placeholder suggestion
/// rather than typed input: the caret row reads `❯ <text>` but the cursor sits
/// at column 2, where typing would have pushed it right. Measured: `m7 is
/// reachable as ssh m7, go` was a suggestion, not a human.
pub fn is_placeholder(rows: &[String], cursor_col: usize) -> bool {
    composer_text(rows).is_some_and(|t| !t.is_empty()) && cursor_col == 2
}

/// A composer separator: a row that is mostly `─`.
fn is_separator(row: &str) -> bool {
    row.chars().filter(|c| *c == '─').count() >= 10
}

/// Claude Code's own status rows, never the worker's words: the done row
/// (`✻ Baked for 1m 10s · done 2:50 PM`, a spinner glyph first) and the
/// banners it parks between the transcript and the composer (`✔ Update
/// installed · Restart to update`, measured on the live worker).
const STATUS_GLYPHS: &[char] = &['✔', '✓', '✘', '✗', '⚠', 'ℹ'];

fn is_status_row(row: &str) -> bool {
    crate::text::trim_start(row)
        .chars()
        .next()
        .is_some_and(|g| SPINNERS.contains(&g) || STATUS_GLYPHS.contains(&g))
}

/// The first non-space column of a right-aligned hint: transcript rows start
/// at 0, 2, 4 or 5, and Claude Code parks its hints against the right edge.
pub(crate) const HINT_COLUMN: usize = 20;

pub(crate) fn leading_spaces(row: &str) -> usize {
    crate::text::leading_space_chars(row)
}

/// A right-aligned hint (`✔ Update installed · Restart to update`, `get pinged
/// when Claude finishes · enable push notifications in /config`): it starts
/// at column 20 or later — or, in a window too narrow for that (the 70-column
/// `get pinged …` hint starts at column 8 of an 80-column grid), it starts
/// past the gutter's text column and ends at the right margin, within three
/// columns of `width` (measured: hints end two columns short of the composer
/// rule).
fn is_hint(row: &str, width: Option<usize>) -> bool {
    let lead = leading_spaces(row);
    let end = crate::text::trim_end(row).chars().count();
    end > lead && (lead >= HINT_COLUMN || (lead >= 6 && width.is_some_and(|w| end + 3 >= w)))
}

/// The last row the worker said (see [`last_said_index`]).
pub fn last_said_row(rows: &[String]) -> Option<&str> {
    last_said_index(rows).map(|i| rows[i].as_str())
}

/// The index of the last thing said above the composer — the worker's words,
/// a tool's output, a notice under the gutter, a user row: walking up from
/// the status row (from the top rule when there is none), blanks, rules,
/// Claude Code's status and banner rows, `⎿  Tip:` rows, todo items, the
/// session survey, right-aligned hints and the vendor's own paragraphs
/// ([`vendor_paragraph`]) are skipped, so a question is still a question with
/// the done row, an update banner or the survey under it. Without a composer,
/// the last non-blank row of the screen (above the caret when there is one).
/// 2.1.300 draws its auto-mode paragraph after a resumed conversation's
/// history, under the turn the conversation ended on (KI-1015-1 LC-10): read
/// as the agent's last words, it hid the usage limit that turn ended on.
pub fn last_said_index(rows: &[String]) -> Option<usize> {
    let (end, width) = match composer_frame(rows) {
        Some(f) => (
            status_block(rows, f.top).status.unwrap_or(f.top),
            Some(crate::text::trim_end(&rows[f.bottom]).chars().count()),
        ),
        None => (composer_index(rows).unwrap_or(rows.len()), None),
    };
    let parked = crate::survey::parked_span(rows);
    let mut i = end;
    while i > 0 {
        i -= 1;
        if parked.is_some_and(|(a, b)| (a..=b).contains(&i)) || !is_said(&rows[i], width) {
            continue;
        }
        match vendor_paragraph(rows, i) {
            Some(open) => i = open,
            None => return Some(i),
        }
    }
    None
}

/// The vendor's OWN `⏺` paragraph that row `i` is a row of, as its bullet
/// row: Claude Code draws a few notices of its own as a message in the
/// transcript, which are no turn's words (anchors `notice.*`: the auto-mode
/// notice after a `--permission-mode auto --resume`, four rows at 144
/// columns, MEASURED on 2.1.300, ATERM-KI-1015-1 LC-10 — journaled as the
/// agent's last words, and read by a stop-phrase check as what it said). The
/// paragraph is its bullet row and the rows under it at column two
/// ([`in_message`]), and it is the vendor's when its TEXT — its rows joined as
/// the one line Ink wrapped ([`crate::wrap::rejoined`]) — opens with one of
/// those notices: below about 64 columns Ink wraps the notice's own words off
/// its bullet row (`⏺ Auto mode lets Claude handle permission prompts` over
/// `  automatically — …` at 50; the review of round 20, 2026-10-15). A
/// worker's reply that opens with the same words is read as the notice: the
/// screen cannot tell them apart.
fn vendor_paragraph(rows: &[String], i: usize) -> Option<usize> {
    const NOTICES: &[&str] = &[crate::anchors::anchor_text("notice.auto_mode")];
    let mut open = i;
    while !rows[open].starts_with(['⏺', '●']) {
        if !in_message(&rows[open]) {
            return None;
        }
        open = open.checked_sub(1)?;
    }
    let end = message_end(rows, open, rows.len());
    if end < i {
        return None;
    }
    let line = crate::wrap::rejoined(&rows[open..=end]);
    let text = crate::text::trim(line.trim_start_matches(['⏺', '●']));
    NOTICES.iter().any(|n| text.starts_with(n)).then_some(open)
}

/// Where the transcript ends on the screen, by POSITION: the index of the
/// first row of the live zone. With Claude Code's composer frame that is the
/// status row ([`status_row`]) when there is one — it and everything under it
/// down to the composer (a tip, a todo list, a hint, the survey, a queued
/// `❯` message) are the live zone; a DONE status row (`✻ Worked for 3m 21s ·
/// done 8:50 PM`) ends the turn and stays, only what hangs under it goes —
/// else the top rule, less the blank rows, hints against the right edge,
/// `⎿  Tip:` rows and the session survey parked right above it. Without the
/// frame, one past the last non-blank row. No row above it is judged by what
/// it says: a done row, a table, a todo item the worker wrote, indented code
/// are all transcript — the report of what the worker said keeps them, where
/// [`last_said_index`]'s filter would not.
pub fn transcript_end(rows: &[String]) -> usize {
    let Some(frame) = composer_frame(rows) else {
        return rows
            .iter()
            .rposition(|r| !crate::text::trim(r).is_empty())
            .map_or(0, |i| i + 1);
    };
    let block = status_block(rows, frame.top);
    if let (Some(status), Some(line)) = (block.status, block.line(rows)) {
        // A done row (`✻ Worked for 3m 21s · done 8:50 PM · 1 monitor still
        // running`) ends the turn in the transcript: it is kept, the rows a
        // narrow pane wrapped it onto with it (its LINE: `✻ Worked for 0s ·
        // done` over `  10:36 AM`), and only what hangs under it is the live
        // zone. A spinner or a `Waiting for …` row is the live zone itself,
        // read on its line too (`✶ Choreog` over `  raphing…`).
        return if is_done_row(&line) {
            block.end + 1
        } else {
            status
        };
    }
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let mut end = frame.top;
    let parked = crate::survey::parked_span(rows);
    while end > 0
        && (is_parked_above_composer(&rows[end - 1], width)
            || parked.is_some_and(|(a, b)| (a..=b).contains(&(end - 1))))
    {
        end -= 1;
    }
    end
}

/// A status row that reports a finished turn rather than work in flight: not
/// a spinner's `…` activity, not `Waiting for …`.
pub fn is_done_row(row: &str) -> bool {
    !is_activity_row(row) && !row.contains("Waiting for ")
}

/// What Claude Code parks between the transcript and an idle composer: a
/// blank row, a hint or banner against the RIGHT edge (within three columns
/// of the composer rule's `width` — a transcript row merely indented 20
/// columns, indented code, is not one), a `⎿  Tip:` row, the survey.
fn is_parked_above_composer(row: &str, width: usize) -> bool {
    let t = crate::text::trim_start(row);
    t.is_empty()
        || is_survey(row)
        || is_against_right_edge(row, width)
        || t.strip_prefix('⎿')
            .is_some_and(|g| crate::text::trim_start(g).starts_with("Tip:"))
}

/// A row against the RIGHT edge, where Claude Code parks its hints, banners
/// and context indicator: a hint ([`is_hint`]) that ends within three columns
/// of the composer rule's `width` (measured: two short). A row that ends
/// short of the edge is not one, however far in it starts — a transcript row
/// indented 20 columns (indented code), a right-aligned row quoted in a
/// tool's output.
pub(crate) fn is_against_right_edge(row: &str, width: usize) -> bool {
    is_hint(row, Some(width)) && crate::text::trim_end(row).chars().count() + 3 >= width
}

pub(crate) fn is_said(row: &str, width: Option<usize>) -> bool {
    let t = crate::text::trim_start(row);
    !t.is_empty()
        && !is_separator(row)
        && !is_status_row(row)
        && !is_survey(row)
        && !is_tip_or_todo(t)
        && !is_hint(row, width)
}

/// The usage or rate limit notice the worker's last turn ENDED on, as
/// `(message, reset)`. The last thing said above the composer
/// ([`last_said_index`]: the done row, hints, tips, the survey and banners
/// under it skipped) is a block under the `⎿` gutter — Claude Code's own
/// output, never the worker's `⏺` prose, a paragraph of it or a user row —
/// and the block OPENS with a limit notice; or the footer under the bottom
/// rule carries one; or a banner row Claude Code parks under the last thing
/// said — between it and the composer's top rule, the status row's own
/// position included, above or below a done row — a status glyph first
/// ([`is_status_row`]: `⚠`, never a spinner's) opens with one: `⚠ Usage
/// limit reached · continuing automatically at 1:50pm · esc to cancel` (its
/// text measured 2026-09-17 13:50 on the live worker; a second reading said
/// `continuing shortly`; where on the screen Claude Code draws it was not
/// measured, so the banner, the footer and the gutter all read it,
/// [`notice`]). Anything said after a notice makes it history: a later
/// turn's words (a background command or a monitor event starts one with no
/// user row), the `/model` output that switched the model (measured), the
/// worker's reply to a retry — the turn the auto-continue notice resumed,
/// once its words are under it. Whether the worker WORKS under a notice is
/// [`worker_phase`]'s to say, not this function's: the auto-continue notice
/// stays on the screen while the worker resumes under it, and a busy status
/// row or footer under a notice makes the screen busy, never limited.
///
/// What counts as a limit is [`classify_wall`]'s table: a notice whose
/// [`WallKind::reads_limited`] — a usage window (`You've hit your session
/// limit · resets 7:30pm (America/Los_Angeles)`, `5-hour limit reached ∙
/// resets 3am`, `Claude usage limit reached. Your limit will reset at 3pm
/// (America/New_York).`, `⚠ Usage limit reached · continuing automatically
/// at 1:50pm`), a model bucket (`You've reached your Fable limit. Run
/// /usage-credits to continue or switch models with /model.`), spend, or an
/// API rate limit (`API Error: Rate limit reached for requests`). A warning
/// (`Approaching …`) never counts, and neither do the notices the worker
/// goes on under (a subagent quota, a subagent budget, fast mode's
/// fallback) or a full context — [`crate::wall::wall`] names that one. The
/// block's continuation rows are part of the message; `reset` is read from
/// whichever of its lines (its rows joined where Ink wrapped them,
/// [`line_at`]) says `resets …` / `reset at …` — or, the
/// auto-continue notice, `continuing automatically at <time>` / `continuing
/// shortly` ([`reset_of`]).
pub fn limit_notice(rows: &[String]) -> Option<(String, Option<String>)> {
    notice(rows, &|text| {
        classify_wall(text).filter(WallKind::reads_limited)
    })
    .map(|w| (w.message, w.reset))
}

/// The notice the worker's last turn ended on, placed as [`limit_notice`]
/// places it — the footer, a banner under the last thing said, the vendor's
/// `⏺` error row ([`error_row_notice`]), the `⎿` block of the last message —
/// and named by `classify`. Shared by [`limit_notice`] (the usage kinds) and
/// [`crate::wall::wall`] (every kind), so the two can never disagree about
/// WHERE a wall is. A footer's or banner's GENERIC limit notice (`Usage limit
/// reached · …`, no window, no model) is the limit the turn's own notice
/// names, where that notice names the same reset: a weekly window, a model's
/// bucket, money (KI-1015-1 W3, 2.1.300; [`crate::wall::named_by_the_turn`]).
///
/// A `⎿` block is read as the VENDOR's row by [`block_is_vendors`]: never
/// at a column but 2 (a tool's output continues at column 5); never the
/// FIRST column-2 row under a call ([`is_tool_call`]), which is the call's
/// output; a later column-2 row under a call, and every column-2 row under
/// any other owner or none on the screen, read — for recall: the screen
/// cannot tell some of those from a tool's output (an MCP result's second
/// block), so the supervisor acts on a wall only where the session's
/// transcript confirms it (aterm-agent, `supervise/wall_confirm.rs`), and
/// a wall read under a call's later row says so ([`Wall::under_call`]). A
/// block that opens with the command's own echo (`⎿  $ touch x`, Claude Code
/// 2.1.280 draws a running command so) is output too ([`crate::wall`],
/// module header).
pub(crate) fn notice(rows: &[String], classify: &dyn Fn(&str) -> Option<WallKind>) -> Option<Wall> {
    let mut found = notice_placed(rows, classify)?;
    // Claude Code 2.1.284 draws an armed auto-continue as
    // its own notification under the wall's row (`limit-next`: `Continuing
    // automatically at 3pm · esc to cancel`), where 2.1.283 opened the one
    // banner with `Usage limit reached · continuing automatically at …`. The
    // countdown belongs to the wall it stands under: kept in its message,
    // where `resumes_by_itself` reads it.
    //
    // The same holds at the instant the vendor
    // FIRES it. `Usage limit reset · continuing automatically` (2.1.283's
    // banner, 284's toast) and `Usage limit available again · continuing now`
    // are no wall of their own (the table: not a wall), but they say the
    // vendor is going on: with the row above them still the old wall, whose
    // reset has passed, leaving them out read as a standing wall and typed a
    // duplicate `keep going` behind the vendor's own resume.
    //
    // WHERE 2.1.284 draws its two
    // notifications is unmeasured, and a transient toast may be gone when the
    // loop looks. So the search is by wording, not by layout: every row of the
    // live zone outside the composer — the rows between the wall and the
    // composer (a hint against the right edge, a banner) and the footer's
    // notification rows under it — for any wall placement, but not the wall's
    // own row or one its message already holds.
    //
    // Each row is read as the line it opens, across the rows Ink wrapped it
    // onto ([`line_at`]): at 26 columns the countdown's `Continuing` stands
    // alone on its first row (2.1.299).
    if let Some(frame) = composer_frame(rows) {
        let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
        let above = found.row + 1..frame.top;
        let below = frame.bottom + 1..rows.len();
        if let Some(toast) = above
            .map(|i| line_at(rows, i, frame.top, width).1)
            .chain(below.map(|i| line_at(rows, i, rows.len(), width).1))
            .map(|line| crate::text::trim(&line).to_string())
            .find(|t| says_the_vendor_goes_on(t) && !found.message.contains(t.as_str()))
        {
            found.message = format!("{} \u{2014} {toast}", found.message);
        }
    }
    Some(found)
}

/// A hint or banner row that says Claude Code goes on by itself: the armed
/// countdown (`Continuing automatically at 3pm · esc to cancel`), the moment
/// it fires (`Usage limit reset · continuing automatically`), `continuing
/// shortly`, or `Usage limit available again · continuing now`.
fn says_the_vendor_goes_on(row: &str) -> bool {
    let flat = row.replace('\u{2219}', "\u{b7}");
    find_ascii_ci(
        &flat,
        const { crate::anchors::anchor_text("wall.auto_continue") },
    )
    .is_some()
        || find_ascii_ci(&flat, "continuing shortly").is_some()
        || find_ascii_ci(&flat, "available again \u{b7} continuing now").is_some()
}

fn notice_placed(rows: &[String], classify: &dyn Fn(&str) -> Option<WallKind>) -> Option<Wall> {
    if let Some(frame) = composer_frame(rows) {
        let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
        // The footer's banner and a banner under the last thing said name no
        // limit in 2.1.295 and later (`Usage limit reached`): the turn's own
        // notice names it ([`crate::wall::named_by_the_turn`]).
        if let Some(found) = footer_notice(rows, frame.bottom + 1, width, classify)
            .or_else(|| banner_notice(rows, frame.top, width, classify))
        {
            return Some(crate::wall::named_by_the_turn(rows, found, classify));
        }
    }
    let last = last_said_index(rows)?;
    // A `/goal` notice is Claude Code's ECHO of the wall row
    // above it; that row is the wall.
    //
    // Only the kind of row the echo's own
    // reason can come from. `the request was rate limited` follows a limit or
    // a 429, never the worker's prose about a login or a full context that
    // happens to open with a wall's words — read through to that, the
    // supervisor typed `/login` or `/compact` on a rate-limited goal check.
    if let Some(echo) = goal_echo_open(rows, last)
        && let Some(prev) = last_said_index(&rows[..echo])
        && let Some(mut cause) = notice_at(rows, prev, classify)
        && let text = goal_echo_text(rows, echo, last)
        && echo_names_the_cause_of(&text, cause.kind)
    {
        cause.message = format!("{} \u{2014} {text}", cause.message);
        return Some(cause);
    }
    notice_at(rows, last, classify)
}

/// Whether a `/goal` echo's own reason can come from a wall of `cause`'s kind
/// (the vendor's fixed reasons, both binaries): `usage limit reached` and
/// `the request was rate limited` follow a limit (or a 429); the API's own
/// (`the API rejected the last request`, `the API was unavailable or the
/// connection dropped`, `the API returned an unexpected response`, `the last
/// turn could not finish`) follow an API error; `Goal cleared after an
/// unrecoverable error (<label>)` names its cause's kind in the label. A
/// reason no wall row can cause (`a hook ended the turn`, `the goal check
/// timed out`, `goal checks kept finding it unmet this turn`, `model
/// unavailable`) reads through to nothing.
fn echo_names_the_cause_of(echo: &str, cause: WallKind) -> bool {
    let e = echo.to_lowercase().replace('\u{2019}', "'");
    let has = |needle: &str| e.contains(needle);
    let api = matches!(cause, WallKind::ApiError { .. } | WallKind::Overloaded);
    if has("unrecoverable error") {
        return if has("authentication failed") || has("organization verification required") {
            cause == WallKind::Auth
        } else if has("credit balance too low") {
            cause == WallKind::Spend
        } else if has("context limit reached") {
            cause == WallKind::Context
        } else {
            false
        };
    }
    (has("usage limit reached") || has("the request was rate limited")) && cause.reads_limited()
        || (has("the api ") || has("connection dropped") || has("the last turn could not finish"))
            && api
}

/// The `⏺` row a `/goal` notice opens on, when the last thing said (ending at
/// `last`) is one: `Goal paused · …`, `Goal still active · …`, `Goal cleared
/// after …`.
fn goal_echo_open(rows: &[String], last: usize) -> Option<usize> {
    let mut open = last;
    while !rows[open].starts_with(['⏺', '●']) {
        let wrapped = rows[open]
            .strip_prefix("  ")
            .is_some_and(|t| t.starts_with(|c: char| !c.is_whitespace() && c != '⎿'));
        if !wrapped {
            return None;
        }
        open = open.checked_sub(1)?;
    }
    let head = rows[open].strip_prefix(['⏺', '●']).map(str::trim)?;
    ["Goal paused", "Goal still active", "Goal cleared"]
        .iter()
        .any(|p| head.starts_with(p))
        .then_some(open)
}

/// The goal echo's own words, its wrapped rows joined.
fn goal_echo_text(rows: &[String], open: usize, last: usize) -> String {
    let head = rows[open].strip_prefix(['⏺', '●']).map_or("", str::trim);
    std::iter::once(head)
        .chain(rows[open + 1..=last].iter().map(|r| crate::text::trim(r)))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The wall the last thing said (ending at `last`) is, by placement.
fn notice_at(
    rows: &[String],
    last: usize,
    classify: &dyn Fn(&str) -> Option<WallKind>,
) -> Option<Wall> {
    if let Some(found) = error_row_notice(rows, last, classify) {
        return Some(found);
    }
    let open = gutter_open(rows, last)?;
    let head = crate::text::trim_start(&rows[open])
        .trim_start_matches('⎿')
        .trim();
    if head.starts_with("$ ") {
        return None;
    }
    let under_call = match block_is_vendors(rows, open) {
        Block::Output | Block::HookWords => return None,
        Block::UnderCall => true,
        Block::Vendors | Block::Unowned => false,
    };
    // The block's lines, each across the rows Ink wrapped it onto
    // ([`line_at`]): the notice is the first, named whole (at 26 columns
    // `⎿  You've hit your` stands over `session limit ·`, 2.1.299), and the
    // reset is whichever line names one, its zone with it (`· resets
    // 10:25am` over `(America/Los_Angeles)` at 34). The last line runs on
    // past `last` over a row it wraps onto that the last-said walk passed
    // over: one that opens with `·` reads as a status row.
    let width = crate::question::screen_width(rows);
    let mut lines: Vec<String> = Vec::new();
    let mut i = open;
    while i <= last {
        let (end, line) = line_at(rows, i, rows.len(), width);
        let text = crate::text::trim(&line);
        let text = if i == open {
            text.trim_start_matches('⎿').trim()
        } else {
            text
        };
        lines.push(text.to_string());
        i = end + 1;
    }
    let kind = classify(&lines[0])?;
    let reset = lines.iter().find_map(|l| reset_of(l));
    Some(Wall {
        kind,
        message: lines.join(" "),
        reset,
        row: open,
        placement: Placement::Gutter,
        under_call,
    })
}

/// How many rows a `<notice> · <remedy>` error row may take in a pane 80
/// columns wide or wider, its wrapped text included ([`error_row_notice`]);
/// in a narrower pane, how many it would take at 80 ([`notice_rows_at_80`]):
/// those notices are one sentence and a remedy, and a longer `⏺` block is
/// the worker's own words. An `API Error` may run longer: the vendor's own
/// messages do (a 529 wraps to three rows at 80 columns, and a message runs
/// to 1000 characters before the expand hint).
const ERROR_ROWS: usize = 3;

/// How many columns short of the pane Claude Code draws the text of its OWN
/// `⏺` rows, its error row and its dotted notices: in a column `columns -
/// 10` cells wide beside the bullet (2.1.300's `su` and `ig`, read from the
/// binary; measured on 2.1.299 at 26 and 34 columns, rows of at most `width
/// - 10` cells). The worker's reply has no such column: its prose runs to
/// the pane's edge, `width - 2` cells from column two (2.1.300,
/// `claude-2.1.300-prose-opens-with-limit-words-34col.txt`).
const NOTICE_MARGIN: usize = 10;

/// The rows the vendor's `⏺` notice on rows `open..=end` would take at 80
/// columns, read in a pane narrower than that, where the same notice takes
/// more rows than [`ERROR_ROWS`] counts (`⏺ Usage limit reached ·
/// continuing automatically at 10:25am · esc to cancel`: four rows at 34
/// columns, two at 80). `None` in a pane 80 columns or wider, where the rows
/// themselves are counted, and for a message not drawn in the vendor's
/// column — a row of it wider than `width - 10` cells ([`NOTICE_MARGIN`]) is
/// the worker's prose. Each line of it — its rows joined where Ink wrapped
/// them, a row too full for the next row's first word in that column
/// ([`crate::wrap::continues`]) — is counted as Ink wraps it in the 70 cells
/// the column holds at 80 columns ([`crate::wrap::rows_in`]).
///
/// THE LIMIT. A hard line break under a row that happens to be that full is
/// joined, so the worker's prose that fits the vendor's column, every row of
/// it at most `width - 10` cells, and opens with a wall's words and a ` · `
/// can still read as the wall in a narrow pane, as such prose of three rows
/// or fewer does at 80 columns; the supervisor then acts on it only where
/// the session's transcript confirms it (aterm-agent,
/// `supervise/wall_confirm.rs`), and where there is none to read, as the
/// screen reads.
fn notice_rows_at_80(rows: &[String], open: usize, end: usize) -> Option<usize> {
    use crate::question::cells;
    const ROW_AT_80: usize = 80 - NOTICE_MARGIN;
    let width = crate::question::screen_width(rows);
    if width >= 80 {
        return None;
    }
    let room = width.saturating_sub(NOTICE_MARGIN);
    let text = |i: usize| {
        let t = crate::text::trim(&rows[i]);
        if i == open {
            crate::text::trim(t.trim_start_matches(['⏺', '●']))
        } else {
            t
        }
    };
    let texts: Vec<&str> = (open..=end).map(text).collect();
    if texts.iter().any(|t| cells(t) > room) {
        return None;
    }
    let mut count = 0;
    let mut line = texts[0].to_string();
    for ((above, row), here) in texts.iter().zip(&rows[open + 1..=end]).zip(&texts[1..]) {
        if crate::wrap::continues(above, 2, row, room) {
            line.push(' ');
        } else {
            count += crate::wrap::rows_in(&line, ROW_AT_80);
            line.clear();
        }
        line.push_str(here);
    }
    Some(count + crate::wrap::rows_in(&line, ROW_AT_80))
}

/// Whether `row` is a row of the `⏺` message above it: indented two columns,
/// its text no `⎿` row — the message's text Claude Code wrapped onto it
/// ([`error_row_notice`], [`line_at`]).
fn in_message(row: &str) -> bool {
    row.strip_prefix("  ")
        .is_some_and(|t| t.starts_with(|c: char| !c.is_whitespace() && c != '⎿'))
}

/// The last row of the `⏺` message (its paragraph) whose bullet row is
/// `open`: the rows under it, short of row `until`, that are its own
/// ([`in_message`]).
fn message_end(rows: &[String], open: usize, until: usize) -> usize {
    let until = until.min(rows.len());
    let mut end = open;
    while end + 1 < until && in_message(&rows[end + 1]) {
        end += 1;
    }
    end
}

/// THE LINE ROW `i` OPENS, read across the rows Ink wrapped it onto
/// ([`crate::wrap`]), none from row `until` on, in a pane `width` cells
/// wide: its last row and its text, the rows joined with one space (the
/// first row's indent kept). A `⏺` or `●` row's line is its message — the
/// rows under it at column two ([`message_end`]), as [`error_row_notice`]
/// always read it, for no rule of full rows finds where a line of it ends:
/// Claude Code draws its own notices there in a column ten columns short of
/// the pane ([`NOTICE_MARGIN`]), never full by the pane's room, and the
/// worker's prose to the pane's edge, a hard line break under a full row as
/// often as not; a `⎿` row's text is wrapped at the column it starts at
/// past the `⎿` (the gutter's text column, [`GUTTER_TEXT_COLUMN`], for a
/// notice; a column less for a local command's output, `  ⎿ Claude Code
/// will continue`); any other row's at its own column. A row that is no line's first is read as one all
/// the same, as a row was before lines were: what each reader looks for
/// opens a line.
pub(crate) fn line_at(rows: &[String], i: usize, until: usize, width: usize) -> (usize, String) {
    let row = rows[i].as_str();
    let end = if row.starts_with(['⏺', '●']) {
        message_end(rows, i, until)
    } else if let Some(after) = crate::text::trim_start(row).strip_prefix('⎿') {
        let head = crate::text::trim(after);
        let col = leading_spaces(row) + 1 + leading_spaces(after);
        crate::wrap::line_end(rows, i, head, col, width, until)
    } else {
        let head = crate::text::trim(row);
        crate::wrap::line_end(rows, i, head, leading_spaces(row), width, until)
    };
    (end, crate::wrap::rejoined(&rows[i..=end]))
}

/// What the vendor adds under a message past 1000 characters.
const EXPAND_HINT: &str = "(ctrl+o to expand)";

/// THE VENDOR'S ERROR ROW the last turn ended on. Claude Code draws an
/// `isApiErrorMessage` row its message renderer has no case of its own for
/// (2.1.281's `du`, 2.1.283's `lu()`, both read from the binary on
/// 2026-09-27) as its `⏺` bullet in column 0 — the `error:` one, in the
/// warning colour, which a row of text cannot tell from the worker's own —
/// then the text, wrapped ten columns short of the width onto rows under
/// column 2. Two incidents of 2026-09-27 ended every turn on one: `⏺ Login
/// expired · Please run /login` for nine hours (tab s-b5cf2faabac5ce5127bd),
/// and `⏺ API Error: Can't reach the API server — check your internet or DNS
/// (ENOTFOUND)` through an hour's outage, typed `keep going` into sixteen
/// times; every one read idle with no wall. In 2.1.283 every `API Error: …`
/// is drawn so — a 529, a 5xx, a rate limit, every connection and mid-stream
/// failure. `Not logged in · …`, `OAuth token revoked · …`, `Request timed
/// out` and a full context have cases of their own and are drawn under the
/// `⎿` gutter, which [`notice`] reads after this.
///
/// A worker's message is drawn the same way, so the row must be the vendor's
/// in its words as well as its place: the LAST thing said, ONE paragraph (the
/// `⏺` row and the rows indented two columns under it, no `⎿` row, nothing
/// else), no tool call, and the vendor's own shape — `API Error` at its head,
/// case and all, or a notice and its remedy joined by ` · ` or ` ∙ ` in at
/// most [`ERROR_ROWS`] rows (in a pane narrower than 80 columns, drawn in
/// the vendor's own column and at most that many at 80,
/// [`notice_rows_at_80`]) — whose text the table ([`classify`]) names. The
/// worker's `⏺ Not logged in to gh, so nothing was pushed.` has no remedy
/// joined on, and its `⏺ The server was overloaded, so I retried` no vendor
/// head: neither is one. The copies this cannot tell from the vendor's row: a
/// message that opens with a wall's words, a `·` and more, and a one-paragraph
/// reply that itself opens `API Error`. The whole message is classified, its
/// rows joined — under ~100 columns the words that decide it (`(ENOTFOUND)`,
/// `mid-response`) wrap onto the second row — and a wrapped row that opens
/// with `·` or `*`, which reads as a status row and so can end the last thing
/// said above it, is its own too. The vendor's [`EXPAND_HINT`] is dropped.
fn error_row_notice(
    rows: &[String],
    last: usize,
    classify: &dyn Fn(&str) -> Option<WallKind>,
) -> Option<Wall> {
    let mut open = last;
    while !rows[open].starts_with(['⏺', '●']) {
        if !in_message(&rows[open]) {
            return None;
        }
        open = open.checked_sub(1)?;
    }
    if is_tool_call(&rows[open]) {
        return None;
    }
    let end = message_end(rows, open, rows.len());
    let head = rows[open]
        .strip_prefix(['⏺', '●'])
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    let mut parts: Vec<&str> = std::iter::once(head)
        .chain(rows[open + 1..=end].iter().map(|r| crate::text::trim(r)))
        .collect();
    if let Some(tail) = parts.last_mut() {
        *tail = tail.strip_suffix(EXPAND_HINT).unwrap_or(tail).trim_end();
    }
    parts.retain(|p| !p.is_empty());
    let text = parts.join(" ");
    let short = end - open < ERROR_ROWS
        || notice_rows_at_80(rows, open, end).is_some_and(|n| n <= ERROR_ROWS);
    let remedy = short && (text.contains(" · ") || text.contains(" ∙ "));
    if !(head.starts_with("API Error") || remedy) {
        return None;
    }
    let kind = classify(&text)?;
    Some(Wall {
        kind,
        reset: reset_of(&text),
        message: text,
        row: open,
        placement: Placement::ErrorRow,
        under_call: false,
    })
}

/// The line the screen's LAST THING SAID opens where Claude Code draws a row
/// of its own there ([`last_said_notice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaidNotice {
    /// Its text, the rows Ink wrapped it onto joined, the vendor's
    /// [`EXPAND_HINT`] dropped.
    pub text: String,
    /// It has the vendor's error row's own shape, which no table needs to
    /// name: `API Error` at its head, or (a `⏺` row) a notice and its remedy
    /// joined by ` · ` in a few rows ([`error_row_notice`]).
    pub shaped: bool,
    /// The row its line opens on: the `⏺` row, or the block's `⎿` row.
    pub row: usize,
    /// [`Placement::ErrorRow`] for a `⏺` row, [`Placement::Gutter`] for a
    /// `⎿` block.
    pub placement: Placement,
    /// The block stands right under a hook's error row, where the hook's own
    /// words do ([`block_is_vendors`], rule 4): never a wall on the screen,
    /// and whether it is the hook's words or Claude Code's notice only the
    /// caller's words can say — a blocking reason of blank space draws no
    /// row, and the wall stands right there (2.1.304, measured). Its text is
    /// the screen's spelling of a notice, which is not the transcript's
    /// (`Context limit reached · …` for `Prompt is too long`).
    pub hook_words: bool,
}

/// THE LAST THING SAID, WHERE CLAUDE CODE DRAWS ITS OWN NOTICES, for a
/// reader that has a notice's words from elsewhere and asks whether the
/// screen shows it (aterm-agent's transcript of the session,
/// `supervise/wall_confirm.rs`): a `⏺` message in column 0 that is no call
/// — where 2.1.300 draws every `isApiErrorMessage` row its renderer has no
/// case of its own for (`su`), in the warning colour a row of text does not
/// keep, so the worker's prose stands there too — or a column-2 `⎿` block
/// that no call's FIRST output row opens ([`block_is_vendors`]), the row
/// right under a hook's error row included ([`SaidNotice::hook_words`]). `None` for
/// anything else: a call's output, a command's echo, the person's `❯` row,
/// a screen with nothing said. The table is not asked: what the screen
/// cannot name (`There's an issue with the selected model (x). …`, drawn
/// like prose) is named by the words the caller holds.
#[must_use]
pub fn last_said_notice(rows: &[String]) -> Option<SaidNotice> {
    let last = last_said_index(rows)?;
    let width = crate::question::screen_width(rows);
    let mut open = last;
    while !rows[open].starts_with(['⏺', '●']) && in_message(&rows[open]) {
        open = open.checked_sub(1)?;
    }
    if rows[open].starts_with(['⏺', '●']) {
        if is_tool_call(&rows[open]) {
            return None;
        }
        let end = message_end(rows, open, rows.len());
        let (_, line) = line_at(rows, open, rows.len(), width);
        let text = crate::text::trim(&line).trim_start_matches(['⏺', '●']);
        let text = crate::text::trim(text.strip_suffix(EXPAND_HINT).unwrap_or(text));
        let short = end - open < ERROR_ROWS
            || notice_rows_at_80(rows, open, end).is_some_and(|n| n <= ERROR_ROWS);
        let remedy = short && (text.contains(" · ") || text.contains(" ∙ "));
        return Some(SaidNotice {
            shaped: text.starts_with("API Error") || remedy,
            text: text.to_string(),
            row: open,
            placement: Placement::ErrorRow,
            hook_words: false,
        });
    }
    let open = gutter_open(rows, last)?;
    let hook_words = match block_is_vendors(rows, open) {
        Block::Output => return None,
        Block::HookWords => true,
        Block::UnderCall | Block::Vendors | Block::Unowned => false,
    };
    let (_, line) = line_at(rows, open, rows.len(), width);
    let text = crate::text::trim(crate::text::trim(&line).trim_start_matches('⎿'));
    if text.starts_with("$ ") {
        return None;
    }
    Some(SaidNotice {
        shaped: text.starts_with("API Error"),
        text: text.to_string(),
        row: open,
        placement: Placement::Gutter,
        hook_words,
    })
}

/// The column Claude Code draws its OWN `⎿` rows at (`  ⎿  …`). A tool's
/// output continues at [`GUTTER_TEXT_COLUMN`], under the gutter's text, so a
/// `⎿` the output itself prints lands at column 5 or deeper — behind a blank
/// row of the output, a file's carriage returns, tabs, cursor moves and
/// backspaces included (measured on 2.1.294:
/// `claude-2.1.294-bash-output-blank-row-then-gutter-phrase.txt`).
const VENDOR_GUTTER_COLUMN: usize = 2;

/// What the owner walk ([`block_is_vendors`]) makes of a `⎿` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// A tool's output: never a wall.
    Output,
    /// The row right under a hook's error row (rule 4): the hook's words,
    /// never a wall on the screen — or, where the hook drew none, Claude
    /// Code's own notice, which only the transcript tells apart
    /// ([`SaidNotice::hook_words`]).
    HookWords,
    /// Read as Claude Code's own row, from a later column-2 row under a call
    /// (rule 3): [`Wall::under_call`].
    UnderCall,
    /// Read as Claude Code's own row under any other owner, or none the
    /// walk reaches before a blank row over a row that opens an item.
    Vendors,
    /// Read as [`Block::Vendors`] is: the walk reached the first row with no
    /// owner found, so the owner, if any, is above the rows read
    /// ([`owner_above_rows`]).
    Unowned,
}

/// Whether the `⎿` block opening at `open` is READ as Claude Code's own row —
/// the SCREEN'S half of H6-HARNESS-1 (ruling 2026-10-08). It reads for
/// RECALL: a wall read here is acted on only where the session's own
/// transcript confirms it (aterm-agent, `supervise/wall_confirm.rs`), so
/// where the screen cannot tell Claude Code's row from a tool's, it reads
/// the wall. Four rules, measured in the real-Claude lab — the first three
/// on Claude Code 2.1.294, the fourth on 2.1.302:
///
/// 1. **The column.** A `⎿` row at any column but 2 is never Claude Code's:
///    its own rows are at column 2, and a tool's output continues at column
///    5 whatever the output prints.
/// 2. **The owner walk** (main's): up from `open`, over the rows of the
///    blocks above it, to the first row in column 0. A CALL there
///    ([`is_tool_call`]: `⏺ Bash(…)`, an MCP tool's `⏺ <server> - <title>
///    (MCP)…`, an integration's `⏺ Name[tool]…`, a shell-mode `! cmd`) owns
///    the block, and its FIRST column-2 row is the call's output — the first
///    line of what the file, the page, the server or the command said —
///    never a wall.
/// 3. **A later column-2 row under a call is read.** Claude Code draws its
///    wall under the call's output when the request after the tool fails
///    (`⏺ Bash(…)` / `⎿  stamped` / `⎿  Context limit reached · …`) — under
///    every call, an MCP tool's and one a hook's row opens included, where
///    the later row can also be the output's own (an MCP result's second
///    content block in verbose output; the output under a PreToolUse hook's
///    row): the screen cannot tell those apart, and the transcript decides
///    ([`Block::UnderCall`]: where no transcript can be read, a supervisor
///    does not act on a wall read here — main never read one under a `⏺
///    Name(…)` call, but did under an MCP call and a `! cmd`, which it did
///    not know as calls: a loss of recall ruled 2026-10-08). A row of
///    the CALL's own — its arguments wrapped onto a row under it (`⏺
///    Bash(…` / `  …)`) — is no column-2 `⎿` row, so the output's first row
///    under a wrapped call is still its first.
///
/// 4. **A hook's words under a row of Claude Code's own are output**
///    (ATERM-KI-1015-14, measured on 2.1.302 in the lab). Under the
///    STOP-HOOK SUMMARY (`⏺ Ran 1 stop hook (ctrl+o to expand)`,
///    [`is_stop_hook_summary`]) the column-2 `⎿` rows up to its first
///    labelled row of its own ([`is_summary_label`]) are the hooks' own: in
///    verbose output (`claude --verbose`, the detailed transcript) a row per
///    hook, its command's path or a prompt hook's text; a Stop hook's
///    `stopReason` (`{"continue": false, "stopReason": …}`: its later lines
///    at column 2 with no `⎿`). The 2.1.302 and 2.1.304 binaries draw those
///    rows, the `stopReason` only where the hooks prevented the
///    continuation, then a `Stop hook error: …` row per blocking error and a
///    `Stop hook feedback: …` row per context a hook added: a continuation,
///    and so a wall its request meets, comes only under one of those, and
///    that wall is read (measured: `⎿  Stop hook error: [./sb]: …` for
///    a stderr, `⎿  Stop hook error: check the tests` for a `{"decision":
///    "block"}`, then the wall, a blank row between or none). Any other
///    label is the hooks' words: a prompt hook's verbose row opening
///    `Review: …` ended the walk, and the `stopReason` under it was read as
///    the wall (2.1.304, measured, KI-1015-14). Under A HOOK'S
///    ERROR ROW (`⎿  <hook> hook error`, `⎿  <hook> hook returned blocking
///    error`, [`is_hook_error_row`]) the next column-2 `⎿` row is the hook's
///    words, the error row's own sub-lines in the binary: its stderr behind
///    `Failed with non-blocking status code: `, its blocking `reason`, or
///    Claude Code's own `Blocked by hook` for none — under any owner, a
///    call's included; a wall comes after it. Read as walls, `aterm drive
///    watch` typed `/login` on a `stopReason` and parked the session until
///    the reset a double-spaced one named (measured).
///    THE LIMIT: a hook's error row with no row of the hook's under it puts
///    Claude Code's wall where the hook's words stand. 2.1.302 draws one for
///    an empty stderr and a reasonless block (measured); a blocking reason
///    of blank space draws none (2.1.304, measured: `⎿  PostToolUse:Bash
///    hook returned blocking error`, then the real context wall), and a
///    reason that IS the wall's words draws the same screen. That row is
///    [`Block::HookWords`]: no wall on the screen, and the row a caller
///    holding the session's transcript asks about ([`last_said_notice`]),
///    which ends on the wall's notice in the one case and on the model's
///    reply in the other.
///
/// Any other owner is read as Claude Code's: the person's `❯` turn, the
/// worker's message, a task notice, a Monitor event, a hook summary past its
/// labelled rows. So is a block whose owner the walk does not reach — a
/// blank row over a row that opens an item (below), or the top of the
/// screen, comes first — which is where Claude Code's walls stand under a
/// prompt's second paragraph, under a collapsed tool group (its row wrapped
/// or not), and under a long output whose call has scrolled off (all
/// measured). A Monitor's event draws its payload on its own `⏺` row and no
/// `⎿` row of its own, so the first column-2 row under it is Claude Code's
/// wall (2.1.302, measured). A hook's row under these owners that carries a
/// program's words behind a label (`Stop says: …`, `Stop hook error: [./h]:
/// …`) is read here, and is no wall by the table ([`crate::wall`]'s
/// `opens_with_label`).
///
/// A BLANK ROW INSIDE A CALL'S ROWS does not end the walk (ruling
/// 2026-10-08, the review of pass 5). A blank row ends it only where the
/// next non-blank row below opens an item of its own: a row at column 2 or
/// less that is no `⎿` row (a prompt's second paragraph, a collapsed
/// group's row). Under any other the walk goes on over it to the owner:
/// * a row past column 2 — an output's own row at column 5, a command's at
///   column 6 — makes it a blank line of the output or the command. So an
///   MCP result whose first block holds a paragraph break reads its second
///   block under the call
///   (`claude-2.1.294-mcp-blank-row-in-first-block-then-login-phrase.txt`),
///   and a heredoc command's output, behind the command's own blank row,
///   is the first row under its call: no wall;
/// * a column-2 `⎿` row — a block hangs from the row above it, and no
///   measured screen draws Claude Code's own `⎿` row behind a blank row
///   under a call's rows (after a `/compact` it draws one under its own
///   `✻ Conversation compacted` row, which no call owns) —
///   makes it a blank line that ENDS a block of the output: an MCP result
///   whose first block ends on a blank line draws two blank rows right
///   above its second block
///   (`claude-2.1.294-mcp-trailing-blank-rows-in-first-block-then-login-phrase.txt`,
///   measured in pass 6: read under no owner, a supervisor with no
///   transcript typed `/login` on it).
///
/// The price: Claude Code's real wall under an output that holds a blank
/// row is read under the call too ([`Block::UnderCall`]), which a
/// supervisor with no transcript does not act on.
fn block_is_vendors(rows: &[String], open: usize) -> Block {
    if leading_spaces(&rows[open]) != VENDOR_GUTTER_COLUMN {
        return Block::Output;
    }
    let mut later = false;
    // A column-2 `⎿` row between the block and its owner that opens with a
    // label (rule 4: the stop-hook summary's own rows end there).
    let mut labelled = false;
    // The walk has met no blank row and no column-2 `⎿` row yet: the next
    // such `⎿` row is the one the block hangs right under (rule 4).
    let mut adjacent = true;
    // The nearest non-blank row below the one the walk stands on.
    let mut below = &rows[open];
    for row in rows[..open].iter().rev() {
        if crate::text::trim(row).is_empty() {
            adjacent = false;
            let opens_an_item = leading_spaces(below) <= VENDOR_GUTTER_COLUMN
                && !crate::text::trim_start(below).starts_with('⎿');
            if opens_an_item {
                return Block::Vendors;
            }
            continue;
        }
        below = row;
        let lead = leading_spaces(row);
        if lead == 0 {
            if !labelled && is_stop_hook_summary(row) {
                return Block::Output;
            }
            return match (is_tool_call(row), later) {
                (false, _) => Block::Vendors,
                (true, true) => Block::UnderCall,
                (true, false) => Block::Output,
            };
        }
        if lead == VENDOR_GUTTER_COLUMN
            && let Some(text) = crate::text::trim_start(row).strip_prefix('⎿')
        {
            let text = crate::text::trim(text);
            if adjacent && is_hook_error_row(text) {
                return Block::HookWords;
            }
            adjacent = false;
            later = true;
            labelled |= is_summary_label(text);
        }
    }
    Block::Unowned
}

/// Whether the owner walk ([`block_is_vendors`]) from the `⎿` block row `i`
/// stands in reaches THE FIRST OF `rows` with no owner found — no row in
/// column 0, no blank row over a row that opens an item — so whatever owns
/// the block, if anything does, is above the rows: a call row scrolled off
/// the screen over its long output (2.1.304, measured: the history right
/// above the screen holds it, `claude-2.1.304-verbose-long-output-context-wall-scrollback.txt`).
/// A caller that can read the rows above (aterm-agent's wall confirmation,
/// with no transcript) reads them up to the owner and reads the wall again
/// with them. `false` for a row under no `⎿` block.
#[must_use]
pub fn owner_above_rows(rows: &[String], i: usize) -> bool {
    gutter_open(rows, i).is_some_and(|open| block_is_vendors(rows, open) == Block::Unowned)
}

/// Whether `row` is Claude Code's STOP-HOOK SUMMARY, an owner whose first
/// column-2 rows are the hooks' own ([`block_is_vendors`], rule 4): `⏺ Ran 1
/// stop hook (ctrl+o to expand)`, `⏺ Ran 2 stop hooks` (`●` off macOS; the
/// anchors `hook.ran` and `hook.ran.stop`, the count between them).
fn is_stop_hook_summary(row: &str) -> bool {
    let Some(rest) = row
        .strip_prefix(['⏺', '●'])
        .map(crate::text::trim_start)
        .and_then(|r| r.strip_prefix(crate::anchors::anchor("hook.ran")))
    else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && rest[digits..]
            .strip_prefix(' ')
            .is_some_and(|r| r.starts_with(crate::anchors::anchor("hook.ran.stop")))
}

/// Whether `text`, a column-2 `⎿` row's past its `⎿`, is one of the
/// STOP-HOOK SUMMARY'S OWN LABELLED ROWS, the first of which ends the hooks'
/// words under it ([`block_is_vendors`], rule 4): `<event> hook error: …`
/// per blocking error (the event one word, `Stop` when the hook names none:
/// the anchor `hook.error.label`) and `Stop hook feedback: …` per context a
/// hook added (`hook.feedback`) — the 2.1.304 bundle's two, after the
/// verbose rows and the `stopReason`. Any other label is a hook's own words:
/// a prompt hook's text opening `Review: …` on its verbose row (measured on
/// 2.1.304), or the bundle's own `prompt: …` form of that row.
fn is_summary_label(text: &str) -> bool {
    text.split_once(crate::anchors::anchor("hook.error.label"))
        .is_some_and(|(name, _)| !name.is_empty() && !name.contains(char::is_whitespace))
        || text.starts_with(crate::anchors::anchor("hook.feedback"))
}

/// Whether `text`, a column-2 `⎿` row's past its `⎿`, is the row A HOOK'S
/// ERROR opens on, whose next column-2 row is the hook's words
/// ([`block_is_vendors`], rule 4): `<hook> hook error` or `<hook> hook
/// returned blocking error` (the anchors `hook.error` and
/// `hook.blocking_error`), the hook's name one word (`UserPromptSubmit`,
/// `PostToolUse:Bash`).
fn is_hook_error_row(text: &str) -> bool {
    ["hook.error", "hook.blocking_error"].iter().any(|id| {
        text.strip_suffix(crate::anchors::anchor(id))
            .is_some_and(|name| !name.is_empty() && !name.contains(char::is_whitespace))
    })
}

/// Whether row `i` carries A PROGRAM'S WORDS, never a notice of Claude Code's
/// own (ATERM-KI-1015-14): its text opens with a label ([`crate::wall`]'s
/// `labelled`: a Monitor's event, a hook's `… says:` or `… hook error:` row),
/// or it stands under a `⎿` row the owner walk reads as output
/// ([`block_is_vendors`]: a call's first row, a hook's words under the
/// stop-hook summary or under its error row, a `⎿` at a column but 2) — that
/// row or one of its own rows under it, a wrapped line or a later line of the
/// same words ([`owning_gutter`]). The readers that walk every row above the
/// composer, splitting a row at wide gaps (`wall::since_limit`,
/// `wall::auto_continue_end`, the turn's own notice behind a generic banner),
/// pass these rows over, as the gutter's reader does: a hook's `stopReason`
/// of `note  Usage limit reached · resets 3pm` (the double space kept,
/// measured on 2.1.302) is no limit's notice. So is a row Ink wrapped a
/// labelled `⏺` message onto ([`message_open`]): a hook's long `stopReason`
/// behind `⏺ Operation stopped by hook: …` wraps at column 2, and its row
/// `  Usage limit reached · resets 3pm` named a session limit
/// (`since_limit`; 2.1.304, measured, KI-1015-14).
pub(crate) fn programs_words(rows: &[String], i: usize) -> bool {
    words_of(rows, i) == WordsOf::Program
}

/// WHOSE WORDS row `i` holds, as [`programs_words`] reads them, with the
/// case it cannot decide from `rows` kept apart ([`WordsOf::Above`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordsOf {
    /// A program's words ([`programs_words`]): no notice of Claude Code's.
    Program,
    /// Claude Code's own row, or read as its own: the walk found its owner,
    /// or a row that opens an item, within `rows`.
    Vendor,
    /// Read as [`WordsOf::Vendor`] is, for want of the rows above: the walk
    /// from row `i` up to its `⎿` row, its message's `⏺` row or the block's
    /// owner reached THE FIRST OF `rows` undecided. A Stop hook's
    /// `stopReason` of 35 lines drawn its summary on row 6 of 50 and its
    /// last line, `Usage limit reached · resets 3pm`, on row 41 (2.1.304,
    /// measured): a 40-row tail of that screen read a session limit, the
    /// whole screen none; one of 60 lines draws its summary in the history
    /// right above the screen. Whoever can read the rows above reads row
    /// `i` again with them (aterm-agent's screen read).
    Above,
}

/// [`WordsOf`] row `i` of `rows`.
#[must_use]
pub fn words_of(rows: &[String], i: usize) -> WordsOf {
    if crate::wall::labelled(&rows[i]) {
        return WordsOf::Program;
    }
    let mut above = false;
    match message_open(rows, i) {
        Walk::Found(open) if crate::wall::labelled(&rows[open]) => return WordsOf::Program,
        Walk::Top => above = true,
        Walk::Found(_) | Walk::None => {}
    }
    match owning_gutter(rows, i) {
        Walk::Found(open) => match block_is_vendors(rows, open) {
            Block::Output | Block::HookWords => return WordsOf::Program,
            Block::Unowned => above = true,
            Block::UnderCall | Block::Vendors => {}
        },
        Walk::Top => above = true,
        Walk::None => {}
    }
    if above {
        WordsOf::Above
    } else {
        WordsOf::Vendor
    }
}

/// Where a walk up from a row ended ([`message_open`], [`owning_gutter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    /// At the row it looks for.
    Found(usize),
    /// At a row that says the row walked from has none.
    None,
    /// At the first of the rows, still looking: the row may be above them.
    Top,
}

/// The `⏺` or `●` row whose message row `i` is a later row of: up from `i`
/// over the message's own rows ([`in_message`]) to that bullet row.
/// [`Walk::None`] for a row that is no such message's later row;
/// [`Walk::Top`] where the message's rows run up to the first of `rows`.
fn message_open(rows: &[String], i: usize) -> Walk {
    let mut j = i;
    while in_message(&rows[j]) {
        let Some(up) = j.checked_sub(1) else {
            return Walk::Top;
        };
        j = up;
    }
    if j < i && rows[j].starts_with(['⏺', '●']) {
        Walk::Found(j)
    } else {
        Walk::None
    }
}

/// The `⎿` row that row `i` stands under: itself, or the nearest above it
/// with no blank row and no row in column 0 between — a `⎿` row's own rows,
/// an output's at column 5 and a `stopReason`'s later lines at column 2
/// (2.1.302, measured). Wider than [`gutter_open`], which leaves a row short
/// of the gutter's text column to the item it opens: a row this finds is
/// judged by its `⎿` row's owner ([`programs_words`]). [`Walk::Top`] where
/// those rows run up to the first of `rows` with no `⎿` row among them.
fn owning_gutter(rows: &[String], mut i: usize) -> Walk {
    loop {
        let row = rows[i].as_str();
        if crate::text::trim_start(row).starts_with('⎿') {
            return Walk::Found(i);
        }
        if crate::text::trim(row).is_empty() || leading_spaces(row) == 0 {
            return Walk::None;
        }
        let Some(up) = i.checked_sub(1) else {
            return Walk::Top;
        };
        i = up;
    }
}

/// A transcript row that opens a CALL — a tool's or a command's, whose `⎿`
/// rows are what it printed, never anybody's words. Claude Code's forms
/// (2.1.294, measured in the real-Claude lab and read from its binary):
/// * `⏺ <Name>(…)`: a name of letters, digits, `_`, `:`, `.` or `-` (`⏺
///   Bash(ls -la)`, `⏺ Update(notes.txt)`, `⏺ mcp__srv__tool(…)`), or one to
///   three capitalised words (`⏺ Web Search(…)`), running straight into `(`;
/// * an MCP tool by its `userFacingName`, `<server> - <title> (MCP)`, then
///   its arguments' `(…)` or, with none, nothing (`⏺ github - get_issue
///   (MCP)(owner: "acme")`, `⏺ github - ping (MCP)`); the server and the title
///   may hold spaces (`⏺ ticket desk - …`, `⏺ claude.ai Gmail - …`), and a
///   tool its server no longer lists is drawn `⏺ mcp__srv__tool (MCP)`;
/// * an integration's tool, `<Name>[<tool>]` and its arguments or none (`⏺
///   Claude in Chrome[get_page_text](tabId: 1)`);
/// * a shell-mode command and its output: `! <command>` in column 0 (`❯
///   !<command>` as an older echo drew it).
///
/// A completion notice (`⏺ Dynamic workflow "…" completed`, `⏺ Background
/// command "…" completed`) and the worker's prose are not. The ONE reader of
/// "a call" for every reader that needs it: this crate's wall placement
/// (`notice`) and the worker's last words ([`crate::turn::said_tail`]), and
/// aterm-agent's report blocks.
pub fn is_tool_call(row: &str) -> bool {
    if let Some(command) = row.strip_prefix('!').or_else(|| {
        row.strip_prefix('❯')
            .map(crate::text::trim_start)
            .and_then(|r| r.strip_prefix('!'))
    }) {
        return !crate::text::trim(command).is_empty();
    }
    let Some(rest) = row.strip_prefix(['⏺', '●']) else {
        return false;
    };
    let rest = crate::text::trim_start(rest);
    if let Some((name, args)) = rest.split_once(" (MCP)")
        && (name.contains(" - ") || name.starts_with("mcp__"))
        && (crate::text::trim_end(args).is_empty() || args.starts_with('('))
    {
        return true;
    }
    bracketed_call(rest)
        || rest
            .find('(')
            .is_some_and(|open| is_call_name(&rest[..open]))
}

/// `<Name>[<tool>]`, then the arguments' `(…)` or nothing: one to four words,
/// the first capitalised, each of letters, digits, `_` or `-`, then the
/// bracketed tool, a name with no space in it (`Claude in Chrome[read_page]`,
/// its `userFacingName` in 2.1.294).
fn bracketed_call(rest: &str) -> bool {
    let Some((name, tail)) = rest.split_once('[') else {
        return false;
    };
    let Some((tool, args)) = tail.split_once(']') else {
        return false;
    };
    let words: Vec<&str> = name.split(' ').collect();
    words.len() <= 4
        && words
            .first()
            .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_uppercase()))
        && words.iter().all(|w| {
            !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
        && !tool.is_empty()
        && tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        && (crate::text::trim_end(args).is_empty() || args.starts_with('('))
}

/// The name of a `Name(…)` call: one run of letters, digits, `_`, `:`, `.`
/// or `-`, or one to three words each a capital letter then letters, digits,
/// `_` or `-` (`Web Search`, `Edit Notebook`), forty characters at most.
fn is_call_name(name: &str) -> bool {
    let one = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'));
    let words: Vec<&str> = name.split(' ').collect();
    let capitalised = name.chars().count() <= 40
        && words.len() <= 3
        && words.iter().all(|w| {
            w.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        });
    one || capitalised
}

/// A notice in the footer, under the bottom rule (from row `from`) and above
/// the first blank row (the agents panel and a workflow's line below it are
/// the worker's words): an item — the text read, a part of it after a wide
/// gap or after ` · ` — that opens with a notice `classify` names, up to the
/// next wide gap. Only where Claude Code draws ITS notices is read
/// ([`footer_text`]): a banner row that opens with a status glyph, read with
/// the rows Ink wrapped its line onto ([`line_at`]: in a pane narrower than
/// the usage-limit banner, `⚠ Usage limit reached · limit` stands over
/// `resets 10:25am`, 34 columns, 2.1.299, and the row alone names no
/// reset); and the notification column, right-aligned or stacked under the
/// mode row.
/// The rest of the footer's left side is the `statusLine` command's text —
/// the person's script, whatever it prints (KI-1015-14, measured on 2.1.304:
/// a status line printing `Not logged in · Run /login` read as the vendor's
/// login notice, and with no transcript `/login` was typed into a logged-in
/// worker) — and the mode row's.
fn footer_notice(
    rows: &[String],
    from: usize,
    width: usize,
    classify: &dyn Fn(&str) -> Option<WallKind>,
) -> Option<Wall> {
    let until = footer_end(rows, from);
    (from..until)
        .filter(|&i| !crate::text::trim_start(&rows[i]).starts_with(['◯', '⏺', '●']))
        .find_map(|i| {
            let line = footer_text(rows, from, i, until, width)?;
            let t = crate::text::trim(&line);
            let starts = std::iter::once(0)
                .chain(t.match_indices("  ").map(|(i, s)| i + s.len()))
                .chain(t.match_indices(" · ").map(|(i, s)| i + s.len()));
            starts
                .map(|at| crate::text::trim_start(&t[at..]))
                .find_map(|item| classify(item).map(|kind| (item, kind)))
                .map(|(item, kind)| {
                    let item = item.split("  ").next().unwrap_or(item).trim();
                    Wall {
                        kind,
                        message: item.to_string(),
                        reset: reset_of(item),
                        row: i,
                        placement: Placement::Footer,
                        under_call: false,
                    }
                })
        })
}

/// What of footer row `i` Claude Code itself drew a notice in, measured on
/// 2.1.304 (`claude-2.1.304-footer-*`, `…-statusline-login-words-*`,
/// `…-wall-session-auto-continue-*`) and on 2.1.299 and 2.1.300 at 26, 34,
/// 50 and 80 columns: the footer is two columns. On the left the vendor's
/// banner (`⚠ Usage limit reached · …`, a status glyph first, its countdown
/// under it), then the `statusLine` command's rows, then the mode row
/// (`⏵⏵ auto mode on …`, `⏸ manual mode on · ? for shortcuts`, [`is_mode_row`]);
/// on the right, right-aligned (the binary's `flex-end`), the vendor's
/// notifications (`Not logged in · Run /login`, `Automatic continue
/// cancelled · …`, `◐ medium · /effort`), level with the left column's first
/// row — and in a pane too narrow for both, stacked under the mode row, where
/// one that fills its row starts at column 2 (`Not logged in · Run /…` at 26
/// columns, `You're now using usage credits · …` at 80). Read: a banner row's
/// line ([`line_at`]); a row under the mode row, whole; else the row's right
/// segment ([`crate::wall::right_segment`]) where it ends against the right
/// edge (within three columns, as every measured notification does: two
/// short of 144, of 80) and starts at the hint column ([`HINT_COLUMN`]) or
/// the row stands there whole ([`is_against_right_edge`]); else nothing. A
/// status line's later field, after two spaces, is no notification short of
/// the edge: `  opus-5.5  ~/proj  Not logged in · Run /login` read as the
/// login notice before (the review of 87faa9bf23). A status line that itself
/// prints a status glyph first, or whose words reach the right edge, reads
/// as the vendor's.
fn footer_text(
    rows: &[String],
    from: usize,
    i: usize,
    until: usize,
    width: usize,
) -> Option<String> {
    let row = crate::text::trim_end(&rows[i]);
    if crate::text::trim_start(row).starts_with(STATUS_GLYPHS) {
        return Some(line_at(rows, i, until, width).1);
    }
    if (from..i).any(|m| is_mode_row(&rows[m])) {
        return Some(crate::text::trim(row).to_string());
    }
    let (column, segment) = crate::wall::right_segment(row);
    let at_edge = row.chars().count() + 3 >= width;
    (at_edge && column >= HINT_COLUMN || is_against_right_edge(row, width))
        .then(|| segment.to_string())
}

/// The footer's mode row, the left column's last: the permission mode
/// (`⏵⏵ auto mode on (shift+tab to cycle)`, `⏵⏵ bypass permissions on`,
/// `⏸ plan mode on`, `⏸ manual mode on · ? for shortcuts`) or, with none
/// drawn, `? for shortcuts` — cut to the pane, never wrapped (measured to 10
/// columns, 2.1.299 to 2.1.304).
fn is_mode_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    t.starts_with(['⏵', '⏸']) || t.starts_with("? for shortcuts")
}

/// One past the footer's last row: the first blank row from row `from` (the
/// row under the bottom rule) on, or the screen's end.
pub(crate) fn footer_end(rows: &[String], from: usize) -> usize {
    (from..rows.len())
        .find(|&i| crate::text::trim(&rows[i]).is_empty())
        .unwrap_or(rows.len())
}

/// A notice on a banner row under the last thing said: the rows after
/// [`last_said_index`]'s (from the top of the screen when nothing is said)
/// up to the composer's top rule — the status row's own position included,
/// so a notice above a done row with nothing said between counts as one
/// below it — the lowest row that starts with a status glyph
/// ([`is_status_row`]: `⚠`, `✔`, …, never a spinner's) and opens with a
/// notice `classify` names. A banner starts where Claude Code's own rows do —
/// in the transcript's columns, short of the gutter's text column, or
/// against the right edge as a hint ([`is_hint`]; the update banner is drawn
/// there) — and never on a row that hangs from a `⎿` gutter
/// ([`gutter_open`]): a tool's output is the worker's words at whatever
/// column it puts them, a peer's screen the worker read included,
/// right-aligned as the peer's Claude Code drew it (the round-20 review:
/// such a copy twenty columns in read as the wall). A blank row ends the
/// gutter's block; a banner under that blank is Claude Code's own. A notice
/// above a said row is history and is not looked at. The banner's text is its
/// line, read across the rows Ink wrapped it onto ([`line_at`]). Where a
/// wrapped banner's rest stands on this screen is not measured (2.1.295 and
/// later draw the usage banner in the footer, [`footer_notice`]): a rest
/// [`last_said_index`] takes for the last thing said hides the banner above
/// it.
fn banner_notice(
    rows: &[String],
    top: usize,
    width: usize,
    classify: &dyn Fn(&str) -> Option<WallKind>,
) -> Option<Wall> {
    let from = last_said_index(rows).map_or(0, |i| i + 1);
    (from..top).rev().find_map(|i| {
        let row = &rows[i];
        let t = crate::text::trim(row);
        let placed = (leading_spaces(row) < GUTTER_TEXT_COLUMN || is_hint(row, Some(width)))
            && gutter_open(rows, i).is_none();
        if !(placed && is_status_row(t) && !is_glyph_row(t)) {
            return None;
        }
        // Its line, across the rows Ink wrapped it onto ([`line_at`]).
        let (_, line) = line_at(rows, i, top, width);
        let t = crate::text::trim(&line);
        classify(t).map(|kind| Wall {
            kind,
            message: t.to_string(),
            reset: reset_of(t),
            row: i,
            placement: Placement::Banner,
            under_call: false,
        })
    })
}

/// The column the `⎿` gutter's text starts at (`  ⎿  queued 8 runs`): a
/// block's continuation rows are indented to it or further.
const GUTTER_TEXT_COLUMN: usize = 5;

/// Whether row `i` stands in a `⎿` block a CALL owns ([`block_is_vendors`]:
/// the call's output, or a later column-2 row under it, which only the
/// transcript tells from the output) or one at a tool's column: words a tool
/// printed, as far as the screen can say. A row under no gutter is not.
pub(crate) fn under_a_call(rows: &[String], i: usize) -> bool {
    gutter_open(rows, i).is_some_and(|open| {
        !matches!(
            block_is_vendors(rows, open),
            Block::Vendors | Block::Unowned
        )
    })
}

/// The `⎿` row that opens the gutter block row `i` belongs to: `i` itself, or
/// the row its continuation rows (indented to the gutter's text,
/// [`GUTTER_TEXT_COLUMN`] or more) hang from. `None` when `i` is not under
/// the gutter.
pub(crate) fn gutter_open(rows: &[String], mut i: usize) -> Option<usize> {
    loop {
        let t = crate::text::trim_start(&rows[i]);
        if t.starts_with('⎿') {
            return Some(i);
        }
        if t.is_empty() || leading_spaces(&rows[i]) < GUTTER_TEXT_COLUMN {
            return None;
        }
        i = i.checked_sub(1)?;
    }
}

/// The reset a limit notice's text names — what [`limit_notice`] hands over
/// as its reset — for a caller that holds the notice's text from elsewhere
/// (the session transcript Claude Code writes the notice into).
#[must_use]
pub fn notice_reset(message: &str) -> Option<String> {
    reset_of(message)
}

/// When the notice says the limit resets: the text after `resets at ` /
/// `reset at ` / `resets `, up to a ` · ` or ` ∙ ` separator, without a closing
/// `.`. The auto-continue notice names no reset but says when Claude Code
/// goes on by itself, which is the same clock: the text after `continuing
/// automatically at ` (`1:50pm`), or `shortly` for `continuing shortly` —
/// the word as printed, so the `reset=` a line carries is what the screen
/// said (`aterm-agent`'s `limit::parse_reset` reads both).
fn reset_of(message: &str) -> Option<String> {
    // Every key, in order of preference, at every place it occurs: the first
    // that is followed by a TIME names the reset. A key followed by no time
    // (`send a message after it resets to continue`) is passed over, so the
    // countdown toast Claude Code 2.1.284 appends after such a row
    // (`continuing automatically at 3pm`) still names its time.
    for key in [
        "resets at ",
        "reset at ",
        "resets ",
        "continuing automatically at ",
    ] {
        let mut from = 0;
        while let Some(i) = find_ascii_ci(&message[from..], key) {
            let at = from + i + key.len();
            if let Some(time) = time_after(&message[at..]) {
                return Some(time);
            }
            from = at;
        }
    }
    // 2.1.283: `continuing shortly`; 2.1.284: `continuing automatically
    // shortly` (its `{when}` is `shortly` when the reset is not ahead).
    ["continuing shortly", "continuing automatically shortly"]
        .iter()
        .find_map(|k| find_ascii_ci(message, k))
        .map(|_| "shortly".to_string())
}

/// The reset text at the start of `rest`, up to a ` · ` or ` ∙ ` separator
/// and no further than its time ([`time_words`]), without a closing `.`;
/// `None` when it is no time: a reset text opens with a digit, a month, a
/// span or `shortly`.
fn time_after(rest: &str) -> Option<String> {
    let cut = [" · ", " ∙ "]
        .iter()
        .filter_map(|sep| rest.find(sep))
        .min()
        .unwrap_or(rest.len());
    let rest = time_words(crate::text::trim(&rest[..cut]))
        .trim_end_matches('.')
        .trim();
    let lower = rest.to_ascii_lowercase();
    let timey = rest.starts_with(|c: char| c.is_ascii_digit())
        || lower.starts_with("in ")
        || lower.starts_with("shortly")
        || [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ]
        .iter()
        .any(|m| lower.starts_with(m));
    (!rest.is_empty() && timey).then(|| rest.to_string())
}

/// The words `text` opens with that are a time — a clock (`10:25am`, `11:05
/// am`), a date (`Oct 15 at`, `September 19,`, `2027`), a span (`in 2h 30m`,
/// `in 3 hours`), `shortly` — and the zone in parentheses after them
/// (`(America/Los_Angeles)`, through its `)`): the reset a notice names ends
/// there, whatever follows it. A line read across the rows Ink wrapped it
/// onto can run on into the next line of the notice (`crate::wrap`, THE
/// LIMITS: `… resets 10:25am (America/Los_Angeles) /upgrade to increase your
/// usage limit.` at 34 columns), and the transcript's own text has that next
/// line after a newline. A word that only joins two others (`at`, `on`,
/// `in`, `of`, `the`, `and`) is the time's only with one after it. A date
/// or a day after the clock is the time's too (`3pm on Oct 15`, `9am
/// tomorrow`, `9am Monday`): cut there, the clock alone would read as
/// today's or tomorrow's (`aterm-agent`'s `limit::parse_reset`, which reads
/// no such spelling whole, and the reset falls to the backstop as before).
fn time_words(text: &str) -> &str {
    let (mut end, mut at) = (0, 0);
    while let Some(skip) = text[at..].find(|c: char| !c.is_whitespace()) {
        at += skip;
        let len = text[at..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - at);
        let word = &text[at..at + len];
        if word.starts_with('(') {
            return text[at..]
                .find(')')
                .map_or(text, |close| &text[..at + close + 1]);
        }
        let joins = matches!(
            word.to_ascii_lowercase().as_str(),
            "at" | "on" | "in" | "of" | "the" | "and"
        );
        if !joins && !is_time_word(word) {
            break;
        }
        at += len;
        if !joins {
            end = at;
        }
    }
    &text[..end]
}

/// A word of a reset's time ([`time_words`]) that is a time's own: one with a
/// digit in it, or a month, a meridian, a span's unit or `shortly` — the
/// words `aterm-agent`'s `limit::parse_reset` reads — or a day (`today`,
/// `tomorrow`, a weekday, `noon`, `midnight`), which it does not, a closing
/// `,` or `.` aside.
fn is_time_word(word: &str) -> bool {
    const WORDS: &[&str] = &[
        "shortly",
        "am",
        "pm",
        "a.m",
        "p.m",
        "jan",
        "january",
        "feb",
        "february",
        "mar",
        "march",
        "apr",
        "april",
        "may",
        "jun",
        "june",
        "jul",
        "july",
        "aug",
        "august",
        "sep",
        "sept",
        "september",
        "oct",
        "october",
        "nov",
        "november",
        "dec",
        "december",
        "h",
        "hr",
        "hrs",
        "hour",
        "hours",
        "m",
        "min",
        "mins",
        "minute",
        "minutes",
        "s",
        "sec",
        "secs",
        "second",
        "seconds",
        "today",
        "tonight",
        "tomorrow",
        "noon",
        "midnight",
        "mon",
        "monday",
        "tue",
        "tues",
        "tuesday",
        "wed",
        "wednesday",
        "thu",
        "thur",
        "thurs",
        "thursday",
        "fri",
        "friday",
        "sat",
        "saturday",
        "sun",
        "sunday",
    ];
    let w = word.trim_end_matches([',', '.']).to_ascii_lowercase();
    w.bytes().any(|b| b.is_ascii_digit()) || WORDS.contains(&w.as_str())
}

/// The byte offset of `needle` (ASCII) in `hay`, ignoring ASCII case.
fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    let n = needle.len();
    hay.char_indices().map(|(i, _)| i).find(|&i| {
        hay.as_bytes()
            .get(i..i + n)
            .is_some_and(|b| b.eq_ignore_ascii_case(needle.as_bytes()))
    })
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::fixtures::{
        CONTEXT_WALL_COMPACT_MENU_MEASURED, SLASH_MENU_POINTER_DOWN_MEASURED, bash_one_row,
        composer, rows,
    };

    fn screen(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
    }

    /// The incident of 2026-09-25 (F1): three live shapes of Claude Code
    /// 2.1.282's question dialog — the preview form (S4-01), the Submit
    /// (review) tab (S6-05) and a tab whose chat row has the focus — read
    /// AUTHORITATIVE IDLE on 0.93.0 and on main: no box, and the session
    /// stalled with nothing answered or escalated. Each is a prompt now, and
    /// so is a dialog withheld behind a draft in the composer.
    #[test]
    fn the_incident_screens_no_longer_read_idle() {
        use crate::prompt::fixtures::{
            QUESTION_CHAT_FOCUSED, QUESTION_DEFERRED, QUESTION_PREVIEW, QUESTION_REVIEW, screen,
        };
        for text in [
            QUESTION_PREVIEW,
            QUESTION_REVIEW,
            QUESTION_CHAT_FOCUSED,
            QUESTION_DEFERRED,
        ] {
            let r = screen(text);
            assert_eq!(worker_phase(&r), Phase::Prompt, "{r:#?}");
            let reading = crate::reader::read(Some("claude"), &r, None);
            assert_eq!(reading.phase, Phase::Prompt);
            assert!(reading.phase_authoritative);
            assert_eq!(
                reading.prompt.map(|p| p.kind),
                Some(crate::prompt::PromptKind::Question)
            );
        }
    }

    /// A DEAD process's last frame above a new one (live, 2026-09-27, Claude
    /// Code 2.1.283 on main 9d1ce4862): `/exit` (or Ctrl-C twice), then a
    /// fresh `claude` at the returned prompt with no `clear`. The walk up
    /// from the new composer's top rule crossed the new launch card, the
    /// shell's lines and the dead frame, and took the dead process's status
    /// row for the live one: its `/exit` spinner read `busy` (`status row:
    /// spinner`) for as long as the new session stayed idle, and a dead
    /// done row under a question read `question` — the dead session's words
    /// — for a session nobody had asked anything. The live zone ends at the
    /// launch card: everything above it was drawn by another process.
    /// ONLY the launch card's own row ends the live zone, never a row that
    /// merely NAMES the banner text: a person's
    /// queued `❯` message and the worker's todo list sit between a live status
    /// row and its frame, and either can say "Claude Code v2.1.284". Stopping
    /// there hid the spinner: a running turn read Idle (or Question) wherever
    /// the footer does not carry `esc to interrupt` itself. NEGATIVE CONTROL:
    /// the same screens without the version read busy both ways.
    #[test]
    fn a_row_that_names_the_banner_is_not_the_launch_card() {
        let frame = |body: &[&str], footer: &str| -> Vec<String> {
            let mut r: Vec<String> = body.iter().map(|s| (*s).to_string()).collect();
            r.push("─".repeat(80));
            r.push("❯ ".to_string());
            r.push("─".repeat(80));
            r.push(footer.to_string());
            r
        };
        let quiet = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents";
        let spin = "✻ Synthesizing… (18s · ↓ 1.1k tokens)";
        for (name, version, plain) in [
            (
                "a todo",
                "     ☐ Check the footer on Claude Code v2.1.284",
                "     ☐ Check the footer notes",
            ),
            (
                "a queued message",
                "❯ then note it in the Claude Code v2.1.284 log",
                "❯ then note it in the log",
            ),
        ] {
            for (label, row) in [("names the banner", version), ("control", plain)] {
                let r = frame(
                    &["⏺ Running the tests.", "", spin, "  ⎿  ☒ Read", row, ""],
                    quiet,
                );
                assert_eq!(worker_phase(&r), Phase::Busy, "{name} ({label})");
                assert_eq!(
                    status_row(&r).map(str::trim),
                    Some(spin),
                    "{name} ({label})"
                );
            }
        }
        // A workflow wait with a queued question naming the version: still
        // the wait, never the question.
        let r = frame(
            &[
                "⏺ Started the workflow.",
                "",
                "✻ Waiting for 1 dynamic workflow to finish",
                "",
                "❯ what changed in Claude Code v2.1.284?",
                "",
            ],
            quiet,
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
    }

    #[test]
    fn a_dead_frame_above_the_launch_card_is_not_the_live_zone() {
        use crate::prompt::fixtures::{
            CTRLC_RELAUNCH_STALE_QUESTION, EXIT_RELAUNCH_STALE_FOOTER, screen,
        };
        for (name, text) in [
            ("exit", EXIT_RELAUNCH_STALE_FOOTER),
            ("ctrl-c", CTRLC_RELAUNCH_STALE_QUESTION),
        ] {
            let r = screen(text);
            let card = r
                .iter()
                .rposition(|row| row.contains(crate::anchor("launch.banner")))
                .expect("the new launch card");
            assert_eq!(busy_signal(&r), None, "{name}");
            assert_eq!(status_row(&r), None, "{name}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
            assert!(crate::turn::fresh(&r), "{name}");
            assert!(!crate::turn::interrupted(&r), "{name}");
            assert!(transcript_end(&r) > card, "{name}");
            assert!(
                last_said_index(&r).is_some_and(|i| i > card),
                "{name}: the dead session's words are not the last thing said"
            );
            let reading = crate::reader::read(Some("claude"), &r, Some(2));
            assert_eq!(reading.phase, Phase::Idle, "{name}");
            assert!(reading.phase_authoritative, "{name}");
            assert!(reading.composer.is_some(), "{name}");
        }
    }

    /// Another composer's rule ends the live zone too, card or no card: a
    /// dead frame's bottom rule sits under everything that frame owned, so
    /// nothing above it is the live frame's status row.
    #[test]
    fn a_rule_above_the_frame_ends_the_live_zone() {
        let mut r = rows(&["⏺ Working on it.", "", "✢ Deliberating… (12s)", ""]);
        r.extend(composer("  ⏵⏵ auto mode on · esc to interrupt"));
        r.extend(rows(&["user@host proj % claude", ""]));
        r.extend(composer("  ? for shortcuts"));
        assert_eq!(busy_signal(&r), None, "{r:#?}");
        assert_eq!(worker_phase(&r), Phase::Idle);
        // Negative control: the live frame's own spinner is still read.
        let mut live = rows(&["user@host proj % claude", "", "✢ Deliberating… (12s)", ""]);
        live.extend(composer("  ⏵⏵ auto mode on · esc to interrupt"));
        assert_eq!(worker_phase(&live), Phase::Busy);
    }

    /// The rows of `r` the busy guard (`esc to interrupt`) matches.
    fn busy_rows(r: &[String]) -> Vec<usize> {
        let busy = crate::anchor("busy.interrupt");
        (0..r.len()).filter(|&i| r[i].contains(busy)).collect()
    }

    /// WHERE A WAIT ON THE BUSY ROW IS ARMED (live, 2026-09-27, Claude Code
    /// 2.1.283, `/exit` and a fresh `claude` with no `clear`): the dead
    /// process's `esc to interrupt` footer stands above the new launch card
    /// in every capture of that pane, and the hosted loop's `await gone
    /// esc.to.interrupt` over the whole screen never latched on it — each
    /// wait ran out its 20 s step, a question dialog's tabs were keyed ~20 s
    /// apart. The live zone starts under it, and keeps the live footer: two
    /// seconds into the new session's first turn (`…_BUSY`) exactly one busy
    /// row is in the zone, the running turn's own; on the idle session, the
    /// question dialogs and the Ctrl-C relaunch, none.
    #[test]
    fn the_live_zone_starts_under_a_dead_frame_and_keeps_the_live_footer() {
        use crate::prompt::fixtures::{
            CTRLC_RELAUNCH_STALE_QUESTION, EXIT_RELAUNCH_STALE_FOOTER,
            EXIT_RELAUNCH_STALE_FOOTER_BUSY, EXIT_RELAUNCH_STALE_FOOTER_QUESTION,
            EXIT_RELAUNCH_STALE_FOOTER_QUESTION2, screen,
        };
        let busy = screen(EXIT_RELAUNCH_STALE_FOOTER_BUSY);
        let live = busy_zone_start(&busy);
        let found = busy_rows(&busy);
        assert_eq!(
            found.len(),
            2,
            "the dead footer and the live one: {found:?}"
        );
        assert!(found[0] < live && live <= found[1], "{live} {found:?}");
        assert_eq!(
            Some(live),
            status_row(&busy).and_then(|s| busy.iter().position(|r| r == s))
        );
        assert_eq!(
            busy_signal(&busy),
            Some(Busy::hard(Zone::Status, "spinner"))
        );
        assert_eq!(worker_phase(&busy), Phase::Busy);
        for (name, text, kind) in [
            ("idle", EXIT_RELAUNCH_STALE_FOOTER, None),
            ("question", EXIT_RELAUNCH_STALE_FOOTER_QUESTION, Some(())),
            ("question2", EXIT_RELAUNCH_STALE_FOOTER_QUESTION2, Some(())),
            ("ctrl-c", CTRLC_RELAUNCH_STALE_QUESTION, None),
        ] {
            let r = screen(text);
            let live = busy_zone_start(&r);
            let card = r
                .iter()
                .rposition(|row| row.contains(crate::anchor("launch.banner")))
                .expect("the new launch card");
            assert!(live > card, "{name}: {live} {card}");
            assert!(
                busy_rows(&r).iter().all(|&b| b < card),
                "{name}: {:?}",
                busy_rows(&r)
            );
            if kind.is_some() {
                assert_eq!(
                    Some(live),
                    crate::prompt::prompt_box_first_row(&r),
                    "{name}"
                );
                assert_eq!(worker_phase(&r), Phase::Prompt, "{name}");
            } else {
                assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
            }
        }
    }

    /// Before the new process draws anything the dead frame is the only one
    /// on the screen — the reading takes it for the live one — but the
    /// resume hint an exiting Claude Code prints under it (`Resume this
    /// session with:`, anchor `exit.resume`) says it is dead: the live zone
    /// starts under the hint, and the dead footer is out of it. Negative
    /// control: the same frame cut at its own footer, as it stood while it
    /// lived, keeps its footer in the zone.
    #[test]
    fn the_live_zone_starts_under_an_exited_processs_resume_hint() {
        use crate::prompt::fixtures::{
            CTRLC_RELAUNCH_STALE_QUESTION, EXIT_RELAUNCH_STALE_FOOTER, screen,
        };
        let notice = crate::anchor("exit.resume");
        for (name, text, footer) in [
            ("exit", EXIT_RELAUNCH_STALE_FOOTER, "esc to interrupt"),
            (
                "ctrl-c",
                CTRLC_RELAUNCH_STALE_QUESTION,
                "Press Ctrl-C again to exit",
            ),
        ] {
            let r = screen(text);
            let hint = r.iter().position(|row| row == notice).expect(name);
            // The screen a fresh `claude` has drawn nothing on yet: the dead
            // frame, its hint and the shell's command line.
            let predraw = r[..hint + 3].to_vec();
            assert_eq!(busy_zone_start(&predraw), hint + 1, "{name}");
            let dead = predraw
                .iter()
                .position(|row| row.contains(footer))
                .expect(name);
            assert!(dead < hint, "{name}");
            // Negative control: the frame as it lived.
            let lived = r[..=dead].to_vec();
            assert!(busy_zone_start(&lived) < dead, "{name}: {lived:#?}");
        }
        // Only the hint alone on its row counts: quoted under the gutter, or
        // in a message, it is the transcript's.
        let r = screen(EXIT_RELAUNCH_STALE_FOOTER);
        let hint = r.iter().position(|row| row == notice).expect("the hint");
        let dead = busy_rows(&r)[0];
        let mut quoted = r[..hint].to_vec();
        quoted.push(format!("  ⎿  {notice}"));
        assert!(busy_zone_start(&quoted) < dead, "{quoted:#?}");
    }

    #[test]
    fn every_busy_signal_is_recognised_and_done_rows_are_not() {
        assert_eq!(busy_reason("  esc to interrupt"), Some("esc to interrupt"));
        assert_eq!(busy_reason("  Still working (12s)"), Some("Still working"));
        assert_eq!(
            busy_reason("  esc to interrupt · ctrl+b to run in background"),
            Some("esc to interrupt")
        );
        assert_eq!(
            busy_reason("  ctrl+b to run in background"),
            Some("ctrl+b to run in background")
        );
        assert_eq!(
            busy_reason("  Waiting for 2 dynamic workflows…"),
            Some("waiting for a dynamic workflow")
        );
        assert_eq!(
            busy_reason("  ⏵⏵ auto mode on · 2 shells still running"),
            Some("a shell still running")
        );
        assert_eq!(
            busy_reason("  ? for shortcuts · 1 shell ·"),
            Some("a shell still running")
        );
        assert_eq!(
            busy_reason("✻ Synthesizing… (18s · ↓ 1.1k tokens)"),
            Some("spinner row")
        );
        assert_eq!(busy_reason("  · Deliberating… (4s)"), Some("spinner row"));
        assert_eq!(busy_reason("* Thinking..."), Some("spinner row"));
        assert_eq!(busy_reason("✻ Cogitated for 2m 54s · done 2:41 PM"), None);
        assert_eq!(busy_reason("✻ Cooked for 9m 27s · done 11:07 AM"), None);
        assert_eq!(busy_reason("⏺ Merged and pushed."), None);
        assert_eq!(busy_reason("  · summarized"), None);
        assert_eq!(busy_reason("✶ …"), None);
        assert_eq!(busy_reason("✶…"), None);
        assert_eq!(busy_reason("  ⏵⏵ auto mode on (shift+tab to cycle)"), None);
    }

    /// A prompt box with a background shell in the footer is a PROMPT: the
    /// worker is blocked on it, whatever the shell does.
    #[test]
    fn a_prompt_beats_a_background_shell() {
        let mut r = bash_one_row();
        let last = r.len() - 1;
        r[last] = "  ⏵⏵ auto mode on · 1 shell still running".to_string();
        assert_eq!(worker_phase(&r), Phase::Prompt);
    }

    /// A `?` in the last said row is a question only when nothing is busy and
    /// no box is up; a `?` earlier in the transcript is not.
    #[test]
    fn question_reads_the_last_said_row_only() {
        let earlier = screen(
            &[
                "⏺ Should I continue?",
                "",
                "⏺ Continuing.",
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&earlier), Phase::Idle);
        let busy_q = screen(&["⏺ Should I continue?", ""], "  esc to interrupt");
        assert_eq!(worker_phase(&busy_q), Phase::Busy);
        // No composer at all (a plain shell): the last non-blank row decides.
        let shell = rows(&["$ ls", "a b", "Continue?", ""]);
        assert_eq!(worker_phase(&shell), Phase::Question);
    }

    /// The live worker's screen: the done row and an update banner sit between
    /// the transcript and the composer separator (which carries the git branch),
    /// the composer holds a dim suggestion after a no-break space. A question
    /// above them is still a question; a statement is idle.
    #[test]
    fn status_rows_under_the_transcript_do_not_hide_a_question() {
        let bottom = [
            "",
            "✻ Baked for 1m 10s · done 2:50 PM",
            "                                              ✔ Update installed · Restart to update",
            "──────────────────────────────────────────────────────────────────── feature-1 ─",
            "❯\u{a0}Wait for the tests, then commit and push as planned",
            "────────────────────────────────────────────────────────────────────────────────",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents                /rc active",
        ];
        let mut question = rows(&[
            "⏺ Local main matches origin/main with nothing ahead or behind.",
            "",
            "  Should I commit and push now, or wait for the tests?",
        ]);
        question.extend(rows(&bottom));
        assert_eq!(worker_phase(&question), Phase::Question);
        assert!(
            is_placeholder(&question, 2),
            "the dim suggestion after ❯\u{a0}"
        );
        assert_eq!(
            composer_text(&question).as_deref(),
            Some("Wait for the tests, then commit and push as planned")
        );

        let mut idle = rows(&["⏺ Local main matches origin/main with nothing ahead or behind."]);
        idle.extend(rows(&bottom));
        assert_eq!(worker_phase(&idle), Phase::Idle);

        // The done row alone under a question is also skipped.
        let done = screen(
            &[
                "⏺ Which do you prefer?",
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&done), Phase::Question);
        assert!(is_status_row("✔ Update installed · Restart to update"));
        assert!(is_status_row("  ✻ Baked for 1m 10s · done 2:50 PM"));
        assert!(!is_status_row("⏺ Done?"));
        assert!(!is_status_row("  Which do you prefer?"));
    }

    #[test]
    fn placeholder_is_composer_text_with_the_cursor_at_column_two() {
        let mut r = screen(&["⏺ Done.", ""], "  ? for shortcuts");
        let c = composer_index(&r).expect("composer");
        assert_eq!(r[c], "❯");
        assert!(
            !is_placeholder(&r, 2),
            "an empty composer is not a placeholder"
        );
        r[c] = "❯ m7 is reachable as ssh m7, go".to_string();
        assert!(is_placeholder(&r, 2), "dim suggestion: cursor at column 2");
        assert!(!is_placeholder(&r, 32), "typed text: the cursor moved");
        assert_eq!(
            composer_text(&r).as_deref(),
            Some("m7 is reachable as ssh m7, go")
        );
        // The option rows of a prompt box are not the composer.
        let p = bash_one_row();
        let ci = composer_index(&p).expect("composer");
        assert_eq!(p[ci], "❯");
        // Nor is the entry of the slash menu under the composer that carries the
        // pointer (2.1.291, 2.1.292), at the top of the list or further down.
        for (menu, typed) in [
            (CONTEXT_WALL_COMPACT_MENU_MEASURED, "/compact"),
            (SLASH_MENU_POINTER_DOWN_MEASURED, "/mod"),
        ] {
            let r = crate::prompt::fixtures::screen(menu);
            let (caret, lines) = composer_draft(&r).expect("the composer over its menu");
            assert_eq!(lines, [typed], "{typed}");
            assert!(
                r[caret].starts_with('❯') && r[caret].ends_with(typed),
                "{typed}"
            );
            assert!(is_rule(&r[caret - 1]) && has_composer_frame(&r), "{typed}");
        }
    }

    /// What Enter may do ([`CompletionMenu::runs`]) by the rows alone: each entry's word,
    /// `<draft>` for sending the draft.
    fn runs_of(rows: &[String]) -> Option<Vec<String>> {
        completion_menu(rows).map(|m| m.runs(None).into_iter().map(said).collect())
    }

    fn said(e: MenuEnter<'_>) -> String {
        match e {
            MenuEnter::Run(w) | MenuEnter::Complete(w) => w.to_string(),
            MenuEnter::Draft => "<draft>".to_string(),
        }
    }

    /// THE ENTRY ENTER WOULD RUN ([`CompletionMenu::runs`]): the slash menu's pointed
    /// entry, at the top (`/compact` under `❯ /compact`) or arrowed down (the third,
    /// `/plugin-authoring`, under `❯ /mod`), both MEASURED on 2.1.292; and a menu of
    /// two with no entry pointed (2.1.287 draws the entries alone), unread, either of
    /// them. NEGATIVE CONTROLS: the measured idle footer under the frame, a `/cl` draft
    /// with its footer under it (the menu dismissed), the shortcuts list, and no frame
    /// are no menu.
    #[test]
    fn the_completion_menus_entry_is_what_enter_would_run() {
        use crate::prompt::fixtures::{
            CONTEXT_WALL_COMPACT_MENU_MEASURED, SLASH_MENU_POINTER_DOWN_MEASURED,
            SLASH_MENU_USAGE_MEASURED, screen as fx,
        };
        let one = |w: &str| Some(vec![w.to_string()]);
        assert_eq!(
            runs_of(&fx(CONTEXT_WALL_COMPACT_MENU_MEASURED)),
            one("/compact")
        );
        assert_eq!(runs_of(&fx(SLASH_MENU_USAGE_MEASURED)), one("/usage"));
        assert_eq!(
            runs_of(&fx(SLASH_MENU_POINTER_DOWN_MEASURED)),
            one("/plugin-authoring")
        );
        let rule = "─".repeat(80);
        let draft = |under: &[&str]| {
            let mut r = rows(&["⏺ Done.", "", &rule, "❯ /cl", &rule]);
            r.extend(rows(under));
            r
        };
        let plain = draft(&[
            "    /clear              Clear conversation history and free up context",
            "    /color              Set the prompt bar color for this session",
        ]);
        assert_eq!(
            runs_of(&plain),
            Some(vec!["/clear".to_string(), "/color".to_string()])
        );
        let lone =
            draft(&["    /clear              Clear conversation history and free up context"]);
        assert_eq!(runs_of(&lone), one("/clear"));
        let pointed = draft(&[
            "  ❯ /clear              Clear conversation history and free up context",
            "    /color              Set the prompt bar color for this session",
        ]);
        assert_eq!(runs_of(&pointed), one("/clear"));
        let menu = completion_menu(&pointed).expect("the menu");
        assert_eq!((menu.entries[0].row, menu.entries[0].col), (5, 4));
        assert_eq!((menu.entries[1].row, menu.entries[1].col), (6, 4));
        // NEGATIVE CONTROLS.
        let idle = fx(include_str!("fixtures/claude-2.1.283-footer-auto-idle.txt"));
        assert_eq!(completion_menu(&idle), None);
        assert_eq!(
            completion_menu(&draft(&["  ⏵⏵ auto mode on (shift+tab to cycle)"])),
            None
        );
        assert_eq!(
            completion_menu(&draft(&[
                "  ! for bash mode          double tap esc to clear input",
                "  / for commands           shift + tab to auto-accept edits",
            ])),
            None
        );
        assert_eq!(completion_menu(&draft(&[])), None);
        assert_eq!(completion_menu(&rows(&["❯ /cl", "    /clear"])), None);
    }

    /// ATERM-KI-1016-1 ("with no pointer drawn, a completion menu is known only by a
    /// first entry starting `/` or `@`"), held to what Enter did, MEASURED in the
    /// real-Claude lab on 2026-10-17. 2.1.302: the `@` menu (files `+`, agents `*`, at
    /// column 4) opens with no entry selected and none pointed, and Enter SENT the draft
    /// (`@`, `and @RE`), so it is no menu Enter diverts; after `down` the pointer is drawn
    /// (`  ❯ + notes.txt`) and Enter completed the entry, so it is read by its pointer.
    /// 2.1.268, a build that draws no pointer: the slash menu's selected entry is drawn
    /// in colour only, so the rows alone leave any entry the one Enter runs (Enter ran
    /// `/clear` on `/cl`, its top entry); its `@` menu (at column 2) opens with none
    /// selected and Enter sent the draft, but after a person's `down` its entry is
    /// highlighted in colour only, the rows unchanged, and Enter completed it, so by the
    /// rows alone Enter may do either (its cells decide:
    /// [`an_at_menus_selection_drawn_in_colour_only_is_read_by_the_cells_measured`]).
    #[test]
    fn a_menu_with_no_pointer_is_what_enter_did_measured() {
        use crate::prompt::fixtures::{
            AT_MENU_2_1_302, AT_MENU_DOWN_2_1_268, AT_MENU_PARTIAL_2_1_268,
            AT_MENU_PARTIAL_2_1_302, AT_MENU_POINTED_2_1_302, SLASH_MENU_2_1_268, screen as fx,
        };
        let draft_or = |words: &[&str]| {
            Some(
                std::iter::once("<draft>")
                    .chain(words.iter().copied())
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        };
        for (what, fixture, runs) in [
            ("2.1.302 `@`, Enter sent it", AT_MENU_2_1_302, None),
            (
                "2.1.302 `and @RE`, Enter sent it",
                AT_MENU_PARTIAL_2_1_302,
                None,
            ),
            (
                "2.1.268 `and @RE`, Enter sent it",
                AT_MENU_PARTIAL_2_1_268,
                draft_or(&["+", "*"]),
            ),
            (
                "2.1.268 after `down`, Enter completed",
                AT_MENU_DOWN_2_1_268,
                draft_or(&["+", "*"]),
            ),
            (
                "2.1.302 pointed, Enter completed",
                AT_MENU_POINTED_2_1_302,
                Some(vec!["+".to_string()]),
            ),
        ] {
            let r = fx(fixture);
            assert!(composer_frame(&r).is_some(), "{what}: the frame");
            assert_eq!(runs_of(&r), runs, "{what}");
        }
        let slash = runs_of(&fx(SLASH_MENU_2_1_268)).expect("2.1.268's slash menu");
        assert!(slash.len() > 2 && slash[0] == "/clear", "{slash:?}");
        assert!(slash.iter().all(|w| w.starts_with('/')), "{slash:?}");
        assert_eq!(fx(AT_MENU_DOWN_2_1_268), fx(AT_MENU_PARTIAL_2_1_268));
    }

    /// The colour of each entry's first glyph, from a cells fixture.
    fn fg_of(menu: &CompletionMenu, cells: &str) -> Vec<Option<String>> {
        let cells = crate::prompt::fixtures::cells(cells);
        menu.entries
            .iter()
            .map(|e| {
                cells
                    .iter()
                    .find(|(r, c, _)| (*r, *c) == (e.row, e.col))
                    .and_then(|(_, _, reply)| reply.split(' ').nth(2).map(str::to_string))
            })
            .collect()
    }

    /// THE SELECTION DRAWN IN COLOUR ONLY (the review of ATERM-KI-1016-1's "no pointer"
    /// item), on 2.1.268's slash menu MEASURED with the `cell` of each row's first glyph
    /// and what Enter did. `/config` over nine entries: the text is the same whichever
    /// is selected; as the menu opened `/config`'s `/` was `b1b9f9` and the others'
    /// `999999`, and Enter opened the settings; after `down` `/update-config`'s was, and
    /// Enter ran `/update-config`. The colours name that entry; a description's wrapped
    /// row (column 32) is no entry. With two entries (`/copy`, `/fork`) the two colours
    /// do not say which is the selection's, so either may run. NEGATIVE CONTROLS: an
    /// unread cell, every entry in one colour, and two unlike the rest leave every entry
    /// a candidate; a pointed menu is read by its pointer whatever the colours say.
    #[test]
    fn a_selection_drawn_in_colour_only_is_read_by_the_cells_measured() {
        use crate::prompt::fixtures::{
            SLASH_MENU_CONFIG_2_1_268, SLASH_MENU_CONFIG_CELLS_2_1_268,
            SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268, SLASH_MENU_COPY_2_1_268,
            SLASH_MENU_COPY_CELLS_2_1_268, SLASH_MENU_COPY_DOWN_CELLS_2_1_268,
            SLASH_MENU_POINTER_DOWN_MEASURED, screen as fx,
        };
        let config = completion_menu(&fx(SLASH_MENU_CONFIG_2_1_268)).expect("the menu");
        let words: Vec<&str> = config.entries.iter().map(|e| e.word.as_str()).collect();
        assert_eq!(
            words,
            [
                "/config",
                "/update-config",
                "/doctor",
                "/hooks",
                "/sandbox",
                "/run",
                "/code-review"
            ]
        );
        assert!(config.colour_only() && config.entries.iter().all(|e| e.col == 2));
        let runs = |menu: &CompletionMenu, fg: Option<&[Option<String>]>| -> Vec<String> {
            menu.runs(fg).into_iter().map(said).collect()
        };
        let fresh = fg_of(&config, SLASH_MENU_CONFIG_CELLS_2_1_268);
        let down = fg_of(&config, SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268);
        assert_eq!(config.runs(Some(&fresh)), [MenuEnter::Run("/config")]);
        assert_eq!(config.runs(Some(&down)), [MenuEnter::Run("/update-config")]);
        assert_eq!(runs(&config, None), words);
        let copy = completion_menu(&fx(SLASH_MENU_COPY_2_1_268)).expect("the menu");
        for cells in [
            SLASH_MENU_COPY_CELLS_2_1_268,
            SLASH_MENU_COPY_DOWN_CELLS_2_1_268,
        ] {
            assert_eq!(runs(&copy, Some(&fg_of(&copy, cells))), ["/copy", "/fork"]);
        }
        // NEGATIVE CONTROLS.
        let mut unread = fresh.clone();
        unread[3] = None;
        assert_eq!(runs(&config, Some(&unread)), words);
        let alike = vec![Some("999999".to_string()); fresh.len()];
        assert_eq!(runs(&config, Some(&alike)), words);
        let mut two = fresh.clone();
        two[4] = Some("d0d0d0".to_string());
        assert_eq!(runs(&config, Some(&two)), words);
        assert_eq!(runs(&config, Some(&fresh[1..])), words);
        let pointed = completion_menu(&fx(SLASH_MENU_POINTER_DOWN_MEASURED)).expect("the menu");
        let mut colours = vec![Some("999999".to_string()); pointed.entries.len()];
        colours[0] = Some("b1b9f9".to_string());
        assert!(!pointed.colour_only());
        assert_eq!(
            pointed.runs(Some(&colours)),
            [MenuEnter::Run("/plugin-authoring")]
        );
    }

    /// THE `@` MENU'S SELECTION DRAWN IN COLOUR ONLY (ATERM-KI-1016-1, dialogs: no
    /// pointer), on 2.1.268 MEASURED with the `cell` of each entry's first glyph and what
    /// Enter did. The `@` menu opens with no entry selected, every entry alike (`999999`,
    /// files `+` and agents `*` alike), and Enter sent the draft; after `down` the entry
    /// selected is `b1b9f9`, the rows unchanged, and Enter completed it and sent nothing
    /// (`read @no` became `read @novel.txt`; `and @RE`, after two, `and @"Explore
    /// (agent)"`). So entries all alike send the draft, and any unlike the rest complete an
    /// entry. Read by its rows alone it was sent: the turn end's Submit completed the
    /// person's entry. One entry alone has no other's colour to be told against: either.
    /// NEGATIVE CONTROLS: an unread cell leaves either; 2.1.302's `@` menu, which leaves
    /// the pointer's room before its entries ([`POINTER_ROOM`]), is no menu unpointed
    /// (Enter sent it) and its pointer's when pointed, whatever the colours say.
    #[test]
    fn an_at_menus_selection_drawn_in_colour_only_is_read_by_the_cells_measured() {
        use crate::prompt::fixtures::{
            AT_MENU_DOWN_2_1_268, AT_MENU_DOWN_CELLS_2_1_268, AT_MENU_DOWN2_CELLS_2_1_268,
            AT_MENU_NO_2_1_268, AT_MENU_NO_CELLS_2_1_268, AT_MENU_NO_DOWN_CELLS_2_1_268,
            AT_MENU_ONE_2_1_268, AT_MENU_PARTIAL_2_1_268, AT_MENU_PARTIAL_2_1_302,
            AT_MENU_PARTIAL_CELLS_2_1_268, AT_MENU_POINTED_2_1_302, screen as fx,
        };
        let draft = [MenuEnter::Draft];
        let re = completion_menu(&fx(AT_MENU_PARTIAL_2_1_268)).expect("the menu");
        assert!(re.mentions && re.colour_only() && re.entries.iter().all(|e| e.col == 2));
        let completes = [MenuEnter::Complete("+"), MenuEnter::Complete("*")];
        assert_eq!(
            re.runs(Some(&fg_of(&re, AT_MENU_PARTIAL_CELLS_2_1_268))),
            draft
        );
        let down = completion_menu(&fx(AT_MENU_DOWN_2_1_268)).expect("the menu");
        assert_eq!(down, re, "the same rows whichever is selected");
        for cells in [AT_MENU_DOWN_CELLS_2_1_268, AT_MENU_DOWN2_CELLS_2_1_268] {
            assert_eq!(re.runs(Some(&fg_of(&re, cells))), completes);
        }
        let no = completion_menu(&fx(AT_MENU_NO_2_1_268)).expect("the menu");
        assert_eq!(no.entries.len(), 4);
        assert_eq!(no.runs(Some(&fg_of(&no, AT_MENU_NO_CELLS_2_1_268))), draft);
        assert_eq!(
            no.runs(Some(&fg_of(&no, AT_MENU_NO_DOWN_CELLS_2_1_268))),
            [MenuEnter::Complete("+"); 4]
        );
        let one = completion_menu(&fx(AT_MENU_ONE_2_1_268)).expect("the menu");
        assert!(one.mentions && !one.colour_only());
        let either = [MenuEnter::Draft, MenuEnter::Complete("+")];
        for fg in ["999999", "b1b9f9"] {
            assert_eq!(one.runs(Some(&[Some(fg.to_string())])), either, "{fg}");
        }
        // NEGATIVE CONTROLS.
        let mut unread = fg_of(&no, AT_MENU_NO_CELLS_2_1_268);
        unread[2] = None;
        assert_eq!(no.runs(Some(&unread)).len(), 5, "either");
        assert_eq!(re.runs(None).first(), Some(&MenuEnter::Draft));
        assert_eq!(completion_menu(&fx(AT_MENU_PARTIAL_2_1_302)), None);
        let pointed = completion_menu(&fx(AT_MENU_POINTED_2_1_302)).expect("the menu");
        assert!(pointed.mentions && pointed.entries[0].col == POINTER_ROOM);
        assert_eq!(
            pointed.runs(Some(&[Some("999999".to_string())])),
            [MenuEnter::Complete("+")]
        );
    }

    // ---- the live zone, against real screens ------------------------------
    //
    // `fixtures/` holds screens saved from a real supervision session (Claude
    // Code 2.1.267 in a 63x138 aterm window, 2026-09-10 to 2026-09-12). The
    // `.txt` is a whole `text` read, one row per line (its cursor was row 60,
    // col 2 — the empty composer). A `wait_bg*.out` is what an older
    // scratchpad waiter printed: a `== KIND … ==` line, the screen's non-blank
    // rows (cut at 150-170 columns), `exit=<n>`. The KIND is what that waiter
    // concluded; the truth each test names is read from the rows by the
    // live-zone rule instead.
    //
    // The session's work was private, so the WORDS in them — the worker's
    // prose, its commands and tool output, task, workflow and monitor names,
    // the manager's messages — were replaced with neutral text of the same
    // shape: row for row, the same gutter and indentation, about the same
    // width. Every row Claude Code draws itself is verbatim: the rules and the
    // composer, the footer, spinner, waiting and done rows, hints, tips, the
    // survey, banners, limit notices, `Ran N shell commands`, the `/model`
    // output.

    const IDLE_AFTER_LIMIT: &str = include_str!("fixtures/idle-after-limit-and-model-switch.txt");

    fn saved_screen(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn waiter_capture(text: &str) -> Vec<String> {
        let mut r = saved_screen(text);
        if r.first().is_some_and(|l| l.starts_with("== ")) {
            r.remove(0);
        }
        if r.last().is_some_and(|l| l.starts_with("exit=")) {
            r.pop();
        }
        r
    }

    fn signal(rows: &[String]) -> Option<String> {
        busy_signal(rows).map(|b| b.to_string())
    }

    /// Replace the footer (the row under the bottom rule) with `footer`.
    fn with_footer(mut r: Vec<String>, footer: &str) -> Vec<String> {
        let bottom = r.iter().rposition(|x| is_rule(x)).expect("bottom rule");
        r[bottom + 1] = footer.to_string();
        r
    }

    /// Insert `extra` directly above the composer's top rule.
    fn above_frame(mut r: Vec<String>, extra: &[&str]) -> Vec<String> {
        let top = composer_frame(&r).expect("frame").top;
        for (k, row) in extra.iter().enumerate() {
            r.insert(top + k, (*row).to_string());
        }
        r
    }

    /// Insert `extra` directly above the status row (the done row).
    fn above_status_row(mut r: Vec<String>, extra: &[&str]) -> Vec<String> {
        let top = composer_frame(&r).expect("frame").top;
        let at = status_block(&r, top).status.expect("a status row");
        for (k, row) in extra.iter().enumerate() {
            r.insert(at + k, (*row).to_string());
        }
        r
    }

    /// Footers an idle worker's screen shows (from wait_bg7 and wait_bg2).
    const IDLE_AUTO_FOOTER: &str = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents                                                                         /rc active";
    const PASTE_FOOTER: &str = "  paste again to expand                                                                                                       /rc active";

    /// The screen that exposed the gap: the turn ended at 11:30 PM, `/model`
    /// ran after it, the composer (row 61) is empty — IDLE. Row 45 still says
    /// `✻ Waiting for 1 dynamic workflow to finish`, which the whole-screen
    /// scan read busy; it is transcript now, fifteen rows above the frame.
    #[test]
    fn the_idle_after_limit_screen_is_idle_though_row_45_says_waiting() {
        let r = saved_screen(IDLE_AFTER_LIMIT);
        assert_eq!(signal(&r), None);
        assert_eq!(
            status_row(&r),
            None,
            "the walk stops at the /model output under the gutter: no live status row"
        );
        assert_eq!(
            last_said_row(&r),
            Some(
                "  ⎿  Set model to Opus 5 (1M context) (default) and saved as your default for new sessions"
            ),
            "the right-aligned banner under it is skipped"
        );
        assert_eq!(worker_phase(&r), Phase::Idle);
    }

    /// The same screen with a live spinner directly above the top rule, and
    /// again above the right-aligned hint (the walk skips the hint): BUSY.
    #[test]
    fn a_live_spinner_above_the_top_rule_is_busy() {
        for at in [59, 58] {
            let mut r = saved_screen(IDLE_AFTER_LIMIT);
            r.insert(at, "✶ Deliberating… (4s · ↓ 120 tokens)".to_string());
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: spinner"),
                "inserted at {at}"
            );
            assert_eq!(worker_phase(&r), Phase::Busy);
        }
    }

    /// The two spinner rows measured on a Claude Code 2.1.267 screen, each as
    /// a fixture, and the three ways a release moves one out from under a
    /// glyph list: a glyph the list does not have, an indented row, and a
    /// footer that does not say `esc to interrupt` (it does not always say it
    /// while a turn runs). Busy is decided by the footer's phrase and by the
    /// spinner row's SHAPE — one non-ASCII glyph, one space, a capitalised
    /// verb, an ellipsis, an optional parenthesised timing — never by a list
    /// of the glyphs the versions we happened to measure cycle through.
    #[test]
    fn the_spinner_row_reads_busy_by_its_shape_not_by_a_glyph_list() {
        const BLOVIATING: &str = include_str!("fixtures/spinner-bloviating.txt");
        const TEMPERING: &str = include_str!("fixtures/spinner-tempering.txt");
        for (name, text) in [("bloviating", BLOVIATING), ("tempering", TEMPERING)] {
            let measured = saved_screen(text);
            let at = measured
                .iter()
                .position(|r| r.contains('…'))
                .unwrap_or_else(|| panic!("{name}: the fixture has no spinner row"));
            assert!(
                measured.iter().any(|r| r.contains("esc to interrupt")),
                "{name}: the fixture is the measured screen, the footer's phrase and all"
            );
            assert_eq!(worker_phase(&measured), Phase::Busy, "{name}: as measured");

            // The footer does not always carry the phrase. Then the spinner
            // row is the only thing left saying a turn is running.
            let quiet: Vec<String> = measured
                .iter()
                .map(|r| r.replace(" · esc to interrupt", ""))
                .collect();
            assert!(!quiet.iter().any(|r| r.contains("esc to interrupt")));
            let glyph = quiet[at].chars().next().expect("a glyph");
            let tail = &quiet[at][glyph.len_utf8()..];
            for (how, row) in [
                ("the measured glyph", quiet[at].clone()),
                ("a glyph the list does not have", format!("✹{tail}")),
                ("indented", format!("  {}", quiet[at])),
                ("indented, and unlisted", format!("  ✹{tail}")),
            ] {
                let mut r = quiet.clone();
                r[at] = row.clone();
                assert_eq!(
                    signal(&r).as_deref(),
                    Some("status row: spinner"),
                    "{name}, {how}: {row:?}"
                );
                assert_eq!(worker_phase(&r), Phase::Busy, "{name}, {how}");
            }
        }
    }

    /// The shape must not hand the transcript's own markers to the status
    /// row: `⏺ Bloviating…` is the worker SAYING what it is about to do, and
    /// a turn that ended on such a row is idle, ellipsis and all.
    #[test]
    fn a_transcript_marker_never_reads_as_a_spinner_row() {
        for glyph in ['⏺', '●', '⎿'] {
            let said = format!("{glyph} Bloviating… (15s · thinking with xhigh effort)");
            let r = screen(
                &[&said, "", ""],
                "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
            );
            assert!(!is_glyph_row(&r[0]), "{glyph}: status row");
            assert_eq!(busy_reason(&r[0]), None, "{glyph}: whole-screen rule");
            assert_eq!(signal(&r), None, "{glyph}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{glyph}");
        }
    }

    /// The older waiter's captures, each read by its live zone: `wait_bg` a
    /// turn finished while a background workflow runs (`Waiting for 1
    /// dynamic workflow` above the frame, its progress line under the
    /// footer); `wait_bg2` the manager answered and the worker deliberates
    /// under a `⎿  Tip:` row, the older `Waiting for` row above the user row
    /// history; `wait_bg3` a done row that counts no shell, the worker asking
    /// for a call without a `?`; `wait_bg4` `1 shell still running` on the
    /// done row and `· 1 shell ·` in the footer; `wait_bg11` the shell on the
    /// done row under a right-aligned hint; `wait_bg12` the live done row
    /// under the update banner, an older one with a shell far up (history).
    #[test]
    fn the_waiter_captures_read_by_their_live_zone() {
        for (name, text, busy, status, phase) in [
            (
                "wait_bg",
                include_str!("fixtures/wait_bg.out"),
                Some("status row: waiting for a dynamic workflow"),
                None,
                Phase::Busy,
            ),
            (
                "wait_bg2",
                include_str!("fixtures/wait_bg2.out"),
                Some("status row: spinner"),
                Some("✶ Deliberating…"),
                Phase::Busy,
            ),
            (
                "wait_bg3",
                include_str!("fixtures/wait_bg3.out"),
                None,
                Some("✻ Sautéed for 50m 35s · done 10:57 AM"),
                Phase::Idle,
            ),
            (
                "wait_bg4",
                include_str!("fixtures/wait_bg4.out"),
                Some("status row: a shell still running"),
                None,
                Phase::Busy,
            ),
            (
                "wait_bg11",
                include_str!("fixtures/wait_bg11.out"),
                Some("status row: a shell still running"),
                None,
                Phase::Busy,
            ),
            (
                "wait_bg12",
                include_str!("fixtures/wait_bg12.out"),
                Some("status row: a shell still running"),
                Some("✻ Sautéed for 1m 7s · done 1:44 PM · 1 shell still running"),
                Phase::Busy,
            ),
        ] {
            let r = waiter_capture(text);
            assert_eq!(signal(&r).as_deref(), busy, "{name}");
            if status.is_some() {
                assert_eq!(status_row(&r), status, "{name}");
            }
            assert_eq!(worker_phase(&r), phase, "{name}");
        }
        let bg3 = waiter_capture(include_str!("fixtures/wait_bg3.out"));
        assert_eq!(last_said_row(&bg3), Some("  Waiting for your call."));
    }

    /// THE AGENT'S OWN BACKGROUND WORK is read apart from a live turn
    /// ([`background_wait`]): a workflow waited on after the turn and a shell
    /// left running under a done row are background work; the same
    /// workflow's progress line under a LIVE spinner is a turn, and an idle
    /// screen or one with no frame is neither.
    #[test]
    fn background_work_after_the_turn_is_told_from_a_live_turn() {
        let bg = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert_eq!(background_wait(&bg), Some("waiting for a dynamic workflow"));
        let shell = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert_eq!(background_wait(&shell), Some("a shell still running"));
        // NEGATIVE CONTROLS: a live spinner over the same workflow's line,
        // an idle screen, a screen with no composer frame.
        let live = waiter_capture(include_str!("fixtures/wait_bg2.out"));
        assert_eq!(worker_phase(&live), Phase::Busy);
        assert_eq!(background_wait(&live), None);
        let idle = waiter_capture(include_str!("fixtures/wait_bg3.out"));
        assert_eq!(background_wait(&idle), None);
        let bare: Vec<String> = vec!["✻ Waiting for 1 dynamic workflow to finish".to_string()];
        assert_eq!(background_wait(&bare), None);
    }

    /// THE TURN OVER, A SHELL OR A MONITOR LEFT RUNNING
    /// ([`turn_over_background`]): a done row over a shell (`wait_bg4.out`)
    /// or over a monitor is the turn's end. NEGATIVE CONTROLS: a turn still
    /// waiting on its workflow (`wait_bg.out`) or its background agent is
    /// background work but not over; a live turn (`wait_bg2.out`) and an
    /// idle screen (`wait_bg3.out`) are neither.
    #[test]
    fn a_turn_over_background_work_is_a_done_row_over_a_shell_or_a_monitor() {
        let shell = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert!(turn_over_background(&shell));
        let done = "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running";
        let at = shell.iter().position(|r| r == done).expect("the done row");
        let mut monitor = with_footer(shell.clone(), IDLE_AUTO_FOOTER);
        monitor[at] = "✻ Crunched for 9m 27s · done 11:07 AM · 1 monitor still running".to_string();
        assert_eq!(background_wait(&monitor), Some("a monitor still running"));
        assert!(turn_over_background(&monitor));
        // NEGATIVE CONTROLS.
        let workflow = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert!(background_wait(&workflow).is_some());
        assert!(!turn_over_background(&workflow));
        let mut agent = shell.clone();
        agent[at] = "✻ Waiting for 1 background agent to finish".to_string();
        assert_eq!(
            background_wait(&agent),
            Some("waiting for a background agent")
        );
        assert!(!turn_over_background(&agent));
        for (name, text) in [
            ("wait_bg2.out", include_str!("fixtures/wait_bg2.out")),
            ("wait_bg3.out", include_str!("fixtures/wait_bg3.out")),
        ] {
            assert!(!turn_over_background(&waiter_capture(text)), "{name}");
        }
    }

    /// WHO OPENED THE TURN DRAWN LAST ([`opened_by_own_wake`]): Claude Code's
    /// own wake — a background command's end (`wait_bg6.out`; `wait_bg12.out`,
    /// whose composer holds a draft that is no message) or a monitor's event
    /// — with no person's row after it. A person's message after the wake,
    /// in the transcript or queued under the status row, opened the turn.
    /// NEGATIVE CONTROLS: a screen with no opener on it, and one with no
    /// frame, say nothing.
    #[test]
    fn the_turn_drawn_last_was_opened_by_the_agents_own_wake_or_a_person() {
        let woke = waiter_capture(include_str!("fixtures/wait_bg6.out"));
        assert_eq!(opened_by_own_wake(&woke), Some(true));
        let drafted = waiter_capture(include_str!("fixtures/wait_bg12.out"));
        assert_eq!(opened_by_own_wake(&drafted), Some(true));
        let shell = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        let monitor = above_status_row(
            shell.clone(),
            &[
                "⏺ Monitor event: \"worker watch\"",
                "⏺ The worker is still at its prompt.",
            ],
        );
        assert_eq!(opened_by_own_wake(&monitor), Some(true));
        // A person's message after the wake: in the transcript, and queued.
        let at = woke
            .iter()
            .rposition(|r| r.starts_with("⏺ Background command"))
            .expect("the wake");
        let mut asked = woke.clone();
        asked.insert(at + 1, "❯ Also update the README.".to_string());
        assert_eq!(opened_by_own_wake(&asked), Some(false));
        let queued = above_frame(woke, &["❯ Also update the README."]);
        assert_eq!(opened_by_own_wake(&queued), Some(false));
        // NEGATIVE CONTROLS.
        assert_eq!(opened_by_own_wake(&shell), None);
        let bare = vec!["⏺ Background command \"x\" completed (exit code 0)".to_string()];
        assert_eq!(opened_by_own_wake(&bare), None);
    }

    /// A MESSAGE SUBMITTED AT A BREAK has begun a turn before its spinner is
    /// drawn: `wait_bg2.out` a frame earlier — the manager's message drawn
    /// under `✻ Waiting for 1 dynamic workflow to finish`, its spinner and tip
    /// not yet — still has the waiting row as its status row, and is no break.
    /// NEGATIVE CONTROL: the same break with nothing submitted is one.
    #[test]
    fn a_message_submitted_under_the_waiting_row_is_no_break() {
        let r = waiter_capture(include_str!("fixtures/wait_bg2.out"));
        let spinner = r
            .iter()
            .position(|row| row == "✶ Deliberating…")
            .expect("the live spinner");
        assert!(r[spinner + 1].contains("Tip:"), "{}", r[spinner + 1]);
        let mut submitted = r.clone();
        submitted.drain(spinner..=spinner + 1);
        assert_eq!(
            signal(&submitted).as_deref(),
            Some("status row: waiting for a dynamic workflow"),
            "the waiting row is still the status row"
        );
        assert_eq!(background_wait(&submitted), None);
        let bg = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert_eq!(background_wait(&bg), Some("waiting for a dynamic workflow"));
        let top = composer_frame(&bg).expect("the frame").top;
        let mut typed = bg.clone();
        typed.insert(top, "❯ keep going".to_string());
        typed.remove(0);
        assert_eq!(background_wait(&typed), None, "{:?}", &typed[top - 3..=top]);
    }

    /// THE SHELL COUNT the survey publishes (`bg=`): the done row's or the
    /// footer's count, whichever names one, and the larger where both do.
    /// NEGATIVE CONTROLS: a monitor is no shell, a screen with a frame that
    /// counts none is `0`, and a screen with no frame is no reading.
    #[test]
    fn the_shell_count_is_the_done_rows_or_the_footers() {
        use crate::prompt::fixtures::IDLE_SHELLS_2_1_271 as TEXT;
        let r = crate::prompt::fixtures::screen(TEXT);
        assert_eq!(shells_running(&r), Some(2));
        let one = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert_eq!(shells_running(&one), Some(1));
        let done = "✻ Cogitated for 3m 12s · done 9:29 PM · 2 shells still running";
        let at = r.iter().position(|x| x == done).expect("the done row");
        let mut more = r.clone();
        more[at] = "✻ Cogitated for 3m 12s · done 9:29 PM · 3 shells still running".to_string();
        assert_eq!(shells_running(&more), Some(3), "the larger count");
        const BARE: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        let mut none = with_footer(r.clone(), BARE);
        none[at] = "✻ Cogitated for 3m 12s · done 9:29 PM · 1 monitor still running".to_string();
        assert_eq!(shells_running(&none), Some(0), "a monitor is no shell");
        assert_eq!(shells_running(&["$ ls".to_string()]), None, "no frame");
    }

    /// CLAUDE CODE 2.1.271 AT REST OVER TWO SHELLS THAT NEVER END (the
    /// upgrade strand of 2026-09-21..27: two background `tail -f … | grep -m1
    /// …` closers whose grep had long since matched held a tab 5.5 days). The
    /// done row counts `2 shells still running` and the bypass-mode footer
    /// `· 2 shells ·`: BUSY, and the busy is the agent's own background work
    /// ([`background_wait`]) — the break the supervisor hands to the live
    /// upgrade, where its notice and a give-up's owed release are typed. The
    /// done row alone, or the footer alone, is still that break. NEGATIVE
    /// CONTROLS: a message submitted under the done row has begun a turn and
    /// is no break, busy as it still reads; with no shell counted anywhere,
    /// the bypass footer over a done row reads idle. The screen is HAND-BUILT
    /// (its line 1 says from what): no capture of the incident was kept.
    #[test]
    fn claude_2_1_271_idle_over_two_shells_is_a_break_of_its_own_work() {
        use crate::prompt::fixtures::IDLE_SHELLS_2_1_271 as TEXT;
        use crate::reader::{Program, identify};
        // This module's `screen` builds rows; the fixture's are read whole.
        let r = crate::prompt::fixtures::screen(TEXT);
        let done = "✻ Cogitated for 3m 12s · done 9:29 PM · 2 shells still running";
        assert_eq!(status_row(&r), Some(done));
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(background_wait(&r), Some("a shell still running"));
        // The supervisor's break, as it asks it (`host_steps_in_background`):
        // busy, background work, no wall — the reader named by the program,
        // or, for a Claude Code started as `node`, by its screen.
        for reader in [identify(Some("claude"), &r), identify(Some("node"), &r)] {
            assert_eq!(reader.program(), Program::Claude);
            assert_eq!(reader.phase(&r), Phase::Busy);
            assert_eq!(reader.background_wait(&r), Some("a shell still running"));
            assert!(reader.wall(&r).is_none(), "{:?}", reader.wall(&r));
        }

        // THE FOOTER ALONE: the done row counts nothing.
        let at = r.iter().position(|x| x == done).expect("the done row");
        let mut footer_only = r.clone();
        footer_only[at] = "✻ Cogitated for 3m 12s · done 9:29 PM".to_string();
        assert_eq!(
            signal(&footer_only).as_deref(),
            Some("footer: a shell running")
        );
        assert_eq!(worker_phase(&footer_only), Phase::Busy);
        assert_eq!(background_wait(&footer_only), Some("a shell running"));

        // THE DONE ROW ALONE: the bypass footer counts nothing.
        const BARE: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        let done_only = with_footer(r.clone(), BARE);
        assert_eq!(
            signal(&done_only).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&done_only), Phase::Busy);
        assert_eq!(background_wait(&done_only), Some("a shell still running"));

        // NEGATIVE CONTROL: a message submitted under the done row (drawn a
        // frame before its turn's spinner) is a turn begun, not a break.
        let top = composer_frame(&r).expect("the frame").top;
        for s in [&r, &footer_only, &done_only] {
            let mut submitted = s.clone();
            submitted.insert(top, "❯ keep going".to_string());
            submitted.remove(0);
            assert_eq!(worker_phase(&submitted), Phase::Busy);
            assert_eq!(
                background_wait(&submitted),
                None,
                "{:?}",
                &submitted[top - 3..=top]
            );
        }

        // NEGATIVE CONTROL: no shell counted anywhere — the bypass footer
        // over a done row is idle.
        let bare = with_footer(footer_only, BARE);
        assert_eq!(signal(&bare), None);
        assert_eq!(worker_phase(&bare), Phase::Idle);
        assert_eq!(background_wait(&bare), None);
        for reader in [
            identify(Some("claude"), &bare),
            identify(Some("node"), &bare),
        ] {
            assert_eq!(reader.program(), Program::Claude);
            assert_eq!(reader.phase(&bare), Phase::Idle);
            assert_eq!(reader.background_wait(&bare), None);
        }
    }

    /// The live worker's screen as `aterm drive phase` first read it
    /// (2026-09-12, 10:35): its turn ended while a background agent and a
    /// shell run on; the status row says so under the update banner, and the
    /// agents panel hangs under the footer.
    #[test]
    fn a_turn_waiting_for_a_background_agent_is_busy_from_its_status_row() {
        let r = rows(&[
            "  ⎿  Backgrounded agent (↓ to manage · ctrl+o to expand)",
            "",
            "⏺ Build and reviewer are running. The notes entry and the handoff need the rerun numbers, so they wait until after the",
            "  measurements.",
            "",
            "✻ Waiting for 1 background agent to finish",
            "                                                                                                  ✔ Update installed · Restart to update",
            "────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────── sandboxed ─",
            "❯ Waiting on the build and the reviewer; both will notify when done.",
            "──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
            "  ⏵⏵ auto mode on · 1 shell · ← for agents · ↓ to manage                                                                      /rc active",
            "",
            "  ⏺ main",
            "  ◯ general-purpose  Searching the report's consumers in tests                                                  4m 28s · ↓ 115.4k tokens",
        ]);
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: waiting for a background agent")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(
            status_busy("✻ Waiting for 2 remote jobs to finish"),
            Some("waiting for background work")
        );
        assert_eq!(status_busy("✻ Waiting for your call"), None);
    }

    /// Each footer rule, on an otherwise idle frame; the footers an idle worker
    /// shows fire none of them. A monitor is the soft rule.
    #[test]
    fn each_footer_rule_reads_busy_and_idle_footers_do_not() {
        let body = ["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""];
        for (footer, rule, soft) in [
            (
                "  ⏵⏵ auto mode on · 1 shell · ← for agents · ↓ to manage",
                "footer: a shell running",
                false,
            ),
            (
                "  ⏵⏵ auto mode on · 2 shells",
                "footer: a shell running",
                false,
            ),
            (
                "  ⏵⏵ auto mode on · 2 shells still running",
                "footer: a shell running",
                false,
            ),
            (
                "  ⏵⏵ auto mode on · 1 monitor · ← for agents",
                "footer: a monitor running",
                true,
            ),
            (
                "  ? for shortcuts · 3 monitors ·                     /rc active",
                "footer: a monitor running",
                true,
            ),
            (
                "  ◯ nightly-sweep  Check every crate against the ledger… 1/4 agents done · 3m 2s",
                "footer: a workflow running",
                false,
            ),
            ("  Still working (12s)", "footer: Still working", false),
            (
                "  esc to interrupt · ctrl+b to run in background",
                "footer: esc to interrupt",
                false,
            ),
        ] {
            let r = screen(&body, footer);
            assert_eq!(signal(&r).as_deref(), Some(rule), "{footer}");
            assert_eq!(busy_signal(&r).map(|b| b.soft), Some(soft), "{footer}");
            assert_eq!(worker_phase(&r), Phase::Busy, "{footer}");
        }
        for footer in [
            "  ? for shortcuts",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents                /rc active",
            "  paste again to expand",
            "  ⏵⏵ auto mode on · shells are fine · 10 files changed",
        ] {
            let r = screen(&body, footer);
            assert_eq!(signal(&r), None, "{footer}");
        }
    }

    /// The status walk: blanks, hints, a `⎿  Tip:` row and a todo list under a
    /// live spinner are passed; a worker's row or tool output above the frame
    /// means no live status row, whatever is above THEM. A user row is passed
    /// too: with nothing under it, it is a message the worker is about to take
    /// or has queued, and the status row above it still stands.
    #[test]
    fn the_status_walk_passes_the_block_and_stops_at_the_transcript() {
        assert!(!is_spinner_row(
            "✻ Implementing the parser… (2m · ↓ 3.1k tokens)"
        ));
        assert!(is_activity_row(
            "✻ Implementing the parser… (2m · ↓ 3.1k tokens)"
        ));
        assert!(!is_activity_row("✻ Cooked for 23m 0s · done 11:24 PM"));
        // The vendor's API-retry row, MEASURED 2026-09-27 (no ellipsis).
        for row in [
            "✻ Can't reach the API server — check your internet or DNS (ENOTFOUND) · Retrying in 1s · attempt 5/10",
            "✻ Can't reach the API server — check your in… · Retrying in 30s · attempt 7/10",
            "✻ API Error: 529 Overloaded · Retrying in 1m 30s · attempt 2/10",
            // The vendor's count of its tries without their total (the
            // queued shape's `attempt 2`).
            "✻ Can't reach the API server · Retrying in 5s · attempt 5",
        ] {
            assert!(is_activity_row(row), "{row}");
        }
        // Controls: a done row, the worker's own words, a malformed tail.
        for row in [
            "✻ Cogitated for 0s · done 11:02 PM",
            "✻ I was Retrying in 5s · attempt soon",
            "✻ Can't reach the API server · Retrying in soon · attempt 5/10",
            "⏺ Can't reach the API server · Retrying in 5s · attempt 5/10",
        ] {
            assert!(!is_activity_row(row), "{row}");
        }
        assert!(!is_activity_row("✶ … (4s)"));
        let todo = screen(
            &[
                "⏺ Working through the list.",
                "",
                "✻ Implementing the parser… (2m · ↓ 3.1k tokens)",
                "  ⎿  ☒ Read the existing parser",
                "     ☐ Implement the parser",
                "     … +2 pending",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(signal(&todo).as_deref(), Some("status row: spinner"));

        // A waiting row above the transcript is no STATUS row; with no done
        // row or person's prompt under it, it is the wait Claude Code carries
        // over the turns it started itself (module docs): soft background
        // work, never the status row's own hard rule.
        for last in [
            "⏺ Stopped the background build.",
            "  ⎿  error: could not compile `x` (lib) due to 1 previous error",
        ] {
            let r = screen(
                &["✻ Waiting for 1 dynamic workflow to finish", "", last, ""],
                "  ⏵⏵ auto mode on (shift+tab to cycle)",
            );
            assert_eq!(status_row(&r), None, "{last}");
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: background work still running"),
                "{last}"
            );
            assert_eq!(busy_signal(&r).map(|b| b.soft), Some(true), "{last}");
        }
        let queued = screen(
            &[
                "✻ Waiting for 1 dynamic workflow to finish",
                "",
                "❯ keep going",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(
            signal(&queued).as_deref(),
            Some("status row: waiting for a dynamic workflow")
        );

        // A hint in a narrow window still starts past column 20.
        let hint = screen(
            &[
                "✶ Deliberating…",
                "                        Approve tool calls from your phone · /remote",
            ],
            "  ⏵⏵ auto mode on",
        );
        assert_eq!(signal(&hint).as_deref(), Some("status row: spinner"));
    }

    /// `body` over Claude Code's frame drawn `width` columns wide, `footer`
    /// under it: a pane of that width.
    fn pane(body: &[&str], width: usize, footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(["─".repeat(width), "❯".to_string(), "─".repeat(width)]);
        r.push(footer.to_string());
        r
    }

    /// SM-3 (KI S2R-F1, measured by the 2026-10-15 sweep at 12 columns): the
    /// status row is read as its LINE. A spinner cut at its text column
    /// (`✶ Choreog` over `  raphing…`) or in column 0 under the glyph reads
    /// busy, not idle; a done row wrapped before `still running` reads the
    /// shell it left running; a spinner's timing wrapped under it is its
    /// progress; the transcript ends before a cut spinner's line and past a
    /// wrapped done row's. NEGATIVE CONTROLS: the measured narrow fixtures
    /// keep their phase and status row (their wrapped done rows join to done
    /// rows); a done row cut before `done`, a done row with a tip, a hint or
    /// a queued message right under it, and a stray row under a full done
    /// row read idle; the whole spinner at 80 columns is a spinner.
    #[test]
    fn a_status_row_ink_wrapped_is_read_as_its_line() {
        for rest in ["  raphing…", "raphing…"] {
            let r = pane(&["⏺ Working.", "", "✶ Choreog", rest], 12, "  ⏵⏵ auto");
            assert_eq!(worker_phase(&r), Phase::Busy, "{rest}");
            assert_eq!(signal(&r).as_deref(), Some("status row: spinner"), "{rest}");
            assert_eq!(status_row(&r), Some("✶ Choreog"), "{rest}");
            // The spinner's line is the live zone, both its rows.
            assert_eq!(transcript_end(&r), 2, "{rest}");
        }
        // A READER test, not a measurement: the narrow fixtures measured so
        // far keep the footer's `· 1 shell` pill, which reads busy on its own;
        // this footer is cut before it by hand.
        let shell = pane(
            &[
                "⏺ Started the build in the background.",
                "",
                "✻ Worked for 9m 2s · done 3:15 PM · 1 shell still",
                "  running",
            ],
            50,
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for…",
        );
        assert_eq!(
            signal(&shell).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&shell), Phase::Busy);
        assert_eq!(
            status_line(&shell).as_deref(),
            Some("✻ Worked for 9m 2s · done 3:15 PM · 1 shell still running")
        );

        // NEGATIVE CONTROLS. The measured narrow screens (2.1.299 at 26 and
        // 34 columns) keep the phase and status row main read them by; the
        // transcript ends past a wrapped done row's whole line (its clock,
        // `  10:36 AM`, is the transcript's, not the live zone's).
        use crate::prompt::fixtures::{
            API_ERROR_529_34_2_1_299, WALL_SESSION_AFTER_CANCEL_26_2_1_299,
            WALL_SESSION_AUTO_CONTINUE_ARMED_26_2_1_299,
            WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299,
            WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299, screen as measured,
        };
        for (text, limited, status, line, end) in [
            (
                WALL_SESSION_AFTER_CANCEL_26_2_1_299,
                true,
                Some("✻ Crunched for 0s · done"),
                Some("✻ Crunched for 0s · done 10:36 AM"),
                42,
            ),
            (
                WALL_SESSION_AUTO_CONTINUE_ARMED_26_2_1_299,
                true,
                Some("✻ Worked for 0s · done"),
                Some("✻ Worked for 0s · done 10:36 AM"),
                23,
            ),
            (
                WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
                true,
                Some("✻ Baked for 0s · done 10:20 AM"),
                Some("✻ Baked for 0s · done 10:20 AM"),
                24,
            ),
            (
                API_ERROR_529_34_2_1_299,
                false,
                Some("✻ Brewed for 2m 58s · done 10:27"),
                Some("✻ Brewed for 2m 58s · done 10:27 AM"),
                19,
            ),
            (
                WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299,
                false,
                None,
                None,
                31,
            ),
            (
                WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299,
                true,
                None,
                None,
                38,
            ),
        ] {
            let r = measured(text);
            let phase = worker_phase(&r);
            assert_eq!(
                matches!(phase, Phase::Limited { .. }),
                limited,
                "{status:?}"
            );
            assert!(limited || phase == Phase::Idle, "{status:?}");
            assert_eq!(status_row(&r), status);
            assert_eq!(status_line(&r).as_deref(), line);
            assert_eq!(transcript_end(&r), end, "{status:?}");
            assert!(busy_signal(&r).is_none(), "{status:?}");
        }
        // A done row cut before `done`.
        let cut = pane(
            &["⏺ Fixed it.", "", "✻ Worked for 0s ·", "  done 1:02 PM"],
            20,
            "  ⏵⏵ auto",
        );
        assert_eq!(worker_phase(&cut), Phase::Idle);
        assert_eq!(
            status_line(&cut).as_deref(),
            Some("✻ Worked for 0s · done 1:02 PM")
        );
        // The done row's line is the transcript's, both its rows.
        assert_eq!(transcript_end(&cut), 4);
        // A full done row (34 columns) with a tip, a hint against the right
        // edge, a queued message, a stray row in column 0 or a word with an
        // ellipsis at its text column right under it: none is its line.
        for under in [
            "  ⎿  Tip: Use /clear to start fresh",
            "             ✔ Update installed",
            "❯ carry on…",
            "Thinking about it…",
            "  more to say…",
        ] {
            let r = pane(
                &[
                    "⏺ Fixed it.",
                    "",
                    "✻ Worked for 3m 2s · done 4:24 PM",
                    under,
                ],
                34,
                "  ⏵⏵ auto mode on (shift+tab to",
            );
            assert_eq!(signal(&r), None, "{under}");
            assert_eq!(
                status_line(&r).as_deref(),
                Some("✻ Worked for 3m 2s · done 4:24 PM"),
                "{under}"
            );
        }
        // The whole spinner at 80 columns.
        let whole = pane(
            &["⏺ Working.", "", "✶ Choreographing… (12s · ↓ 1.2k tokens)"],
            80,
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(signal(&whole).as_deref(), Some("status row: spinner"));
        assert_eq!(
            status_line(&whole).as_deref(),
            Some("✶ Choreographing… (12s · ↓ 1.2k tokens)")
        );
    }

    /// SM-4: the vendor's API-retry rows (2.1.300's `uyn`, read from the
    /// binary) read busy whatever their kind, with no `esc to interrupt` in
    /// the footer (a narrow pane, or any pane with a draft in the composer):
    /// a stalled stream, no response, the auto-mode check unavailable, a
    /// low-priority wait, an error with the limit's reset after its wait, a
    /// wait of days, and the measured 2.1.283 screens; and a retry row a
    /// narrow pane wrapped, read on its line. NEGATIVE CONTROLS: a done row,
    /// the compaction's row and the worker's own words in the vendor's
    /// shape read idle, and a done row's shell keeps its own rule.
    #[test]
    fn the_vendors_retry_rows_read_busy_whatever_their_kind() {
        let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
        for row in [
            "✻ Waiting for API response · will retry in 5s · check your network",
            "✻ No response from the API after 30s · retrying, waiting up to 1m · attempt 2/10",
            "✻ No response from the API after 30s · retrying once, waiting up to 1m",
            "✻ Auto mode check unavailable · next try in 5s · attempt 2/10 · esc to interrupt",
            "✻ Your request is queued · next try in 5s · attempt 2 · esc to interrupt",
            "✻ Usage limit reached · Retrying in 14s (9:50pm) · attempt 3/10",
            "✻ Usage limit reached · Retrying in 2d 3h · attempt 3/10",
        ] {
            let r = screen(&["⏺ Reading the parser.", "", row, ""], footer);
            assert_eq!(worker_phase(&r), Phase::Busy, "{row}");
            assert_eq!(signal(&r).as_deref(), Some("status row: spinner"), "{row}");
        }
        // No response, with its second row under it at column two.
        let proxy = screen(
            &[
                "⏺ Reading the parser.",
                "",
                "✻ No response from the API after 30s · retrying, waiting up to 1m · attempt 2/10",
                "  A proxy or gateway that buffers streaming responses can cause this.",
                "",
            ],
            footer,
        );
        assert_eq!(worker_phase(&proxy), Phase::Busy);
        // The measured 2.1.283 retry screens.
        use crate::prompt::fixtures::{
            API_RETRYING_DRAFT_MEASURED, API_RETRYING_MEASURED, API_RETRYING_QUEUED_80_MEASURED,
            API_RETRYING_QUEUED_DRAFT_MEASURED, API_RETRYING_QUEUED_MEASURED, screen as measured,
        };
        for text in [
            API_RETRYING_MEASURED,
            API_RETRYING_DRAFT_MEASURED,
            API_RETRYING_QUEUED_MEASURED,
            API_RETRYING_QUEUED_80_MEASURED,
            API_RETRYING_QUEUED_DRAFT_MEASURED,
        ] {
            assert_eq!(worker_phase(&measured(text)), Phase::Busy);
        }
        // SYNTHETIC: a retry row wrapped at its text column, the join's own
        // rule (the vendor draws a narrow retry row with its glyph on a row
        // of its own: [`a_status_glyph_ink_wrapped_away_from_its_text_is_the_status_row`]).
        let wrapped = pane(
            &[
                "⏺ Reading the parser.",
                "",
                "✻ Usage limit reached · Retrying",
                "  in 14s (9:50pm) · attempt 3/10",
            ],
            34,
            "  ⏵⏵ auto mode on (shift+tab to",
        );
        assert_eq!(worker_phase(&wrapped), Phase::Busy);
        // NEGATIVE CONTROLS.
        for row in [
            "✻ Worked for 0s · done 11:02 PM",
            "✻ Conversation compacted (ctrl+o for history)",
        ] {
            let r = screen(&["⏺ Fixed it.", "", row, ""], footer);
            assert_eq!(worker_phase(&r), Phase::Idle, "{row}");
        }
        let said = screen(
            &["⏺ It printed · Retrying in 5s · attempt 2/10", ""],
            footer,
        );
        assert_eq!(worker_phase(&said), Phase::Idle);
        let shell = screen(
            &[
                "⏺ Started it.",
                "",
                "✻ Churned for 4m · done 1:02 PM · 1 shell still running",
                "",
            ],
            footer,
        );
        assert_eq!(
            signal(&shell).as_deref(),
            Some("status row: a shell still running")
        );
    }

    /// A JOIN NEVER LOSES THE ROW'S OWN BUSY READING (the cross-lane review's
    /// screen F2): the vendor's no-response retry row draws its proxy hint
    /// under it at column 2 (2.1.300's `uyn`), and a pane 82-85 columns wide
    /// (72-75 for the last attempt's row) puts the row's end within the
    /// join's slack of the edge, so the hint was taken as the line's last
    /// item: with a draft in the composer (no `esc to interrupt`) the retry
    /// read idle. Both read busy. A row busy by its tail alone stands alone
    /// over such a hint (synthetic: `· esc to interrupt`, the last-item rule
    /// no clause of the hint keeps). NEGATIVE CONTROLS: the same hint under
    /// the turn's done row reads idle (the busy reading is the row's, never
    /// the hint's); a retry row wrapped at its text column, whose row alone is
    /// no retry, still joins and reads busy.
    #[test]
    fn a_status_line_join_never_loses_the_rows_own_busy_reading() {
        let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
        let hint = "A proxy or gateway that buffers streaming responses can cause this · set \
                    CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS to change the first wait";
        // The hint as Ink wraps it, greedily at spaces, at column 2.
        let wrapped = |width: usize| {
            let mut out: Vec<String> = Vec::new();
            let mut line = String::new();
            for word in hint.split(' ') {
                if !line.is_empty() && 2 + line.chars().count() + 1 + word.chars().count() > width {
                    out.push(format!("  {line}"));
                    line.clear();
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
            out.push(format!("  {line}"));
            out
        };
        let with_hint = |status: &str, width: usize| {
            let mut body = vec!["⏺ Reading the parser.".to_string(), String::new()];
            body.push(status.to_string());
            body.extend(wrapped(width));
            body.push(String::new());
            let body: Vec<&str> = body.iter().map(String::as_str).collect();
            pane(&body, width, footer)
        };
        for (row, widths) in [
            (
                "✻ No response from the API after 30s · retrying, waiting up to 1m · attempt 2/10",
                82..=85,
            ),
            (
                "✻ No response from the API after 30s · retrying once, waiting up to 1m",
                72..=75,
            ),
        ] {
            for width in widths {
                let r = with_hint(row, width);
                assert_eq!(worker_phase(&r), Phase::Busy, "{width}: {r:#?}");
                // NEGATIVE CONTROL: the hint under the done row.
                let done = with_hint("✻ Worked for 3m 2s · done 4:24 PM", width);
                assert_eq!(worker_phase(&done), Phase::Idle, "{width}: {done:#?}");
            }
        }
        // Busy by its tail alone: the row stands alone over the hint.
        let tail = "✻ Waiting on the server · esc to interrupt";
        for width in 43..=46 {
            let r = with_hint(tail, width);
            assert_eq!(status_line(&r).as_deref(), Some(tail), "{width}: {r:#?}");
            assert_eq!(worker_phase(&r), Phase::Busy, "{width}");
        }
        // NEGATIVE CONTROL: a true wrap still joins.
        let wrap = pane(
            &[
                "⏺ Reading the parser.",
                "",
                "✻ Usage limit reached · Retrying",
                "  in 14s (9:50pm) · attempt 3/10",
            ],
            34,
            "  ⏵⏵ auto mode on (shift+tab to",
        );
        assert_eq!(
            status_line(&wrap).as_deref(),
            Some("✻ Usage limit reached · Retrying in 14s (9:50pm) · attempt 3/10")
        );
        assert_eq!(worker_phase(&wrap), Phase::Busy);
    }

    /// THE STATUS GLYPH INK WRAPPED AWAY FROM ITS TEXT IS THE STATUS ROW (the
    /// cross-lane review's screen F1, MEASURED on 2.1.301): under about 44
    /// columns the vendor's 2-cell glyph box wraps, the glyph alone on its
    /// row and the text under it in column 0 — the retry row at 34, 38 and 42
    /// columns (at 34 and 38 the cut error and the dim tail interleaved), the
    /// spinner at 10. Each reads busy by its status row, its line the rows
    /// under the glyph; so does a retry mid-turn under a running tool, and
    /// one under a `·` glyph. NEGATIVE CONTROLS: each capture with the retry
    /// block replaced by the turn's done row reads idle; a lone glyph over a
    /// blank row is no status row (the walk goes on to the done row above),
    /// nor over a `⏺` row (the walk stops); a done row wrapped the same way
    /// is the status row and reads idle.
    ///
    /// UNDER 24 COLUMNS (the decision review's RR-LC-1, MEASURED on 2.1.301
    /// at 18, 20 and 22): the cut `API error` and the dim tail are two text
    /// columns side by side, and the line is read column by column — `✻ API
    /// error · Retrying in 4s · attempt 1/10`, busy — where row by row it
    /// read `Retrying in error 4s`, and at 18 the last row, filling the pane,
    /// was dropped as a hint. NEGATIVE CONTROLS: the block replaced by the
    /// done row reads idle, and a done row wrapped under a bare glyph over two
    /// rows stays one done line.
    #[test]
    fn a_status_glyph_ink_wrapped_away_from_its_text_is_the_status_row() {
        use crate::prompt::fixtures::screen as measured;
        let captures = [
            include_str!("fixtures/claude-2.1.301-retry-34col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-38col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-42col.txt"),
            include_str!("fixtures/claude-2.1.301-spinner-10col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-18col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-20col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-22col.txt"),
        ];
        let narrow = [
            include_str!("fixtures/claude-2.1.301-retry-18col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-20col.txt"),
            include_str!("fixtures/claude-2.1.301-retry-22col.txt"),
        ];
        for text in narrow {
            assert_eq!(
                status_line(&measured(text)).as_deref(),
                Some("✻ API error · Retrying in 4s · attempt 1/10")
            );
        }
        for text in captures {
            let r = measured(text);
            assert_eq!(worker_phase(&r), Phase::Busy, "{r:#?}");
            assert_eq!(signal(&r).as_deref(), Some("status row: spinner"), "{r:#?}");
            let line = status_line(&r).unwrap_or_default();
            assert!(line.starts_with("✻ API") || line == "✻ Infusing…", "{line}");
            // NEGATIVE CONTROL: the block replaced by the turn's done row.
            let mut done = r.clone();
            let at = done
                .iter()
                .rposition(|row| row == "✻")
                .expect("the bare glyph");
            let end = (at..done.len())
                .find(|&k| done[k].trim().is_empty() || is_rule(&done[k]))
                .unwrap_or(at + 1);
            done.splice(
                at..end,
                ["✻ Crunched for 3m 2s · done 12:55 PM".to_string()],
            );
            assert_eq!(worker_phase(&done), Phase::Idle, "{done:#?}");
        }
        let footer = "  ⏵⏵ auto mode on (shift+tab to";
        for glyph in ["✻", "·"] {
            let mid_turn = pane(
                &[
                    "⏺ Bash(targo test)",
                    "  ⎿  Running…",
                    "",
                    glyph,
                    "API     · Retrying in 1s · attempt",
                    "error  1/10",
                    "",
                ],
                34,
                footer,
            );
            assert_eq!(worker_phase(&mid_turn), Phase::Busy, "{glyph}");
        }
        // NEGATIVE CONTROLS.
        let over_blank = pane(
            &[
                "⏺ Fixed it.",
                "",
                "✻ Worked for 3m · done 1:02 PM",
                "",
                "✻",
                "",
            ],
            34,
            footer,
        );
        assert_eq!(worker_phase(&over_blank), Phase::Idle);
        assert_eq!(
            status_line(&over_blank).as_deref(),
            Some("✻ Worked for 3m · done 1:02 PM")
        );
        let over_said = pane(&["✻", "⏺ Here is the glyph.", ""], 34, footer);
        assert_eq!(worker_phase(&over_said), Phase::Idle);
        assert_eq!(status_line(&over_said), None);
        let done = pane(
            &[
                "⏺ Fixed it.",
                "",
                "✻",
                "Crunched for 30s · done 12:52 PM",
                "",
            ],
            34,
            footer,
        );
        assert_eq!(
            status_line(&done).as_deref(),
            Some("✻ Crunched for 30s · done 12:52 PM")
        );
        assert_eq!(worker_phase(&done), Phase::Idle);
        let done = pane(
            &[
                "⏺ Fixed it.",
                "",
                "✻",
                "Crunched for 30s ·",
                "done 12:52 PM",
                "",
            ],
            18,
            "  ⏵⏵ auto mode",
        );
        assert_eq!(
            status_line(&done).as_deref(),
            Some("✻ Crunched for 30s · done 12:52 PM")
        );
        assert_eq!(worker_phase(&done), Phase::Idle);
    }

    /// A HEAD THAT FILLS ITS COLUMN IS STILL READ BY ITS COLUMNS: the stalled
    /// stream's retry row (2.1.301's `aSn`), whose head `Waiting for API
    /// response` is never cut, laid out at 34 to 41 columns as the measured
    /// retry blocks are — the glyph on its own row, the head's column
    /// `Waiting for API` wide, so its first row has one space before the
    /// tail's ` · `. Read row by row that was `will retry in 2m response`,
    /// no wait, and with the footer's `esc to interrupt` cut away the retry
    /// read idle: the merge of main's strict retry rule typed `keep going`
    /// into it. It reads busy, its line the vendor's. NEGATIVE CONTROLS: the
    /// same block with a tail that is not the vendor's (`attempt soon`) stays
    /// the rows joined and idle, the done row in its place reads idle, so does
    /// a done row wrapped under its bare glyph, and rows that joined are a
    /// done row stay one (synthetic: split, they would read as a retry).
    #[test]
    fn a_retry_head_that_fills_its_column_is_still_read_by_its_columns() {
        let footer = "  ⏵⏵ auto mode on (shift+tab to";
        let at = |width: usize, block: &[&str]| {
            let mut body = vec!["⏺ Bash(targo test)", "  ⎿  Running…", ""];
            body.extend_from_slice(block);
            body.push("");
            pane(&body, width, footer)
        };
        for (width, wait, block) in [
            (
                34,
                "2m",
                &[
                    "✻",
                    "Waiting for API · will retry in 2m",
                    "response       · check your",
                    "               network",
                ][..],
            ),
            (
                35,
                "30s",
                &[
                    "✻",
                    "Waiting for API · will retry in 30s",
                    "response       · check your network",
                ][..],
            ),
            (
                36,
                "30s",
                &[
                    "✻",
                    "Waiting for API · will retry in 30s",
                    "response       · check your network",
                ][..],
            ),
            (
                40,
                "2m",
                &[
                    "✻",
                    "Waiting for API · will retry in 2m ·",
                    "response       check your network",
                ][..],
            ),
            (
                41,
                "30s",
                &[
                    "✻",
                    "Waiting for API · will retry in 30s ·",
                    "response       check your network",
                ][..],
            ),
        ] {
            let r = at(width, block);
            let line =
                format!("✻ Waiting for API response · will retry in {wait} · check your network");
            assert_eq!(worker_phase(&r), Phase::Busy, "{width}: {r:#?}");
            assert_eq!(status_line(&r).as_deref(), Some(line.as_str()), "{width}");
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: spinner"),
                "{width}"
            );
        }
        // NEGATIVE CONTROLS: a tail not the vendor's, the done row.
        let said = at(
            36,
            &[
                "✻",
                "Waiting for API · will retry in 30s",
                "response       · attempt soon",
            ],
        );
        assert_eq!(
            status_line(&said).as_deref(),
            Some("✻ Waiting for API · will retry in 30s response       · attempt soon")
        );
        assert_eq!(worker_phase(&said), Phase::Idle);
        let done = at(36, &["✻ Crunched for 3m 2s · done 12:55 PM"]);
        assert_eq!(worker_phase(&done), Phase::Idle);
        // The done row wrapped under its bare glyph, its second row short of
        // the first's ` · `: one done line.
        let wrapped = at(23, &["✻", "Crunched for 30s · done", "12:52 PM"]);
        assert_eq!(
            status_line(&wrapped).as_deref(),
            Some("✻ Crunched for 30s · done 12:52 PM")
        );
        assert_eq!(worker_phase(&wrapped), Phase::Idle);
        // SYNTHETIC: rows that joined are a done row stay one, though split
        // they would read as the vendor's retry.
        let shaped = at(
            34,
            &[
                "✻",
                "Worked for 3m · Retrying in 2m",
                "attempt      · attempt 2/10",
            ],
        );
        assert_eq!(
            status_line(&shaped).as_deref(),
            Some("✻ Worked for 3m · Retrying in 2m attempt      · attempt 2/10")
        );
        assert_eq!(worker_phase(&shaped), Phase::Idle);
    }

    /// BG-1: a turn Claude Code started itself (a `<task-notification>`, a
    /// monitor's expiry, a `/loop` wakeup) whose pending work did not change
    /// draws no done row (2.1.295 `Ad`, 2.1.300 `Nd`): the waiting row above
    /// its `⏺` rows still says what the session waits on, and reads SOFT
    /// background work (module docs) — measured as the collapsed shape that
    /// the trust session's journal continued 47 times — on its LINE where a
    /// narrow pane wrapped it, and nothing else busy ([`carried_wait_alone`]).
    /// NEGATIVE CONTROLS: a person's prompt between them, a drawn done row
    /// under the notification turn (the wait_bg18 shape, the work done) and
    /// woken turns drawn under that done row, the measured `/model` screen, a
    /// question the notification turn ended on, a wall it ended on, the drawn
    /// wait (wait_bg) read as before, and a busy footer under the carried
    /// wait (not alone).
    #[test]
    fn a_wait_claude_code_hid_under_its_own_wake_still_stands() {
        let footer = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        let collapsed = |end: &[&str]| {
            let mut body = vec![
                "✻ Waiting for 1 dynamic workflow to finish",
                "",
                "⏺ Monitor event: \"lander\"",
                "",
            ];
            body.extend_from_slice(end);
            body.push("");
            screen(&body, footer)
        };
        let r = collapsed(&["⏺ Still in its crates-tests lane. I restarted the watch."]);
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(
            busy_signal(&r),
            Some(Busy::soft(Zone::Status, "background work still running"))
        );
        assert_eq!(background_wait(&r), Some("background work still running"));
        assert!(background_only(&r));
        assert_eq!(status_row(&r), None);
        assert!(!status_unseen(&r), "a glyph row is on the screen");
        assert!(carried_wait_alone(&r));
        // A pane 16 columns wide: Ink wrapped the waiting row right after
        // `Waiting for`, and the wait is read on its LINE (module docs).
        let narrow = pane(
            &[
                "✻ Waiting for",
                "  1 dynamic",
                "  workflow to",
                "  finish",
                "",
                "⏺ Monitor event",
                "",
                "⏺ Restarted it.",
                "",
            ],
            16,
            "  ⏵⏵ bypass",
        );
        assert_eq!(worker_phase(&narrow), Phase::Busy);
        assert_eq!(
            busy_signal(&narrow),
            Some(Busy::soft(Zone::Status, "background work still running"))
        );
        assert!(carried_wait_alone(&narrow));

        // (a) A person's prompt between the waiting row and the turn.
        let mut prompted = vec![
            "✻ Waiting for 1 dynamic workflow to finish",
            "",
            "❯ keep going",
            "",
            "⏺ No change since my last update.",
            "",
        ];
        let r = screen(&prompted, footer);
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(busy_signal(&r), None);
        assert!(status_unseen(&r), "the walk stops at the prompt");
        // (b) The work finished: the completion turn drew its done row.
        let r = collapsed(&[
            "⏺ Dynamic workflow \"Second review pass\" completed · 5s",
            "",
            "⏺ The review is in; nothing to fix.",
            "",
            "✻ Cooked for 23m 0s · done 11:24 PM",
        ]);
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(busy_signal(&r), None);
        // ...and turns Claude Code started itself after that done row, drawn
        // with no row of their own: the walk over them meets the done row
        // first, and the waiting row above it is history.
        let r = screen(
            &[
                "✻ Waiting for 1 dynamic workflow to finish",
                "",
                "⏺ Dynamic workflow \"Second review pass\" completed · 5s",
                "",
                "⏺ The review is in; nothing to fix.",
                "",
                "✻ Cooked for 23m 0s · done 11:24 PM",
                "",
                "⏺ Monitor event: \"lander\"",
                "",
                "⏺ Restarted.",
                "",
            ],
            footer,
        );
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(busy_signal(&r), None);
        assert!(!status_unseen(&r), "the done row is on the screen");
        assert!(!carried_wait_alone(&r));
        // (c) The notification turn ended on a question.
        let r = collapsed(&["⏺ Shall I land it now?"]);
        assert_eq!(worker_phase(&r), Phase::Question);
        // ...or on a wall: the wall stands, behind the carried wait.
        let r = collapsed(&[
            "⏺ Background command \"Run the tests\" completed (exit code 0)",
            "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
        ]);
        assert!(matches!(worker_phase(&r), Phase::Limited { .. }), "{r:#?}");
        // (d) The measured `/model` screen: its `❯ /model` row stops the walk.
        let r = saved_screen(IDLE_AFTER_LIMIT);
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(busy_signal(&r), None);
        // (e) The drawn wait reads its own hard rule, as before.
        prompted.truncate(1);
        let drawn = screen(&prompted, footer);
        assert_eq!(
            signal(&drawn).as_deref(),
            Some("status row: waiting for a dynamic workflow")
        );
        assert_eq!(busy_signal(&drawn).map(|b| b.soft), Some(false));
        assert!(!carried_wait_alone(&drawn), "a status row is drawn");
        // The carried wait under a footer that reads busy is not alone.
        let footed = screen(
            &[
                "✻ Waiting for 1 dynamic workflow to finish",
                "",
                "⏺ Monitor event: \"lander\"",
                "",
            ],
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt",
        );
        assert_eq!(worker_phase(&footed), Phase::Busy);
        assert!(!carried_wait_alone(&footed), "the footer's own rule");
        // No glyph row anywhere above the frame: the screen says nothing of
        // how the last turn ended.
        let bare = screen(
            &["⏺ Monitor event: \"lander\"", "", "⏺ Restarted it.", ""],
            footer,
        );
        assert_eq!(worker_phase(&bare), Phase::Idle);
        assert!(status_unseen(&bare));
        assert!(!status_unseen(&drawn));
    }

    /// Without the composer frame — a plain shell, or a box drawn over the
    /// whole screen — every row is scanned exactly as before.
    #[test]
    fn with_no_composer_frame_every_row_is_scanned_as_before() {
        let shell = rows(&["$ make", "✶ Building…", "", "$ "]);
        assert_eq!(
            signal(&shell).as_deref(),
            Some("whole screen, no composer frame: spinner row")
        );
        assert!(!has_composer_frame(&shell));
        let no_bottom = rows(&[
            "✻ Waiting for 1 dynamic workflow to finish",
            "⏺ Done.",
            "──────────────────────────────",
            "❯",
        ]);
        assert_eq!(
            signal(&no_bottom).as_deref(),
            Some("whole screen, no composer frame: waiting for a dynamic workflow")
        );
        // A table border is not a rule.
        assert!(!is_rule("└──────────┴─────────┴───────────┘"));
        assert!(is_rule(
            "──────────────────────────────────────────────────── sandboxed ─"
        ));
        assert!(!is_rule("  ──────────────"));
    }

    // ---- what sits between the status row and the frame never hides it ----

    /// `wait_bg6.out`: a live `✽ Ebbing…` under which Claude Code parked a
    /// Tip and its session survey. The survey is neither the status row nor
    /// transcript: the spinner is read through it, whatever the footer says —
    /// here `esc to interrupt`, but also wait_bg7's idle auto-mode footer and
    /// wait_bg2's `paste again to expand`, which wait_bg2 shows under a live
    /// spinner.
    #[test]
    fn the_session_survey_never_hides_a_live_spinner() {
        let r = waiter_capture(include_str!("fixtures/wait_bg6.out"));
        assert_eq!(status_row(&r), Some("✽ Ebbing… (3m 2s · ↓ 10.8k tokens)"));
        assert_eq!(signal(&r).as_deref(), Some("status row: spinner"));
        for footer in [IDLE_AUTO_FOOTER, PASTE_FOOTER] {
            let quiet = with_footer(r.clone(), footer);
            assert_eq!(signal(&quiet).as_deref(), Some("status row: spinner"));
            assert_eq!(worker_phase(&quiet), Phase::Busy, "{footer}");
        }
    }

    /// `wait_bg7.out`: an idle worker whose done row sits over the survey. The
    /// last thing it said is its own last row, not the survey's options — and
    /// the same screen ending in a question is a question, with the survey or
    /// without it, with the done row or without it.
    #[test]
    fn the_survey_is_never_what_the_worker_said() {
        let r = waiter_capture(include_str!("fixtures/wait_bg7.out"));
        let said = "  bookkeeping, then site B, then compaction, then the post-merge reschedule. Waiting for your call.";
        assert_eq!(last_said_row(&r), Some(said));
        assert_eq!(worker_phase(&r), Phase::Idle);

        let asks = "  bookkeeping, then site B, then compaction, then the post-merge reschedule. Shall I start with the backfill bookkeeping?";
        let mut q = r.clone();
        let at = q.iter().position(|row| row == said).expect("the said row");
        q[at] = asks.to_string();
        assert_eq!(last_said_row(&q), Some(asks));
        assert_eq!(worker_phase(&q), Phase::Question);
        let mut no_survey = q.clone();
        no_survey.retain(|row| !is_survey(row));
        assert_eq!(worker_phase(&no_survey), Phase::Question);
        let mut no_done_row = q.clone();
        no_done_row.retain(|row| !row.starts_with("✻ "));
        assert_eq!(status_row(&no_done_row), None);
        assert_eq!(worker_phase(&no_done_row), Phase::Question);
    }

    // ---- the session survey: open above the composer, or only quoted ------

    /// `survey-open.txt`: the bottom 12 rows of a real worker's screen
    /// (measured 2026-09-13), idle under the survey, with a blank row and the
    /// update banner between its options row and the top rule. The words in
    /// the composer and the rule's label were replaced, as in the other
    /// fixtures.
    const SURVEY_OPEN: &str = include_str!("fixtures/survey-open.txt");

    /// The survey's two rows as Claude Code parks them.
    const SURVEY: [&str; 2] = [
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];

    /// The real screen: the survey is open, the worker idle — the survey is
    /// neither its status row nor what it said. Without the options row it
    /// is not open; nor is it without the composer frame.
    #[test]
    fn the_survey_parked_above_the_composer_is_open() {
        let r = saved_screen(SURVEY_OPEN);
        assert!(survey_open(&r));
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(status_row(&r), Some("✻ Cooked for 52m 54s · done 12:31 PM"));
        assert_eq!(last_said_row(&r), Some("  Free disk is 31 GiB."));

        let mut no_options = r.clone();
        no_options.retain(|row| !row.trim_start().starts_with("1: Bad"));
        assert!(!survey_open(&no_options));

        // wait_bg7: the done row over the survey, the survey on the rule.
        let bg7 = waiter_capture(include_str!("fixtures/wait_bg7.out"));
        assert!(survey_open(&bg7));
        assert!(!survey_open(&rows(&[SURVEY[0], SURVEY[1], "$ "])));
    }

    /// The survey is parked during turns too: under a live spinner it is
    /// open (wait_bg6; a spinner with the survey put above its frame), and
    /// on a screen with an approval box it is open as well — the box is
    /// what the worker waits on, so the phase is still prompt.
    #[test]
    fn a_survey_under_a_spinner_or_beside_a_box_is_open() {
        let bg6 = waiter_capture(include_str!("fixtures/wait_bg6.out"));
        assert!(survey_open(&bg6));
        assert_eq!(worker_phase(&bg6), Phase::Busy);

        let spinning = screen(
            &["⏺ Running the tests.", "", "✻ Synthesizing… (18s)"],
            "  esc to interrupt",
        );
        let spinning = above_frame(spinning, &SURVEY);
        assert!(survey_open(&spinning));
        assert_eq!(worker_phase(&spinning), Phase::Busy);

        let boxed = above_frame(bash_one_row(), &SURVEY);
        assert!(survey_open(&boxed));
        assert_eq!(worker_phase(&boxed), Phase::Prompt);
        assert!(!survey_open(&bash_one_row()));
    }

    // ---- the context indicator: how much is left before auto-compact -----

    /// `context-low.txt`: the bottom 7 rows of a real worker's screen (Claude
    /// Code 2.1.267/2.1.268, measured 2026-09-13), three hours into a turn:
    /// the spinner, a `⎿  Tip:` under it, and `1% until auto-compact`
    /// right-aligned on the row above the composer's top rule. The worker
    /// auto-compacted later in the task, and afterwards the row was gone.
    /// The rows Claude Code drew are as recorded; the two rules were not, and
    /// are drawn 138 columns wide — two past the indicator's end, where a
    /// hint ends — without the label the top rule may have carried.
    const CONTEXT_LOW: &str = include_str!("fixtures/context-low.txt");

    /// `text` as Claude Code parks it above a 120-column composer:
    /// right-aligned, ending two columns short of the rule.
    fn indicator(text: &str) -> String {
        format!("{}{text}", " ".repeat(118 - text.chars().count()))
    }

    /// The real screen reads `Some(1)`, and the worker is busy under it. The
    /// other spelling (`Context left until auto-compact: 7%`) reads too; a
    /// reading over 100, or the indicator with anything else on its row, is
    /// not the indicator; a framed screen without it reads `None`.
    #[test]
    fn the_context_indicator_above_the_composer_is_read() {
        let r = saved_screen(CONTEXT_LOW);
        assert_eq!(context_left(&r), Some(1));
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(
            status_row(&r),
            Some("✢ Booping… (3h 3m 27s · ↓ 143.3k tokens)")
        );
        assert!(!survey_open(&r));

        let idle = screen(
            &["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""],
            "  ? for shortcuts",
        );
        assert_eq!(context_left(&idle), None);
        for (text, left) in [
            ("Context left until auto-compact: 7%", Some(7)),
            ("0% until auto-compact", Some(0)),
            ("100% until auto-compact", Some(100)),
            ("101% until auto-compact", None),
            ("Context left until auto-compact: 300%", None),
            ("~1% until auto-compact", None),
            ("% until auto-compact", None),
            ("1% until auto-compact · run /compact now", None),
        ] {
            let r = above_frame(idle.clone(), &[indicator(text).as_str()]);
            assert_eq!(context_left(&r), left, "{text}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{text}");
            assert_eq!(last_said_row(&r), Some("⏺ Done."), "{text}");
        }
    }

    /// The indicator quoted in the transcript is not read: as the worker's
    /// `⏺` message, on a row of its message, in a tool's output under the `⎿`
    /// gutter (the first row or one under it), or flush left above the rule;
    /// nor a right-aligned copy above the status row (history); nor any of it
    /// without the composer frame.
    #[test]
    fn the_context_indicator_in_the_transcript_is_not_read() {
        for body in [
            vec!["⏺ 1% until auto-compact", ""],
            vec![
                "⏺ Claude Code shows this above the composer:",
                "  1% until auto-compact",
                "",
            ],
            vec![
                "⏺ Bash(tail -1 status.txt)",
                "  ⎿  1% until auto-compact",
                "",
            ],
            vec![
                "⏺ Bash(tail -2 status.txt)",
                "  ⎿  the indicator:",
                "     Context left until auto-compact: 7%",
                "",
            ],
            vec!["⏺ Done.", "", "1% until auto-compact"],
        ] {
            let r = screen(&body, "  ? for shortcuts");
            assert_eq!(context_left(&r), None, "{body:?}");
        }
        let old = indicator("4% until auto-compact");
        let history = screen(
            &[
                old.as_str(),
                "⏺ Done.",
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(context_left(&history), None);

        let one = indicator("1% until auto-compact");
        let bare = rows(&["⏺ Done.", one.as_str(), "$ "]);
        assert_eq!(context_left(&bare), None);
        let mut unframed = saved_screen(CONTEXT_LOW);
        unframed.retain(|row| !row.starts_with('─'));
        assert_eq!(context_left(&unframed), None);
    }

    /// The indicator shares the live zone with whatever else Claude Code
    /// parks there — a spinner and its `⎿  Tip:`, the session survey above it
    /// or under it — and is read beside them, the survey still open and the
    /// worker still busy.
    #[test]
    fn the_context_indicator_beside_a_survey_a_tip_and_a_spinner_is_read() {
        let busy = screen(
            &[
                "⏺ Running the tests.",
                "",
                "✻ Synthesizing… (18s)",
                "  ⎿  Tip: Use /clear to start fresh when switching topics and free up context",
            ],
            "  esc to interrupt",
        );
        let left = indicator("9% until auto-compact");
        for extra in [
            vec![SURVEY[0], SURVEY[1], "", left.as_str()],
            vec![left.as_str(), SURVEY[0], SURVEY[1]],
        ] {
            let r = above_frame(busy.clone(), &extra);
            assert_eq!(context_left(&r), Some(9), "{extra:?}");
            assert!(survey_open(&r), "{extra:?}");
            assert_eq!(worker_phase(&r), Phase::Busy, "{extra:?}");
            assert_eq!(status_row(&r), Some("✻ Synthesizing… (18s)"), "{extra:?}");
        }
        // The real screen, the survey parked between its indicator and the rule.
        let real = above_frame(saved_screen(CONTEXT_LOW), &SURVEY);
        assert_eq!(context_left(&real), Some(1));
        assert!(survey_open(&real));
    }

    /// `text` as Claude Code parks it above a composer whose rules are
    /// `width` columns: against the right edge, ending two columns short.
    fn parked(width: usize, text: &str) -> String {
        format!("{}{text}", " ".repeat(width - 2 - text.chars().count()))
    }

    /// A peer's indicator quoted in the LAST transcript block, with no status
    /// row under the block to end it: `aterm ctl @s-2 text` run on a
    /// 100-column peer shows the peer's indicator row under the `⎿` gutter,
    /// 82 columns in — right-aligned by the peer, so it ends at column 103 of
    /// this screen, well short of its right edge. It is the block's own row,
    /// not the indicator: not read over a box (in the tool's output, or in
    /// the worker's message), nor at idle under a `!` command's output, nor
    /// under a slash command's `⎿` row (`❯ /model`, the real screen) — and an
    /// indicator Claude Code parks against the right edge under the same
    /// block is still read.
    #[test]
    fn a_quoted_indicator_in_the_last_block_is_not_read() {
        let quote = format!("     {}1% until auto-compact", " ".repeat(77));
        let boxed = |head: &[&str]| {
            let mut r = rows(head);
            r.extend(bash_one_row().into_iter().skip(1));
            r
        };
        let mut model = saved_screen(IDLE_AFTER_LIMIT);
        let set = model
            .iter()
            .position(|r| r.trim_start().starts_with("⎿  Set model to"))
            .expect("the /model output");
        model.insert(set + 1, quote.clone());
        for (what, r) in [
            (
                "a tool's output over a box",
                boxed(&["⏺ Bash(aterm ctl @s-2 text)", "  ⎿  ✢ Booping…", &quote]),
            ),
            (
                "a message over a box",
                boxed(&["⏺ The peer's screen ends:", &quote]),
            ),
            (
                "a `!` command's output at idle",
                screen(
                    &["❯ !aterm ctl @s-2 text", "  ⎿  ✢ Booping…", &quote, ""],
                    "  ? for shortcuts",
                ),
            ),
            ("under `/model`'s output", model),
        ] {
            assert_eq!(context_left(&r), None, "{what}");
            let width = composer_frame(&r).map(|f| r[f.bottom].chars().count());
            let width = width.expect("framed");
            let parked = above_frame(r, &[parked(width, "4% until auto-compact").as_str()]);
            assert_eq!(context_left(&parked), Some(4), "{what}");
        }
    }

    /// In a narrow pane the indicator starts left of column 20 — `Context left
    /// until auto-compact: 8%` ending two columns short of a 55-column rule
    /// starts at column 18, `1% until auto-compact` in 40 columns at 17 — and
    /// is read all the same: it is against the right edge, where Claude Code
    /// parks it. What starts at a transcript column (5, the `⎿` gutter's
    /// text) is not, even against the edge.
    #[test]
    fn the_context_indicator_in_a_narrow_pane_is_read() {
        let narrow = |width: usize, text: &str| {
            let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
            r.push(parked(width, text));
            r.extend(["─".repeat(width), "❯".to_string(), "─".repeat(width)]);
            r.push("  esc to interrupt".to_string());
            r
        };
        let long = narrow(55, "Context left until auto-compact: 8%");
        assert_eq!(context_left(&long), Some(8));
        assert_eq!(worker_phase(&long), Phase::Busy);
        assert_eq!(context_left(&narrow(40, "1% until auto-compact")), Some(1));
        assert_eq!(context_left(&narrow(28, "1% until auto-compact")), None);
        assert_eq!(context_left(&narrow(29, "1% until auto-compact")), Some(1));
    }

    /// Claude Code 2.1.305 draws the indicator at the right end of its mode row under the
    /// composer (measured 2026-10-20 in the real-Claude lab, idle and busy, at 80 and 144
    /// columns): `context_left_shown` reads it there, and still reads it above the composer
    /// where `context_left` does. Not the indicator: a run on that row short of the right edge,
    /// one with words after it, the row's `◐ medium · /effort`, a copy in the transcript, and
    /// a screen with no composer.
    #[test]
    fn the_context_indicator_on_the_mode_row_is_shown() {
        let on_mode_row = |width: usize, hints: &str, end: &str| {
            let mut r = rows(&[
                "⏺ Hi! I am the local lab stub, not a model. (reply 1)",
                "",
                "✻ Churned for 0s · done 7:22 PM",
                "",
            ]);
            r.extend(["─".repeat(width), "❯".to_string(), "─".repeat(width)]);
            let pad = width - hints.chars().count() - end.chars().count();
            r.push(format!("{hints}{}{end}", " ".repeat(pad)));
            r
        };
        let idle = "  ⏸ manual mode on · ? for shortcuts";
        for width in [80, 144] {
            for hints in [idle, "  ⏸ manual mode on · esc to interrupt"] {
                for (end, left) in [
                    ("0% until auto-compact", 0),
                    ("37% until auto-compact", 37),
                    ("Context left until auto-compact: 8%", 8),
                ] {
                    let r = on_mode_row(width, hints, end);
                    assert_eq!(context_left_shown(&r), Some(left), "{width} {hints} {end}");
                }
            }
        }
        assert_eq!(context_left_shown(&saved_screen(CONTEXT_LOW)), Some(1));
        for (what, r) in [
            (
                "short of the edge",
                on_mode_row(80, idle, "0% until auto-compact      "),
            ),
            (
                "words after it",
                on_mode_row(80, idle, "0% until auto-compact · /compact"),
            ),
            (
                "the effort",
                on_mode_row(
                    80,
                    "  ⏸ manual mode on · ← for agents",
                    "◐ medium · /effort",
                ),
            ),
            (
                "quoted in the transcript",
                screen(&["⏺ 0% until auto-compact", ""], "  ? for shortcuts"),
            ),
            (
                "no composer",
                rows(&["⏺ Done.", "", "                0% until auto-compact"]),
            ),
        ] {
            assert_eq!(context_left_shown(&r), None, "{what}");
        }
    }

    /// A long hint in an 80-column window starts at column 8, not past column
    /// 20: it is passed like any row under the status row, so the live spinner
    /// above it is read — and it is never what the worker said. The same for
    /// a queued message and a wrapped todo item between the spinner and the
    /// frame.
    #[test]
    fn nothing_between_the_spinner_and_the_frame_hides_it_in_a_narrow_window() {
        let text = "get pinged when Claude finishes · enable push notifications in /config";
        let hint = format!("{}{text}", " ".repeat(78 - text.chars().count()));
        let narrow = |body: &[&str]| {
            let mut r = rows(body);
            r.extend([
                "─".repeat(80),
                "❯".to_string(),
                "─".repeat(80),
                "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string(),
            ]);
            r
        };
        for between in [
            vec![hint.as_str()],
            vec!["❯ also do X after this"],
            vec![
                "  ⎿  ☒ Read the existing parser and the three call sites that feed",
                "       it from the importer",
                "     ☐ Implement the parser",
            ],
        ] {
            let mut body = vec![
                "⏺ Running the tests.",
                "",
                "✻ Synthesizing… (18s · ↓ 1.1k tokens)",
            ];
            body.extend(between.iter().copied());
            let r = narrow(&body);
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: spinner"),
                "{between:?}"
            );
            assert_eq!(worker_phase(&r), Phase::Busy, "{between:?}");
        }
        let asks = narrow(&["⏺ Which of the two do you want?", "", hint.as_str()]);
        assert_eq!(
            last_said_row(&asks),
            Some("⏺ Which of the two do you want?")
        );
        assert_eq!(worker_phase(&asks), Phase::Question);

        // `Still working` under the status row is busy, as it was anywhere.
        let still = narrow(&[
            "⏺ Running the suite.",
            "",
            "                              Still working. Check in from your phone · /remote",
        ]);
        assert_eq!(signal(&still).as_deref(), Some("hint: Still working"));
        assert_eq!(worker_phase(&still), Phase::Busy);
    }

    /// `wait_bg17.out`: the turn ended while a `persistent` monitor runs,
    /// `· 1 monitor still running` on the done row and `· 1 monitor ·` in the
    /// footer. Both are the soft rule, so the status row says so even when the
    /// footer does not — and a question or a limit notice outranks it.
    #[test]
    fn a_monitor_still_running_is_soft_busy() {
        let r = waiter_capture(include_str!("fixtures/wait_bg17.out"));
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: a monitor still running")
        );
        assert_eq!(busy_signal(&r).map(|b| b.soft), Some(true));
        assert_eq!(worker_phase(&r), Phase::Busy);
        let quiet = with_footer(r.clone(), PASTE_FOOTER);
        assert_eq!(
            signal(&quiet).as_deref(),
            Some("status row: a monitor still running")
        );
        assert_eq!(worker_phase(&quiet), Phase::Busy);

        let said = "⏺ Two runs in, both binaries agree and every checksum verifies. Waiting for the next events.";
        let mut q = r.clone();
        let at = q.iter().position(|row| row == said).expect("the said row");
        q[at] =
            "⏺ Two runs in and both binaries agree. Should I stop after twelve runs to save time?"
                .to_string();
        assert_eq!(worker_phase(&q), Phase::Question);

        let wall = above_frame(
            r,
            &[
                "⏺ Monitor event: \"24-run load test progress (baseline vs candidate), one line per run\"",
                "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
                "✻ Churned for 0s · done 8:52 PM · 1 monitor still running",
            ],
        );
        assert_eq!(
            status_row(&wall),
            Some("✻ Churned for 0s · done 8:52 PM · 1 monitor still running")
        );
        assert_eq!(
            worker_phase(&wall),
            Phase::Limited {
                message: "You've reached your Fable limit. Run /usage-credits to continue or \
                          switch models with /model."
                    .to_string(),
                reset: None,
            }
        );
        // A shell is not soft: the turn it runs in will end and wake the worker.
        let shell = screen(
            &[
                "⏺ Should I also run the benchmarks?",
                "",
                "✻ Worked for 9s · done 9:00 AM · 1 shell still running",
            ],
            "  ⏵⏵ auto mode on",
        );
        assert_eq!(worker_phase(&shell), Phase::Busy);
    }

    /// WORK LEFT RUNNING UNDER A LABEL aterm HAS NO WORD FOR IS STILL THE
    /// AGENT'S BACKGROUND WORK: Claude Code 2.1.286 writes one label for
    /// whatever background tasks still run, on the done row (`· <label> still
    /// running`) and in the footer's pill — `1 Artifact comment monitor` for
    /// the task kinds `1 monitor` counts, `1 MCP task`, `2 background tasks`
    /// for mixed kinds, `◇ 1 cloud session`. `wait_bg17.out` with its label
    /// swapped is a break, never an idle point, wherever the label stands:
    /// both rows, the done row alone, the footer alone (no done row is drawn
    /// with the turn's duration turned off). NEGATIVE CONTROL: with nothing
    /// left running the same screen is idle.
    #[test]
    fn work_left_running_under_any_label_is_background_work() {
        let r = waiter_capture(include_str!("fixtures/wait_bg17.out"));
        let idle: Vec<String> = r
            .iter()
            .map(|row| {
                row.replace(" · 1 monitor still running", "")
                    .replace(" · 1 monitor ·", " ·")
            })
            .collect();
        assert_eq!(background_wait(&idle), None);
        assert_eq!(worker_phase(&idle), Phase::Idle);
        let done_row = "✻ Cogitated for 2s · done 8:50 PM";
        for label in [
            "1 Artifact comment monitor",
            "1 MCP task",
            "2 background tasks",
            "◇ 1 cloud session",
        ] {
            let footer = format!("  ⏵⏵ auto mode on · {label} · ← for agents · ↓ to manage");
            let both: Vec<String> = r
                .iter()
                .map(|row| row.replace("1 monitor", label))
                .collect();
            let mut done_alone = idle.clone();
            let at = done_alone
                .iter()
                .rposition(|row| row == done_row)
                .expect("done row");
            done_alone[at] = format!("{done_row} · {label} still running");
            let footer_alone = with_footer(idle.clone(), &footer);
            for (place, s) in [
                ("both", both),
                ("done row", done_alone),
                ("footer", footer_alone),
            ] {
                assert!(background_wait(&s).is_some(), "{label} on the {place}");
                assert_eq!(worker_phase(&s), Phase::Busy, "{label} on the {place}");
            }
        }
    }

    // ---- a manager's screen ------------------------------------------------

    /// The footer a MANAGER session showed on 0.86.0 (2026-09-15, rows
    /// constructed here, never copied): bypass permissions, one persistent
    /// Monitor (the manager's mail watcher), and under the footer the
    /// artifact bar, `⧉` and the pages' names.
    const MANAGER_FOOTER: &str = "  ⏵⏵ bypass permissions on · 1 monitor";
    const MANAGER_FOOTER_QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    const ARTIFACT_BAR: &str = "  ⧉  name · name";
    const WORKFLOW_LINE: &str =
        "  ◯ fan-out  collect the standings  1/4 agents done · 3m 2s · ↓ 12.4k tokens";

    /// The three ways the rows under the footer were laid out: the bar right
    /// under it, a blank row between, and a workflow's line between (the
    /// measured order: footer, blank, `◯` line, bar, blank).
    const UNDER: [&[&str]; 3] = [
        &[ARTIFACT_BAR],
        &["", ARTIFACT_BAR],
        &["", WORKFLOW_LINE, ARTIFACT_BAR, ""],
    ];

    /// A manager's screen: `body` over the composer, the footer, then `under`.
    fn manager(body: &[&str], footer: &str, under: &[&str]) -> Vec<String> {
        let mut r = screen(body, footer);
        r.extend(rows(under));
        r
    }

    /// **THE MANAGER'S FOOTER IS NOT A BOX.** `bypass permissions on · 1
    /// monitor` over the artifact bar reads what the live zone says: a
    /// spinner or a `Waiting for` row is busy, a workflow's line under the
    /// footer is busy, a finished turn with the monitor counted is busy on the
    /// soft rule (the monitor, [`Busy::soft`]) and loses to a question, and
    /// without the monitor it is idle — never `prompt`, in any of the three
    /// layouts under the footer.
    #[test]
    fn a_manager_footer_with_a_monitor_and_the_artifact_bar_reads_busy_or_idle() {
        let working = [
            "⏺ Dispatching the next item.",
            "",
            "✻ Orchestrating… (2m 3s · ↓ 12.1k tokens)",
            "",
        ];
        let waiting = [
            "⏺ Launched the workflow.",
            "",
            "✻ Waiting for 1 dynamic workflow…",
            "",
        ];
        let done = [
            "⏺ Merged and reported.",
            "",
            "✻ Worked for 3m 21s · done 8:50 PM",
            "",
        ];
        let asked = [
            "⏺ Two items left. Should I start the next one?",
            "",
            "✻ Worked for 9s · done 8:51 PM",
            "",
        ];
        for under in UNDER {
            let has_workflow = under.contains(&WORKFLOW_LINE);
            let r = manager(&working, MANAGER_FOOTER, under);
            assert_eq!(worker_phase(&r), Phase::Busy, "{under:?}");
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: spinner"),
                "{under:?}"
            );

            let r = manager(&waiting, MANAGER_FOOTER, under);
            assert_eq!(worker_phase(&r), Phase::Busy, "{under:?}");
            assert_eq!(busy_signal(&r).map(|b| b.soft), Some(false), "{under:?}");

            let r = manager(&done, MANAGER_FOOTER, under);
            assert_eq!(worker_phase(&r), Phase::Busy, "{under:?}");
            let expect = if has_workflow {
                "footer: a workflow running"
            } else {
                "footer: a monitor running"
            };
            assert_eq!(signal(&r).as_deref(), Some(expect), "{under:?}");
            assert_eq!(
                busy_signal(&r).map(|b| b.soft),
                Some(!has_workflow),
                "{under:?}"
            );

            let r = manager(&asked, MANAGER_FOOTER, under);
            let expect = if has_workflow {
                Phase::Busy
            } else {
                Phase::Question
            };
            assert_eq!(worker_phase(&r), expect, "{under:?}");

            let r = manager(&done, MANAGER_FOOTER_QUIET, under);
            let expect = if has_workflow {
                Phase::Busy
            } else {
                Phase::Idle
            };
            assert_eq!(worker_phase(&r), expect, "{under:?}");

            for body in [&working[..], &waiting, &done, &asked] {
                for footer in [MANAGER_FOOTER, MANAGER_FOOTER_QUIET] {
                    let r = manager(body, footer, under);
                    assert_eq!(parse_prompt(&r), None, "{body:?} {footer} {under:?}");
                    assert_ne!(
                        worker_phase(&r),
                        Phase::Prompt,
                        "{body:?} {footer} {under:?}"
                    );
                    assert!(!survey_open(&r) && limit_notice(&r).is_none(), "{under:?}");
                }
            }
        }
    }

    /// A worker's approval box as a manager's transcript shows it: a Monitor
    /// event (or a tool's output) that printed the worker's screen, under the
    /// `⎿` gutter, indented past it.
    const BOX_UNDER_GUTTER: [&str; 12] = [
        "⏺ Monitor event: \"worker watch\"",
        "  ⎿  EVENT prompt seq=41",
        "      Bash command",
        "",
        "        git push origin main",
        "        Push the branch",
        "",
        "      Do you want to proceed?",
        "      ❯ 1. Yes",
        "        2. No",
        "",
        "      Esc to cancel · Tab to amend",
    ];

    /// The same box quoted in the manager's own words (a `⏺` message's rows,
    /// indented as a code block is) — not under a gutter.
    const BOX_IN_PROSE: [&str; 9] = [
        "⏺ The worker is blocked on its box:",
        "",
        "   Bash command",
        "     git push origin main",
        "   Do you want to proceed?",
        "   ❯ 1. Yes",
        "     2. No",
        "   Esc to cancel · Tab to amend",
        "",
    ];

    /// **A WORKER'S BOX IN A MANAGER'S TRANSCRIPT IS NOT THE MANAGER'S
    /// PROMPT.** The one way a manager's screen reads `prompt` (the footer
    /// test above shows its live zone never does): an `Esc to cancel` row
    /// anywhere on it would be a box, and a manager's transcript carries
    /// its workers' boxes. Under the
    /// `⎿` gutter it is output, whatever is under it — the manager idle (the
    /// soft monitor), the manager busy. Quoted in prose — every row in the
    /// message's continuation column — it is never a box, whatever is under
    /// it, and a REAL box over the manager's composer is a prompt with this
    /// footer as with any.
    #[test]
    fn a_workers_box_in_a_managers_transcript_is_not_a_prompt() {
        let done_row = "✻ Worked for 12s · done 9:01 PM";
        let spinner = "✻ Orchestrating… (4s · ↓ 310 tokens)";
        for under in UNDER {
            let has_workflow = under.contains(&WORKFLOW_LINE);
            let with = |rest: &[&str], footer: &str, quoted: &[&str]| {
                let mut body: Vec<&str> = quoted.to_vec();
                body.extend_from_slice(rest);
                manager(&body, footer, under)
            };

            // Under the gutter: never a box — idle, soft-busy or busy.
            let r = with(&["", done_row, ""], MANAGER_FOOTER, &BOX_UNDER_GUTTER);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_eq!(crate::prompt::prompt_box_span(&r), None, "{under:?}");
            assert_eq!(worker_phase(&r), Phase::Busy, "{under:?}");
            assert_eq!(
                busy_signal(&r).map(|b| b.soft),
                Some(!has_workflow),
                "{under:?}"
            );
            let r = with(&["", done_row, ""], MANAGER_FOOTER_QUIET, &BOX_UNDER_GUTTER);
            let expect = if has_workflow {
                Phase::Busy
            } else {
                Phase::Idle
            };
            assert_eq!(worker_phase(&r), expect, "{under:?}");
            let r = with(&["", spinner, ""], MANAGER_FOOTER, &BOX_UNDER_GUTTER);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_eq!(worker_phase(&r), Phase::Busy, "{under:?}");
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: spinner"),
                "{under:?}"
            );
            // The gutter block as the LAST thing on the screen above the rule.
            let r = with(&[""], MANAGER_FOOTER_QUIET, &BOX_UNDER_GUTTER);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_eq!(worker_phase(&r), expect, "{under:?}");

            // In prose: history under the manager's later words or a done row.
            let later = ["⏺ Approved it; the push went through.", "", done_row, ""];
            let r = with(&later, MANAGER_FOOTER_QUIET, &BOX_IN_PROSE);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_eq!(worker_phase(&r), expect, "{under:?}");
            let r = with(&[done_row, ""], MANAGER_FOOTER, &BOX_IN_PROSE);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_ne!(worker_phase(&r), Phase::Prompt, "{under:?}");

            // Only a spinner under the prose copy: not a box either — every
            // row of it sits in the message's continuation column.
            let r = with(&[spinner, ""], MANAGER_FOOTER, &BOX_IN_PROSE);
            assert_eq!(parse_prompt(&r), None, "{under:?}");

            // A real box over the manager's composer: a prompt.
            let mut live = rows(&["⏺ Pushing the branch.", ""]);
            live.extend(rows(&[
                " Bash command",
                "",
                "   git push origin main",
                "   Push the branch",
                "",
                " Do you want to proceed?",
                " ❯ 1. Yes",
                "   2. No",
                "",
                " Esc to cancel · Tab to amend",
            ]));
            live.extend(composer(MANAGER_FOOTER));
            live.extend(rows(under));
            assert_eq!(worker_phase(&live), Phase::Prompt, "{under:?}");
            let p = parse_prompt(&live).expect("the live box");
            assert_eq!(p.kind, crate::prompt::PromptKind::Bash);
            assert_eq!(p.command, "git push origin main");
        }
    }

    // ---- the usage-limit wall ---------------------------------------------

    /// The fixture cut just before `❯ /model` (rows 1-56) with its frame and
    /// the banner over it (rows 59-63): the worker's turn ended on the limit.
    /// Kept with row 58 too — the `/model` output, without its `❯ /model` —
    /// the model switch is the last thing said, and the notice is history.
    #[test]
    fn the_limit_screen_cut_before_the_model_switch_is_limited() {
        let full = saved_screen(IDLE_AFTER_LIMIT);
        let mut r = full[..56].to_vec();
        r.extend_from_slice(&full[58..]);
        assert_eq!(signal(&r), None);
        assert_eq!(status_row(&r), Some("✻ Churned for 0s · done 11:30 PM"));
        assert_eq!(
            worker_phase(&r),
            Phase::Limited {
                message: "You've reached your Fable limit. Run /usage-credits to continue or \
                          switch models with /model."
                    .to_string(),
                reset: None,
            }
        );
        let mut switched = full[..56].to_vec();
        switched.extend_from_slice(&full[57..]);
        assert_eq!(limit_notice(&switched), None);
        assert_eq!(worker_phase(&switched), Phase::Idle);
    }

    /// The whole fixture: the same notice, but above a later user row — IDLE.
    #[test]
    fn a_limit_above_a_later_user_row_is_history() {
        let r = saved_screen(IDLE_AFTER_LIMIT);
        assert_eq!(limit_notice(&r), None);
        assert_eq!(worker_phase(&r), Phase::Idle);

        // A question asked after a limit notice and a later user row is a
        // question.
        let q = screen(
            &[
                "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                "",
                "❯ /model",
                "  ⎿  Set model to Opus 5 (1M context)",
                "",
                "❯ carry on with the parser",
                "",
                "⏺ The parser is done. Shall I start on the tests?",
                "",
            ],
            "  ⏵⏵ auto mode on",
        );
        assert_eq!(worker_phase(&q), Phase::Question);
    }

    /// A turn can start with no user row — a background command's completion
    /// (rows 52-53 of the fixture itself) or a monitor event. A notice above
    /// such a turn, when that turn ended normally, is history.
    #[test]
    fn a_limit_above_a_later_turn_without_a_user_row_is_history() {
        let full = saved_screen(IDLE_AFTER_LIMIT);
        for opener in [
            "⏺ Background command \"Rebuild the cache service\" completed (exit code 0)",
            "⏺ Monitor event: \"24-run load test progress (baseline vs candidate), one line per run\"",
        ] {
            let mut r = full[..56].to_vec();
            r.extend(rows(&[
                opener,
                "",
                "⏺ The rebuild passed and the fix is committed as 1a2b3c4.",
                "",
                "✻ Worked for 1m 12s · done 12:41 AM",
                "",
            ]));
            r.extend_from_slice(&full[58..]);
            assert_eq!(limit_notice(&r), None, "{opener}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{opener}");
        }
    }

    /// `wait_bg18.out`: two turns that ended on the limit, the second started
    /// by a background command's completion (no user row), under the survey
    /// and the update banner. LIMITED — the stale `Waiting for` row is
    /// history.
    #[test]
    fn wait_bg18_is_limited_after_a_background_completion() {
        let r = waiter_capture(include_str!("fixtures/wait_bg18.out"));
        assert_eq!(signal(&r), None);
        assert_eq!(status_row(&r), Some("✻ Churned for 0s · done 11:30 PM"));
        assert_eq!(
            worker_phase(&r),
            Phase::Limited {
                message: "You've reached your Fable limit. Run /usage-credits to continue or \
                          switch models with /model."
                    .to_string(),
                reset: None,
            }
        );
    }

    /// A RESET ENDS WHERE ITS TIME DOES ([`time_words`]), one table over
    /// every spelling. A line read across the rows Ink wrapped it onto runs
    /// on into the notice's next line when the row above it was full (at 34
    /// columns `(America/Los_Angeles)` over `/upgrade to increase your`,
    /// 2.1.299), or into a row under it at the same column; the transcript's
    /// own text has the next line after a newline. The time and its zone are
    /// the reset, nothing after them — but A DATE IS NEVER CUT OFF ITS TIME
    /// (the review of fix/r15-banner, 2026-10-14: `resets at 3pm on Oct 15`
    /// cut to `3pm` is read by `aterm-agent`'s `limit::parse_reset` as today's
    /// session clock). A key followed by no time is passed over: the
    /// countdown toast that follows a `/goal` echo names the time.
    #[test]
    fn a_reset_ends_where_its_time_does() {
        for (said, want) in [
            (
                "You've hit your session limit · resets 10:25am (America/Los_Angeles) /upgrade \
                 to increase your usage limit.",
                Some("10:25am (America/Los_Angeles)"),
            ),
            (
                "You've hit your session limit · resets 10:25am (America/Los_Angeles)\n/upgrade \
                 to increase your usage limit.",
                Some("10:25am (America/Los_Angeles)"),
            ),
            (
                "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \
                 /upgrade to increase your usage limit.",
                Some("Oct 15 at 12:33pm (America/Los_Angeles)"),
            ),
            (
                "⚠ Usage limit reached · limit resets 10:25am ⏵⏵ auto mode on (shift+tab to cycle)",
                Some("10:25am"),
            ),
            (
                "Usage limit reached · resets September 19, 11:05 am and then some",
                Some("September 19, 11:05 am"),
            ),
            (
                "You've hit your weekly limit · resets in 2h 30m at most",
                Some("in 2h 30m"),
            ),
            (
                "Usage limit reached · continuing shortly. Hang on",
                Some("shortly"),
            ),
            (
                "⚠ Usage limit reached · continuing shortly.",
                Some("shortly"),
            ),
            (
                "Usage limit reached · Continuing automatically at 11:05 am · esc to cancel",
                Some("11:05 am"),
            ),
            (
                "Claude usage limit reached. Your limit will reset at 3pm (America/New_York).",
                Some("3pm (America/New_York)"),
            ),
            ("5-hour limit reached ∙ resets 3am", Some("3am")),
            (
                "resets soon, then it resets 4pm (America/Los_Angeles)",
                Some("4pm (America/Los_Angeles)"),
            ),
            (
                "Goal paused · usage limit reached · send a message after it resets to \
                 continue ⚠ Usage limit reached · continuing automatically at 3pm · esc to cancel",
                Some("3pm"),
            ),
            (
                "Usage limit reached · resets at 3pm on Oct 15",
                Some("3pm on Oct 15"),
            ),
            (
                "Usage limit reached · resets 3pm on the 15th",
                Some("3pm on the 15th"),
            ),
            (
                "Usage limit reached · resets 9am tomorrow",
                Some("9am tomorrow"),
            ),
            (
                "Usage limit reached · resets 9am Monday",
                Some("9am Monday"),
            ),
            ("You've reached your Fable limit.", None),
            (
                "Goal paused · usage limit reached · send a message after it resets to continue",
                None,
            ),
            ("resets /upgrade to increase your usage limit.", None),
        ] {
            assert_eq!(reset_of(said).as_deref(), want, "{said}");
        }
    }

    /// A notice split over two rows keeps both, and the reset its first row
    /// names; a `<X> limit reached ∙ resets …` row is a notice; so is one in
    /// the footer where Claude Code draws its notifications — right-aligned,
    /// or stacked under the mode row in a narrow pane — but never a warning
    /// there, and never the same words where the `statusLine` command's rows
    /// stand, at column 2 above the mode row (KI-1015-14, 2.1.304).
    #[test]
    fn a_split_notice_and_a_footer_notice_are_read_whole() {
        let split = screen(
            &[
                "❯ carry on",
                "",
                "⏺ Picking the parser back up.",
                "  ⎿  5-hour limit reached ∙ resets 3am",
                "     /upgrade to increase your usage limit.",
                "",
                "✻ Worked for 5s · done 10:02 PM",
                "",
            ],
            "  ⏵⏵ auto mode on",
        );
        assert_eq!(
            worker_phase(&split),
            Phase::Limited {
                message: "5-hour limit reached ∙ resets 3am /upgrade to increase your usage limit."
                    .to_string(),
                reset: Some("3am".to_string()),
            }
        );
        let body = ["⏺ Done.", "", "✻ Worked for 5s · done 10:02 PM", ""];
        const NOTICE: &str = "You've hit your limit · resets 3am (Europe/London)";
        const MODE: &str = "  ⏵⏵ auto mode on";
        let limited = Phase::Limited {
            message: NOTICE.to_string(),
            reset: Some("3am (Europe/London)".to_string()),
        };
        let right = screen(&body, &crate::prompt::fixtures::right_aligned(MODE, NOTICE));
        assert_eq!(
            worker_phase(&right),
            limited,
            "right-aligned on the mode row"
        );
        let mut stacked = screen(&body, MODE);
        stacked.push(format!("  {NOTICE}"));
        assert_eq!(
            worker_phase(&stacked),
            limited,
            "stacked under the mode row"
        );
        let mut status_line = screen(&body, &format!("  {NOTICE}"));
        status_line.push(MODE.to_string());
        assert_eq!(
            worker_phase(&status_line),
            Phase::Idle,
            "a status line's words"
        );
        assert_eq!(
            crate::wall::wall(&status_line),
            None,
            "a status line's words"
        );
        let warns = screen(
            &body,
            "  ⏵⏵ auto mode on     Approaching usage limit · resets at 7pm",
        );
        assert_eq!(worker_phase(&warns), Phase::Idle);
    }

    /// THE VENDOR'S AUTO-MODE PARAGRAPH IS NO LAST WORD (KI-1015-1 LC-10,
    /// measured on 2.1.300: after a relaunch's `--resume` it is drawn under
    /// the resumed history, and was journaled as the agent's last words; the
    /// paragraph's words and wrap are 2.1.287's measured ones). A history
    /// that ended on a weekly limit reads as that limit, the paragraph passed
    /// over whole; one that ended on the agent's message reads as that
    /// message. NEGATIVE CONTROLS, each said: the agent's own words that open
    /// `Auto mode …` otherwise, and the vendor's words quoted in a tool's
    /// output.
    #[test]
    fn the_vendors_auto_mode_paragraph_is_no_last_word() {
        const PARAGRAPH: [&str; 4] = [
            "⏺ Auto mode lets Claude handle permission prompts automatically — Claude checks \
             each tool call for risky actions and prompt injection",
            "  before executing. Actions Claude identifies as safe are executed, while actions \
             Claude identifies as risky are blocked and Claude may",
            "  try a different approach. Ideal for long-running tasks. Sessions are slightly more \
             expensive. Claude can make mistakes that allow",
            "  harmful commands to run, it's recommended to only use in isolated environments. \
             Shift+Tab to change mode.",
        ];
        let resumed = |history: &[&str], after: &[&str]| {
            let mut body = vec![
                " ▐▛███▛█   Claude Code v2.1.300",
                "▝▜██████▀  Opus 5.5 · Claude Max",
                " ▝▝   ▝▝   /private/tmp/at.Y4ko/proj",
                "",
                "❯ carry on with the task",
            ];
            body.extend_from_slice(history);
            body.push("");
            body.extend_from_slice(after);
            body.push("");
            screen(
                &body,
                "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
            )
        };
        let limit = [
            "  ⎿  You've hit your weekly limit · resets Oct 18 at 6pm (America/Los_Angeles)",
            "     /upgrade to increase your usage limit.",
        ];
        let walled = resumed(&limit, &PARAGRAPH);
        let upgrade = walled
            .iter()
            .position(|r| r.contains("/upgrade to increase"))
            .expect("the notice's rest");
        assert_eq!(last_said_index(&walled), Some(upgrade));
        assert!(
            matches!(worker_phase(&walled), Phase::Limited { reset: Some(r), .. } if r.starts_with("Oct 18 at 6pm")),
            "{:?}",
            worker_phase(&walled)
        );
        assert_eq!(
            crate::wall::wall(&walled).map(|w| w.kind),
            Some(WallKind::UsageWeekly)
        );
        let said = resumed(
            &["", "⏺ The parser is green; the docs are next."],
            &PARAGRAPH,
        );
        assert_eq!(
            crate::turn::said_tail(&said).as_deref(),
            Some("The parser is green; the docs are next.")
        );
        assert_eq!(worker_phase(&said), Phase::Idle);
        // NEGATIVE CONTROLS: said.
        let own = resumed(
            &limit,
            &["⏺ Auto mode is on here, so I went ahead with the migration."],
        );
        assert_eq!(
            crate::turn::said_tail(&own).as_deref(),
            Some("Auto mode is on here, so I went ahead with the migration.")
        );
        assert_eq!(worker_phase(&own), Phase::Idle);
        let quoted = resumed(
            &limit,
            &[
                "⏺ Bash(cat notes.txt)",
                "  ⎿  Auto mode lets Claude handle permission prompts automatically — quoted",
            ],
        );
        let row = quoted
            .iter()
            .position(|r| r.contains("— quoted"))
            .expect("the output");
        assert_eq!(last_said_index(&quoted), Some(row));
        assert_eq!(worker_phase(&quoted), Phase::Idle);
    }

    /// A GENERIC FOOTER IS THE LIMIT THE TURN'S OWN ROW NAMES (KI-1015-1 W3,
    /// measured on 2.1.300; the rows hand-built in its shapes): the footer's
    /// `⚠ Usage limit reached · limit resets 4pm` names no window and no
    /// model, the turn's `⎿  You've hit your Sonnet limit · resets 4pm (…)`
    /// a model's bucket — read as the bucket, where the footer is
    /// ([`crate::wall::named_by_the_turn`]: the same reset, the zone aside).
    /// So too a weekly window and a spend limit. A generic notice that is the
    /// vendor's countdown (2.1.283's `⚠ Usage limit reached · continuing
    /// automatically at …` banner) over a weekly row takes its kind and keeps
    /// its countdown: the screen's wall ([`crate::wall::wall`]) keeps it in
    /// its message, footer or banner, where the supervisor reads that the
    /// vendor goes on by itself. NEGATIVE CONTROLS, the footer
    /// kept: the turn's row a session limit (no kind of its own over the
    /// footer's); a footer that names its window (`You've hit your session
    /// limit · …`); the bucket's words as a tool's output, under its call,
    /// keep the footer's kind and place (F2 of the merge review, 2026-10-16:
    /// read as the bucket, the default fallback relaunched on opus under the
    /// session limit) — the turn's own notice below the call still names it.
    #[test]
    fn a_generic_footer_notice_yields_to_the_turns_own_limit_row() {
        let walled = |row: &str, footer: &str| {
            let mut r = screen(
                &[
                    "❯ carry on with the task",
                    row,
                    "     /upgrade to increase your usage limit.",
                    "",
                    "✻ Baked for 0s · done 3:11 PM",
                    "",
                ],
                footer,
            );
            r.push("  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string());
            r
        };
        const GENERIC: &str = "  ⚠ Usage limit reached · limit resets 4pm";
        let bucket = walled(
            "  ⎿  You've hit your Sonnet limit · resets 4pm (America/Los_Angeles)",
            GENERIC,
        );
        let read = notice_placed(&bucket, &classify_wall).expect("a wall");
        assert_eq!(
            (read.kind, read.placement, read.reset.as_deref()),
            (
                WallKind::ModelBucket { consent: false },
                Placement::Footer,
                Some("4pm")
            ),
            "{read:?}"
        );
        assert!(
            crate::wall::wall(&bucket).is_some_and(|w| w.kind == read.kind),
            "the screen's wall is the bucket"
        );
        for (row, footer, kind) in [
            (
                "  ⎿  You've hit your weekly limit · resets Oct 18 at 4pm (America/Los_Angeles)",
                "  ⚠ Usage limit reached · limit resets Oct 18 at 4pm",
                WallKind::UsageWeekly,
            ),
            (
                "  ⎿  You're out of extra usage · resets 4pm",
                GENERIC,
                WallKind::Spend,
            ),
        ] {
            let read = notice_placed(&walled(row, footer), &classify_wall);
            assert_eq!(read.map(|w| w.kind), Some(kind), "{row}");
        }
        // The generic notice that is the vendor's COUNTDOWN (2.1.283's
        // banner: no window, no model, the reset's clock) takes the weekly
        // row's kind, and its message keeps the countdown, where
        // `resumes_by_itself` reads that the vendor goes on — in the footer
        // and in the live zone above the composer alike.
        const WEEKLY: &str =
            "  ⎿  You've hit your weekly limit · resets Oct 18 at 4pm (America/Los_Angeles)";
        const BANNER: &str =
            "⚠ Usage limit reached · continuing automatically at Oct 18 at 4pm · esc to cancel";
        let footer = walled(WEEKLY, &format!("  {BANNER}"));
        let mut above = walled(WEEKLY, "");
        let top = above
            .iter()
            .position(|r| r.starts_with('╭') || r.starts_with('─'))
            .expect("the composer's top");
        above.insert(top, BANNER.to_string());
        for (name, r) in [("footer", &footer), ("above", &above)] {
            let read = crate::wall::wall(r).expect("a wall");
            assert_eq!(read.kind, WallKind::UsageWeekly, "{name}: {read:?}");
            assert!(
                read.message.starts_with("You've hit your weekly limit")
                    && read
                        .message
                        .contains("continuing automatically at Oct 18 at 4pm"),
                "{name}: {read:?}"
            );
        }
        // NEGATIVE CONTROLS: the footer stands.
        let session = walled(
            "  ⎿  You've hit your session limit · resets 4pm (America/Los_Angeles)",
            GENERIC,
        );
        let named = walled(
            "  ⎿  You've hit your Sonnet limit · resets 4pm (America/Los_Angeles)",
            "  ⚠ You've hit your session limit · resets 4pm",
        );
        for r in [&session, &named] {
            let read = notice_placed(r, &classify_wall).expect("a wall");
            assert_eq!(
                (read.kind, read.placement),
                (WallKind::UsageSession, Placement::Footer),
                "{read:?}"
            );
        }
        // The bucket's words in a tool's output, under its call — a later
        // `⎿` row, the output's first row, a row the output continues onto —
        // name no limit of the turn's: the footer is the session limit it
        // says, and no model fallback runs on another tool's words.
        const QUOTE: &str = "You've hit your Sonnet limit · resets 4pm (America/Los_Angeles)";
        let under = |output: &[String]| {
            let mut r = bucket.clone();
            r.splice(
                0..2,
                std::iter::once("⏺ Bash(cat limits.log)".to_string()).chain(output.iter().cloned()),
            );
            r
        };
        let later = under(&["  ⎿  Pasted earlier:".to_string(), format!("  ⎿  {QUOTE}")]);
        let first = under(&[format!("  ⎿  {QUOTE}")]);
        let continued = under(&["  ⎿  Pasted earlier:".to_string(), format!("     {QUOTE}")]);
        let mut asked = later.clone();
        asked.insert(0, "❯ carry on with the task".to_string());
        for (name, r) in [
            ("later", &later),
            ("first", &first),
            ("continued", &continued),
            ("asked", &asked),
        ] {
            let read = notice_placed(r, &classify_wall).expect("a wall");
            assert_eq!(
                (read.kind, read.placement, read.reset.as_deref()),
                (WallKind::UsageSession, Placement::Footer, Some("4pm")),
                "{name}: {read:?}"
            );
            assert_eq!(
                crate::wall::wall(r).map(|w| w.kind),
                Some(WallKind::UsageSession),
                "{name}"
            );
        }
        // CONTROL: the turn's own notice under the person's next prompt,
        // below the quoting call, is the bucket.
        let mut own = later.clone();
        let at = own
            .iter()
            .position(|r| r.starts_with("✻ Baked"))
            .expect("the done row");
        own.splice(
            at..at,
            [
                "❯ carry on with the task".to_string(),
                format!("  ⎿  {QUOTE}"),
                "     /upgrade to increase your usage limit.".to_string(),
                String::new(),
            ],
        );
        let read = notice_placed(&own, &classify_wall).expect("a wall");
        assert_eq!(
            (read.kind, read.placement),
            (WallKind::ModelBucket { consent: false }, Placement::Footer),
            "{read:?}"
        );
    }

    /// The auto-continue notice (its text measured 2026-09-17 13:50 on the
    /// live worker, the rows around it reconstructed): a banner row Claude
    /// Code parks in the live zone, `⚠` first, that names no reset the old
    /// spellings had — and it STAYS on the screen while the worker resumes
    /// under it. Alone above the top rule, with nothing busy under it, the
    /// worker is LIMITED, and the reset is the time it says it continues at
    /// (`1:50pm`), or `shortly`; with a spinner status row under it, or a
    /// busy footer (`esc to interrupt`, `2 shells`), the worker is BUSY —
    /// the notice is history the moment the worker works — and the same
    /// three rows in the footer or under the `⎿` gutter read the same way.
    /// A banner above a done row is history, idle.
    #[test]
    fn the_auto_continue_notice_is_limited_alone_and_busy_once_the_worker_works() {
        const NOTICE: &str =
            "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel";
        const SHORTLY: &str = "⚠ Usage limit reached · continuing shortly · esc to cancel";
        const SPINNER: &str = "✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)";
        const IDLE_FOOTER: &str = "  ⏵⏵ auto mode on (shift+tab to cycle)";
        const BUSY_FOOTER: &str =
            "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · 2 shells";
        let transcript = [
            "❯ run the campaign",
            "",
            "⏺ Bash(./run.sh --queue 8)",
            "  ⎿  queued 8 runs",
            "",
        ];
        let with = |live: &[&str], footer: &str| {
            let mut body: Vec<&str> = transcript.to_vec();
            body.extend_from_slice(live);
            screen(&body, footer)
        };
        // The wait: the notice alone, nothing busy under it.
        let waiting = with(&[NOTICE], IDLE_FOOTER);
        assert_eq!(signal(&waiting), None);
        assert_eq!(
            limit_notice(&waiting),
            Some((NOTICE.to_string(), Some("1:50pm".to_string())))
        );
        assert_eq!(
            worker_phase(&waiting),
            Phase::Limited {
                message: NOTICE.to_string(),
                reset: Some("1:50pm".to_string()),
            }
        );
        assert_eq!(
            worker_phase(&with(&[SHORTLY], IDLE_FOOTER)),
            Phase::Limited {
                message: SHORTLY.to_string(),
                reset: Some("shortly".to_string()),
            }
        );
        // The exact three rows of the live defect: the notice, a spinner
        // status row, the busy footer — BUSY, by the status row.
        let resumed = with(&[NOTICE, SPINNER], BUSY_FOOTER);
        assert_eq!(signal(&resumed).as_deref(), Some("status row: spinner"));
        assert_eq!(worker_phase(&resumed), Phase::Busy);
        // The footer alone busy, no spinner: busy by the footer.
        let shells = with(&[NOTICE], BUSY_FOOTER);
        assert_eq!(signal(&shells).as_deref(), Some("footer: esc to interrupt"));
        assert_eq!(worker_phase(&shells), Phase::Busy);
        assert_eq!(
            worker_phase(&with(&[SHORTLY, SPINNER], IDLE_FOOTER)),
            Phase::Busy
        );
        // The turn over, the notice above the done row is history.
        let ended = with(
            &[
                NOTICE,
                "⏺ All 8 runs are in.",
                "",
                "✻ Worked for 3m 2s · done 2:10 PM",
                "",
            ],
            IDLE_FOOTER,
        );
        assert_eq!(limit_notice(&ended), None);
        assert_eq!(worker_phase(&ended), Phase::Idle);
        // The same notice in the footer, and under the gutter: limited alone,
        // busy with a spinner under it.
        let footer_wait = with(&[], &format!("  {NOTICE}"));
        assert_eq!(
            worker_phase(&footer_wait),
            Phase::Limited {
                message: NOTICE.to_string(),
                reset: Some("1:50pm".to_string()),
            }
        );
        assert_eq!(
            worker_phase(&with(&[SPINNER], &format!("  {NOTICE}"))),
            Phase::Busy
        );
        // Under the gutter, hanging from the worker's words as Claude Code
        // hangs its notices from the last thing drawn.
        const SAID: &str = "⏺ The runs are queued.";
        let gutter = format!("  ⎿  {SHORTLY}");
        assert_eq!(
            worker_phase(&with(&[SAID, gutter.as_str(), ""], IDLE_FOOTER)),
            Phase::Limited {
                message: SHORTLY.to_string(),
                reset: Some("shortly".to_string()),
            }
        );
        assert_eq!(
            worker_phase(&with(&[SAID, gutter.as_str(), "", SPINNER], BUSY_FOOTER)),
            Phase::Busy
        );
        // A banner that is not a notice stays what it was: parked, not the
        // wall.
        let banner = with(&["✔ Update installed · Restart to update"], IDLE_FOOTER);
        assert_eq!(worker_phase(&banner), Phase::Idle);
        // The continuation cancelled by hand (`esc`), the turn ended on the
        // notice: the notice above a done row with nothing said between is
        // the wall still — and so is one drawn against the right edge, where
        // Claude Code parks its update banner.
        let cancelled = with(
            &[NOTICE, "", "✻ Worked for 3m 2s · done 1:52 PM", ""],
            IDLE_FOOTER,
        );
        assert_eq!(
            worker_phase(&cancelled),
            Phase::Limited {
                message: NOTICE.to_string(),
                reset: Some("1:50pm".to_string()),
            }
        );
        let right = format!("{NOTICE:>118}");
        assert_eq!(
            worker_phase(&with(&[right.as_str()], IDLE_FOOTER)),
            Phase::Limited {
                message: NOTICE.to_string(),
                reset: Some("1:50pm".to_string()),
            }
        );
        // A copy of the notice in a tool's output — a peer's screen the
        // worker read, indented under the gutter — is the worker's words,
        // not the wall.
        let quoted = with(
            &[
                "⏺ Bash(aterm ctl @s-9 text)",
                "  ⎿  ✻ Worked for 3m 2s · done 1:50 PM",
                &format!("     {NOTICE}"),
                "",
            ],
            IDLE_FOOTER,
        );
        assert_eq!(limit_notice(&quoted), None);
        assert_eq!(worker_phase(&quoted), Phase::Idle);
    }

    /// A copy of the notice in a tool's output can sit anywhere on its row:
    /// `aterm ctl @s-9 text` of a peer whose Claude Code parks the banner
    /// against ITS right edge lands the row right-aligned under the `⎿`
    /// gutter — twenty columns in and more, where a hint starts. The rows
    /// under the gutter are the worker's words whatever their column; the
    /// wall is never read there.
    #[test]
    fn a_right_aligned_copy_under_the_gutter_is_not_the_wall() {
        const NOTICE: &str =
            "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel";
        let right = format!("     {NOTICE:>100}");
        let quoted = screen(
            &[
                "❯ is the other worker limited?",
                "",
                "⏺ Bash(aterm ctl @s-9 text | grep limit)",
                "  ⎿  ✻ Worked for 3m 2s · done 1:50 PM",
                &right,
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(limit_notice(&quoted), None, "{right:?}");
        assert_eq!(worker_phase(&quoted), Phase::Idle);
        // A copy against the screen's own right edge, as the peer's was,
        // reads the same — however many rows of the block are above it.
        let edge = format!("{NOTICE:>118}");
        let quoted = screen(
            &[
                "⏺ Bash(aterm ctl @s-9 text)",
                "  ⎿  ⏺ Running the A/B on m7 now.",
                "     ✻ Worked for 3m 2s · done 1:50 PM",
                &edge,
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(worker_phase(&quoted), Phase::Idle);
        // A blank row under the gutter ends the block: a banner after it,
        // wherever Claude Code draws it, is the wall.
        let after = screen(
            &["⏺ Bash(./run.sh)", "  ⎿  queued 8 runs", "", &edge],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert!(matches!(worker_phase(&after), Phase::Limited { .. }));
    }

    /// The worker's words about limits are never the wall: its `⏺` prose, a
    /// later paragraph or a bullet of it, a tool's output it went on to talk
    /// about, a quoted message — in the first person or the second. Nor are
    /// the manager's words, wrapped onto the rows under its `❯`, nor a shell's
    /// output. A warning is not the wall; a busy worker is busy whatever its
    /// transcript says.
    #[test]
    fn words_about_limits_are_not_the_wall() {
        let tail = "  ⏵⏵ auto mode on (shift+tab to cycle)";
        let done = "✻ Worked for 20s · done 9:01 AM";
        for (body, want) in [
            (
                vec!["⏺ Should I add a rate limit to the client?"],
                Phase::Question,
            ),
            (
                vec![
                    "⏺ The retry path now honours the server's rate limit and the",
                    "  usage limit headers.",
                ],
                Phase::Idle,
            ),
            (
                vec![
                    "⏺ Implemented the retry.",
                    "",
                    "  The client now honours the server's rate limit headers. Should I also add jitter?",
                ],
                Phase::Question,
            ),
            (
                vec![
                    "⏺ Done:",
                    "",
                    "  - the importer respects the usage limit of the upstream API",
                    "  - the tests pass",
                ],
                Phase::Idle,
            ),
            (
                vec![
                    "⏺ The server now prints \"you've hit your memory limit (512 MiB)\" before the kernel would kill it.",
                ],
                Phase::Idle,
            ),
            (
                vec![
                    "⏺ Once you've reached your API limit the client backs off; should I also cap retries?",
                ],
                Phase::Question,
            ),
            (
                vec![
                    "⏺ The deploy failed: you've hit your GitHub API rate limit. Should I retry in an hour?",
                ],
                Phase::Question,
            ),
            (
                vec![
                    "⏺ Bash(gh api repos/o/r/pulls)",
                    "  ⎿  Error: API rate limit exceeded for user ID 1234.",
                    "",
                    "⏺ GitHub throttled the call. Want me to wait and retry?",
                ],
                Phase::Question,
            ),
            (
                vec![
                    "❯ find where we throttle requests",
                    "",
                    "⏺ Bash(grep -rn \"rate limit\" crates/net)",
                    "  ⎿  crates/net/src/client.rs:88:    // back off when the server answers 429 (rate limit)",
                    "     crates/net/src/client.rs:91:    let retry = parse_retry_after(&resp);",
                    "",
                    "⏺ The throttle lives in crates/net/src/client.rs:88; it honours Retry-After.",
                ],
                Phase::Idle,
            ),
            (
                vec![
                    "❯ Add a retry to the client: when the server answers 429, treat it as a",
                    "  rate limit, back off exponentially and try again. Then run the tests.",
                    "",
                    "⏺ Added the backoff and the tests pass.",
                ],
                Phase::Idle,
            ),
            (
                vec![
                    "❯ Add a retry to the client: when the server answers 429, treat it as a",
                    "  rate limit, back off exponentially and try again. Then run the tests.",
                ],
                Phase::Idle,
            ),
            (
                vec![
                    "⏺ Done.",
                    "                                     Approaching usage limit · resets at 7pm",
                ],
                Phase::Idle,
            ),
        ] {
            let mut full = body.clone();
            full.extend(["", done, ""]);
            let r = screen(&full, tail);
            assert_eq!(limit_notice(&r), None, "{body:?}");
            assert_eq!(worker_phase(&r), want, "{body:?}");
        }

        let still_running = screen(
            &[
                "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                "",
                "✶ Retrying…",
            ],
            tail,
        );
        assert_eq!(worker_phase(&still_running), Phase::Busy);
        let shell = rows(&[
            "$ grep -rn 'rate limit' src",
            "src/client.rs:88: // back off on 429 (rate limit)",
            "  ⎿  You've hit your session limit",
            "$ ",
        ]);
        assert_eq!(worker_phase(&shell), Phase::Idle);
        let api = screen(
            &[
                "⏺ Running the suite again.",
                "  ⎿  API Error: Rate limit reached for requests",
                "",
            ],
            tail,
        );
        assert_eq!(
            worker_phase(&api),
            Phase::Limited {
                message: "API Error: Rate limit reached for requests".to_string(),
                reset: None,
            }
        );
        // Claude Code 2.1.283's own 429 is a message of its own, its status
        // in parentheses: the same limit (`error_row_notice`).
        let own = screen(
            &[
                "⏺ Running the suite again.",
                "",
                "⏺ API Error: Request rejected (429) · this may be a temporary capacity issue.",
                "",
            ],
            tail,
        );
        assert_eq!(
            worker_phase(&own),
            Phase::Limited {
                message: "API Error: Request rejected (429) · this may be a temporary capacity \
                          issue."
                    .to_string(),
                reset: None,
            }
        );
    }

    /// THE CURSOR CAN ONLY PROVE TYPED TEXT, NEVER A PLACEHOLDER
    /// ([`cursor_inside_text`]): inside the text of the caret row it does; at
    /// column 2 (a placeholder's place, and where a draft whose caret went
    /// home leaves it), on another row (a displaced grid) or absent it says
    /// nothing. NEGATIVE CONTROL: the old reading, the cursor's row alone, took
    /// the displaced grid's rule glyph below the prompt for the composer.
    #[test]
    fn the_cursor_only_proves_text_it_stands_inside() {
        let caret = 55;
        assert!(cursor_inside_text(Some((55, 9)), caret));
        assert!(cursor_inside_text(Some((55, 3)), caret));
        assert!(
            !cursor_inside_text(Some((55, 2)), caret),
            "a placeholder's place"
        );
        assert!(
            !cursor_inside_text(Some((56, 2)), caret),
            "the rule below a displaced grid"
        );
        assert!(
            !cursor_inside_text(Some((56, 9)), caret),
            "another row says nothing"
        );
        assert!(!cursor_inside_text(None, caret));
    }

    /// THE FRAME'S REACH IS EVERY ROW ITS SEARCH READS ([`FrameReach`]): on
    /// every fixture screen, whole and cut to two widths, each row above
    /// [`FrameReach::from`] — one at a time, and all of them at once —
    /// replaced by each of a set of rows the search tests for (a rule, a
    /// caret, an option-like caret, an indented `❯`, a list's entry, a mode
    /// row, a blank, words) leaves the frame and the reach as they were.
    ///
    /// NEGATIVE CONTROLS: the reach is no deeper than it must be — on every
    /// screen with a frame whose first read row is its top rule, that row
    /// replaced by words moves the frame (a reach one row shallower would be
    /// refuted); and a `❯` row past column 0 under the caret, whose list
    /// walk goes up the screen, reaches the screen's first row — on a screen
    /// where the walk from the `❯` under a later rule finds the rule three
    /// rows over the caret's and reads that `❯` as a list's entry, that rule
    /// taken away makes it the caret, and the frame moves to it.
    #[test]
    fn the_frame_reach_is_every_row_the_search_reads() {
        let rule = "\u{2500}".repeat(30);
        let swaps: Vec<String> = [
            "",
            "plain words here",
            rule.as_str(),
            "\u{276F} draft",
            "\u{276F} 1. Yes",
            "  \u{276F} /compact   Free up context",
            " \u{276F}",
            "\u{2500}\u{2500}\u{2500} label \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)",
            "    /autocompact   Toggle it",
        ]
        .map(str::to_owned)
        .into();
        // The walk: the caret's own rule is no rule, and the `❯` under the
        // second rule is a list's entry only while a rule stands over the
        // caret's row.
        let walk: Vec<String> = [
            rule.as_str(),
            "\u{276F} older",
            "not a rule",
            "\u{276F} caret",
            rule.as_str(),
            "  \u{276F} entry",
            rule.as_str(),
        ]
        .map(str::to_owned)
        .into();
        let walked = FrameReach::of(&walk);
        assert_eq!(walked.rules(), None, "the caret has no rule over it");
        assert_eq!(walked.from(), 0, "CONTROL: the walk reads to the top");
        let mut moved = walk.clone();
        moved[0] = "plain words".to_owned();
        assert_eq!(
            FrameReach::of(&moved).rules(),
            Some((4, 6)),
            "CONTROL: a row over the caret's row moved the frame"
        );
        let mut screens = crate::text::fixture_screens();
        screens.push(walk);
        let mut tight = 0;
        for screen in screens {
            for width in [usize::MAX, 40, 12] {
                let mut rows: Vec<String> = screen
                    .iter()
                    .map(|r| r.chars().take(width).collect())
                    .collect();
                let reach = FrameReach::of(&rows);
                assert_eq!(reach.rules(), composer_rules(&rows), "the frame: {rows:?}");
                for j in 0..reach.from() {
                    for swap in &swaps {
                        let was = std::mem::replace(&mut rows[j], swap.clone());
                        assert_eq!(
                            FrameReach::of(&rows),
                            reach,
                            "row {j} as {swap:?}: {rows:?}"
                        );
                        rows[j] = was;
                    }
                }
                for swap in &swaps {
                    let mut all = rows.clone();
                    all[..reach.from()].fill(swap.clone());
                    assert_eq!(FrameReach::of(&all), reach, "all as {swap:?}: {rows:?}");
                }
                if let Some((top, _)) = reach.rules()
                    && reach.from() == top
                {
                    rows[top] = "plain words here".to_owned();
                    assert_ne!(FrameReach::of(&rows).rules(), reach.rules(), "{rows:?}");
                    tight += 1;
                }
            }
        }
        assert!(tight > 100, "CONTROL: screens with a frame: {tight}");

        // A literal indented caret in a Muse draft makes the generic search
        // find no frame; Muse still reads its column-zero composer. Its
        // drawer eligibility also reads above the top rule.
        let mut muse: Vec<String> = [
            "ordinary reply",
            "────────────────────────",
            "❯ draft",
            "  ❯ quoted draft",
            "────────────────────────",
            "  model · high · 50% · $0",
        ]
        .map(str::to_owned)
        .into();
        assert_eq!(FrameReach::of(&muse).rules(), None);
        assert_eq!(crate::agy::composer_rules(&muse), None);
        assert_eq!(crate::muse::composer_rules(&muse), Some((1, 4)));
        let reached = FrameReach::of_or_agy(&muse);
        assert_eq!(reached.rules(), Some((1, 4)));
        assert!(reached.unmoved(&[]));
        muse[0] = "  ↑↓ move · enter confirm · esc cancel".to_owned();
        assert_eq!(crate::muse::composer_rules(&muse), None);
        assert!(!reached.unmoved(&[0]));
        assert_ne!(FrameReach::of_or_agy(&muse).rules(), reached.rules());
    }

    /// ATERM-KI-1015-1 S2R-F1 (the harness sweep of 2026-10-15, Claude Code
    /// 2.1.300 at 10 columns): a running turn's spinner row is wider than the
    /// pane, and Ink wraps it at column 0 (`✻ Choreogr` over `aphing…`), or
    /// leaves its glyph alone on its row (`·` over `Precipitat` over `ing…`);
    /// the footer's `esc to interrupt` is cut off. Read a row at a time the
    /// glyph row held no `…`: a done row, or (its glyph alone) no status row,
    /// so the running turn read idle for 130 s and the continue policy typed
    /// `keep going` into it. The status row is read across its rows, the
    /// rows Ink broke between the spans of its shimmering verb too (`g` over
    /// `…`: the fix's first cut read such frames idle mid-turn in the lab),
    /// and a done row's (`✻ Brewed for 2m 58s · done 10:27` over `  AM` at 34
    /// columns, at its text column), which still ends the turn. CONTROL: a
    /// queued `❯` message right under the spinner is no part of its line,
    /// though the row over it is full.
    #[test]
    fn a_spinner_row_ink_wrapped_reads_busy() {
        use crate::prompt::fixtures::{
            API_ERROR_529_34_2_1_299, SPINNER_ELLIPSIS_ALONE_10_2_1_300,
            SPINNER_GLYPH_ALONE_10_2_1_300, SPINNER_WRAPPED_10_2_1_300,
            SPINNER_WRAPPED_B_10_2_1_300, screen,
        };
        for (text, line) in [
            (SPINNER_WRAPPED_10_2_1_300, "✻ Choreogr aphing…"),
            (SPINNER_WRAPPED_B_10_2_1_300, "✳ Choreog raphing…"),
            (SPINNER_GLYPH_ALONE_10_2_1_300, "· Precipitat ing…"),
            (SPINNER_ELLIPSIS_ALONE_10_2_1_300, "✻ Calculatin g …"),
        ] {
            let r = screen(text);
            assert_eq!(worker_phase(&r), Phase::Busy, "{r:#?}");
            assert_eq!(busy_signal(&r).map(|b| b.rule), Some("spinner"));
            assert_eq!(status_line(&r).as_deref(), Some(line));
            // The row the spinner is on is its first, as drawn.
            let first = line.split(' ').next().expect("a glyph");
            assert!(status_row(&r).is_some_and(|row| row.starts_with(first)));
            let reading = crate::reader::read(Some("claude"), &r, None);
            assert_eq!(reading.phase, Phase::Busy);
        }
        // A done row wrapped at its text column: one done row, the turn over.
        let r = screen(API_ERROR_529_34_2_1_299);
        assert_eq!(
            status_line(&r).as_deref(),
            Some("✻ Brewed for 2m 58s · done 10:27 AM")
        );
        let done = r
            .iter()
            .position(|x| x.starts_with("✻ Brewed"))
            .expect("the done row");
        assert_eq!(transcript_end(&r), done + 2);
        assert!(turn_over(&r));
        // CONTROL: a queued message under the spinner's line.
        let mut queued = screen(SPINNER_WRAPPED_10_2_1_300);
        let at = queued
            .iter()
            .position(|x| x == "aphing…")
            .expect("the line's rest");
        queued.insert(at + 1, "❯ keep".to_string());
        assert_eq!(status_line(&queued).as_deref(), Some("✻ Choreogr aphing…"));
        assert!(turn_just_begun(&queued));
    }

    /// ATERM-KI-1015-1 LC-10 (the harness sweep of 2026-10-15, Claude Code
    /// 2.1.300): relaunched `--permission-mode auto --resume <id>`, Claude
    /// Code draws its auto-mode notice (`⏺ Auto mode lets Claude handle
    /// permission prompts automatically — …`, four rows at 144 columns) as the
    /// last block over the composer, after the first reply. It was journaled
    /// as the agent's last words (`EVENT idle … Shift+Tab to change mode.`),
    /// and a stop-phrase check read it as what the worker said. It is the
    /// vendor's notice: the last thing said is the reply above it.
    #[test]
    fn the_vendors_auto_mode_notice_is_not_the_workers_last_words() {
        use crate::prompt::fixtures::{RESUME_AUTO_MODE_NOTICE_2_1_300, screen};
        let r = screen(RESUME_AUTO_MODE_NOTICE_2_1_300);
        let reply = "⏺ Hi! I am the local lab stub, not a model. (reply 2)";
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(last_said_row(&r), Some(reply));
        assert_eq!(
            crate::turn::said_tail(&r).as_deref(),
            Some("Hi! I am the local lab stub, not a model. (reply 2)")
        );
        // CONTROL: a reply that only QUOTES the notice is the worker's words.
        let quoted: Vec<String> = r
            .iter()
            .map(|x| x.replacen("⏺ Auto mode lets", "⏺ It said: Auto mode lets", 1))
            .collect();
        assert!(
            last_said_row(&quoted).is_some_and(|x| x.ends_with("Shift+Tab to change mode.")),
            "{quoted:#?}"
        );
    }

    /// ATERM-KI-1015-1 LC-10 in a pane narrower than the notice's first line
    /// (the review of round 20, 2026-10-15): at 50 columns Ink wraps the
    /// auto-mode notice's anchor itself (`⏺ Auto mode lets Claude handle
    /// permission prompts` over `automatically — Claude checks …`), so the
    /// bullet row alone opened with no notice, and the paragraph was still
    /// read as the worker's last words. The paragraph is read as its text,
    /// across its rows. CONTROL: a reply that quotes the notice is the
    /// worker's words at this width too.
    #[test]
    fn the_vendors_auto_mode_notice_wrapped_narrow_is_not_the_workers_last_words() {
        let frame = |notice: &str| {
            let mut r: Vec<String> = [
                "❯ hi",
                "",
                "⏺ Hi! I am the local lab stub, not a model.",
                "  (reply 2)",
                "",
                "✻ Cogitated for 0s · done 7:06 AM",
                "",
                notice,
                "  automatically — Claude checks each tool call for",
                "  risky actions and prompt injection before",
                "  executing. Actions Claude identifies as safe are",
                "  executed, while actions Claude identifies as",
                "  risky are blocked and Claude may try a different",
                "  approach. Ideal for long-running tasks. Sessions",
                "  are slightly more expensive. Claude can make",
                "  mistakes that allow harmful commands to run,",
                "  it's recommended to only use in isolated",
                "  environments. Shift+Tab to change mode.",
                "",
            ]
            .iter()
            .map(|x| (*x).to_string())
            .collect();
            r.push("─".repeat(50));
            r.push("❯".to_string());
            r.push("─".repeat(50));
            r.push("  ⏵⏵ auto mode on (shift+tab to cycle)".to_string());
            r
        };
        let r = frame("⏺ Auto mode lets Claude handle permission prompts");
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(last_said_row(&r), Some("  (reply 2)"), "{r:#?}");
        let said = crate::turn::said_tail(&r).expect("the reply");
        assert!(
            said.contains("(reply 2)") && !said.contains("Auto mode"),
            "{said:?}"
        );
        // CONTROL: the worker quoting it.
        let quoted = frame("⏺ It said: Auto mode lets Claude handle permission");
        assert_eq!(
            last_said_row(&quoted),
            Some("  environments. Shift+Tab to change mode.")
        );
    }

    /// A STATUS ROW ALREADY WHOLE takes no row under it (the review of round
    /// 20's S2R-F1 fix, 2026-10-15): the done row's text wraps at its text
    /// column two (`✻ Baked for 0s · done 7:06` over `  AM`), and the rule
    /// that reads that took, under a retry row that fills its row, the
    /// vendor's own notice Claude Code 2.1.283 draws there at column two
    /// (`  Fast mode unavailable: …`). The joined line was no retry status,
    /// so it read as a done row: the turn over, and with the footer's `esc to
    /// interrupt` not drawn the session read idle mid-retry. A row that
    /// already reads busy is the whole status line. CONTROL: the same retry
    /// row with the footer's hint is busy either way.
    #[test]
    fn a_retry_row_takes_no_notice_under_it() {
        let retry = "✻ API Error: 529 Overloaded, please wait · Retrying in 22s · attempt 7/10";
        let frame = |footer: &str| {
            let mut r: Vec<String> = [
                "⏺ Working.",
                "",
                retry,
                "  Fast mode unavailable: network connectivity is down right now",
            ]
            .iter()
            .map(|x| (*x).to_string())
            .collect();
            r.push("─".repeat(80));
            r.push("❯".to_string());
            r.push("─".repeat(80));
            r.push(footer.to_string());
            r
        };
        for footer in [
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
            "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt",
        ] {
            let r = frame(footer);
            assert_eq!(status_line(&r).as_deref(), Some(retry), "{r:#?}");
            assert!(!turn_over(&r));
            assert_eq!(transcript_end(&r), 2);
            assert_eq!(worker_phase(&r), Phase::Busy, "{footer}");
        }
    }
}
