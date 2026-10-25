// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker phase classification from a single terminal screen snapshot.

use std::borrow::Cow;
use std::fmt;

use crate::prompt::parse_prompt;
use crate::wall::{Placement, Wall, WallKind, classify_wall};

/// Major component of the Claude Code (`claude`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MAJOR: u16 = 0;

/// Minor component of the Claude Code (`claude`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MINOR: u16 = 1;

/// Version of the Claude Code harness, tracked independently per harness starting at `"0.1"`.
pub const HARNESS_VERSION: &str = "0.1";

/// Structured incremental numeric version of the Claude Code (`claude`) harness.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(
    HARNESS_VERSION_MAJOR,
    HARNESS_VERSION_MINOR,
    HARNESS_VERSION,
);

/// The worker's phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// A turn (or a background shell, workflow, agent or monitor) is running.
    Busy,
    /// An approval box is showing ([`parse_prompt`] is `Some`).
    Prompt,
    /// Not busy, no prompt, and the worker's last turn ended on a usage or rate limit notice.
    Limited {
        /// The notice as Claude Code says it (`You've reached your Fable limit. …`).
        message: String,
        /// When it resets, if the notice says (`7:30pm (America/Los_Angeles)`).
        reset: Option<String>,
    },
    /// Not busy, no prompt, the last thing said does not end in `?`.
    Idle,
    /// Not busy, no prompt, and the last thing the worker said ([`last_said_row`]) ends with `?`.
    Question,
}

impl Phase {
    /// The lowercase word the CLI prints.
    #[inline]
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Phase::Busy => "busy",
            Phase::Prompt => "prompt",
            Phase::Limited { .. } => "limited",
            Phase::Idle => "idle",
            Phase::Question => "question",
        }
    }
}

/// The glyphs Claude Code's spinner cycles through, plus the static ones it leaves mid-thought.
const SPINNERS: &[char] = &['✢', '✽', '✻', '✶', '✳', '✧', '✦', '⚡', '·', '*'];

/// Glyphs that lead a row the spinner never leads.
const NOT_SPINNER: &[char] = &['⏺', '●', '⎿', '❯', '⏵', '◯'];

/// One glyph that can lead the spinner row.
fn is_spinner_glyph(g: char) -> bool {
    SPINNERS.contains(&g)
        || (!g.is_ascii()
            && !NOT_SPINNER.contains(&g)
            && !is_box_drawing(g)
            && !g.is_alphanumeric())
}

/// A box-drawing character (U+2500–U+257F: `│`, `─`, `╌`, `╭` …), the layout's own strokes.
#[inline]
const fn is_box_drawing(g: char) -> bool {
    matches!(g, '\u{2500}'..='\u{257F}')
}

/// The shape of Claude Code's spinner row, whatever glyph it cycles.
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

/// Classify one screen.
pub fn worker_phase(rows: &[String]) -> Phase {
    if parse_prompt(rows).is_some() {
        return Phase::Prompt;
    }
    let busy = busy_signal(rows);
    if let Some(b) = busy.filter(|b| !b.soft) {
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
    /// No composer frame on the screen.
    Screen,
}

/// One busy verdict: where it was read and which rule fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub zone: Zone,
    pub rule: &'static str,
    /// Only a background monitor is running.
    pub soft: bool,
}

impl Busy {
    #[inline]
    const fn hard(zone: Zone, rule: &'static str) -> Self {
        Self {
            zone,
            rule,
            soft: false,
        }
    }
    #[inline]
    const fn soft(zone: Zone, rule: &'static str) -> Self {
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

/// The busy signal on the screen, read from the live zone when the composer frame is there.
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

/// The wait claude code carries.
fn carried_wait(rows: &[String], top: usize) -> bool {
    carried_row(rows, top).is_some()
}

/// The waiting row [`carried_wait`] reads, its first row as drawn.
fn carried_row(rows: &[String], top: usize) -> Option<usize> {
    const WAITING: [&str; 3] = [
        "waiting for a dynamic workflow",
        "waiting for a background agent",
        "waiting for background work",
    ];
    let width = crate::question::screen_width(rows);
    let i = glyph_above(rows, top)?;
    let end = status_line_end(rows, i, top, width).unwrap_or(i);
    status_busy(&status_text(&rows[i..=end]))
        .is_some_and(|rule| WAITING.contains(&rule))
        .then_some(i)
}

/// The screen reads busy by nothing but the wait claude code carries.
#[must_use]
pub fn carried_wait_alone(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| status_block(rows, f.top).status.is_none())
        && busy_signal(rows) == Some(CARRIED)
}

/// Walking up from the top rule `top` over every row, the first glyph row ([`is_glyph_row`]) met.
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

/// No status row on the screen at all.
#[must_use]
pub fn status_unseen(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        status_block(rows, f.top).status.is_none() && glyph_above(rows, f.top).is_none()
    })
}

/// A done row that says work is `still running` under any label.
fn left_running(row: &str) -> bool {
    row.split(" · ").skip(1).any(|item| {
        crate::text::trim_end(item)
            .strip_suffix(" still running")
            .is_some_and(|label| !crate::text::trim(label).is_empty())
    })
}

/// A footer row whose pill counts background work under any label.
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

/// The rules of [`busy_signal`] that are the agent's own background work with its turn over (a dynamic workflow, a background agent or other task it waits for, a shell or a monitor it left running, a workflow's progress line), never a live turn's (a spinner, `esc to interrupt`, `Still working`, a foreground command's `ctrl+b to run in background`).
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

/// The wall the turn ended on, seen through the agent's own background work.
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

/// The status row above the composer is a done row.
fn turn_over(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        status_block(rows, f.top)
            .line(rows)
            .is_some_and(|line| is_done_row(&line))
    })
}

/// A message was submitted under the status row (`❯ …` between the status row and the composer).
fn turn_just_begun(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        let block = status_block(rows, f.top);
        rows[block.from..f.top].iter().any(|r| r.starts_with('❯'))
    })
}

/// Whether the agent's own background work (a shell, a monitor, a workflow, a background agent, hard or soft), and nothing else, keeps this screen busy.
#[must_use]
pub fn background_only(rows: &[String]) -> bool {
    composer_frame(rows).is_some()
        && busy_signal(rows).is_some_and(|b| BACKGROUND_RULES.contains(&b.rule))
        && no_live_turn(rows)
        && !turn_just_begun(rows)
}

/// How many background shells Claude Code's screen counts.
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

/// One footer or status item's shell count.
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

/// No signal of a live turn on the screen.
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

/// The busy rule when the agent's own background work, and nothing else, keeps this screen busy.
#[must_use]
pub fn background_wait(rows: &[String]) -> Option<&'static str> {
    composer_frame(rows)?;
    if turn_just_begun(rows) {
        return None;
    }
    let busy = busy_signal(rows)?;
    BACKGROUND_RULES.contains(&busy.rule).then_some(busy.rule)
}

/// The [`BACKGROUND_RULES`] of a SHELL or a monitor the turn left running.
const LEFT_RUNNING_RULES: &[&str] = &[
    "a shell still running",
    "a shell running",
    "a monitor still running",
    "a monitor running",
];

/// The turn is over, and what runs on is a SHELL or a monitor it left.
#[must_use]
pub fn turn_over_background(rows: &[String]) -> bool {
    turn_over(rows) && background_wait(rows).is_some_and(|rule| LEFT_RUNNING_RULES.contains(&rule))
}

/// Who opened the turn drawn last, read up from the composer's top rule.
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

/// The status row's hard rule.
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

/// `N <noun> still running` / `N <noun>s still running` somewhere in `row`; a row that
/// never says ` still running` is not searched for either.
fn still_running(row: &str, noun: &str) -> bool {
    crate::text::contains(row, " still running")
        && (number_then(row, "", &format!(" {noun} still running"))
            || number_then(row, "", &format!(" {noun}s still running")))
}

/// `N <noun>` or `N <noun>s` as one footer item.
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

/// A workflow's progress line under the footer.
fn is_workflow_progress(row: &str) -> bool {
    use crate::agents_panel::{Bullet, PROGRESS_OLD, RowKind, one_cell, parse_row};
    (crate::text::trim_start(row).starts_with('◯')
        && PROGRESS_OLD
            .iter()
            .any(|words| row.contains(crate::text::trim_start(words))))
        || parse_row(row, one_cell).is_some_and(|r| {
            r.kind == RowKind::Workflow && r.bullet.is_some_and(|(_, b)| b != Bullet::Paused)
        })
}

/// `^\s*<glyph>\s+\S+…`.
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

/// A status row naming an activity in progress.
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

/// The text of the vendor's API-retry status row, which no ellipsis ends.
fn is_retry_status(text: &str) -> bool {
    let items: Vec<&str> = text.split(" · ").map(crate::text::trim_end).collect();
    let [_, .., last] = items.as_slice() else {
        return false;
    };
    if *last == "esc to interrupt" {
        return true;
    }
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let wait = |s: &str| {
        !s.is_empty()
            && s.split(' ')
                .all(|w| w.strip_suffix(['s', 'm', 'h', 'd']).is_some_and(number))
    };
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
    let count = |a: &str| match a.split_once('/') {
        Some((k, n)) => number(k) && number(n),
        None => number(a),
    };
    const WORDS: [&str; 2] = ["check your network", "esc to interrupt"];
    let tail =
        |item: &str| item.strip_prefix("attempt ").is_some_and(count) || WORDS.contains(&item);
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

/// The network or no-response class an API-retry row names ([`api_retry_row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryCause {
    /// The last try never reached the API: `Can't reach the API server — …`, `Connection
    /// dropped (…)`, `No internet route — …` (Claude Code 2.1.310's connection-error texts for
    /// a name that does not resolve, a socket the network dropped, and no route).
    Unreachable,
    /// The try in flight has had no first byte: `No response from the API after …`.
    NoResponse,
    /// The stream went quiet: `Waiting for API response · will retry in … · check your network`.
    Stalled,
}

/// The leads of the rows [`api_retry_row`] reads, by class. Every other lead the vendor's retry
/// row carries (a usage or rate limit, `Your request is queued`, `Auto mode check
/// unavailable`, an overload, a refused connection, a proxy) is a wait of the vendor's own or a
/// cause no network coming back cures, and reads as none.
const RETRY_LEADS: [(&str, RetryCause); 5] = [
    ("Can't reach the API server", RetryCause::Unreachable),
    ("Connection dropped (", RetryCause::Unreachable),
    ("No internet route", RetryCause::Unreachable),
    ("No response from the API after ", RetryCause::NoResponse),
    ("Waiting for API response", RetryCause::Stalled),
];

