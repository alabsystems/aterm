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

use std::fmt;

use super::prompt::parse_prompt;

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
fn is_glyph_row(row: &str) -> bool {
    let mut cs = row.chars();
    cs.next().is_some_and(|g| SPINNERS.contains(&g)) && cs.next().is_some_and(char::is_whitespace)
}

/// A row only the transcript has: the worker's message or tool call in column
/// 0 (`⏺`; `●` where the platform draws that — never the session survey), or
/// output under the `⎿` gutter that is not a tip or a todo item.
fn is_transcript_row(row: &str) -> bool {
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
fn is_survey(row: &str) -> bool {
    let t = row.trim_start();
    (t.starts_with('●') && t.contains("How is Claude doing this session"))
        || (t.starts_with("1: Bad") && t.contains("0: Dismiss"))
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
/// option row (`❯ 1. Yes`).
pub fn composer_index(rows: &[String]) -> Option<usize> {
    rows.iter().rposition(|r| {
        let t = r.trim_start();
        t.starts_with('❯') && !is_option_like(t)
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

fn leading_spaces(row: &str) -> usize {
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
    use super::super::prompt::fixtures::{bash_one_row, composer, rows};
    use super::*;

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
