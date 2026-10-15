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
/// With no frame, the first row [`busy_reason`] names, anywhere.
pub fn busy_signal(rows: &[String]) -> Option<Busy> {
    let Some(frame) = composer_frame(rows) else {
        return rows
            .iter()
            .find_map(|r| busy_reason(r))
            .map(|rule| Busy::hard(Zone::Screen, rule));
    };
    let block = status_block(rows, frame.top);
    let status = block.status.map(|i| rows[i].as_str());
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
            .status
            .is_some_and(|i| is_done_row(&rows[i]))
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
        .status
        .map(|i| rows[i].as_str())
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

/// The text of the vendor's API-retry status row, which no ellipsis ends
/// (measured on 2.1.283, 2026-09-27): the whole formatted error, then
/// `· Retrying in 14s · attempt 6/10` — `Can't reach the API server — check
/// your internet or DNS (ENOTFOUND) · Retrying in 1s · attempt 5/10`. The
/// worker is busy: the vendor is retrying its own request. Read as the tail
/// of the row, so the error in front may be cut (`…`) at a narrow width, and
/// a done row (`Cogitated for 0s · done 11:02 PM`) or the worker's words are
/// not one.
fn is_retry_status(text: &str) -> bool {
    let Some((_, tail)) = text.rsplit_once(" · Retrying in ") else {
        return false;
    };
    let Some((wait, attempt)) = tail.split_once(" · attempt ") else {
        return false;
    };
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    // `14s`, `2m`, `1m 30s`: each word a number and its unit.
    let wait_ok = !wait.is_empty()
        && wait
            .split(' ')
            .all(|w| w.strip_suffix(['s', 'm', 'h']).is_some_and(number));
    let attempt_ok = attempt
        .trim_end()
        .split_once('/')
        .is_some_and(|(k, n)| number(k) && number(n));
    wait_ok && attempt_ok
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
    let caret = composer_index(rows)?;
    let top = caret.checked_sub(1).filter(|&t| is_rule(&rows[t]))?;
    let bottom = (caret + 1..rows.len()).find(|&i| is_rule(&rows[i]))?;
    Some(Frame { top, bottom })
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

/// The live zone above the top rule, found by walking up from it: the status
/// row, if one comes before the transcript, and where the rows under it (or
/// under the transcript row the walk stopped at) begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusBlock {
    pub(crate) status: Option<usize>,
    pub(crate) from: usize,
}

pub(crate) fn status_block(rows: &[String], top: usize) -> StatusBlock {
    // A rating card (or the drafts card) parked above the composer is ONE block the walk
    // steps over, whatever its rows say: read as a transcript row, an unread card would end
    // the walk and hide the status row under it.
    let parked = crate::survey::parked_span(rows);
    for i in (0..top).rev() {
        if parked.is_some_and(|(first, last)| (first..=last).contains(&i)) {
            continue;
        }
        if is_glyph_row(&rows[i]) {
            return StatusBlock {
                status: Some(i),
                from: i + 1,
            };
        }
        if is_transcript_row(&rows[i]) || ends_live_zone(&rows[i]) {
            return StatusBlock {
                status: None,
                from: i + 1,
            };
        }
    }
    StatusBlock {
        status: None,
        from: 0,
    }
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
/// transcript.
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
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
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
pub fn status_row(rows: &[String]) -> Option<&str> {
    let frame = composer_frame(rows)?;
    status_block(rows, frame.top)
        .status
        .map(|i| rows[i].as_str())
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
/// session survey and right-aligned hints are skipped, so a question is still
/// a question with the done row, an update banner or the survey under it.
/// Without a composer, the last non-blank row of the screen (above the caret
/// when there is one).
pub fn last_said_index(rows: &[String]) -> Option<usize> {
    let (end, width) = match composer_frame(rows) {
        Some(f) => (
            status_block(rows, f.top).status.unwrap_or(f.top),
            Some(crate::text::trim_end(&rows[f.bottom]).chars().count()),
        ),
        None => (composer_index(rows).unwrap_or(rows.len()), None),
    };
    let parked = crate::survey::parked_span(rows);
    (0..end)
        .rev()
        .find(|&i| !parked.is_some_and(|(a, b)| (a..=b).contains(&i)) && is_said(&rows[i], width))
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
    if let Some(status) = status_block(rows, frame.top).status {
        // A done row (`✻ Worked for 3m 21s · done 8:50 PM · 1 monitor still
        // running`) ends the turn in the transcript: it is kept, and only what
        // hangs under it is the live zone. A spinner or a `Waiting for …` row
        // is the live zone itself.
        return if is_done_row(&rows[status]) {
            status + 1
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
/// WHERE a wall is.
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
        if let Some(found) = footer_notice(rows, frame.bottom + 1, width, classify) {
            return Some(found);
        }
        if let Some(found) = banner_notice(rows, frame.top, width, classify) {
            return Some(found);
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
        Block::Output => return None,
        Block::UnderCall => true,
        Block::Vendors => false,
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
    /// Read as Claude Code's own row, from a later column-2 row under a call
    /// (rule 3): [`Wall::under_call`].
    UnderCall,
    /// Read as Claude Code's own row under any other owner, or none the
    /// walk reaches.
    Vendors,
}

/// Whether the `⎿` block opening at `open` is READ as Claude Code's own row —
/// the SCREEN'S half of H6-HARNESS-1 (ruling 2026-10-08). It reads for
/// RECALL: a wall read here is acted on only where the session's own
/// transcript confirms it (aterm-agent, `supervise/wall_confirm.rs`), so
/// where the screen cannot tell Claude Code's row from a tool's, it reads
/// the wall. Three rules, each measured on Claude Code 2.1.294 in
/// `tools/real-claude-lab`:
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
/// Any other owner is read as Claude Code's: the person's `❯` turn, the
/// worker's message, a task notice, a Monitor event (its payload included),
/// a hook summary. So is a block whose owner the walk does not reach — a
/// blank row over a row that opens an item (below), or the top of the
/// screen, comes first — which is where Claude Code's walls stand under a
/// prompt's second paragraph, under a collapsed tool group (its row wrapped
/// or not), and under a long output whose call has scrolled off (all
/// measured).
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
    // The nearest non-blank row below the one the walk stands on.
    let mut below = &rows[open];
    for row in rows[..open].iter().rev() {
        if crate::text::trim(row).is_empty() {
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
            return match (is_tool_call(row), later) {
                (false, _) => Block::Vendors,
                (true, true) => Block::UnderCall,
                (true, false) => Block::Output,
            };
        }
        later |= lead == VENDOR_GUTTER_COLUMN && crate::text::trim_start(row).starts_with('⎿');
    }
    Block::Vendors
}

/// A transcript row that opens a CALL — a tool's or a command's, whose `⎿`
/// rows are what it printed, never anybody's words. Claude Code's forms
/// (2.1.294, measured in `tools/real-claude-lab` and read from its binary):
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
/// the worker's words): an item — the row, a part after a wide gap or after
/// ` · ` — that opens with a notice `classify` names, up to the next wide gap.
/// The row is read with the rows Ink wrapped its line onto ([`line_at`]): in
/// a pane narrower than the usage-limit banner, `⚠ Usage limit reached ·
/// limit` stands over `resets 10:25am` (34 columns, 2.1.299), and the row
/// alone names no reset.
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
            let (_, line) = line_at(rows, i, until, width);
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

    /// Non-vacuity for [`a_dead_frame_above_the_launch_card_is_not_the_live_zone`]:
    /// the same fixtures cut at the dead process's footer — its last frame as
    /// it stood while it lived — still read as that frame said: the `/exit`
    /// spinner busy on the status row, the done row under a question a
    /// question.
    #[test]
    fn the_dead_frame_alone_still_reads_as_it_lived() {
        use crate::prompt::fixtures::{
            CTRLC_RELAUNCH_STALE_QUESTION, EXIT_RELAUNCH_STALE_FOOTER, screen,
        };
        let cut = |text: &str, footer: &str| {
            let r = screen(text);
            let at = r
                .iter()
                .rposition(|row| row.contains(footer))
                .expect(footer);
            r[..=at].to_vec()
        };
        let exit = cut(EXIT_RELAUNCH_STALE_FOOTER, "esc to interrupt");
        assert_eq!(
            busy_signal(&exit),
            Some(Busy::hard(Zone::Status, "spinner")),
            "{exit:#?}"
        );
        assert_eq!(worker_phase(&exit), Phase::Busy);
        let ctrlc = cut(CTRLC_RELAUNCH_STALE_QUESTION, "Press Ctrl-C again to exit");
        assert_eq!(worker_phase(&ctrlc), Phase::Question, "{ctrlc:#?}");
    }

    /// The session of [`a_dead_frame_above_the_launch_card_is_not_the_live_zone`]
    /// with a question dialog up: the dialog is the live box, and the only
    /// `esc to interrupt` on the screen is the dead footer ABOVE the new
    /// launch card — the row a whole-grid `await gone esc.to.interrupt` (the
    /// hosted loop's busy guard) waits on for its whole step.
    #[test]
    fn a_question_under_a_dead_footer_is_the_live_box() {
        use crate::prompt::fixtures::{EXIT_RELAUNCH_STALE_FOOTER_QUESTION, screen};
        let r = screen(EXIT_RELAUNCH_STALE_FOOTER_QUESTION);
        assert_eq!(worker_phase(&r), Phase::Prompt);
        let p = crate::prompt::parse_prompt_v2(&r).expect("the dialog");
        assert_eq!(p.kind, crate::prompt::PromptKind::Question);
        let card = r
            .iter()
            .rposition(|row| row.contains(crate::anchor("launch.banner")))
            .expect("the new launch card");
        let busy = r
            .iter()
            .rposition(|row| row.contains(crate::anchor("busy.interrupt")));
        assert!(busy.is_some_and(|b| b < card), "{busy:?} {card}");
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
            assert!(
                predraw[hint + 2].ends_with("% claude --model haiku"),
                "{name}"
            );
            assert!(
                composer_rules(&predraw).is_some(),
                "{name}: the dead frame reads"
            );
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
        // The /exit variant's dead footer is the busy guard's row: out of
        // the zone, and in it while the frame lived.
        let r = screen(EXIT_RELAUNCH_STALE_FOOTER);
        let hint = r.iter().position(|row| row == notice).expect("the hint");
        let predraw = r[..hint + 3].to_vec();
        assert!(busy_rows(&predraw[busy_zone_start(&predraw)..]).is_empty());
        let dead = busy_rows(&predraw)[0];
        let lived = r[..=dead].to_vec();
        assert!(
            busy_rows(&lived[busy_zone_start(&lived)..])
                .contains(&(dead - busy_zone_start(&lived)))
        );
        // Only the hint alone on its row counts: quoted under the gutter, or
        // in a message, it is the transcript's.
        let mut quoted = r[..hint].to_vec();
        quoted.push(format!("  ⎿  {notice}"));
        assert!(busy_zone_start(&quoted) < dead, "{quoted:#?}");
    }

    #[test]
    fn idle_prompt_question_busy_each_have_a_fixture() {
        let idle = screen(
            &[
                "⏺ Merged and pushed.",
                "",
                "✻ Cogitated for 2m 54s · done 2:41 PM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&idle), Phase::Idle);

        assert_eq!(worker_phase(&bash_one_row()), Phase::Prompt);

        let question = screen(
            &[
                "⏺ Two ways to do this: keep the harness or rewrite it.",
                "  Which do you prefer?",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&question), Phase::Question);

        let busy = screen(
            &[
                "⏺ Running the tests.",
                "",
                "✻ Synthesizing… (18s · ↓ 1.1k tokens)",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt",
        );
        assert_eq!(worker_phase(&busy), Phase::Busy);
    }

    /// The four misclassifications the real session produced, each now busy.
    #[test]
    fn the_four_misclassified_busy_screens_read_busy() {
        // 1. An idle-looking screen mid-thought with a STATIC spinner row and an
        //    idle-looking footer.
        let static_spinner = screen(
            &[
                "⏺ Reading the harness helper before changing anything.",
                "",
                "✢ Thinking…",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&static_spinner), Phase::Busy);

        // 2. A spinner word without its timer (the first second).
        let no_timer = screen(&["✶ Deliberating…", ""], "  ? for shortcuts");
        assert_eq!(worker_phase(&no_timer), Phase::Busy);

        // 3. A DONE row whose footer still counts a background shell.
        let shell = screen(
            &[
                "⏺ Started the build in the background.",
                "",
                "✻ Cooked for 9m 27s · done 11:07 AM · 1 shell still running",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(worker_phase(&shell), Phase::Busy);
        assert_eq!(
            busy_reason("✻ Cooked for 9m 27s · done 11:07 AM · 1 shell still running"),
            Some("a shell still running")
        );

        // 4. A footer without `esc to interrupt`: the live spinner is the only
        //    signal.
        let auto = screen(
            &[
                "⏺ Running the tests.",
                "",
                "✻ Synthesizing… (18s · ↓ 1.1k tokens)",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(worker_phase(&auto), Phase::Busy);
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
        assert_eq!(r.len(), 63);
        assert_eq!(r[44], "✻ Waiting for 1 dynamic workflow to finish");
        assert_eq!(
            busy_reason(&r[44]),
            Some("waiting for a dynamic workflow"),
            "the stale row the whole-screen scan fired on"
        );
        assert_eq!(r[60], "❯", "the empty composer, where the cursor was");
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

    /// The same screen with `esc to interrupt` in the footer and nowhere else.
    #[test]
    fn esc_to_interrupt_in_the_footer_alone_is_busy() {
        let mut r = saved_screen(IDLE_AFTER_LIMIT);
        r[62] = "  ⏵⏵ accept edits on (shift+tab to cycle) · esc to interrupt".to_string();
        assert_eq!(signal(&r).as_deref(), Some("footer: esc to interrupt"));
        assert_eq!(worker_phase(&r), Phase::Busy);
    }

    /// A spinner-looking row far up the transcript over an empty status block
    /// (only blanks between the last transcript row and the frame): IDLE.
    #[test]
    fn a_spinner_far_up_over_an_empty_status_block_is_idle() {
        let r = screen(
            &[
                "✶ Deliberating…",
                "⏺ Reading the harness helper before changing anything.",
                "",
                "  Ran 3 shell commands",
                "",
                "⏺ The helper is fine as it is; nothing to change.",
                "",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        );
        assert_eq!(busy_reason(&r[0]), Some("spinner row"));
        assert_eq!(signal(&r), None);
        assert_eq!(status_row(&r), None, "the worker's row comes first");
        assert_eq!(worker_phase(&r), Phase::Idle);
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

    /// `wait_bg.out`: a turn finished while a background workflow runs —
    /// `✻ Waiting for 1 dynamic workflow to finish` directly above the frame,
    /// and the workflow's progress line under the footer.
    #[test]
    fn wait_bg_is_busy_waiting_for_a_workflow() {
        let r = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: waiting for a dynamic workflow")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
        let footer = r.last().expect("rows");
        assert_eq!(footer_busy(footer), Some("a workflow running"), "{footer}");
    }

    /// `wait_bg2.out`: the manager answered and the worker is deliberating; the
    /// `✻ Waiting for 1 dynamic workflow` above the user row is history, the
    /// live spinner sits under a `⎿  Tip:` row.
    #[test]
    fn wait_bg2_is_busy_the_live_spinner_under_its_tip_row() {
        let r = waiter_capture(include_str!("fixtures/wait_bg2.out"));
        assert_eq!(signal(&r).as_deref(), Some("status row: spinner"));
        assert_eq!(status_row(&r), Some("✶ Deliberating…"));
        assert_eq!(worker_phase(&r), Phase::Busy);
    }

    /// `wait_bg3.out`: the done row counts no shell and the footer says
    /// nothing busy; the worker asked for a call without a `?`.
    #[test]
    fn wait_bg3_is_idle_its_done_row_counts_no_shell() {
        let r = waiter_capture(include_str!("fixtures/wait_bg3.out"));
        assert_eq!(
            status_row(&r),
            Some("✻ Sautéed for 50m 35s · done 10:57 AM")
        );
        assert_eq!(signal(&r), None);
        assert_eq!(last_said_row(&r), Some("  Waiting for your call."));
        assert_eq!(worker_phase(&r), Phase::Idle);
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

    /// `wait_bg4.out`: the turn is done but `1 shell still running` rides on
    /// the done row, and the footer counts `· 1 shell ·` too.
    #[test]
    fn wait_bg4_is_busy_a_shell_still_running() {
        let r = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
        let footer = r.last().expect("rows");
        assert_eq!(footer_busy(footer), Some("a shell running"), "{footer}");
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
        use crate::prompt::fixtures::{IDLE_SHELLS_2_1_271 as TEXT, provenance};
        use crate::reader::{ClaudeReader, Program, ScreenReader, identify};
        let head = provenance(TEXT).expect("a provenance line");
        assert!(
            head.starts_with("claude-code 2.1.271 · HAND-BUILT ")
                && head.contains("no capture of the incident screen was kept"),
            "{head}"
        );
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
        let footer = r.last().expect("rows");
        assert!(
            footer.starts_with("  ⏵⏵ bypass permissions on · 2 shells · "),
            "{footer}"
        );
        assert_eq!(footer_busy(footer), Some("a shell running"), "{footer}");
        // The supervisor's break, as it asks it (`host_steps_in_background`):
        // busy, background work, no wall — the reader named by the program,
        // or, for a Claude Code started as `node`, by its screen.
        for reader in [identify(Some("claude"), &r), identify(Some("node"), &r)] {
            assert_eq!(reader.program(), Program::Claude);
            assert_eq!(reader.phase(&r), Phase::Busy);
            assert_eq!(reader.background_wait(&r), Some("a shell still running"));
            assert!(reader.wall(&r).is_none(), "{:?}", reader.wall(&r));
        }
        assert_eq!(
            ClaudeReader.background_wait(&r),
            Some("a shell still running")
        );

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

    /// `wait_bg11.out`: the shell rides on the done row under a right-aligned
    /// `get pinged when Claude finishes` hint.
    #[test]
    fn wait_bg11_is_busy_a_shell_still_running_under_a_hint_row() {
        let r = waiter_capture(include_str!("fixtures/wait_bg11.out"));
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
    }

    /// `wait_bg12.out`: an older done row with a shell sits far up (history);
    /// the live one is under the update banner, and the composer holds text.
    #[test]
    fn wait_bg12_is_busy_a_shell_still_running_under_the_update_banner() {
        let r = waiter_capture(include_str!("fixtures/wait_bg12.out"));
        assert_eq!(
            status_row(&r),
            Some("✻ Sautéed for 1m 7s · done 1:44 PM · 1 shell still running")
        );
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
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
        ] {
            assert!(is_activity_row(row), "{row}");
        }
        // Controls: a done row, the worker's own words, a malformed tail.
        for row in [
            "✻ Cogitated for 0s · done 11:02 PM",
            "✻ I was Retrying in 5s · attempt soon",
            "✻ Can't reach the API server · Retrying in soon · attempt 5/10",
            "✻ Can't reach the API server · Retrying in 5s · attempt 5",
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

        for last in [
            "⏺ Stopped the background build.",
            "  ⎿  error: could not compile `x` (lib) due to 1 previous error",
        ] {
            let r = screen(
                &["✻ Waiting for 1 dynamic workflow to finish", "", last, ""],
                "  ⏵⏵ auto mode on (shift+tab to cycle)",
            );
            assert_eq!(status_row(&r), None, "{last}");
            assert_eq!(signal(&r), None, "{last}");
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

        // wait_bg2 without its workflow line and with the survey above the
        // frame: still the spinner, under either footer.
        let mut bg2 = waiter_capture(include_str!("fixtures/wait_bg2.out"));
        bg2.retain(|row| !row.trim_start().starts_with('◯'));
        let bg2 = above_frame(
            bg2,
            &[
                "● How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
        );
        for footer in [PASTE_FOOTER, IDLE_AUTO_FOOTER] {
            let r = with_footer(bg2.clone(), footer);
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: spinner"),
                "{footer}"
            );
            assert_eq!(worker_phase(&r), Phase::Busy, "{footer}");
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
        assert_eq!(r.len(), 12);
        assert!(survey_open(&r));
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(status_row(&r), Some("✻ Cooked for 52m 54s · done 12:31 PM"));
        assert_eq!(last_said_row(&r), Some("  Free disk is 31 GiB."));

        let mut no_options = r.clone();
        no_options.retain(|row| !row.trim_start().starts_with("1: Bad"));
        assert_eq!(no_options.len(), 11);
        assert!(!survey_open(&no_options));

        // wait_bg7: the done row over the survey, the survey on the rule.
        let bg7 = waiter_capture(include_str!("fixtures/wait_bg7.out"));
        assert!(survey_open(&bg7));
        assert!(!survey_open(&rows(&[SURVEY[0], SURVEY[1], "$ "])));
    }

    /// A survey in the transcript is not open: quoted in the worker's `⏺`
    /// message, as the message itself, in a tool's output under the `⎿`
    /// gutter, with the worker's words after it, or above a done row.
    #[test]
    fn a_survey_in_the_transcript_is_not_open() {
        for body in [
            vec![
                "⏺ Claude Code parks this above the composer:",
                "  ● How is Claude doing this session? (optional)",
                "    1: Bad    2: Fine   3: Good   0: Dismiss",
                "",
            ],
            vec![
                "⏺ How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
            vec![
                "⏺ Bash(cat survey-open.txt)",
                "  ⎿  ● How is Claude doing this session? (optional)",
                "       1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
            vec![SURVEY[0], SURVEY[1], "  I left it for you to answer."],
            vec![
                SURVEY[0],
                SURVEY[1],
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ],
        ] {
            let r = screen(&body, "  ? for shortcuts");
            assert!(!survey_open(&r), "{body:?}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{body:?}");
        }
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
        assert_eq!(r.len(), 7);
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
        assert_eq!(unframed.len(), 5);
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
        assert_eq!(leading_spaces(&long[4]), 18);
        assert_eq!(context_left(&long), Some(8));
        assert_eq!(worker_phase(&long), Phase::Busy);
        let short = narrow(40, "1% until auto-compact");
        assert_eq!(leading_spaces(&short[4]), 17);
        assert_eq!(context_left(&short), Some(1));
        let gutter = narrow(28, "1% until auto-compact");
        assert_eq!(leading_spaces(&gutter[4]), 5);
        assert_eq!(context_left(&gutter), None);
        assert_eq!(context_left(&narrow(29, "1% until auto-compact")), Some(1));
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
        assert_eq!(leading_spaces(&hint), 8, "short of HINT_COLUMN");
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
            // And the same live box with a copy of another box in the
            // transcript above it: the live one is read.
            let mut both = rows(&BOX_UNDER_GUTTER);
            both.push(String::new());
            both.extend(live);
            assert_eq!(worker_phase(&both), Phase::Prompt, "{under:?}");
            assert_eq!(
                parse_prompt(&both).map(|p| p.command),
                Some("git push origin main".into())
            );
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
        assert_eq!(full[56], "❯ /model");
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
        assert!(r[47].contains("You've reached your Fable limit"));
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

    #[test]
    fn the_session_limit_spelling_carries_its_reset() {
        let r = screen(
            &[
                "❯ run the replicates",
                "",
                "⏺ Starting the benchmark replicates now.",
                "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                "",
                "✻ Worked for 3m 2s · done 5:02 PM",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        );
        assert_eq!(
            worker_phase(&r),
            Phase::Limited {
                message: "You've hit your session limit · resets 7:30pm (America/Los_Angeles)"
                    .to_string(),
                reset: Some("7:30pm (America/Los_Angeles)".to_string()),
            }
        );
        assert_eq!(
            reset_of(
                "Claude usage limit reached. Your limit will reset at 3pm (America/New_York)."
            ),
            Some("3pm (America/New_York)".to_string())
        );
        assert_eq!(
            reset_of("5-hour limit reached ∙ resets 3am"),
            Some("3am".to_string())
        );
        assert_eq!(reset_of("You've reached your Fable limit."), None);
        // A key followed by no time is passed over: the countdown toast that
        // follows a `/goal` echo names the time.
        assert_eq!(
            reset_of(
                "Goal paused · usage limit reached · send a message after it resets to \
                 continue ⚠ Usage limit reached · continuing automatically at 3pm · esc to cancel"
            ),
            Some("3pm".to_string())
        );
        assert_eq!(
            reset_of(
                "Goal paused · usage limit reached · send a message after it resets to continue"
            ),
            None
        );
        assert_eq!(
            reset_of("resets soon, then it resets 4pm (America/Los_Angeles)"),
            Some("4pm (America/Los_Angeles)".to_string())
        );
    }

    /// A RESET ENDS WHERE ITS TIME DOES ([`time_words`]). A line read across
    /// the rows Ink wrapped it onto runs on into the notice's next line when
    /// the row above it was full (at 34 columns `(America/Los_Angeles)` over
    /// `/upgrade to increase your`, 2.1.299), or into a row under it at the
    /// same column; the transcript's own text has the next line after a
    /// newline. The time and its zone are the reset, nothing after them.
    #[test]
    fn a_reset_ends_where_its_time_does() {
        for (said, want) in [
            (
                "You've hit your session limit · resets 10:25am (America/Los_Angeles) /upgrade \
                 to increase your usage limit.",
                "10:25am (America/Los_Angeles)",
            ),
            (
                "You've hit your session limit · resets 10:25am (America/Los_Angeles)\n/upgrade \
                 to increase your usage limit.",
                "10:25am (America/Los_Angeles)",
            ),
            (
                "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \
                 /upgrade to increase your usage limit.",
                "Oct 15 at 12:33pm (America/Los_Angeles)",
            ),
            (
                "⚠ Usage limit reached · limit resets 10:25am ⏵⏵ auto mode on (shift+tab to cycle)",
                "10:25am",
            ),
            (
                "Usage limit reached · resets September 19, 11:05 am and then some",
                "September 19, 11:05 am",
            ),
            (
                "You've hit your weekly limit · resets in 2h 30m at most",
                "in 2h 30m",
            ),
            (
                "Usage limit reached · continuing shortly. Hang on",
                "shortly",
            ),
        ] {
            assert_eq!(reset_of(said).as_deref(), Some(want), "{said}");
        }
        // Nothing of a time after the key: no reset.
        assert_eq!(
            reset_of("resets /upgrade to increase your usage limit."),
            None
        );
    }

    /// A RESET'S DATE IS NEVER CUT OFF ITS TIME (the review of fix/r15-banner,
    /// 2026-10-14). The cut at the first word that is no time word
    /// ([`time_words`]) made `resets at 3pm on Oct 15` the bare `3pm`, which
    /// `aterm-agent`'s `limit::parse_reset` reads as today's or tomorrow's
    /// session clock: the date was lost, where main handed over the whole and
    /// the unread spelling fell to the backstop. No measured Claude Code
    /// notice is spelled so (its weekly reset is `Oct 15 at 12:33pm`), and
    /// none of these is cut: each is handed over as main handed it over.
    #[test]
    fn a_resets_date_is_never_cut_off_its_time() {
        for (said, want) in [
            (
                "Usage limit reached · resets at 3pm on Oct 15",
                "3pm on Oct 15",
            ),
            (
                "Usage limit reached · resets 3pm on the 15th",
                "3pm on the 15th",
            ),
            ("Usage limit reached · resets 9am tomorrow", "9am tomorrow"),
            ("Usage limit reached · resets 9am Monday", "9am Monday"),
        ] {
            assert_eq!(reset_of(said).as_deref(), Some(want), "{said}");
        }
    }

    /// A notice split over two rows keeps both, and the reset its first row
    /// names; a `<X> limit reached ∙ resets …` row is a notice; so is one in
    /// the footer, but never a warning there.
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
        let footer = screen(
            &body,
            "  ⏵⏵ auto mode on · You've hit your limit · resets 3am (Europe/London)",
        );
        assert_eq!(
            worker_phase(&footer),
            Phase::Limited {
                message: "You've hit your limit · resets 3am (Europe/London)".to_string(),
                reset: Some("3am (Europe/London)".to_string()),
            }
        );
        let warns = screen(
            &body,
            "  ⏵⏵ auto mode on     Approaching usage limit · resets at 7pm",
        );
        assert_eq!(worker_phase(&warns), Phase::Idle);
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
        assert_eq!(
            reset_of("Usage limit reached · Continuing automatically at 11:05 am · esc to cancel"),
            Some("11:05 am".to_string())
        );
        assert_eq!(
            reset_of("⚠ Usage limit reached · continuing shortly."),
            Some("shortly".to_string())
        );
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

    /// A continuation that said
    /// nothing — the notice, a blank, a done row, no words — is the same
    /// screen as the continuation cancelled by hand, and reads LIMITED
    /// on purpose: the two cannot be told apart, and the wall missed is the
    /// two-day stall while the wall read wrongly is one probe's answer
    /// (`EVENT resumed`) away — and Claude Code prints its words before a
    /// done row on every measured turn.
    #[test]
    fn a_silent_continuation_reads_as_the_cancelled_wall() {
        const NOTICE: &str =
            "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel";
        let silent = screen(
            &[
                "⏺ Bash(./run.sh --queue 8)",
                "  ⎿  queued 8 runs",
                "",
                NOTICE,
                "",
                "✻ Worked for 3m 2s · done 1:52 PM",
                "",
            ],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(
            worker_phase(&silent),
            Phase::Limited {
                message: NOTICE.to_string(),
                reset: Some("1:50pm".to_string()),
            }
        );
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

        // The same words on a real idle screen (wait_bg7), above its done row.
        let real = waiter_capture(include_str!("fixtures/wait_bg7.out"));
        for (said, want) in [
            (
                "⏺ The server now prints \"you've hit your memory limit (512 MiB)\" before the kernel would kill it.",
                Phase::Idle,
            ),
            (
                "⏺ Once you've reached your API limit the client backs off; should I also cap retries?",
                Phase::Question,
            ),
        ] {
            let r = above_status_row(real.clone(), &[said]);
            assert_eq!(last_said_row(&r), Some(said));
            assert_eq!(worker_phase(&r), want, "{said}");
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

    /// The byte readings are the char readings they replaced: the rule, the
    /// option-like caret row and the composer row, on every fixture screen,
    /// whole and with each row cut to a few widths, and on every tail of it.
    #[test]
    fn the_byte_readings_are_the_char_readings() {
        fn is_rule_by_chars(row: &str) -> bool {
            let t = row.trim_end();
            t.starts_with('─')
                && t.ends_with('─')
                && t.chars().filter(|c| *c == '─').count() >= 10
                && !t.contains(['┬', '┴', '┼', '├', '┤', '┌', '┐', '└', '┘', '│'])
        }
        fn is_option_like_by_chars(t: &str) -> bool {
            let after = t.trim_start_matches('❯').trim_start();
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            digits > 0 && after[digits..].starts_with('.')
        }
        fn composer_index_by_chars(rows: &[String]) -> Option<usize> {
            (0..rows.len()).rev().find(|&i| {
                let t = rows[i].trim_start();
                t.starts_with('❯')
                    && (!is_option_like_by_chars(t)
                        || (rows[i].starts_with('❯') && i > 0 && is_rule_by_chars(&rows[i - 1])))
                    && !is_list_entry(rows, i)
            })
        }
        for screen in crate::text::fixture_screens() {
            for width in [usize::MAX, 1, 2, 3, 6, 12, 40] {
                let rows: Vec<String> = screen
                    .iter()
                    .map(|r| r.chars().take(width).collect())
                    .collect();
                for r in &rows {
                    assert_eq!(is_rule(r), is_rule_by_chars(r), "{r:?}");
                    assert_eq!(is_option_like(r), is_option_like_by_chars(r), "{r:?}");
                    assert_eq!(
                        leading_spaces(r),
                        r.chars().take_while(|c| c.is_whitespace()).count(),
                        "{r:?}"
                    );
                }
                for k in 0..=rows.len() {
                    assert_eq!(
                        composer_index(&rows[k..]),
                        composer_index_by_chars(&rows[k..]),
                        "{:?}",
                        &rows[k..]
                    );
                }
            }
        }
    }
}