/// Claude Code's API-retry status row of a network or no-response class ([`api_retry_row`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRetryRow {
    /// The row's index.
    pub row: usize,
    pub cause: RetryCause,
    /// The countdown to the next try, in seconds (`Retrying in 32s`, `will retry in 5s`);
    /// `None` where the row counts the try in flight instead (`retrying, waiting up to 1m`).
    pub next_try_s: Option<u64>,
    /// How long the try in flight has had no response (`No response from the API after 1m 5s`),
    /// where the row says it whole.
    pub waited_s: Option<u64>,
    /// The row's text past its spinner glyph.
    pub text: String,
    /// A `key if=` guard that matches this row while it stands, whatever glyph its spinner shows
    /// and whatever its countdown and attempt read: the glyph's column, then the lead and the
    /// retry clause's words as literals, nothing after them.
    pub guard: String,
}

/// Claude Code's live API-retry status row, where it names a network or no-response class
/// ([`RetryCause`]) and sits on one row directly above the composer's frame; `None` for every
/// other status row, a retry row of another class, or one wrapped across rows.
#[must_use]
pub fn api_retry_row(rows: &[String]) -> Option<ApiRetryRow> {
    let frame = composer_frame(rows)?;
    let block = status_block(rows, frame.top);
    let at = block.status.filter(|&i| block.end == i)?;
    let row = rows[at].as_str();
    if !is_activity_row(row) {
        return None;
    }
    let t = crate::text::trim_start(row);
    let mut cs = t.chars();
    cs.next()?;
    let text = crate::text::trim(cs.as_str());
    if !is_retry_status(text) {
        return None;
    }
    let items: Vec<&str> = text.split(" · ").map(crate::text::trim_end).collect();
    let lead = items.first()?;
    let cause = RETRY_LEADS
        .iter()
        .find(|(head, _)| lead.starts_with(head))
        .map(|&(_, cause)| cause)?;
    const COUNTDOWNS: [&str; 3] = ["Retrying in ", "will retry in ", "next try in "];
    const IN_FLIGHT: [&str; 2] = ["retrying once, waiting up to ", "retrying, waiting up to "];
    let (clause, next_try_s) = items.iter().skip(1).find_map(|item| {
        COUNTDOWNS
            .iter()
            .find_map(|c| {
                item.strip_prefix(c).map(|wait| {
                    let wait = wait.split_once(" (").map_or(wait, |(w, _)| w);
                    (*c, wait_secs(wait))
                })
            })
            .or_else(|| {
                IN_FLIGHT
                    .iter()
                    .find(|c| item.starts_with(**c))
                    .map(|c| (*c, None))
            })
    })?;
    let waited_s = match cause {
        RetryCause::NoResponse => lead
            .strip_prefix("No response from the API after ")
            .and_then(wait_secs),
        RetryCause::Unreachable | RetryCause::Stalled => None,
    };
    // The lead as the screen shows it, but where it counts the try's own wait (`after 31s`),
    // which moves every second: its words up to the count.
    let fixed = match cause {
        RetryCause::NoResponse => "No response from the API after ".to_string(),
        RetryCause::Unreachable | RetryCause::Stalled => format!("{lead} · {clause}"),
    };
    let literal: String = fixed
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let glyph: String = NOT_SPINNER.iter().collect();
    Some(ApiRetryRow {
        row: at,
        cause,
        next_try_s,
        waited_s,
        text: text.to_string(),
        guard: format!(
            "^\\s*[^\\s{glyph}]\\s+{}",
            crate::anchors::guard_regex(&literal)
        ),
    })
}

/// A retry row's wait (`32s`, `1m 30s`, `2d 3h`) in seconds; `None` for anything else, a wait the
/// row cut (`1m…`) among them.
fn wait_secs(s: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut any = false;
    for w in crate::text::trim(s).split(' ') {
        let unit = w.chars().next_back()?;
        let n = &w[..w.len() - unit.len_utf8()];
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let per = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        total = total.checked_add(n.parse::<u64>().ok()?.checked_mul(per)?)?;
        any = true;
    }
    any.then_some(total)
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

/// Claude Code's composer frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) top: usize,
    pub(crate) bottom: usize,
}

pub(crate) fn composer_frame(rows: &[String]) -> Option<Frame> {
    frame_at(rows, composer_index(rows)?)
}

/// The frame around caret row `caret`.
fn frame_at(rows: &[String], caret: usize) -> Option<Frame> {
    let top = caret.checked_sub(1).filter(|&t| is_rule(&rows[t]))?;
    let bottom = (caret + 1..rows.len()).find(|&i| is_rule(&rows[i]))?;
    Some(Frame { top, bottom })
}

/// The composer's frame on a screen ([`composer_rules`]) and how much of the screen the search for it read — for a caller that keeps a reading of the frame and the rows under its bottom rule from one rebuild of a pane's rows to the next, and knows which rows changed between them (aterm's footer and lights: `footer::mode_row_under`, `footer::pill_row_under`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameReach {
    frame: Option<Frame>,
    /// The first row the search read.
    from: usize,
}

impl FrameReach {
    /// The frame on `rows` and what its search read.
    #[must_use]
    pub fn of(rows: &[String]) -> Self {
        let Some(caret) = composer_index(rows) else {
            return Self {
                frame: None,
                from: 0,
            };
        };
        let under_rule = rows[caret].starts_with('❯') && caret > 0 && is_rule(&rows[caret - 1]);
        let walked = !under_rule
            || rows[caret + 1..]
                .iter()
                .any(|r| crate::text::trim_start(r).starts_with('❯'));
        Self {
            frame: frame_at(rows, caret),
            from: if walked { 0 } else { caret.saturating_sub(1) },
        }
    }

    /// [`Self::of`], falling back to agy's or Muse Code's composer rules.
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
            return Self {
                frame: Some(Frame { top, bottom }),
                from: 0,
            };
        }
        reach
    }

    /// The frame's rules, `(top, bottom)`.
    #[must_use]
    pub fn rules(&self) -> Option<(usize, usize)> {
        self.frame.map(|f| (f.top, f.bottom))
    }

    /// The first row the search read ([`FrameReach`]).
    #[must_use]
    pub const fn from(&self) -> usize {
        self.from
    }

    /// Whether the search reads the rows as it read them before the rows `changed` changed.
    #[must_use]
    pub fn unmoved(&self, changed: &[usize]) -> bool {
        changed.iter().all(|&j| j < self.from)
    }

    /// Whether a reading of the frame and of the rows under its bottom rule alone.
    #[must_use]
    pub fn holds_under(&self, now: &FrameReach, changed: &[usize]) -> bool {
        now.frame == self.frame
            && self
                .frame
                .is_none_or(|f| changed.iter().all(|&j| j <= f.bottom))
    }
}

/// Whether the screen shows Claude Code's composer between its two rules.
pub fn has_composer_frame(rows: &[String]) -> bool {
    composer_frame(rows).is_some()
}

/// Whether Claude Code's composer frame on `rows` is caught mid-repaint.
#[must_use]
pub fn composer_frame_torn(rows: &[String]) -> bool {
    let Some(Frame { top, bottom }) = composer_frame(rows) else {
        return false;
    };
    let width = |r: &str| crate::text::trim_end(r).chars().count();
    bottom + 1 == rows.len() || width(&rows[bottom]) < width(&rows[top])
}

/// Claude Code's prompt box in shell mode.
pub(crate) fn has_shell_mode_frame(rows: &[String]) -> bool {
    shell_mode_frame(rows).is_some()
}

/// The frame of Claude Code's prompt box in shell mode: the lowest `!` caret row (alone, or
/// before a space) under a rule, down to the first rule under it.
pub(crate) fn shell_mode_frame(rows: &[String]) -> Option<Frame> {
    (1..rows.len()).rev().find_map(|i| {
        let caret = rows[i]
            .strip_prefix('!')
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\u{a0}']));
        caret.then(|| frame_at(rows, i)).flatten()
    })
}

/// Whether row `row` of `rows` is inside one of Claude Code's prompt boxes.
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

/// The index of the composer's bottom rule, when the frame is on the screen.
pub fn composer_bottom(rows: &[String]) -> Option<usize> {
    composer_frame(rows).map(|f| f.bottom)
}

/// Both composer rules, `(top, bottom)`, when the frame is on the screen.
pub fn composer_rules(rows: &[String]) -> Option<(usize, usize)> {
    composer_frame(rows).map(|f| (f.top, f.bottom))
}

/// The index of the composer's top rule, when the frame is on the screen.
pub(crate) fn composer_top(rows: &[String]) -> Option<usize> {
    composer_frame(rows).map(|f| f.top)
}

/// A full-width composer rule.
pub fn is_rule(row: &str) -> bool {
    // A rule opens on `─` (and the trim below cuts only its end): any other
    // first char rejects the row before the char-wise trims and counts.
    if !row.as_bytes().starts_with("─".as_bytes()) {
        return false;
    }
    let t = crate::text::trim_end(row);
    // Most rules are `─` and nothing else: one compare with a run of them
    // answers what `rule_run` would, without its walk of every char.
    if t.len() >= 10 * "─".len()
        && PLAIN_RULE
            .get(..t.len())
            .is_some_and(|dashes| t.as_bytes() == dashes)
    {
        return true;
    }
    t.ends_with('─') && rule_run(t)
}

/// `─` 1024 times, wider than any terminal row: a plain rule is a prefix of it.
const PLAIN_RULE: [u8; 3 * 1024] = {
    let mut run = [0u8; 3 * 1024];
    let mut i = 0;
    while i < run.len() {
        run[i] = 0xE2;
        run[i + 1] = 0x94;
        run[i + 2] = 0x80;
        i += 3;
    }
    run
};

