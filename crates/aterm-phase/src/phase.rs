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
//! A signal counts only in the LIVE ZONE. Claude Code draws its composer
//! between two full-width rules (the top one may carry a right-aligned label,
//! `──── sandboxed ─`); the rows under the bottom rule are the FOOTER. The
//! STATUS ROW is the lowest row above the top rule that starts, in column 0,
//! with a spinner glyph — a spinner, `Waiting for …`, or the done row a
//! finished turn leaves — found before any row only the transcript has: the
//! worker's `⏺` message or tool call, or output under the `⎿` gutter. What
//! sits between the status row and the top rule never hides it, whatever it
//! is: a `⎿  Tip:` row, a todo list, a right-aligned hint of any length, the
//! session survey (`● How is Claude doing this session?`), a banner, a queued
//! message. A status row ABOVE a transcript row is history: measured, a worker
//! whose turn ended at 11:30 PM, with `/model` run after it and an empty
//! composer, still showed `✻ Waiting for 1 dynamic workflow to finish` fifteen
//! rows up, and a whole-screen scan read it busy. Without the composer frame
//! (not Claude Code, or a full-screen box) every row is scanned, as before.
//!
//! A background MONITOR alone is soft ([`Busy::soft`]): a `persistent` one
//! runs for as long as the worker lives and wakes it by itself, so a worker
//! whose turn ended `· 1 monitor still running` (measured) is waiting on the
//! monitor's next event — and could be asking a question, or be at its usage
//! limit, all the same. A question or a limit notice outranks it.
//!
//! The live zone also carries how much of the worker's context is left before
//! Claude Code auto-compacts it ([`context_left`]): `1% until auto-compact`,
//! right-aligned on the row above the top rule (measured 2026-09-13, three
//! hours into a turn, before the compaction that took the row away).

use std::fmt;

use crate::prompt::parse_prompt;

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
/// leaves on a row mid-thought.
const SPINNERS: &[char] = &['✢', '✽', '✻', '✶', '✳', '✧', '✦', '⚡', '·', '*'];

/// Classify one screen. A prompt wins over busy: a worker blocked on an
/// approval box cannot proceed however many background shells its footer
/// counts, and a supervisor that waited for those would wait forever. Busy
/// wins over a limit notice (a worker still running is not at the wall yet),
/// and a limit notice over a question — but a busy that is only a background
/// monitor ([`Busy::soft`]) loses to both: it can outlive any budget.
pub fn worker_phase(rows: &[String]) -> Phase {
    if parse_prompt(rows).is_some() {
        return Phase::Prompt;
    }
    let busy = busy_signal(rows);
    if busy.is_some_and(|b| !b.soft) {
        return Phase::Busy;
    }
    if let Some((message, reset)) = limit_notice(rows) {
        return Phase::Limited { message, reset };
    }
    if last_said_row(rows).is_some_and(|r| r.trim_end().ends_with('?')) {
        return Phase::Question;
    }
    if busy.is_some() {
        Phase::Busy
    } else {
        Phase::Idle
    }
}

/// Any busy signal in the live zone ([`busy_signal`]), a soft one included.
pub fn is_busy(rows: &[String]) -> bool {
    busy_signal(rows).is_some()
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
    /// Only a background monitor is running: a question or a limit notice
    /// outranks it in [`worker_phase`].
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
/// `◯ <name> … agents done` progress line). Then the soft one: a monitor still
/// running, on the status row (`· 1 monitor still running`) or in the footer
/// (`· N monitor(s) ·`). With no frame, the first row [`busy_reason`] names,
/// anywhere.
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
    let t = row.trim_end();
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
            && (lead.ends_with("· ") || lead.trim().is_empty())
            && (after.is_empty()
                || after.starts_with(" ·")
                || after.starts_with("  ")
                || after.starts_with(" still running"))
    })
}

/// A workflow's progress line under the footer (`◯ <name>  <description…>
/// 1/4 agents done · 20m 59s · ↓ 428.2k tokens`).
fn is_workflow_progress(row: &str) -> bool {
    row.trim_start().starts_with('◯') && (row.contains("agents done") || row.contains("agent done"))
}