/// Whether `t` holds ten `─` or more and none of the box's joins and sides (`┬ ┴ ┼ ├ ┤ ┌ ┐ └ ┘ │`).
const fn rule_run(t: &str) -> bool {
    let b = t.as_bytes();
    let mut dashes = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0xE2 && i + 2 < b.len() && b[i + 1] == 0x94 {
            match b[i + 2] {
                0x80 => dashes += 1,
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

/// The width in chars of a plain rule.
pub(crate) fn plain_rule_width(row: &str) -> Option<usize> {
    let t = crate::text::trim_end(row).as_bytes();
    let dash = "─".as_bytes();
    (t.len() >= 10 * dash.len() && t.chunks(dash.len()).all(|c| c == dash))
        .then_some(t.len() / dash.len())
}

/// The live zone above the top rule, found by walking up from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusBlock {
    pub(crate) status: Option<usize>,
    /// The status line's last row.
    pub(crate) end: usize,
    pub(crate) from: usize,
}

impl StatusBlock {
    /// The status row's text, its line read across the rows Ink wrapped it onto, joined with one space ([`crate::wrap::rejoined`]).
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

/// The status row's line, wrapped in a narrow pane.
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
    let run = crate::wrap::run_end(rows, i, lead, until);
    if text.is_empty() && !is_glyph_row(row) {
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

/// Whether `text` opens a parenthesis it does not close.
fn paren_open(text: &str) -> bool {
    text.matches('(').count() > text.matches(')').count()
}

/// A row that opens a block of its own under the status row, never the rest of its line.
fn opens_block(row: &str) -> bool {
    crate::text::trim_start(row).starts_with(['⎿', '❯', '!', '●', '⏺'])
        || is_glyph_row(row)
        || is_transcript_row(row)
        || ends_live_zone(row)
}

/// The text of a status line's rows `block` ([`StatusBlock::line`]).
fn status_text(block: &[String]) -> String {
    let mut cs = crate::text::trim(&block[0]).chars();
    if cs.next().is_some() && cs.next().is_none() {
        bare_line(block)
    } else {
        crate::wrap::rejoined(block)
    }
}

/// The line of a bare glyph's block (`block[0]` the glyph's row, [`status_line_end`]).
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

/// A status line with a shape of its own.
fn has_status_shape(line: &str) -> bool {
    is_activity_row(line)
        || line
            .split_once("Waiting for ")
            .is_some_and(|(_, n)| n.starts_with(|c: char| c.is_ascii_digit()))
        || is_done_shape(line)
}

/// A done row's shape: past the glyph, a word, ` for `.
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

/// A row no live zone reaches past, walking up from the composer's top rule.
fn ends_live_zone(row: &str) -> bool {
    is_rule(row) || is_launch_card_row(row)
}

/// The launch card's own logo row (` ▐▛███▜▌ Claude Code v2.1.283`).
fn is_launch_card_row(row: &str) -> bool {
    crate::text::trim_start(row).starts_with('\u{2590}')
        && row.contains(const { crate::anchor!("launch.banner") })
}

/// A row that can be the status row.
pub fn is_glyph_row(row: &str) -> bool {
    let mut cs = row.chars();
    (cs.next().is_some_and(|g| SPINNERS.contains(&g)) && cs.next().is_some_and(char::is_whitespace))
        || is_spinner_shape(row)
}

/// A row only the transcript has.
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

/// A `⎿ Tip: …` row, or a row of the todo list Claude Code can hang under its spinner.
fn is_tip_or_todo(t: &str) -> bool {
    let under_gutter = t.strip_prefix('⎿').map(str::trim_start);
    under_gutter.is_some_and(|g| g.starts_with("Tip:")) || {
        let item = under_gutter.unwrap_or(t);
        item.starts_with(['☐', '☒', '☑', '◻', '◼', '□', '■']) || item.starts_with("… +")
    }
}

/// Claude Code's session survey, which it parks above the composer between turns and during them.
pub fn is_survey(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    (t.starts_with('●') && t.contains("How is Claude doing this session"))
        || (t.starts_with("1: Bad") && t.contains("0: Dismiss"))
}

/// Whether a rating card (the session survey or another, [`crate::survey::survey_block`]) is open.
pub fn survey_open(rows: &[String]) -> bool {
    crate::survey::survey_block(rows).is_some()
}

/// How much of the worker's context is left before Claude Code auto-compacts it, as its screen
/// shows it: the one reading of Claude Code's indicator, which the supervisor's watch
/// (`EVENT context`), `aterm drive phase` and the window's displays all take.
///
/// Every build measured (2.1.281, 2.1.282, 2.1.304 and 2.1.310) draws the indicator under the
/// composer's bottom rule, against the rule's right edge (`context_under`), and keeps it there in
/// shell mode, whose box has a `!` caret (`context_fixtures::SHELL_MODE`). 2.1.267 and 2.1.268
/// drew it in the live zone above the composer's top rule (`context_above`,
/// `context_fixtures::ABOVE_OLDER`), which is read when the footer holds none: the move came
/// between 2.1.268 and 2.1.281.
pub fn context_left(rows: &[String]) -> Option<u8> {
    let frame = composer_frame(rows).or_else(|| shell_mode_frame(rows))?;
    let width = rule_width(rows, frame);
    context_under(rows, frame, width).or_else(|| context_above(rows, frame, width))
}

/// [`context_left`]'s reading under `frame`, its rule `width` cells: the indicator ending a row
/// of the footer (the rows down to a blank) against the rule's right edge. It is the mode row's
/// last run, after many spaces where the row has room and after one where it has little, and on a
/// row of its own under the mode row where the pane is too narrow for both (measured on 2.1.310
/// at 120, 60 and 44 columns, `context_fixtures`). The last recognized footer row establishes
/// the reading's start, after a custom status line if present: completion-menu descriptions and
/// custom status lines occupy the same space and can imitate the footer's words too.
fn context_under(rows: &[String], frame: Frame, width: usize) -> Option<u8> {
    let from = frame.bottom + 1;
    let footer = &rows[from..footer_end(rows, from)];
    let first = footer.iter().rposition(|row| {
        let text = crate::text::trim_start(row);
        let mode = text
            .strip_prefix(crate::anchor!("vim.insert"))
            .and_then(|after| after.strip_prefix(' '))
            .unwrap_or(text);
        let bare = ["footer.shortcuts", "footer.clear", "busy.interrupt"]
            .into_iter()
            .any(|id| text.starts_with(crate::anchor(id)));
        let shell = text.starts_with("! for shell mode") && shell_mode_frame(rows) == Some(frame);
        is_mode_row(mode) || bare || shell
    })?;
    footer[first..]
        .iter()
        .enumerate()
        .map(|(i, row)| (i, crate::text::trim_end(row)))
        .filter(|(_, row)| row.chars().count() + 3 >= width)
        .find_map(|(i, row)| {
            if i == 0 {
                context_at_end(row)
            } else {
                context_reading(crate::text::trim(row))
            }
        })
}

/// The percentage an indicator ending `row` reads: the indicator's words alone, or after the
/// row's other words and a space.
fn context_at_end(row: &str) -> Option<u8> {
    let t = crate::text::trim(row);
    let digits = |head: &str| head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let start = t
        .strip_suffix("% until auto-compact")
        .map(digits)
        .or_else(|| {
            let head = &t[..digits(t.strip_suffix('%')?)];
            head.strip_suffix("Context left until auto-compact: ")
                .map(str::len)
        })?;
    let (words, indicator) = t.split_at(start);
    (words.is_empty() || words.ends_with(' '))
        .then(|| context_reading(indicator))
        .flatten()
}

/// The cells of the composer's bottom rule in `frame`, trimmed.
fn rule_width(rows: &[String], frame: Frame) -> usize {
    crate::text::trim_end(&rows[frame.bottom]).chars().count()
}

/// [`context_left`]'s reading in the live zone above `frame` (Claude Code 2.1.267 and 2.1.268), its
/// rule `width` cells.
fn context_above(rows: &[String], frame: Frame, width: usize) -> Option<u8> {
    let from = status_block(rows, frame.top).from;
    rows[from..frame.top]
        .iter()
        .rev()
        .filter(|row| is_against_right_edge(row, width))
        .find_map(|row| context_reading(crate::text::trim(row)))
}

/// The percentage an indicator reads, `t` its row trimmed.
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

/// The status row: walking up from the composer's top rule.
pub fn status_row(rows: &[String]) -> Option<&str> {
    let frame = composer_frame(rows)?;
    status_block(rows, frame.top)
        .status
        .map(|i| rows[i].as_str())
}

/// The status row's line ([`status_row`]).
#[must_use]
pub fn status_line(rows: &[String]) -> Option<Cow<'_, str>> {
    let frame = composer_frame(rows)?;
    status_block(rows, frame.top).line(rows)
}

/// The first row of the live zone, by position.
#[must_use]
pub fn busy_zone_start(rows: &[String]) -> usize {
    let boxed = crate::prompt::prompt_box_first_row(rows);
    let start = match composer_frame(rows) {
        Some(frame) => {
            let block = status_block(rows, frame.top);
            let start = block
                .status
                .or_else(|| carried_row(rows, frame.top))
                .unwrap_or(block.from);
            boxed.map_or(start, |b| b.min(start))
        }
        None => boxed.unwrap_or(0),
    };
    let notice = const { crate::anchor!("exit.resume") };
    let exited = rows
        .iter()
        .rposition(|r| crate::text::trim_end(r) == notice)
        .map_or(0, |i| i + 1);
    start.max(exited)
}

/// The composer row: the last row whose first glyph is `❯` and that is not an option row.
pub fn composer_index(rows: &[String]) -> Option<usize> {
    let mut i = rows.len();
    while i > 0 {
        i -= 1;
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

/// Whether the indented `❯` row `i` is an entry of a list drawn under the composer's bottom rule.
fn is_list_entry(rows: &[String], i: usize) -> bool {
    if rows[i].starts_with('❯') && i > 0 && is_rule(&rows[i - 1]) {
        return false;
    }
    let mut first = i;
    while first > 0
        && !is_rule(&rows[first - 1])
        && (leading_spaces(&rows[first - 1]) > 0
            || crate::text::trim_end(&rows[first - 1]).is_empty())
    {
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

/// The completion menu Claude Code opens under the composer's bottom rule.
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

/// The column a build that draws the completion menu's pointer starts every entry at.
pub const POINTER_ROOM: usize = 4;

/// One thing Enter may do over Claude Code's completion menu ([`CompletionMenu::runs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEnter<'a> {
    /// Run this slash-menu entry, by its first word (`/clear`).
    Run(&'a str),
    /// Complete this `@`-menu entry into the draft (`+` for a file, `*` for an agent).
    Complete(&'a str),
    /// Send the draft as the composer shows it.
    Draft,
}

/// Claude Code's completion menu as [`completion_menu`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMenu {
    /// The entries shown, top to bottom.
    pub entries: Vec<MenuEntry>,
    /// The entry the Select's pointer marks.
    pub pointed: Option<usize>,
    /// The `@` menu of files (`+`) and agents (`*`).
    pub mentions: bool,
}

impl CompletionMenu {
    /// Whether the selected entry is drawn only in colour.
    #[must_use]
    pub const fn colour_only(&self) -> bool {
        self.pointed.is_none() && self.entries.len() > 1
    }

    /// What enter may do: run the pointed or selected entry (the slash menu).
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

/// Whether every colour of `fg` is the same.
fn all_alike(fg: &[Option<String>]) -> Option<bool> {
    let fg: Vec<String> = fg
        .iter()
        .map(|c| c.as_deref().map(str::to_ascii_lowercase))
        .collect::<Option<_>>()?;
    Some(fg.windows(2).all(|w| w[0] == w[1]))
}

/// The one of `fg` unlike all the others, which are at least two and all alike.
fn odd_one_out(fg: &[Option<String>]) -> Option<usize> {
    let fg: Vec<String> = fg
        .iter()
        .map(|c| c.as_deref().map(str::to_ascii_lowercase))
        .collect::<Option<_>>()?;
    if fg.len() < 3 {
        return None;
    }
    let rest = if fg[0] == fg[1] { &fg[0] } else { &fg[2] };
    let mut odd = (0..fg.len()).filter(|&k| fg[k] != *rest);
    let k = odd.next()?;
    odd.next().is_none().then_some(k)
}

fn is_option_like(t: &str) -> bool {
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

/// Whether the cursor sits inside the composer's typed text.
#[must_use]
pub const fn cursor_inside_text(cursor: Option<(usize, usize)>, caret: usize) -> bool {
    matches!(cursor, Some((row, col)) if row == caret && col != 2)
}

/// The composer's rows: the caret row's index.
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

/// Whether the composer's text is Claude Code's dim placeholder suggestion rather than typed input.
pub fn is_placeholder(rows: &[String], cursor_col: usize) -> bool {
    composer_text(rows).is_some_and(|t| !t.is_empty()) && cursor_col == 2
}

/// A composer separator: a row that is mostly `─`.
fn is_separator(row: &str) -> bool {
    row.chars().filter(|c| *c == '─').count() >= 10
}

/// Claude Code's own status rows, never the worker's words.
const STATUS_GLYPHS: &[char] = &['✔', '✓', '✘', '✗', '⚠', 'ℹ'];

fn is_status_row(row: &str) -> bool {
    crate::text::trim_start(row)
        .chars()
        .next()
        .is_some_and(|g| SPINNERS.contains(&g) || STATUS_GLYPHS.contains(&g))
}

/// The first non-space column of a right-aligned hint.
pub(crate) const HINT_COLUMN: usize = 20;

pub(crate) fn leading_spaces(row: &str) -> usize {
    crate::text::leading_space_chars(row)
}

/// A right-aligned hint.
fn is_hint(row: &str, width: Option<usize>) -> bool {
    let lead = leading_spaces(row);
    let end = crate::text::trim_end(row).chars().count();
    end > lead && (lead >= HINT_COLUMN || (lead >= 6 && width.is_some_and(|w| end + 3 >= w)))
}

/// The last row the worker said (see [`last_said_index`]).
pub fn last_said_row(rows: &[String]) -> Option<&str> {
    last_said_index(rows).map(|i| rows[i].as_str())
}

/// The index of the last thing said above the composer.
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

/// The vendor's own `⏺` paragraph that row `i` is a row of, as its bullet row.
fn vendor_paragraph(rows: &[String], i: usize) -> Option<usize> {
    const NOTICES: &[&str] = &[crate::anchor!("notice.auto_mode")];
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

/// Where the transcript ends on the screen, by position.
pub fn transcript_end(rows: &[String]) -> usize {
    let Some(frame) = composer_frame(rows) else {
        return rows
            .iter()
            .rposition(|r| !crate::text::trim(r).is_empty())
            .map_or(0, |i| i + 1);
    };
    let block = status_block(rows, frame.top);
    if let (Some(status), Some(line)) = (block.status, block.line(rows)) {
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

/// A status row that reports a finished turn rather than work in flight.
pub fn is_done_row(row: &str) -> bool {
    !is_activity_row(row) && !row.contains("Waiting for ")
}

/// What Claude Code parks between the transcript and an idle composer.
fn is_parked_above_composer(row: &str, width: usize) -> bool {
    let t = crate::text::trim_start(row);
    t.is_empty()
        || is_survey(row)
        || is_against_right_edge(row, width)
        || t.strip_prefix('⎿')
            .is_some_and(|g| crate::text::trim_start(g).starts_with("Tip:"))
}

/// A row against the right edge, where Claude Code parks its hints, banners and context indicator.
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

/// The usage or rate limit notice the worker's last turn ended on, as `(message, reset)`.
pub fn limit_notice(rows: &[String]) -> Option<(String, Option<String>)> {
    notice(rows, &|text| {
        classify_wall(text).filter(WallKind::reads_limited)
    })
    .map(|w| (w.message, w.reset))
}

/// The notice the worker's last turn ended on, placed as [`limit_notice`] places it.
pub(crate) fn notice(rows: &[String], classify: &dyn Fn(&str) -> Option<WallKind>) -> Option<Wall> {
    let mut found = notice_placed(rows, classify)?;
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

/// A hint or banner row that says Claude Code goes on by itself.
fn says_the_vendor_goes_on(row: &str) -> bool {
    let flat = row.replace('\u{2219}', "\u{b7}");
    find_ascii_ci(&flat, const { crate::anchor!("wall.auto_continue") }).is_some()
        || find_ascii_ci(&flat, "continuing shortly").is_some()
        || find_ascii_ci(&flat, "available again \u{b7} continuing now").is_some()
}

fn notice_placed(rows: &[String], classify: &dyn Fn(&str) -> Option<WallKind>) -> Option<Wall> {
    if let Some(frame) = composer_frame(rows) {
        let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
        if let Some(found) = footer_notice(rows, frame.bottom + 1, width, classify)
            .or_else(|| banner_notice(rows, frame.top, width, classify))
        {
            return Some(crate::wall::named_by_the_turn(rows, found, classify));
        }
    }
    let last = last_said_index(rows)?;
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

/// Whether a `/goal` echo's own reason can come from a wall of `cause`'s kind.
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

/// The `⏺` row a `/goal` notice opens on, when the last thing said (ending at `last`) is one.
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

/// How many rows a `<notice> · <remedy>` error row may take in a pane 80 columns wide or wider.
const ERROR_ROWS: usize = 3;

/// How many columns short of the pane Claude Code draws the text of its own `⏺` rows.
const NOTICE_MARGIN: usize = 10;

/// The rows the vendor's `⏺` notice on rows `open..=end` would take at 80 columns.
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

/// Whether `row` is a row of the `⏺` message above it.
fn in_message(row: &str) -> bool {
    row.strip_prefix("  ")
        .is_some_and(|t| t.starts_with(|c: char| !c.is_whitespace() && c != '⎿'))
}

/// The last row of the `⏺` message (its paragraph) whose bullet row is `open`.
fn message_end(rows: &[String], open: usize, until: usize) -> usize {
    let until = until.min(rows.len());
    let mut end = open;
    while end + 1 < until && in_message(&rows[end + 1]) {
        end += 1;
    }
    end
}

/// The line row `i` opens, read across the rows Ink wrapped it onto ([`crate::wrap`]).
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

/// The vendor's error row the last turn ended on.
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

/// The line the screen's last thing said opens where Claude Code draws a row of its own there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaidNotice {
    /// Its text, the rows Ink wrapped it onto joined, the vendor's [`EXPAND_HINT`] dropped.
    pub text: String,
    /// It has the vendor's error row's own shape, which no table needs to name.
    pub shaped: bool,
    /// The row its line opens on: the `⏺` row, or the block's `⎿` row.
    pub row: usize,
    /// [`Placement::ErrorRow`] for a `⏺` row, [`Placement::Gutter`] for a `⎿` block.
    pub placement: Placement,
    /// The block stands right under a hook's error row, where the hook's own words do.
    pub hook_words: bool,
}

/// The last thing said, where claude code draws its own notices.
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

/// The column Claude Code draws its own `⎿` rows at (` ⎿ …`).
const VENDOR_GUTTER_COLUMN: usize = 2;

/// What the owner walk ([`block_is_vendors`]) makes of a `⎿` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// A tool's output: never a wall.
    Output,
    /// The row right under a hook's error row (rule 4).
    HookWords,
    /// Read as Claude Code's own row, from a later column-2 row under a call (rule 3).
    UnderCall,
    /// Read as Claude Code's own row under any other owner.
    Vendors,
    /// Read as [`Block::Vendors`].
    Unowned,
}

/// Whether the `⎿` block opening at `open` is read as Claude Code's own row.
fn block_is_vendors(rows: &[String], open: usize) -> Block {
    if leading_spaces(&rows[open]) != VENDOR_GUTTER_COLUMN {
        return Block::Output;
    }
    let mut later = false;
    let mut labelled = false;
    let mut adjacent = true;
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

/// Whether the owner walk ([`block_is_vendors`]) from the `⎿` block row `i` stands in reaches the first of `rows` with no owner found — no row in column 0, no blank row over a row that opens an item — so whatever owns the block, if anything does, is above the rows.
#[must_use]
pub fn owner_above_rows(rows: &[String], i: usize) -> bool {
    gutter_open(rows, i).is_some_and(|open| block_is_vendors(rows, open) == Block::Unowned)
}

/// Whether `row` is Claude Code's stop-hook summary.
fn is_stop_hook_summary(row: &str) -> bool {
    let Some(rest) = row
        .strip_prefix(['⏺', '●'])
        .map(crate::text::trim_start)
        .and_then(|r| r.strip_prefix(crate::anchor!("hook.ran")))
    else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0
        && rest[digits..]
            .strip_prefix(' ')
            .is_some_and(|r| r.starts_with(crate::anchor!("hook.ran.stop")))
}

/// Whether `text`, a column-2 `⎿` row's past its `⎿`.
fn is_summary_label(text: &str) -> bool {
    text.split_once(crate::anchor!("hook.error.label"))
        .is_some_and(|(name, _)| !name.is_empty() && !name.contains(char::is_whitespace))
        || text.starts_with(crate::anchor!("hook.feedback"))
}

/// Whether `text`, a column-2 `⎿` row's past its `⎿`, is the row a hook's error opens.
fn is_hook_error_row(text: &str) -> bool {
    [
        crate::anchor!("hook.error"),
        crate::anchor!("hook.blocking_error"),
    ]
    .iter()
    .any(|tail| {
        text.strip_suffix(tail)
            .is_some_and(|name| !name.is_empty() && !name.contains(char::is_whitespace))
    })
}

/// Whether row `i` carries a program's words, never a notice of Claude Code's own.
pub(crate) fn programs_words(rows: &[String], i: usize) -> bool {
    words_of(rows, i) == WordsOf::Program
}

/// Whose words row `i` holds, as [`programs_words`] reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordsOf {
    /// A program's words ([`programs_words`]).
    Program,
    /// Claude Code's own row, or read as its own.
    Vendor,
    /// Read as [`WordsOf::Vendor`] is, for want of the rows above.
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
    /// At the first of the rows, still looking.
    Top,
}

/// The `⏺` or `●` row whose message row `i` is a later row of.
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

/// The `⎿` row that row `i` stands under.
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

/// A transcript row that opens a call.
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

/// `<Name>[<tool>]`, then the arguments' `(…)` or nothing.
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

/// The name of a `Name(…)` call.
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

/// A notice in the footer, under the bottom rule (from row `from`) and above the first blank row.
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

/// What of footer row `i` Claude Code itself drew a notice in, measured on 2.1.304 (`claude-2.1.304-footer-*`, `…-statusline-login-words-*`, `…-wall-session-auto-continue-*`) and on 2.1.299 and 2.1.300 at 26, 34, 50 and 80 columns.
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

/// The footer's mode row, the left column's last.
fn is_mode_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    t.starts_with(['⏵', '⏸']) || t.starts_with("? for shortcuts")
}

/// One past the footer's last row.
pub(crate) fn footer_end(rows: &[String], from: usize) -> usize {
    (from..rows.len())
        .find(|&i| crate::text::trim(&rows[i]).is_empty())
        .unwrap_or(rows.len())
}

/// A notice on a banner row under the last thing said.
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

/// The column the `⎿` gutter's text starts at (` ⎿ queued 8 runs`).
const GUTTER_TEXT_COLUMN: usize = 5;

/// Whether row `i` stands in a `⎿` block a call owns.
pub(crate) fn under_a_call(rows: &[String], i: usize) -> bool {
    gutter_open(rows, i).is_some_and(|open| {
        !matches!(
            block_is_vendors(rows, open),
            Block::Vendors | Block::Unowned
        )
    })
}

/// The `⎿` row that opens the gutter block row `i` belongs to.
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

/// The reset a limit notice's text names.
#[must_use]
pub fn notice_reset(message: &str) -> Option<String> {
    reset_of(message)
}

/// When the notice says the limit resets.
fn reset_of(message: &str) -> Option<String> {
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
    ["continuing shortly", "continuing automatically shortly"]
        .iter()
        .find_map(|k| find_ascii_ci(message, k))
        .map(|_| "shortly".to_string())
}

/// The reset text at the start of `rest`.
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

/// The words `text` opens with that are a time.
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

/// A word of a reset's time ([`time_words`]) that is a time's own.
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

/// Claude Code's context indicator on recorded screens (rows by `prompt::fixtures::screen`), for
/// the tests of [`context_left`] and of what takes its reading: the supervisor's watch and the
/// window's footer.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod context_fixtures {
    /// 2.1.310 at rest after a long stub reply: `14% until auto-compact` ends the mode row.
    pub const MODE_ROW_14: &str =
        include_str!("fixtures/claude-2.1.310-context-mode-row-turn1.txt");
    /// 2.1.310 after the next reply: `7% until auto-compact`, under the watch's threshold of 10.
    pub const MODE_ROW_7: &str = include_str!("fixtures/claude-2.1.310-context-mode-row-turn2.txt");
    /// [`MODE_ROW_7`] narrowed to 60 columns: one space between the mode row's words and it.
    pub const MODE_ROW_60COL: &str =
        include_str!("fixtures/claude-2.1.310-context-mode-row-60col.txt");
    /// [`MODE_ROW_7`] narrowed to 44 columns: on a row of its own under the mode row.
    pub const MODE_ROW_44COL: &str =
        include_str!("fixtures/claude-2.1.310-context-mode-row-44col.txt");
    /// The `/` menu open over [`MODE_ROW_7`]: it stands in the mode row's place.
    pub const SLASH_MENU: &str = include_str!("fixtures/claude-2.1.310-context-slash-open.txt");
    /// The `@` menu open over [`MODE_ROW_7`].
    pub const AT_MENU: &str = include_str!("fixtures/claude-2.1.310-context-at-open.txt");
    /// The `?` shortcuts open over [`MODE_ROW_7`].
    pub const SHORTCUTS: &str = include_str!("fixtures/claude-2.1.310-context-help-open.txt");
    /// 1 s after `/compact`: Claude Code's `◐ medium · /effort` notice in the indicator's slot.
    pub const COMPACTED_EFFORT: &str =
        include_str!("fixtures/claude-2.1.310-context-compacted-effort.txt");
    /// 15 s after `/compact`: the slot empty.
    pub const COMPACTED: &str = include_str!("fixtures/claude-2.1.310-context-compacted.txt");
    /// 15 s after `/compact` at 44 columns: the slot empty (no effort notice stood in it at 1 s).
    pub const COMPACTED_44COL: &str =
        include_str!("fixtures/claude-2.1.310-context-compacted-44col.txt");
    /// 2.1.281, the oldest build measured (2.1.281, 2.1.282, 2.1.304 and 2.1.310 all draw it on
    /// the mode row): `0% until auto-compact` ends the mode row.
    pub const MODE_ROW_2_1_281: &str =
        include_str!("fixtures/claude-2.1.281-context-mode-row-turn1.txt");
    /// 2.1.310 in shell mode (`!` typed into an empty composer over [`MODE_ROW_7`]): the box's
    /// caret is `!` and the indicator stays at the mode row's end.
    pub const SHELL_MODE: &str = include_str!("fixtures/claude-2.1.310-context-shell-mode.txt");
    /// 2.1.267/2.1.268, the builds a driven worker ran when the watch was written (seen
    /// 2026-09-13, cc86c5cfe9; recorded 2026-09-14): `1% until auto-compact` above the composer's
    /// top rule. The indicator moved to the mode row between 2.1.268 and 2.1.281.
    pub const ABOVE_OLDER: &str = include_str!("fixtures/context-low.txt");

    /// A constructed agent-menu description ending exactly at the rule's right edge, where
    /// the real indicator sits. The recorded menu's composer and other entries stay intact.
    pub fn menu_description(left: u8) -> String {
        let mut rows = crate::prompt::fixtures::screen(AT_MENU);
        let frame = super::composer_frame(&rows).expect("the recorded menu's composer");
        let width = super::rule_width(&rows, frame);
        let prefix = "    * audit (agent) – ";
        let suffix = format!("{left}% until auto-compact");
        let pad = width - prefix.chars().count() - suffix.chars().count() - 1;
        let entry = rows
            .iter_mut()
            .find(|row| row.starts_with("    * claude (agent)"))
            .expect("the recorded menu's agent entry");
        *entry = format!("{prefix}{} {suffix}", "a".repeat(pad));
        rows.join("\n")
    }
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

    /// `is_rule`'s compare with `PLAIN_RULE` reads every row as the walk of its chars reads it:
    /// plain rules of every width to past the run's end, and each with trailing spaces, a join,
    /// a label or another char.
    #[test]
    fn a_rule_read_by_its_compare_is_the_rule_its_walk_reads() {
        let walked = |row: &str| {
            let t = crate::text::trim_end(row);
            t.starts_with('─') && t.ends_with('─') && rule_run(t)
        };
        for n in 0..=PLAIN_RULE.len() / 3 + 2 {
            let dashes = "─".repeat(n);
            for row in [
                dashes.clone(),
                format!("{dashes}  \u{a0}"),
                format!("{dashes}┬"),
                format!("{dashes} title ─"),
                format!("{dashes}x"),
                format!("─┼{dashes}"),
            ] {
                assert_eq!(is_rule(&row), walked(&row), "{n} dashes: {row:?}");
            }
        }
    }

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
                .rposition(|row| row.contains(crate::anchor!("launch.banner")))
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

    #[test]
    fn a_rule_above_the_frame_ends_the_live_zone() {
        let mut r = rows(&["⏺ Working on it.", "", "✢ Deliberating… (12s)", ""]);
        r.extend(composer("  ⏵⏵ auto mode on · esc to interrupt"));
        r.extend(rows(&["user@host proj % claude", ""]));
        r.extend(composer("  ? for shortcuts"));
        assert_eq!(busy_signal(&r), None, "{r:#?}");
        assert_eq!(worker_phase(&r), Phase::Idle);
        let mut live = rows(&["user@host proj % claude", "", "✢ Deliberating… (12s)", ""]);
        live.extend(composer("  ⏵⏵ auto mode on · esc to interrupt"));
        assert_eq!(worker_phase(&live), Phase::Busy);
    }

    fn busy_rows(r: &[String]) -> Vec<usize> {
        let busy = crate::anchor!("busy.interrupt");
        (0..r.len()).filter(|&i| r[i].contains(busy)).collect()
    }

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
                .rposition(|row| row.contains(crate::anchor!("launch.banner")))
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

    #[test]
    fn the_live_zone_starts_under_an_exited_processs_resume_hint() {
        use crate::prompt::fixtures::{
            CTRLC_RELAUNCH_STALE_QUESTION, EXIT_RELAUNCH_STALE_FOOTER, screen,
        };
        let notice = crate::anchor!("exit.resume");
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
            let predraw = r[..hint + 3].to_vec();
            assert_eq!(busy_zone_start(&predraw), hint + 1, "{name}");
            let dead = predraw
                .iter()
                .position(|row| row.contains(footer))
                .expect(name);
            assert!(dead < hint, "{name}");
            let lived = r[..=dead].to_vec();
            assert!(busy_zone_start(&lived) < dead, "{name}: {lived:#?}");
        }
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

    #[test]
    fn a_prompt_beats_a_background_shell() {
        let mut r = bash_one_row();
        let last = r.len() - 1;
        r[last] = "  ⏵⏵ auto mode on · 1 shell still running".to_string();
        assert_eq!(worker_phase(&r), Phase::Prompt);
    }

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
        let shell = rows(&["$ ls", "a b", "Continue?", ""]);
        assert_eq!(worker_phase(&shell), Phase::Question);
    }

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
        let p = bash_one_row();
        let ci = composer_index(&p).expect("composer");
        assert_eq!(p[ci], "❯");
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

    fn runs_of(rows: &[String]) -> Option<Vec<String>> {
        completion_menu(rows).map(|m| m.runs(None).into_iter().map(said).collect())
    }

    fn said(e: MenuEnter<'_>) -> String {
        match e {
            MenuEnter::Run(w) | MenuEnter::Complete(w) => w.to_string(),
            MenuEnter::Draft => "<draft>".to_string(),
        }
    }

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

    fn with_footer(mut r: Vec<String>, footer: &str) -> Vec<String> {
        let bottom = r.iter().rposition(|x| is_rule(x)).expect("bottom rule");
        r[bottom + 1] = footer.to_string();
        r
    }

    fn above_frame(mut r: Vec<String>, extra: &[&str]) -> Vec<String> {
        let top = composer_frame(&r).expect("frame").top;
        for (k, row) in extra.iter().enumerate() {
            r.insert(top + k, (*row).to_string());
        }
        r
    }

    fn above_status_row(mut r: Vec<String>, extra: &[&str]) -> Vec<String> {
        let top = composer_frame(&r).expect("frame").top;
        let at = status_block(&r, top).status.expect("a status row");
        for (k, row) in extra.iter().enumerate() {
            r.insert(at + k, (*row).to_string());
        }
        r
    }

    const IDLE_AUTO_FOOTER: &str = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents                                                                         /rc active";
    const PASTE_FOOTER: &str = "  paste again to expand                                                                                                       /rc active";

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

    #[test]
    fn background_work_after_the_turn_is_told_from_a_live_turn() {
        let bg = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert_eq!(background_wait(&bg), Some("waiting for a dynamic workflow"));
        let shell = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert_eq!(background_wait(&shell), Some("a shell still running"));
        let live = waiter_capture(include_str!("fixtures/wait_bg2.out"));
        assert_eq!(worker_phase(&live), Phase::Busy);
        assert_eq!(background_wait(&live), None);
        let idle = waiter_capture(include_str!("fixtures/wait_bg3.out"));
        assert_eq!(background_wait(&idle), None);
        let bare: Vec<String> = vec!["✻ Waiting for 1 dynamic workflow to finish".to_string()];
        assert_eq!(background_wait(&bare), None);
    }

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
        let at = woke
            .iter()
            .rposition(|r| r.starts_with("⏺ Background command"))
            .expect("the wake");
        let mut asked = woke.clone();
        asked.insert(at + 1, "❯ Also update the README.".to_string());
        assert_eq!(opened_by_own_wake(&asked), Some(false));
        let queued = above_frame(woke, &["❯ Also update the README."]);
        assert_eq!(opened_by_own_wake(&queued), Some(false));
        assert_eq!(opened_by_own_wake(&shell), None);
        let bare = vec!["⏺ Background command \"x\" completed (exit code 0)".to_string()];
        assert_eq!(opened_by_own_wake(&bare), None);
    }

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

    #[test]
    fn claude_2_1_271_idle_over_two_shells_is_a_break_of_its_own_work() {
        use crate::prompt::fixtures::IDLE_SHELLS_2_1_271 as TEXT;
        use crate::reader::{Program, identify};
        let r = crate::prompt::fixtures::screen(TEXT);
        let done = "✻ Cogitated for 3m 12s · done 9:29 PM · 2 shells still running";
        assert_eq!(status_row(&r), Some(done));
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(background_wait(&r), Some("a shell still running"));
        for reader in [identify(Some("claude"), &r), identify(Some("node"), &r)] {
            assert_eq!(reader.program(), Program::Claude);
            assert_eq!(reader.phase(&r), Phase::Busy);
            assert_eq!(reader.background_wait(&r), Some("a shell still running"));
            assert!(reader.wall(&r).is_none(), "{:?}", reader.wall(&r));
        }

        let at = r.iter().position(|x| x == done).expect("the done row");
        let mut footer_only = r.clone();
        footer_only[at] = "✻ Cogitated for 3m 12s · done 9:29 PM".to_string();
        assert_eq!(
            signal(&footer_only).as_deref(),
            Some("footer: a shell running")
        );
        assert_eq!(worker_phase(&footer_only), Phase::Busy);
        assert_eq!(background_wait(&footer_only), Some("a shell running"));

        const BARE: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        let done_only = with_footer(r.clone(), BARE);
        assert_eq!(
            signal(&done_only).as_deref(),
            Some("status row: a shell still running")
        );
        assert_eq!(worker_phase(&done_only), Phase::Busy);
        assert_eq!(background_wait(&done_only), Some("a shell still running"));

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

    #[test]
    fn a_pointed_agents_panel_row_is_not_the_composer() {
        let rule = "\u{2500}".repeat(60);
        for (main, agent) in [
            (
                "  ⏺ main",
                "  ◯ salvage-merge  ▱▱▱▱  0/5   12m43s · ↓ 943.7k tokens",
            ),
            (
                "❯ ⏺ main",
                "  ◯ salvage-merge  ▱▱▱▱  0/5   12m43s · ↓ 943.7k tokens",
            ),
            (
                "  ⏺ main",
                "❯ ◯ salvage-merge  ▱▱▱▱  0/5   12m43s · ↓ 943.7k tokens",
            ),
        ] {
            let r = rows(&[
                "⏺ Done.",
                "",
                &rule,
                "❯ ",
                &rule,
                "  ⏵⏵ bypass permissions on · 2 shells · ↓ to manage",
                "",
                main,
                agent,
            ]);
            assert_eq!(composer_index(&r), Some(3), "{r:?}");
            let reach = FrameReach::of(&r);
            assert_eq!(reach.rules(), Some((2, 4)), "{r:?}");
            assert_eq!(reach.rules(), composer_rules(&r));
        }
        let agent = "◯ salvage-merge  ▱▱▱▱  0/5   12m43s · ↓ 943.7k tokens";
        for under in [format!("  {agent}"), format!("❯ {agent}")] {
            let r = rows(&[
                "⏺ Done.",
                "",
                &rule,
                &format!("❯ {agent}"),
                &rule,
                "  ⏵⏵ bypass permissions on · 2 shells · ↓ to manage",
                "",
                "  ⏺ main",
                &under,
            ]);
            assert_eq!(composer_index(&r), Some(3), "{r:?}");
        }
    }

    #[test]
    fn the_measured_selected_panel_rows_are_no_caret() {
        for text in [
            include_str!("fixtures/claude-2.1.304-agents-panel.txt"),
            include_str!("fixtures/claude-2.1.304-agents-panel-later.txt"),
            include_str!("fixtures/claude-2.1.304-agents-panel-main-selected.txt"),
            include_str!("fixtures/claude-2.1.304-agents-panel-agent-selected.txt"),
        ] {
            let r: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let caret = r
                .iter()
                .position(|row| row == "❯")
                .expect("the empty caret");
            assert_eq!(composer_index(&r), Some(caret), "{text}");
            let frame = composer_frame(&r).expect("the frame stands");
            assert_eq!((frame.top, frame.bottom), (caret - 1, caret + 1));
            assert_eq!(worker_phase(&r), Phase::Busy);
            assert_eq!(
                signal(&r).as_deref(),
                Some("status row: waiting for a background agent")
            );
        }
    }

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
            (
                "  ◯ nightly-sweep  ▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/4 · 3m02s · ↓ 12.4k tokens",
                "footer: a workflow running",
                false,
            ),
            (
                "  ◯ nightly-sweep  ▰▰▰▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱  16/21 · 4h42m · ↓ 1.2m tokens · ⚠ Large workflow · /workflows to stop",
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
            "  ⏸ nightly-sweep  ▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/4 · 3m02s · ↓ 12.4k tokens",
            "  ◯ general-purpose  Searching the report's consumers in tests      4m 28s · ↓ 115.4k tokens",
        ] {
            let r = screen(&body, footer);
            assert_eq!(signal(&r), None, "{footer}");
        }
    }

    #[test]
    fn the_status_walk_passes_the_block_and_stops_at_the_transcript() {
        assert!(!is_spinner_row(
            "✻ Implementing the parser… (2m · ↓ 3.1k tokens)"
        ));
        assert!(is_activity_row(
            "✻ Implementing the parser… (2m · ↓ 3.1k tokens)"
        ));
        assert!(!is_activity_row("✻ Cooked for 23m 0s · done 11:24 PM"));
        for row in [
            "✻ Can't reach the API server — check your internet or DNS (ENOTFOUND) · Retrying in 1s · attempt 5/10",
            "✻ Can't reach the API server — check your in… · Retrying in 30s · attempt 7/10",
            "✻ API Error: 529 Overloaded · Retrying in 1m 30s · attempt 2/10",
            "✻ Can't reach the API server · Retrying in 5s · attempt 5",
        ] {
            assert!(is_activity_row(row), "{row}");
        }
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

        let hint = screen(
            &[
                "✶ Deliberating…",
                "                        Approve tool calls from your phone · /remote",
            ],
            "  ⏵⏵ auto mode on",
        );
        assert_eq!(signal(&hint).as_deref(), Some("status row: spinner"));
    }

    fn pane(body: &[&str], width: usize, footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(["─".repeat(width), "❯".to_string(), "─".repeat(width)]);
        r.push(footer.to_string());
        r
    }

    #[test]
    fn a_status_row_ink_wrapped_is_read_as_its_line() {
        for rest in ["  raphing…", "raphing…"] {
            let r = pane(&["⏺ Working.", "", "✶ Choreog", rest], 12, "  ⏵⏵ auto");
            assert_eq!(worker_phase(&r), Phase::Busy, "{rest}");
            assert_eq!(signal(&r).as_deref(), Some("status row: spinner"), "{rest}");
            assert_eq!(status_row(&r), Some("✶ Choreog"), "{rest}");
            assert_eq!(transcript_end(&r), 2, "{rest}");
        }
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
        assert_eq!(transcript_end(&cut), 4);
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

    #[test]
    fn only_the_network_and_no_response_retry_rows_read_as_an_api_retry_row() {
        let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
        let read =
            |row: &str| api_retry_row(&screen(&["⏺ Reading the parser.", "", row, ""], footer));
        for (row, cause, next, waited) in [
            (
                "✻ Waiting for API response · will retry in 5s · check your network",
                RetryCause::Stalled,
                Some(5),
                None,
            ),
            (
                "✻ No response from the API after 30s · retrying, waiting up to 1m · attempt 2/10",
                RetryCause::NoResponse,
                None,
                Some(30),
            ),
            (
                "✶ No response from the API after 1m 5s · retrying once, waiting up to 1m",
                RetryCause::NoResponse,
                None,
                Some(65),
            ),
            (
                "✻ Connection dropped (ECONNRESET) · Retrying in 1m 4s · attempt 8/10",
                RetryCause::Unreachable,
                Some(64),
                None,
            ),
            (
                "✳ No internet route — check your connection or VPN (ENETUNREACH) · Retrying in \
                 9s · attempt 4/10",
                RetryCause::Unreachable,
                Some(9),
                None,
            ),
        ] {
            let r = read(row).unwrap_or_else(|| panic!("read: {row}"));
            assert_eq!(
                (r.row, r.cause, r.next_try_s, r.waited_s),
                (2, cause, next, waited),
                "{row}"
            );
        }
        for row in [
            "✻ Auto mode check unavailable · next try in 5s · attempt 2/10 · esc to interrupt",
            "✻ Your request is queued · next try in 5s · attempt 2 · esc to interrupt",
            "✻ Usage limit reached · Retrying in 14s (9:50pm) · attempt 3/10",
            "✻ Usage limit reached · Retrying in 2d 3h · attempt 3/10",
            "✻ Connection refused — a firewall or proxy may be blocking it (ECONNREFUSED) · \
             Retrying in 9s · attempt 4/10",
            "✻ API Error (500 Internal server error) · Retrying in 9s · attempt 4/10",
            "✻ Can't reach the API server… (12s · ↓ 1.1k tokens)",
            "⏺ Can't reach the API server — check your internet or DNS (ENOTFOUND) · Retrying in \
             2s · attempt 5/10",
        ] {
            assert_eq!(read(row), None, "{row}");
        }
        let said = screen(
            &["⏺ It printed · Retrying in 5s · attempt 2/10", ""],
            footer,
        );
        assert_eq!(api_retry_row(&said), None, "prose");
        let wrapped = pane(
            &[
                "⏺ Reading the parser.",
                "",
                "✻ Can't reach the API server — check your internet or DNS (ENOTFOUND) ·",
                "  Retrying in 14s · attempt 6/10",
            ],
            72,
            "  ⏵⏵ auto mode on (shift+tab to",
        );
        assert_eq!(api_retry_row(&wrapped), None, "wrapped across rows");
        use crate::prompt::fixtures::{
            API_RETRYING_DRAFT_MEASURED, API_RETRYING_MEASURED, API_RETRYING_QUEUED_80_MEASURED,
            API_RETRYING_QUEUED_DRAFT_MEASURED, API_RETRYING_QUEUED_MEASURED, screen as measured,
        };
        for (text, next) in [
            (API_RETRYING_MEASURED, 2),
            (API_RETRYING_DRAFT_MEASURED, 14),
            (API_RETRYING_QUEUED_MEASURED, 13),
            (API_RETRYING_QUEUED_80_MEASURED, 30),
            (API_RETRYING_QUEUED_DRAFT_MEASURED, 28),
        ] {
            let rows = measured(text);
            let r = api_retry_row(&rows).expect("the measured retry row");
            assert_eq!(
                (r.cause, r.next_try_s),
                (RetryCause::Unreachable, Some(next))
            );
            assert!(rows[r.row].contains(&r.text), "{:?}", r.text);
        }
        let r = api_retry_row(&measured(API_RETRYING_MEASURED)).expect("read");
        assert_eq!(
            r.guard,
            "^\\s*[^\\s⏺●⎿❯⏵◯]\\s+Can't.reach.the.API.server.—.check.your.internet.or.DNS.\
             \\(ENOTFOUND\\).·.Retrying.in."
        );
        let r = read(
            "✻ No response from the API after 30s · retrying, waiting up to 1m · attempt 2/10",
        )
        .expect("read");
        assert_eq!(
            r.guard, "^\\s*[^\\s⏺●⎿❯⏵◯]\\s+No.response.from.the.API.after.",
            "the try's own count moves: the guard stops before it"
        );
        assert_eq!(wait_secs("1m…"), None, "a cut wait");
        assert_eq!(wait_secs("2d 3h"), Some(2 * 86_400 + 3 * 3600));
    }

    #[test]
    fn a_status_line_join_never_loses_the_rows_own_busy_reading() {
        let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
        let hint = "A proxy or gateway that buffers streaming responses can cause this · set \
                    CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS to change the first wait";
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
                let done = with_hint("✻ Worked for 3m 2s · done 4:24 PM", width);
                assert_eq!(worker_phase(&done), Phase::Idle, "{width}: {done:#?}");
            }
        }
        let tail = "✻ Waiting on the server · esc to interrupt";
        for width in 43..=46 {
            let r = with_hint(tail, width);
            assert_eq!(status_line(&r).as_deref(), Some(tail), "{width}: {r:#?}");
            assert_eq!(worker_phase(&r), Phase::Busy, "{width}");
        }
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
        let wrapped = at(23, &["✻", "Crunched for 30s · done", "12:52 PM"]);
        assert_eq!(
            status_line(&wrapped).as_deref(),
            Some("✻ Crunched for 30s · done 12:52 PM")
        );
        assert_eq!(worker_phase(&wrapped), Phase::Idle);
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
        let waiting = r
            .iter()
            .position(|row| row.starts_with("\u{273B} Waiting for"))
            .expect("the waiting row");
        assert_eq!(busy_zone_start(&r), waiting);
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
        let reply = r
            .iter()
            .position(|row| row.starts_with("\u{23FA} No change"))
            .expect("the reply");
        assert_eq!(busy_zone_start(&r), reply + 1);
        let r = collapsed(&[
            "⏺ Dynamic workflow \"Second review pass\" completed · 5s",
            "",
            "⏺ The review is in; nothing to fix.",
            "",
            "✻ Cooked for 23m 0s · done 11:24 PM",
        ]);
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(busy_signal(&r), None);
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
        let r = collapsed(&["⏺ Shall I land it now?"]);
        assert_eq!(worker_phase(&r), Phase::Question);
        let r = collapsed(&[
            "⏺ Background command \"Run the tests\" completed (exit code 0)",
            "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
        ]);
        assert!(matches!(worker_phase(&r), Phase::Limited { .. }), "{r:#?}");
        let r = saved_screen(IDLE_AFTER_LIMIT);
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(busy_signal(&r), None);
        prompted.truncate(1);
        let drawn = screen(&prompted, footer);
        assert_eq!(
            signal(&drawn).as_deref(),
            Some("status row: waiting for a dynamic workflow")
        );
        assert_eq!(busy_signal(&drawn).map(|b| b.soft), Some(false));
        assert!(!carried_wait_alone(&drawn), "a status row is drawn");
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
        let bare = screen(
            &["⏺ Monitor event: \"lander\"", "", "⏺ Restarted it.", ""],
            footer,
        );
        assert_eq!(worker_phase(&bare), Phase::Idle);
        assert!(status_unseen(&bare));
        assert!(!status_unseen(&drawn));
    }

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
        assert!(!is_rule("└──────────┴─────────┴───────────┘"));
        assert!(is_rule(
            "──────────────────────────────────────────────────── sandboxed ─"
        ));
        assert!(!is_rule("  ──────────────"));
    }

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

    const SURVEY_OPEN: &str = include_str!("fixtures/survey-open.txt");

    const SURVEY: [&str; 2] = [
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];

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

        let bg7 = waiter_capture(include_str!("fixtures/wait_bg7.out"));
        assert!(survey_open(&bg7));
        assert!(!survey_open(&rows(&[SURVEY[0], SURVEY[1], "$ "])));
    }

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

    const CONTEXT_LOW: &str = include_str!("fixtures/context-low.txt");

    fn indicator(text: &str) -> String {
        format!("{}{text}", " ".repeat(118 - text.chars().count()))
    }

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
        let real = above_frame(saved_screen(CONTEXT_LOW), &SURVEY);
        assert_eq!(context_left(&real), Some(1));
        assert!(survey_open(&real));
    }

    fn parked(width: usize, text: &str) -> String {
        format!("{}{text}", " ".repeat(width - 2 - text.chars().count()))
    }

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

    #[test]
    fn the_context_indicator_on_the_mode_row_is_read() {
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
            for hints in [
                idle,
                "  ⏸ manual mode on · esc to interrupt",
                "  -- INSERT -- ⏸ manual mode on",
                "  ? for shortcuts",
                "  esc to clear",
                "  esc to interrupt",
            ] {
                for (end, left) in [
                    ("0% until auto-compact", 0),
                    ("37% until auto-compact", 37),
                    ("Context left until auto-compact: 8%", 8),
                ] {
                    let r = on_mode_row(width, hints, end);
                    assert_eq!(context_left(&r), Some(left), "{width} {hints} {end}");
                }
            }
        }
        assert_eq!(context_left(&saved_screen(CONTEXT_LOW)), Some(1));
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
            (
                "glued to the mode row's words",
                on_mode_row(80, "", &format!("{idle}7% until auto-compact")),
            ),
            (
                "glued to a version",
                on_mode_row(80, idle, "v2.17% until auto-compact"),
            ),
            (
                "the long form glued to the mode row's words",
                on_mode_row(
                    80,
                    "",
                    &format!("{idle}Context left until auto-compact: 8%"),
                ),
            ),
        ] {
            assert_eq!(context_left(&r), None, "{what}");
        }
        let mut statusline = crate::prompt::fixtures::screen(include_str!(
            "fixtures/claude-2.1.304-statusline-login-words-after-reply.txt"
        ));
        let frame = composer_frame(&statusline).expect("the recorded composer");
        let width = rule_width(&statusline, frame);
        let mode = frame.bottom + 2;
        assert!(is_mode_row(&statusline[mode]));
        let at_edge = |row: &str, left: u8| {
            let indicator = format!("{left}% until auto-compact");
            let pad = width - row.chars().count() - indicator.chars().count();
            format!("{row}{}{indicator}", " ".repeat(pad))
        };
        let actual_mode = statusline[mode].clone();
        statusline[mode] = at_edge(&actual_mode, 7);
        assert_eq!(context_left(&statusline), Some(7));
        statusline[frame.bottom + 1] = at_edge(&statusline[frame.bottom + 1], 50);
        assert_eq!(context_left(&statusline), Some(7));
        for imitation in [
            "  ? for shortcuts",
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
            "  ⏸ manual mode on · ? for shortcuts",
            "  -- INSERT -- ⏸ manual mode on",
            "  esc to clear",
            "  esc to interrupt",
        ] {
            for false_left in [0, 50] {
                statusline[frame.bottom + 1] = at_edge(imitation, false_left);
                statusline[mode] = at_edge(&actual_mode, 7);
                assert_eq!(context_left(&statusline), Some(7), "{imitation}");
                statusline[mode] = actual_mode.clone();
                assert_eq!(context_left(&statusline), None, "{imitation}");
                let mut wrapped = statusline.clone();
                wrapped.insert(mode + 1, parked(width, "7% until auto-compact"));
                assert_eq!(context_left(&wrapped), Some(7), "{imitation}");
            }
        }
    }

    /// The one reading on recorded screens of each build: at the mode row's end at any width and
    /// in shell mode (2.1.281 to 2.1.310), above the composer on 2.1.267/2.1.268, and none while a
    /// menu, the shortcuts or a notice stands in its slot, or after `/compact`.
    #[test]
    fn claude_codes_context_indicator_is_read_where_each_build_draws_it() {
        use super::context_fixtures as f;
        let low_menu = f::menu_description(7);
        let high_menu = f::menu_description(50);
        for (what, text, left) in [
            ("2.1.310 at 120 columns", f::MODE_ROW_14, Some(14)),
            ("2.1.310 lower", f::MODE_ROW_7, Some(7)),
            (
                "2.1.310 at 60 columns, after one space",
                f::MODE_ROW_60COL,
                Some(7),
            ),
            (
                "2.1.310 at 44 columns, on a row of its own",
                f::MODE_ROW_44COL,
                Some(7),
            ),
            ("2.1.281", f::MODE_ROW_2_1_281, Some(0)),
            ("2.1.310 in shell mode", f::SHELL_MODE, Some(7)),
            (
                "2.1.267/2.1.268, above the composer",
                f::ABOVE_OLDER,
                Some(1),
            ),
            ("the / menu", f::SLASH_MENU, None),
            ("the @ menu", f::AT_MENU, None),
            (
                "a low percentage in an agent description",
                low_menu.as_str(),
                None,
            ),
            (
                "a rising percentage in an agent description",
                high_menu.as_str(),
                None,
            ),
            ("the shortcuts", f::SHORTCUTS, None),
            (
                "the effort notice after /compact",
                f::COMPACTED_EFFORT,
                None,
            ),
            ("after /compact", f::COMPACTED, None),
            ("after /compact at 44 columns", f::COMPACTED_44COL, None),
        ] {
            let r = crate::prompt::fixtures::screen(text);
            assert_eq!(context_left(&r), left, "{what}");
        }
    }

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

        let still = narrow(&[
            "⏺ Running the suite.",
            "",
            "                              Still working. Check in from your phone · /remote",
        ]);
        assert_eq!(signal(&still).as_deref(), Some("hint: Still working"));
        assert_eq!(worker_phase(&still), Phase::Busy);
    }

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

    const MANAGER_FOOTER: &str = "  ⏵⏵ bypass permissions on · 1 monitor";
    const MANAGER_FOOTER_QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    const ARTIFACT_BAR: &str = "  ⧉  name · name";
    const WORKFLOW_LINE: &str =
        "  ◯ fan-out  collect the standings  1/4 agents done · 3m 2s · ↓ 12.4k tokens";

    const UNDER: [&[&str]; 3] = [
        &[ARTIFACT_BAR],
        &["", ARTIFACT_BAR],
        &["", WORKFLOW_LINE, ARTIFACT_BAR, ""],
    ];

    fn manager(body: &[&str], footer: &str, under: &[&str]) -> Vec<String> {
        let mut r = screen(body, footer);
        r.extend(rows(under));
        r
    }

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
            let r = with(&[""], MANAGER_FOOTER_QUIET, &BOX_UNDER_GUTTER);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_eq!(worker_phase(&r), expect, "{under:?}");

            let later = ["⏺ Approved it; the push went through.", "", done_row, ""];
            let r = with(&later, MANAGER_FOOTER_QUIET, &BOX_IN_PROSE);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_eq!(worker_phase(&r), expect, "{under:?}");
            let r = with(&[done_row, ""], MANAGER_FOOTER, &BOX_IN_PROSE);
            assert_eq!(parse_prompt(&r), None, "{under:?}");
            assert_ne!(worker_phase(&r), Phase::Prompt, "{under:?}");

            let r = with(&[spinner, ""], MANAGER_FOOTER, &BOX_IN_PROSE);
            assert_eq!(parse_prompt(&r), None, "{under:?}");

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

    #[test]
    fn a_limit_above_a_later_user_row_is_history() {
        let r = saved_screen(IDLE_AFTER_LIMIT);
        assert_eq!(limit_notice(&r), None);
        assert_eq!(worker_phase(&r), Phase::Idle);

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
        let resumed = with(&[NOTICE, SPINNER], BUSY_FOOTER);
        assert_eq!(signal(&resumed).as_deref(), Some("status row: spinner"));
        assert_eq!(worker_phase(&resumed), Phase::Busy);
        let shells = with(&[NOTICE], BUSY_FOOTER);
        assert_eq!(signal(&shells).as_deref(), Some("footer: esc to interrupt"));
        assert_eq!(worker_phase(&shells), Phase::Busy);
        assert_eq!(
            worker_phase(&with(&[SHORTLY, SPINNER], IDLE_FOOTER)),
            Phase::Busy
        );
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
        let banner = with(&["✔ Update installed · Restart to update"], IDLE_FOOTER);
        assert_eq!(worker_phase(&banner), Phase::Idle);
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
        let after = screen(
            &["⏺ Bash(./run.sh)", "  ⎿  queued 8 runs", "", &edge],
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert!(matches!(worker_phase(&after), Phase::Limited { .. }));
    }

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
        let panel: Vec<String> = [
            rule.as_str(),
            "\u{276F} ",
            rule.as_str(),
            "  \u{23F5}\u{23F5} bypass permissions on",
            "",
            "  \u{23FA} main",
            "\u{276F} \u{25EF} general-purpose  Searching",
        ]
        .map(str::to_owned)
        .into();
        let mut screens = crate::text::fixture_screens();
        screens.push(walk);
        screens.push(panel);
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
            let first = line.split(' ').next().expect("a glyph");
            assert!(status_row(&r).is_some_and(|row| row.starts_with(first)));
            let reading = crate::reader::read(Some("claude"), &r, None);
            assert_eq!(reading.phase, Phase::Busy);
        }
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
        let mut queued = screen(SPINNER_WRAPPED_10_2_1_300);
        let at = queued
            .iter()
            .position(|x| x == "aphing…")
            .expect("the line's rest");
        queued.insert(at + 1, "❯ keep".to_string());
        assert_eq!(status_line(&queued).as_deref(), Some("✻ Choreogr aphing…"));
        assert!(turn_just_begun(&queued));
    }

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
        let quoted: Vec<String> = r
            .iter()
            .map(|x| x.replacen("⏺ Auto mode lets", "⏺ It said: Auto mode lets", 1))
            .collect();
        assert!(
            last_said_row(&quoted).is_some_and(|x| x.ends_with("Shift+Tab to change mode.")),
            "{quoted:#?}"
        );
    }

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
        let quoted = frame("⏺ It said: Auto mode lets Claude handle permission");
        assert_eq!(
            last_said_row(&quoted),
            Some("  environments. Shift+Tab to change mode.")
        );
    }

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

    #[test]
    fn a_composer_frame_caught_mid_repaint_is_torn() {
        use crate::prompt::fixtures::screen as fx;
        let body: Vec<String> = (30..=65).map(|i| format!("  - check {i} passed")).collect();
        let framed = |bottom: &str, under: &[&str]| {
            let mut r = body.clone();
            r.extend(["".to_string(), "─".repeat(144), "❯".to_string()]);
            r.push(bottom.to_string());
            r.extend(under.iter().map(|s| (*s).to_string()));
            r
        };
        let short = framed(&"─".repeat(109), &[]);
        let unfooted = framed(&"─".repeat(144), &[]);
        for (name, r) in [("short", &short), ("unfooted", &unfooted)] {
            assert!(has_composer_frame(r), "{name}");
            assert_eq!(worker_phase(r), Phase::Idle, "{name}: idle to the readers");
            assert!(composer_frame_torn(r), "{name}");
        }
        assert!(composer_frame_torn(&framed(
            &"─".repeat(109),
            &["  ⏵⏵ auto mode on (shift+tab to cycle)"]
        )));
        for text in [
            include_str!("fixtures/claude-2.1.307-footer-after-turn.txt"),
            include_str!("fixtures/claude-2.1.307-footer-auto-idle.txt"),
            include_str!("fixtures/claude-2.1.307-footer-busy.txt"),
            include_str!("fixtures/claude-2.1.307-footer-auto-draft.txt"),
            include_str!("fixtures/claude-2.1.307-footer-accept-edits.txt"),
            include_str!("fixtures/claude-2.1.307-footer-bypass-idle.txt"),
            include_str!("fixtures/claude-2.1.307-footer-manual.txt"),
            include_str!("fixtures/claude-2.1.307-footer-plan.txt"),
        ] {
            let r = fx(text);
            assert!(has_composer_frame(&r), "{text}");
            assert!(!composer_frame_torn(&r), "{text}");
        }
        assert!(!composer_frame_torn(&framed(&"─".repeat(144), &[""])));
        assert!(!composer_frame_torn(&framed(
            &"─".repeat(144),
            &["  ⏵⏵ auto mode on (shift+tab to cycle)"]
        )));
        assert!(!composer_frame_torn(&body));
    }
}