/// `^\s*[✢✽✻✶✳✧✦⚡·*]\s+\S+…` — a spinner glyph, whitespace, then a word that
/// runs into an ellipsis (`…` or `...`).
pub fn is_spinner_row(row: &str) -> bool {
    let t = row.trim_start();
    let mut cs = t.chars();
    let Some(g) = cs.next() else {
        return false;
    };
    if !SPINNERS.contains(&g) {
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
    let t = row.trim_start();
    let mut cs = t.chars();
    if !cs.next().is_some_and(|g| SPINNERS.contains(&g)) {
        return false;
    }
    let rest = cs.as_str();
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let text = rest.trim_start();
    let head = text.split(" (").next().unwrap_or(text);
    head.chars().next().is_some_and(|c| c != '…' && c != '.')
        && (head.contains('…') || head.contains("..."))
}

/// `<prefix><digits><suffix>` somewhere in `row`.
fn number_then(row: &str, prefix: &str, suffix: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = row[from..].find(suffix) {
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
struct Frame {
    top: usize,
    bottom: usize,
}

fn composer_frame(rows: &[String]) -> Option<Frame> {
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

/// The index of the composer's top rule, when the frame is on the screen —
/// for the prompt parser, which must know where the live zone begins.
pub(crate) fn composer_top(rows: &[String]) -> Option<usize> {
    composer_frame(rows).map(|f| f.top)
}

/// A full-width composer rule: `─` from column 0 to the last column, with at
/// most a label inside it (`──── sandboxed ─`) and none of a table's joints.
fn is_rule(row: &str) -> bool {
    let t = row.trim_end();
    t.starts_with('─')
        && t.ends_with('─')
        && t.chars().filter(|c| *c == '─').count() >= 10
        && !t.contains(['┬', '┴', '┼', '├', '┤', '┌', '┐', '└', '┘', '│'])
}

/// The live zone above the top rule, found by walking up from it: the status
/// row, if one comes before the transcript, and where the rows under it (or
/// under the transcript row the walk stopped at) begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusBlock {
    status: Option<usize>,
    from: usize,
}

fn status_block(rows: &[String], top: usize) -> StatusBlock {
    for i in (0..top).rev() {
        if is_glyph_row(&rows[i]) {
            return StatusBlock {
                status: Some(i),
                from: i + 1,
            };
        }
        if is_transcript_row(&rows[i]) {
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

/// A row that can be the status row: a spinner glyph in column 0, then
/// whitespace (`✶ Deliberating…`, `· Mustering… (38s)`, `✻ Cooked for 4s ·
/// done 2:41 PM`).
pub fn is_glyph_row(row: &str) -> bool {
    let mut cs = row.chars();
    cs.next().is_some_and(|g| SPINNERS.contains(&g)) && cs.next().is_some_and(char::is_whitespace)
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
    let t = row.trim_start();
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
    let t = row.trim_start();
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
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let width = rows[frame.bottom].trim_end().chars().count();
    for i in (1..frame.top).rev() {
        let row = &rows[i];
        if is_survey_options(row) {
            return is_survey_question(&rows[i - 1]);
        }
        if is_survey(row) || !is_parked_above_composer(row, width) {
            return false;
        }
    }
    false
}

/// The survey's question row as Claude Code draws it: `●` in column 0.
fn is_survey_question(row: &str) -> bool {
    row.starts_with('●') && row.contains("How is Claude doing this session")
}

/// The survey's options row: `1: Bad    2: Fine   3: Good   0: Dismiss`.
fn is_survey_options(row: &str) -> bool {
    let t = row.trim_start();
    t.starts_with("1: Bad") && t.contains("0: Dismiss")
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
    let width = rows[frame.bottom].trim_end().chars().count();
    let from = status_block(rows, frame.top).from;
    rows[from..frame.top]
        .iter()
        .rev()
        .filter(|row| is_against_right_edge(row, width))
        .find_map(|row| context_reading(row.trim()))
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

/// The composer row: the last row whose first glyph is `❯` and that is not an
/// option row (`❯ 1. Yes`) — unless it is the caret in column 0 right under a
/// composer rule: a draft that starts with a number (`❯ 1. Keep the harness`,
/// an answer to a numbered question typed but not sent) is still the composer.
pub fn composer_index(rows: &[String]) -> Option<usize> {
    (0..rows.len()).rev().find(|&i| {
        let t = rows[i].trim_start();
        t.starts_with('❯')
            && (!is_option_like(t) || (rows[i].starts_with('❯') && i > 0 && is_rule(&rows[i - 1])))
    })
}

fn is_option_like(t: &str) -> bool {
    let after = t.trim_start_matches('❯').trim_start();
    let digits = after.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && after[digits..].starts_with('.')
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
    lines.extend(rows[caret + 1..end].iter().map(|r| r.trim().to_string()));
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
    row.trim_start()
        .chars()
        .next()
        .is_some_and(|g| SPINNERS.contains(&g) || STATUS_GLYPHS.contains(&g))
}

/// The first non-space column of a right-aligned hint: transcript rows start
/// at 0, 2, 4 or 5, and Claude Code parks its hints against the right edge.
const HINT_COLUMN: usize = 20;

pub(crate) fn leading_spaces(row: &str) -> usize {
    row.chars().take_while(|c| c.is_whitespace()).count()
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
    let end = row.trim_end().chars().count();
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
            Some(rows[f.bottom].trim_end().chars().count()),
        ),
        None => (composer_index(rows).unwrap_or(rows.len()), None),
    };
    (0..end).rev().find(|&i| is_said(&rows[i], width))
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
            .rposition(|r| !r.trim().is_empty())
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
    let width = rows[frame.bottom].trim_end().chars().count();
    let mut end = frame.top;
    while end > 0 && is_parked_above_composer(&rows[end - 1], width) {
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
    let t = row.trim_start();
    t.is_empty()
        || is_survey(row)
        || is_against_right_edge(row, width)
        || t.strip_prefix('⎿')
            .is_some_and(|g| g.trim_start().starts_with("Tip:"))
}

/// A row against the RIGHT edge, where Claude Code parks its hints, banners
/// and context indicator: a hint ([`is_hint`]) that ends within three columns
/// of the composer rule's `width` (measured: two short). A row that ends
/// short of the edge is not one, however far in it starts — a transcript row
/// indented 20 columns (indented code), a right-aligned row quoted in a
/// tool's output.
fn is_against_right_edge(row: &str, width: usize) -> bool {
    is_hint(row, Some(width)) && row.trim_end().chars().count() + 3 >= width
}

fn is_said(row: &str, width: Option<usize>) -> bool {
    let t = row.trim_start();
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
/// rule carries one. Anything said after a notice makes it history: a later
/// turn's words (a background command or a monitor event starts one with no
/// user row), the `/model` output that switched the model (measured), the
/// worker's reply to a retry.
///
/// A notice opens with `You've reached your … limit` / `You've hit your …
/// limit` (Claude Code's second person — `You've reached your Fable limit. Run
/// /usage-credits to continue or switch models with /model.`, `You've hit your
/// session limit · resets 7:30pm (America/Los_Angeles)`), with a few words and
/// `limit reached` (`5-hour limit reached ∙ resets 3am`, `Claude usage limit
/// reached. Your limit will reset at 3pm (America/New_York).`), or with `API
/// Error` and a rate or usage limit (`API Error: Rate limit reached for
/// requests`). Text that says `Approaching` a limit is a warning, and never
/// counts. The block's continuation rows are part of the message; `reset` is
/// read from whichever of its rows says `resets …` / `reset at …`.
pub fn limit_notice(rows: &[String]) -> Option<(String, Option<String>)> {
    if let Some(found) = composer_frame(rows).and_then(|f| footer_limit(&rows[f.bottom + 1..])) {
        return Some(found);
    }
    let last = last_said_index(rows)?;
    let open = gutter_open(rows, last)?;
    let head = rows[open].trim_start().trim_start_matches('⎿').trim();
    if !is_limit_notice(head) {
        return None;
    }
    let parts: Vec<&str> = std::iter::once(head)
        .chain(rows[open + 1..=last].iter().map(|r| r.trim()))
        .filter(|p| !p.is_empty())
        .collect();
    let reset = parts.iter().find_map(|p| reset_of(p));
    Some((parts.join(" "), reset))
}

/// A notice in the footer, under the bottom rule and above the first blank row
/// (the agents panel and a workflow's line below it are the worker's words):
/// an item — the row, a part after a wide gap or after ` · ` — that opens with
/// a limit notice, up to the next wide gap.
fn footer_limit(footer: &[String]) -> Option<(String, Option<String>)> {
    footer
        .iter()
        .take_while(|r| !r.trim().is_empty())
        .filter(|r| !r.trim_start().starts_with(['◯', '⏺', '●']))
        .find_map(|row| {
            let t = row.trim();
            let starts = std::iter::once(0)
                .chain(t.match_indices("  ").map(|(i, s)| i + s.len()))
                .chain(t.match_indices(" · ").map(|(i, s)| i + s.len()));
            starts
                .map(|i| t[i..].trim_start())
                .find(|item| is_limit_notice(item))
                .map(|item| {
                    let item = item.split("  ").next().unwrap_or(item).trim();
                    (item.to_string(), reset_of(item))
                })
        })
}

/// The `⎿` row that opens the gutter block row `i` belongs to: `i` itself, or
/// the row its continuation rows (indented to the gutter's text, five columns
/// or more) hang from. `None` when `i` is not under the gutter.
fn gutter_open(rows: &[String], mut i: usize) -> Option<usize> {
    loop {
        let t = rows[i].trim_start();
        if t.starts_with('⎿') {
            return Some(i);
        }
        if t.is_empty() || leading_spaces(&rows[i]) < 5 {
            return None;
        }
        i = i.checked_sub(1)?;
    }
}

/// Whether `text` OPENS with a limit notice (see [`limit_notice`]).
fn is_limit_notice(text: &str) -> bool {
    let lower = text.replace('’', "'").to_lowercase();
    if lower.contains("approaching") {
        return false;
    }
    let head = lower
        .split(['.', '·', '∙', '|'])
        .next()
        .unwrap_or("")
        .trim();
    let reached = head
        .find("limit reached")
        .is_some_and(|at| head[..at].split_whitespace().count() <= 3);
    (lower.starts_with("you") && second_person_limit(head))
        || reached
        || (lower.starts_with("api error")
            && ["rate limit", "rate_limit", "usage limit", "429"]
                .iter()
                .any(|k| lower.contains(k)))
}

/// `reached your <up to three words> limit` / `hit your … limit`, in a
/// lowercased row.
fn second_person_limit(lower: &str) -> bool {
    ["reached your ", "hit your "].iter().any(|lead| {
        lower.match_indices(lead).any(|(at, _)| {
            lower[at + lead.len()..]
                .split_whitespace()
                .take(4)
                .any(|w| w.trim_matches(|c: char| !c.is_alphanumeric()) == "limit")
        })
    })
}

/// When the notice says the limit resets: the text after `resets at ` /
/// `reset at ` / `resets `, up to a ` · ` or ` ∙ ` separator, without a closing
/// `.`.
fn reset_of(message: &str) -> Option<String> {
    let at = ["resets at ", "reset at ", "resets "]
        .iter()
        .find_map(|k| find_ascii_ci(message, k).map(|i| i + k.len()))?;
    let rest = &message[at..];
    let cut = [" · ", " ∙ "]
        .iter()
        .filter_map(|sep| rest.find(sep))
        .min()
        .unwrap_or(rest.len());
    let rest = rest[..cut].trim().trim_end_matches('.').trim();
    (!rest.is_empty()).then(|| rest.to_string())
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
mod tests {
    use super::*;
    use crate::prompt::fixtures::{bash_one_row, composer, rows};

    fn screen(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
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

    // ---- a manager's screen (round 16 addendum) ----------------------------

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
    /// anywhere on it used to be a box, and a manager's transcript carries
    /// its workers' boxes. Under the
    /// `⎿` gutter it is output, whatever is under it — the manager idle (the
    /// soft monitor), the manager busy. Quoted in prose, it is history once
    /// the manager's later words or a done row stand between it and the
    /// composer. With only a spinner under it the parser still errs towards
    /// `prompt` (the module header of `prompt.rs` says why), and a REAL box
    /// over the manager's composer is a prompt with this footer as with any.
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

            // Only a spinner under the prose copy: still read as a box.
            let r = with(&[spinner, ""], MANAGER_FOOTER, &BOX_IN_PROSE);
            assert!(parse_prompt(&r).is_some(), "{under:?}");

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
    }
}
