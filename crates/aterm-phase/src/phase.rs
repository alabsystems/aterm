// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker phase classification from a single terminal screen snapshot.

use std::borrow::Cow;
use std::fmt;

use crate::prompt::parse_prompt;
use crate::wall::{Placement, Wall, WallKind, classify_wall};

/// The Claude Code (`claude`) harness version, `MAJOR.MINOR` with no third part.
///
/// Its minor goes up once per public release window in which its sources changed
/// ([`crate::CliHarness::sources`]); `targo --unverified ship harness-bump` writes the bump.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 13, "0.13");

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

/// Claude Code's window title says its REPL is busy: one of the two frames it flips while busy
/// (`title.running`), a blank, then the title. At rest the title leads with `title.rest` (`✳`).
/// This is the one reading of a running turn that a status line leaves: with one set, the footer
/// drops `esc to interrupt`, and the spinner row goes once the reply's first line is drawn.
/// Claude Code writes no title where its title is off (`CLAUDE_CODE_DISABLE_TERMINAL_TITLE`),
/// leads it with no glyph where its status goes to the terminal's tab instead
/// (`showStatusInTerminalTab`, or a terminal that answers its OSC 7501 program-status probe), and
/// holds it at `title.rest` under a terminal multiplexer: there no title says it runs.
#[must_use]
pub fn title_runs(title: &str) -> bool {
    let mut chars = title.chars();
    chars
        .next()
        .is_some_and(|c| const { crate::anchor!("title.running") }.contains(c))
        && chars.next() == Some(' ')
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
    if asked_in_prose(rows) {
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
/// With no frame it is read from the whole screen, but for a `/login` dialog drawn in the
/// composer's place, from the dialog and the row said above it
/// ([`crate::login::dialog_live_from`]).
pub fn busy_signal(rows: &[String]) -> Option<Busy> {
    let Some(frame) = composer_frame(rows) else {
        let from = crate::login::dialog_live_from(rows).unwrap_or(0);
        return rows[from..]
            .iter()
            .find_map(|r| busy_reason(r))
            .map(|rule| Busy::hard(Zone::Screen, rule));
    };
    let block = status_block(rows, frame.top);
    let line = block.line(rows);
    let status = line.as_deref();
    let footer = &rows[frame.bottom + 1..];
    status
        .and_then(status_busy)
        .map(|rule| Busy::hard(Zone::Status, rule))
        .or_else(|| {
            still_working(rows, block.from, frame.top)
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

/// The waiting row [`carried_wait`] reads, its first row as drawn: the first glyph row above the
/// composer's top rule `top` waits for work of Claude Code's own process, and not all of that
/// work ended in the rows drawn under it ([`waited_out`]).
fn carried_row(rows: &[String], top: usize) -> Option<usize> {
    const WAITING: [&str; 3] = [
        "waiting for a dynamic workflow",
        "waiting for a background agent",
        "waiting for background work",
    ];
    let width = crate::question::screen_width(rows);
    let i = glyph_above(rows, top)?;
    let end = status_line_end(rows, i, top, width).unwrap_or(i);
    let text = status_words(rows, i, end);
    (status_busy(&text).is_some_and(|rule| WAITING.contains(&rule))
        && !waited_out(&text, rows, end + 1, top))
    .then_some(i)
}

/// WHETHER EVERY DYNAMIC WORKFLOW A CARRIED WAIT NAMES ENDED UNDER IT: the waiting row's words
/// `text` name dynamic workflows and nothing else (`Waiting for N dynamic workflow(s) to
/// finish`), and the rows drawn between it and the composer, `rows[from..until]`, hold a
/// `⏺ Dynamic workflow "…" completed` row for each of them and for each workflow launched there
/// (`⏺ Workflow(…)`), its words read up to the side panel's rule ([`wrapped_item`]).
/// That row is the wake a workflow's end opens a turn with, measured on 2.1.304
/// (`claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`, and the walled wake of
/// `idle-after-limit-and-model-switch.txt`), each followed there by a done row; where no done row
/// is drawn after it (`showTurnDuration` off), the waiting row stays the first glyph row above the
/// composer and would name the ended work for as long as it stood. A wait for a background agent
/// or other work never ends here. An agent's completion row is measured (`⏺ Agent "…" finished ·
/// 3m 20s`, 2.1.304's agents-panel captures), but the wait's count does not follow it: an agent
/// that says it waits on its own background work (`⏺ Agent "…" is waiting on its own background
/// work`) leaves the count and finishes later, so its finished row stands under `Waiting for 1
/// background agent to finish` while the one agent counted still runs
/// (`claude-2.1.304-agents-panel-long-bash-4m57s-120col.txt`). No completion row of other work is
/// measured.
fn waited_out(text: &str, rows: &[String], from: usize, until: usize) -> bool {
    let Some(waited) = workflows_waited(text) else {
        return false;
    };
    let mut ended = 0usize;
    let mut launched = 0usize;
    for (i, row) in rows.iter().enumerate().take(until).skip(from) {
        let Some(said) = row.strip_prefix('⏺').or_else(|| row.strip_prefix('●')) else {
            continue;
        };
        let said = crate::text::trim_start(said);
        if said.starts_with("Workflow(") {
            launched += 1;
        } else if said.starts_with("Dynamic workflow \"")
            && wrapped_item(rows, i, until).contains("\" completed")
        {
            ended += 1;
        }
    }
    ended >= waited.saturating_add(launched)
}

/// `N` of a waiting row's words `text` that wait for `N dynamic workflow(s)` alone.
fn workflows_waited(text: &str) -> Option<usize> {
    let (_, rest) = text.split_once("Waiting for ")?;
    let (count, rest) = rest.split_once(' ')?;
    let n = count.parse().ok()?;
    (rest.starts_with("dynamic workflow to finish")
        || rest.starts_with("dynamic workflows to finish"))
    .then_some(n)
}

/// A transcript item's words, its first row `rows[first]` joined with the rows its text wrapped
/// onto: each row under it, short of row `until`, up to a blank row or one that opens a block of
/// its own ([`opens_block`]: a `⏺` or `⎿` row, a glyph row, a rule), each cut at the side panel's
/// rule beside the item's first row ([`panel_cut`]), so the panel's text is no word of the item
/// and a row blank up to the rule ends it. Claude Code wraps a long `⏺ Dynamic workflow "…"
/// completed` row at column 0, not at its text's column (measured on 2.1.302 at 175 columns,
/// `claude-2.1.302-login-handoff-over-weekly-limit.txt`: `pushed)" completed · 2h 1m 12s`).
fn wrapped_item(rows: &[String], first: usize, until: usize) -> String {
    let rule = panel_rule(rows, first, first);
    let mut item = panel_cut(&rows[first], rule).to_string();
    for row in rows[first + 1..until]
        .iter()
        .map(|r| panel_cut(r, rule))
        .take_while(|r| !crate::text::trim(r).is_empty() && !opens_block(r))
    {
        item.push(' ');
        item.push_str(crate::text::trim(row));
    }
    item
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
    done_items(row).any(|item| {
        crate::text::trim_end(item)
            .strip_suffix(" still running")
            .is_some_and(|label| !crate::text::trim(label).is_empty())
    })
}

/// A done row's ` · ` items after the first, `row` a status line ([`StatusBlock::line`]), which
/// ends at the side panel's rule ([`status_words`]).
fn done_items(row: &str) -> impl Iterator<Item = &str> {
    row.split(" · ").skip(1)
}

/// A footer row whose pill counts background work under any label: some item ([`row_items`])
/// counts work ([`work_count`]).
fn counts_work(row: &str) -> bool {
    row_items(row).any(work_count)
}

/// An item ([`row_items`]) that counts background work under any label: a count ([`counted`])
/// whose label's last word is a kind of work, whatever words qualify it (`1 MCP task`, `1 Artifact
/// comment monitor`, `◇ 1 cloud session`, `2 background tasks`, `1 shell`).
fn work_count(item: &str) -> bool {
    const WORK: [&str; 7] = [
        "shell", "monitor", "task", "agent", "workflow", "session", "team",
    ];
    counted(item).is_some_and(|(_, label)| {
        label
            .rsplit(' ')
            .next()
            .is_some_and(|noun| WORK.contains(&singular(noun)))
    })
}

/// An item ([`row_items`], or a part of a done row's label) that counts the work the process
/// table names: `N shell(s)` or `N monitor(s)` with no word qualifying it ([`item_count`]).
fn process_count(item: &str) -> bool {
    item_count(item, "shell")
        .or_else(|| item_count(item, "monitor"))
        .is_some()
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
    let wall = notice(rows)?;
    if matches!(wall.placement, Placement::Footer | Placement::Banner) && !turn_over(rows) {
        return None;
    }
    Some(crate::wall::memory_wall(rows).unwrap_or(wall))
}

/// Claude Code's done row (`✻ Brewed for 6s · done 3:32 PM`) stands between its reply and the
/// composer: the turn's end, drawn on the screen. Claude Code draws it once the turn is over, in
/// the frame that draws the reply's last line or a later one (2.1.311 lab `mr`: the last line,
/// held back while the reply streamed, and the done row came in one frame, 23 ms after the
/// title rested). A turn can end with none, as where `showTurnDuration` is off or where it was
/// interrupted.
#[must_use]
pub fn done_row_drawn(rows: &[String]) -> bool {
    turn_over(rows)
}

/// The status row above the composer, or the one Claude Code's idle compaction left it under, is
/// a done row.
fn turn_over(rows: &[String]) -> bool {
    composer_frame(rows)
        .is_some_and(|f| turn_end_line(rows, f.top).is_some_and(|line| is_done_row(&line)))
}

/// The status line that speaks for the turn: the one above the composer's top rule `top`, else,
/// with none there, the one above the rows Claude Code's idle compaction drew
/// ([`crate::turn::idle_compaction_from`]). That compaction is no turn and runs with the turn
/// over, but its notice is a row of the transcript, which ends the live zone ([`status_block`]).
/// A message under the row read there opened a turn that row did not end. Drawn long ago, that
/// row says when the turn ended and nothing of now: what runs now is read off the live footer
/// ([`busy_signal`]).
fn turn_end_line(rows: &[String], top: usize) -> Option<Cow<'_, str>> {
    let block = status_block(rows, top);
    if block.status.is_some() {
        return block.line(rows);
    }
    let from = crate::turn::idle_compaction_from(rows)?;
    let block = status_block(rows, from);
    if rows[block.from..from].iter().any(|r| r.starts_with('❯')) {
        return None;
    }
    block.line(rows)
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

/// THE TURN OVER UNDER THE AGENT'S OWN BACKGROUND WORK: the phase a screen [`worker_phase`] reads
/// busy rests at when that busy is the agent's own background work alone ([`background_only`]: a
/// dynamic workflow, a background agent, a shell or a monitor it left running, under a composer
/// that takes the next message, no live turn on the screen): a question it asked (its last words
/// end in `?`), or idle. `None` for every other screen, a live turn's among them.
///
/// Measured on Claude Code 2.1.310 (the cruise stall of 2026-10-24..27): its transcript writes a
/// `turn_duration` row with `pendingWorkflowCount` where the reply stops over such work, and each
/// later wake (`<task-notification>`) runs a turn of its own with its own `turn_duration` row.
/// That stop is a turn's end, though no done row is drawn until nothing is pending (`turn_over`,
/// whose done row times the whole chain from the person's message).
///
/// Two screens give no rest, as measured on Claude Code 2.1.283:
/// - a notice under the composer or in a banner over a status row that is no done row: the vendor
///   draws it while the turn wraps up, and the turn ends later (`background_wall` declines it
///   for the same reason; `claude-2.1.283-wall-session-wrapup-waiting-measured.txt`);
/// - `◎ /goal active` ([`crate::goal_active`]), opening its row or as the rightmost item of
///   another (run into 2.1.283's update hint, at the right end of 2.1.311's footer): Claude Code's
///   goal runs on through the wait by its own check-ins, its done row timing it whole
///   (`claude-2.1.283-wall-session-goal-checkin-shell-measured.txt`).
///
/// The phase stays [`Phase::Busy`] there, because the work runs on and wakes the agent when it
/// ends, and the supervisor's background breaks read it so; this is what the turn's end was, for
/// the published `agent=` word and the turns of the session log, which close at it.
#[must_use]
pub fn background_rest(rows: &[String]) -> Option<Phase> {
    if worker_phase(rows) != Phase::Busy
        || !background_only(rows)
        || crate::turn::goal_active(rows)
        || notice(rows).is_some()
    {
        return None;
    }
    Some(if asked_in_prose(rows) {
        Phase::Question
    } else {
        Phase::Idle
    })
}

/// Whether the worker asked in prose: the last row it said ([`last_said_row`]) ends in `?`, and
/// is not Claude Code's notice that a person stopped the turn with Esc
/// ([`crate::turn::interrupt_notice`]). That notice ends in the vendor's question to the person
/// (`What should Claude do instead?`), and the turn it ends is the person's (fix J2-4, measured
/// on 2.1.316 after a declined approval box: `claude-2.1.316-box-bash-declined-esc.txt`).
fn asked_in_prose(rows: &[String]) -> bool {
    last_said_row(rows).is_some_and(|r| {
        crate::text::trim_end(r).ends_with('?') && !crate::turn::interrupt_notice(r)
    })
}

/// How many background shells Claude Code's screen counts, on its status line
/// ([`StatusBlock::line`]) or a footer row.
#[must_use]
pub fn shells_running(rows: &[String]) -> Option<u32> {
    let frame = composer_frame(rows)?;
    let line = status_block(rows, frame.top).line(rows);
    let footer = &rows[frame.bottom + 1..];
    let counted = line
        .as_deref()
        .into_iter()
        .chain(footer.iter().map(String::as_str))
        .flat_map(row_items)
        .filter_map(|item| item_count(item, "shell"))
        .max();
    Some(counted.unwrap_or(0))
}

/// THE ITEMS A FOOTER OR STATUS ROW DRAWS: its ` · ` items, the parts of each between runs of
/// three spaces and the counts a pill joins with `, ` (`· 1 shell, 1 monitor ·` on 2.1.283,
/// `claude-2.1.283-wall-session-wrapup-waiting-measured.txt`; `· 13 shells, 3 monitors` on
/// 2.1.316, `claude-2.1.316-waiting-agents-and-workflows-turn-over-panel.txt`), each trimmed and
/// without the `·` of a row that opens or ends on one or its leading `◇ `. The one item reader
/// of [`footer_count`], [`shells_running`], [`counts_work`] and [`footer_work`].
fn row_items(row: &str) -> impl Iterator<Item = &str> {
    row.split(" · ")
        .flat_map(|item| item.split("   "))
        .flat_map(|part| part.split(", "))
        .map(|item| {
            let item = crate::text::trim(item);
            let item = item.strip_prefix("· ").unwrap_or(item);
            let item = item.strip_suffix(" ·").unwrap_or(item);
            item.strip_prefix("◇ ").unwrap_or(item)
        })
}

/// THE COUNT ONE ITEM NAMES ([`row_items`], or a part of a done row's label): `N <label>`, alone
/// or followed by ` still running`, `N` ASCII digits that fit a `u32`, with `label` the words
/// after it. The one reading of a count that [`item_count`] (and through it [`process_count`])
/// and [`work_count`] share.
fn counted(item: &str) -> Option<(u32, &str)> {
    let item = item.strip_suffix(" still running").unwrap_or(item);
    let (count, label) = item.split_once(' ')?;
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((count.parse().ok()?, label))
}

/// `noun` without a plural `s`.
fn singular(noun: &str) -> &str {
    noun.strip_suffix('s').unwrap_or(noun)
}

/// The count one item ([`row_items`], or a part of a done row's label) names of `noun`: `N
/// <noun>` or `N <noun>s` ([`counted`]), the noun alone, with no word qualifying it.
fn item_count(item: &str, noun: &str) -> Option<u32> {
    counted(item)
        .filter(|&(_, label)| singular(label) == noun)
        .map(|(n, _)| n)
}

/// CLAUDE CODE'S `Still working` HINT, A LIVE TURN'S: some row between the status block and the
/// composer's top rule, rows `from..top`, where Claude Code draws its right-aligned hints, says
/// `Still working` up to the side panel's rule beside it ([`row_words`]), so text the panel draws
/// there is none, but for a row no row beside draws the rule at ([`panel_cut`]: the one row of a
/// live zone at the screen's top), read whole and so busy by the panel's words too. The one
/// reader of that hint, [`busy_signal`]'s and [`no_live_turn`]'s.
fn still_working(rows: &[String], from: usize, top: usize) -> bool {
    (from..top).any(|i| row_words(rows, i).contains("Still working"))
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
    let hint = still_working(rows, block.from, frame.top);
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

/// The [`BACKGROUND_RULES`] of work Claude Code runs in its own process (a dynamic workflow, a
/// background agent, other work it waits for), which its status row names only while that work
/// is pending: `Waiting for …`, the wait it carries above its own wake ([`carried_wait`]) and a
/// done row's `… still running`. No footer rule is one: a workflow's progress row stays drawn at
/// its full bar after the workflow ended (measured on 2.1.304,
/// `claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`), and the work the footer's pill
/// counts is read beside it where a restart would end it ([`unlisted_work`], [`work_running`]).
pub const IN_PROCESS_RULES: [&str; 4] = [
    "waiting for a dynamic workflow",
    "waiting for a background agent",
    "waiting for background work",
    "background work still running",
];

/// THE WORK CLAUDE CODE RUNS IN ITS OWN PROCESS, PENDING: the [`IN_PROCESS_RULES`] rule its status
/// row's own words read (its `Waiting for …`, the wait it carries above its own wake with no
/// status row drawn, a done row's `… still running` naming more than shells and monitors), read
/// whatever rule [`busy_signal`] reads first. That reader stops at its first rule, and a footer's
/// (a workflow's progress row, a shell count) comes ahead of the carried wait and the done row,
/// as the status row's own shell rule comes ahead of the agents a done row lists with a shell:
/// its one rule can hide this work, which a restart would end and no process row names. The
/// status row is the one that speaks for the turn ([`turn_end_line`]), so a done row Claude Code's
/// idle compaction drew its rows under still names the work it left. `None` with no composer
/// frame or a message submitted under the status row ([`background_wait`]'s bounds).
#[must_use]
pub fn in_process_work(rows: &[String]) -> Option<&'static str> {
    let frame = composer_frame(rows)?;
    if turn_just_begun(rows) {
        return None;
    }
    let Some(line) = turn_end_line(rows, frame.top) else {
        return carried_wait(rows, frame.top).then_some(CARRIED.rule);
    };
    status_busy(&line)
        .filter(|rule| IN_PROCESS_RULES.contains(rule))
        .or_else(|| {
            (is_done_row(&line) && left_in_process(&line))
                .then_some("background work still running")
        })
}

/// THE AGENT'S OWN WORK RUNNING UNDER A TURN'S END THAT A RESTART IN PLACE WOULD END, AS CLAUDE
/// CODE'S SCREEN NAMES IT, in [`busy_signal`]'s words: the work it runs in its own process
/// ([`in_process_work`]: its `Waiting for …`, the wait it carries above its own wake until the
/// workflows it waits for completed under it, a done row's `… still running` naming more than
/// shells and monitors), which no process row names; else a shell or a monitor the turn left
/// running, which the process table names too ([`shell_or_monitor_left`], read wherever the screen
/// names one); else any other work the footer's pill counts, which no process row names either
/// ([`footer_work`]: `1 MCP task`, `2 background tasks`, `◇ 1 cloud session`, `1 Artifact
/// comment monitor`, as `background work running`). The first and the last together are
/// [`unlisted_work`], which a reader of the process table (the upgrade's) reads beside that
/// table. Never a footer's workflow progress row alone: measured on 2.1.304, a workflow's progress
/// row stays drawn at its full bar after the workflow ended
/// (`claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`), so a restart held on it would
/// be held for as long as the row stays drawn, while a workflow still running is named by its
/// status row's `Waiting for …`. `None` with no composer frame or a message submitted under the
/// status row ([`background_wait`]'s bounds).
#[must_use]
pub fn work_running(rows: &[String]) -> Option<&'static str> {
    in_process_work(rows)
        .or_else(|| shell_or_monitor_left(rows))
        .or_else(|| footer_work(rows))
}

/// THE AGENT'S OWN WORK RUNNING UNDER A TURN'S END THAT NO PROCESS ROW NAMES, AS CLAUDE CODE'S
/// SCREEN NAMES IT, in [`busy_signal`]'s words: the work it runs in its own process
/// ([`in_process_work`]: its `Waiting for …`, the wait it carries above its own wake until the
/// workflows it waits for completed under it, a done row's `… still running` naming more than
/// shells and monitors); else any work the footer's pill counts that is no shell or monitor
/// counted alone ([`footer_work`]: `1 MCP task`, `2 background tasks`, `◇ 1 cloud session`, `1
/// Artifact comment monitor`): [`work_running`] without the shells and monitors the turn left,
/// which the process table names too, so a reader of that table (the upgrade's) reads this and
/// the table and never a shell or a monitor off the screen. `None` with no composer frame or a
/// message submitted under the status row ([`background_wait`]'s bounds).
#[must_use]
pub fn unlisted_work(rows: &[String]) -> Option<&'static str> {
    in_process_work(rows).or_else(|| footer_work(rows))
}

/// THE WORK THE FOOTER'S PILL COUNTS THAT NO PROCESS ROW NAMES: some item counts work
/// ([`work_count`]) and is no shell or monitor counted alone ([`process_count`]), as
/// [`busy_signal`]'s footer rule for it names it, `background work running`, with
/// [`background_wait`]'s bounds. The pill is a count of what runs now, redrawn as it changes (`·
/// 10 shells, 1 monitor`, then `· 13 shells, 3 monitors` 17 minutes later in 2.1.316's
/// captures), and with no done row drawn (`showTurnDuration` off) or one that names none of its
/// work, the one place the screen names that work. A cloud session runs elsewhere, and whether a
/// restart in place ends it or its wake was not established, so it holds a restart as the rest do
/// (fail closed).
fn footer_work(rows: &[String]) -> Option<&'static str> {
    let frame = composer_frame(rows)?;
    let unlisted = |item: &str| work_count(item) && !process_count(item);
    (!turn_just_begun(rows)
        && rows[frame.bottom + 1..]
            .iter()
            .flat_map(|r| row_items(r))
            .any(unlisted))
    .then_some("background work running")
}

/// A SHELL OR A MONITOR THE TURN LEFT RUNNING, read off each place the screen names one, in
/// [`busy_signal`]'s words and order (the status row's `N shell(s) still running`, a footer row's
/// `N shell(s)`, the status row's `N monitor(s) still running`, a footer row's `N monitor(s)`:
/// those of its [`BACKGROUND_RULES`] that the process table names too), whatever rule
/// [`busy_signal`] reads first: its one rule can be a footer
/// workflow progress row's, which outlives its workflow and comes ahead of a monitor in either
/// place and of a shell a later footer row counts. The one reading of what the turn left that
/// [`work_running`], [`turn_over_background`] and [`work_left`] share. `None` with no composer
/// frame or a message submitted under the status row ([`background_wait`]'s bounds).
fn shell_or_monitor_left(rows: &[String]) -> Option<&'static str> {
    let frame = composer_frame(rows)?;
    if turn_just_begun(rows) {
        return None;
    }
    let block = status_block(rows, frame.top);
    let line = block.line(rows);
    let status = line.as_deref();
    let footer = &rows[frame.bottom + 1..];
    let counted = |noun: &str| footer.iter().any(|r| footer_count(r, noun));
    if status.is_some_and(|r| still_running(r, "shell")) {
        Some("a shell still running")
    } else if counted("shell") {
        Some("a shell running")
    } else if status.is_some_and(|r| still_running(r, "monitor")) {
        Some("a monitor still running")
    } else if counted("monitor") {
        Some("a monitor running")
    } else {
        None
    }
}

/// A done row that says work other than shells and monitors (the process table's) is `still
/// running`: some ` · ` item's label ([`done_items`]) is more than `N shell(s)` and `N
/// monitor(s)` parts ([`process_count`]), a qualified monitor (`1 Artifact comment monitor`)
/// among the rest.
fn left_in_process(row: &str) -> bool {
    done_items(row).any(|item| {
        crate::text::trim_end(item)
            .strip_suffix(" still running")
            .is_some_and(|label| {
                let label = crate::text::trim(label);
                !label.is_empty()
                    && !label
                        .split(", ")
                        .all(|part| process_count(crate::text::trim(part)))
            })
    })
}

/// THE TURN IS OVER, AND WHAT RUNS ON IS A SHELL OR A MONITOR IT LEFT: a done row speaks for the
/// turn ([`turn_over`]), the agent's own background work keeps the screen busy with no live turn's
/// rule read ahead of it ([`background_wait`]), and the status row or the footer names a shell or
/// a monitor ([`shell_or_monitor_left`], wherever the screen names one). Not [`busy_signal`]'s
/// first rule alone: a workflow's progress row, which 2.1.304 leaves drawn after its workflow
/// ended (`claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`), comes ahead of a
/// monitor's rule in either place, and read so a monitor running under it was no break of the
/// agent's own background work where the host offers its fact, for as long as the row stood.
#[must_use]
pub fn turn_over_background(rows: &[String]) -> bool {
    turn_over(rows) && background_wait(rows).is_some() && shell_or_monitor_left(rows).is_some()
}

/// THE WORK THE TURN LEFT RUNNING, AS THE SCREEN NAMES IT: a shell or a monitor, in
/// [`busy_signal`]'s words, wherever the status row or the footer names it
/// ([`shell_or_monitor_left`]), where the turn rests over the agent's own background work
/// ([`background_rest`]), a done row ended it, and that row names no work of Claude Code's own
/// process ([`left_in_process`]). The title rests over a shell or a monitor as it does over a
/// screen no reader names, so these words are what tells the two apart. `None` elsewhere: the
/// title runs while a background agent or a workflow does, its turn over; a footer's progress row
/// outlives its workflow, so it alone names nothing the turn left (and its rule, which
/// [`busy_signal`] reads ahead of a monitor's, hides none); and no done row, an active `/goal` or a
/// wall says nothing of what the turn left.
#[must_use]
pub fn work_left(rows: &[String]) -> Option<&'static str> {
    background_rest(rows)?;
    let line = turn_end_line(rows, composer_frame(rows)?.top)?;
    if !is_done_row(&line) || left_in_process(&line) {
        return None;
    }
    shell_or_monitor_left(rows)
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

/// `N <noun>` or `N <noun>s` as one item of a footer row ([`row_items`]), a pill's joined
/// counts among them.
fn footer_count(row: &str, noun: &str) -> bool {
    row_items(row).any(|item| item_count(item, noun).is_some())
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
pub(crate) fn is_activity_row(row: &str) -> bool {
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
    /// The last try had no response headers within its first-byte window, and a retry is in
    /// flight: `No response from the API after 3m · retrying once, waiting up to 10m`. The row
    /// names the failed try's window and the retry's ([`ApiRetryRow::failed_wait_s`]), and never
    /// when the retry went out; it stands unchanged while that retry waits.
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
    /// How long the try that failed waited for response headers, in seconds (`No response from
    /// the API after 3m`): the first-byte window Claude Code gave it, 10 s at the least (measured
    /// on 2.1.310: `after 13s` under `CLAUDE_STREAM_FIRST_BYTE_TIMEOUT_MS=10000`). The try in
    /// flight is the retry after it, which the row does not time. `None` where the row cut the
    /// count, and for every other class.
    pub failed_wait_s: Option<u64>,
    /// The row's text past its spinner glyph, up to the side panel's rule ([`panel_cut`]).
    pub text: String,
    /// A `key if=` guard that matches this row while it stands, whatever glyph its spinner shows:
    /// the glyph's column, then, where the row counts down, the lead and the retry clause's words
    /// as literals and nothing after them (the countdown and the attempt move), and for a
    /// no-response row, which does not move while its retry waits, its whole text to the end of
    /// the row or to the side panel's rule, whatever the panel draws after it, so a press never
    /// lands on the row of a later try, and holds the row beside the panel and with none alike.
    pub guard: String,
}

/// Claude Code's live API-retry status row, where it names a network or no-response class
/// ([`RetryCause`]) and sits on one row directly above the composer's frame, read up to the side
/// panel's rule beside it ([`panel_cut`]); `None` for every other status row, a retry row of
/// another class, or one wrapped across rows.
#[must_use]
pub fn api_retry_row(rows: &[String]) -> Option<ApiRetryRow> {
    let frame = composer_frame(rows)?;
    let block = status_block(rows, frame.top);
    let at = block.status.filter(|&i| block.end == i)?;
    let row = row_words(rows, at);
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
    let failed_wait_s = match cause {
        RetryCause::NoResponse => lead
            .strip_prefix("No response from the API after ")
            .and_then(wait_secs),
        RetryCause::Unreachable | RetryCause::Stalled => None,
    };
    // A countdown row moves every second past its retry clause's words; a no-response row
    // stands as drawn while its retry waits, so the guard holds all of it, up to the side
    // panel's rule where the panel is drawn beside it.
    let (fixed, end) = match cause {
        RetryCause::NoResponse => (text.to_string(), "\\s*(?:│.*)?$"),
        RetryCause::Unreachable | RetryCause::Stalled => (format!("{lead} · {clause}"), ""),
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
        failed_wait_s,
        text: text.to_string(),
        guard: format!(
            "^\\s*[^\\s{glyph}]\\s+{}{end}",
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

/// The index of the bottom rule of Claude Code's prompt box, when one is on the screen: the
/// composer's ([`composer_bottom`]), else shell mode's, whose box has a `!` caret and whose
/// footer opens with the same mode row (vim's `-- INSERT --` included).
pub fn prompt_box_bottom(rows: &[String]) -> Option<usize> {
    composer_frame(rows)
        .or_else(|| shell_mode_frame(rows))
        .map(|f| f.bottom)
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
    /// The status row's text, its line read across the rows Ink wrapped it onto, joined with one space ([`crate::wrap::rejoined`]), up to the side panel's rule ([`status_words`]).
    pub(crate) fn line<'a>(&self, rows: &'a [String]) -> Option<Cow<'a, str>> {
        Some(status_words(rows, self.status?, self.end))
    }
}

/// THE WORDS OF A STATUS LINE drawn on rows `first..=end` (rejoined by [`status_text`] where Ink
/// wrapped it), each of its rows cut at the side panel's rule beside the line ([`panel_cut`]).
/// [`StatusBlock::line`] and the wait Claude Code carries ([`carried_row`]) read their lines here,
/// and every reader of those lines reads these words.
fn status_words(rows: &[String], first: usize, end: usize) -> Cow<'_, str> {
    let rule = panel_rule(rows, first, end);
    if end == first {
        return Cow::Borrowed(panel_cut(&rows[first], rule));
    }
    if rule.is_none() {
        return Cow::Owned(status_text(&rows[first..=end]));
    }
    let cut: Vec<String> = rows[first..=end]
        .iter()
        .map(|row| panel_cut(row, rule).to_string())
        .collect();
    Cow::Owned(status_text(&cut))
}

/// ONE ROW OF A BLOCK THE SIDE PANEL IS DRAWN BESIDE, CUT AT THE PANEL'S RULE: `rule` is the
/// rule's cell column beside the block ([`panel_rule`]), and the row's words are those before the
/// `│` it draws at that column, the padding before it trimmed; with no rule beside the block, or
/// none drawn at that column, the row whole, so a `│` of its own words is one of them. THE ONE
/// CUT OF THE PANEL OFF THE ROWS IT IS DRAWN BESIDE: a status line ([`status_words`]) and the rows
/// it is wrapped onto ([`status_line_end`]), the status row ([`status_row`]) and the API retry row
/// ([`api_retry_row`]), a hint row ([`still_working`]), a transcript item's rows
/// ([`wrapped_item`]) and the transcript its readers read ([`Transcript`]: what the agent last
/// said, the person's stop, where it ends, the rows an idle compaction drew, the notice a turn
/// ended on, the walks up to a usage limit, a hint over the composer, the cards parked over it) are
/// read through it, so beside a rule
/// [`panel_rule`] finds the panel's text is none of the agent's words and a row blank up to the
/// rule is a blank row. A block no row beside draws the rule at is read whole, the panel's text
/// with it: the one row of a live zone at the screen's top, over the composer's top rule, which
/// the panel is not drawn across (no capture draws the panel beside such a row). A row no longer
/// on the screen is read through it, to match words already held, at each rule it may have been
/// drawn beside ([`rule_columns`]).
#[must_use]
pub fn panel_cut(row: &str, rule: Option<usize>) -> &str {
    let Some(rule) = rule else {
        return row;
    };
    rule_cells(row)
        .find(|&(cell, _)| cell == rule)
        .map_or(row, |(_, at)| crate::text::trim_end(&row[..at]))
}

/// EACH SIDE PANEL'S RULE A ROW NO LONGER ON THE SCREEN MAY HAVE BEEN DRAWN BESIDE: the cell
/// column of each `│` `row` draws ([`rule_cells`]), each a rule [`panel_cut`] cuts it at. A row
/// the screen shows is read by the screen's [`Transcript`], whose rule [`panel_rule`] finds beside
/// the row over the composer's top rule; a row the screen no longer shows (one aterm-core's
/// archive kept: the row's text whole as it scrolled off, beside the panel the padding, the rule
/// and the panel's text with it, and no column or panel of its own) says by nothing which of its
/// `│`, if any, was a panel's rule, and the screen does not say either: by the time it is read the
/// panel may be closed or its rule at another column. So a reader that only matches such a row
/// against words it already holds reads it whole and cut at each of these, and keeps the row
/// itself whole (aterm-agent's `supervise::report::begins_with`, a turn's text).
pub fn rule_columns(row: &str) -> impl Iterator<Item = usize> + '_ {
    rule_cells(row).map(|(cell, _)| cell)
}

/// Row `i` cut at the side panel's rule beside it ([`panel_cut`], the rule [`panel_rule`] reads
/// beside that row alone).
fn row_words(rows: &[String], i: usize) -> &str {
    panel_cut(&rows[i], panel_rule(rows, i, i))
}

/// THE SIDE PANEL'S RULE BESIDE A BLOCK drawn on rows `first..=end` (a status line, or one row),
/// as the cell column ([`crate::question::cells`]) of a `│` its first row draws where a row
/// beside the block, the one above it or the one under it, draws a `│` too. With that panel open,
/// Claude Code 2.1.310 and later pad each row it is drawn beside out to the panel's rule and draw
/// the panel's own text after it, so the rows above and under a row draw the rule at its column
/// (`claude-2.1.310-waiting-3-workflows-turn-over-panel.txt`,
/// `claude-2.1.316-waiting-agents-and-workflows-turn-over-panel.txt`,
/// `claude-2.1.316-inferring-over-shells-and-agents-panel.txt`: every row from the screen's top
/// down to the composer's top rule, the transcript's and the live zone's), while a `│` the
/// block's own words hold (the spinner row draws an in-progress todo's words, the model's) has
/// none beside it at its column. `None`: no panel's rule beside the block.
pub(crate) fn panel_rule(rows: &[String], first: usize, end: usize) -> Option<usize> {
    let beside = |cell: usize| {
        [first.checked_sub(1), end.checked_add(1)]
            .into_iter()
            .flatten()
            .filter_map(|i| rows.get(i))
            .any(|row| rule_cells(row).any(|(at, _)| at == cell))
    };
    rule_cells(&rows[first])
        .map(|(cell, _)| cell)
        .find(|&cell| beside(cell))
}

/// Each `│` `row` draws in a cell of its own, as its cell column (the grid's layout of the row,
/// [`crate::question::Columns`]) and its byte offset.
fn rule_cells(row: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut columns = crate::question::Columns::default();
    let drawn = row.rfind('│').map_or(0, |last| last + '│'.len_utf8());
    row[..drawn].char_indices().filter_map(move |(at, c)| {
        let cell = columns.column();
        (columns.step(c) && c == '│').then_some((cell, at))
    })
}

/// THE TRANSCRIPT AS ITS READERS READ IT BESIDE THE SIDE PANEL: the screen with each row above
/// the composer's top rule cut at the panel's rule ([`panel_cut`]), the rule [`panel_rule`] finds
/// beside the live zone's last row, the one over that top rule, and the room those rows are laid
/// out in ([`Transcript::room`]). Claude Code 2.1.310 and later draw the panel beside every row
/// from the screen's top down to the composer's top rule and beside none of the composer's rows or
/// the rows under it (each measured panel capture, `panel_rule`'s), so the rule beside that last
/// row is the rule of every row above it, and a `│` standing anywhere else (a table's, a box's)
/// is none of the panel's. Every reader of what the agent last said ([`last_said_index`],
/// [`last_said_row`] and so [`asked_in_prose`], [`last_said_line`], the `said_*` and `turn_*`
/// readers of [`crate::turn`], the idle compaction's walk, [`crate::wall::login_restored`]), of
/// the person's stop ([`crate::turn::interrupted`]: an Esc, a declined question), of where the
/// transcript ends over a composer with no status line ([`transcript_end`]), of the notice the
/// turn ended on ([`notice`], and so [`limit_notice`] and [`crate::wall::wall`],
/// with the turn's own notice that names a generic banner, [`crate::wall::named_by_the_turn`], and
/// [`last_said_notice`]), the walks up to the newest usage limit and what was said under it
/// ([`crate::wall::since_limit`], [`crate::wall::auto_continue_end`], [`crate::wall::left_wall`],
/// [`crate::wall::limit_words_above`]) and of a right-edge hint over the composer
/// ([`crate::turn::goal_active`], [`crate::wall::memory_wall`], [`crate::wall::login_expires_in`],
/// [`crate::credits::in_use`] and [`crate::credits::usage_warning`], each against the rule's room)
/// and of the cards parked over the composer ([`crate::survey::parked_span`], which the status
/// walk, [`transcript_end`] and the last-said walk step over, and [`crate::survey::survey_block`],
/// which [`survey_open`] reads, each against the rule's room) reads the transcript's rows here;
/// with no composer frame, or no rule beside that row, the screen is read whole. Outside this
/// crate it is Claude Code's reader's [`crate::reader::ScreenReader::transcript`]: the worker's
/// report and the harness's point key read their rows through it (aterm-agent's
/// `supervise::report` and `supervise::run`).
pub struct Transcript<'a> {
    /// The screen's rows, each above the composer's top rule cut at the rule, the rest whole.
    pub(crate) rows: Cow<'a, [String]>,
    /// The rule's cell column, `None` with no panel drawn.
    rule: Option<usize>,
}

impl<'a> Transcript<'a> {
    /// The transcript of `rows`.
    pub(crate) fn of(rows: &'a [String]) -> Self {
        let rule = composer_frame(rows).and_then(|frame| {
            let last = frame.top.checked_sub(1)?;
            panel_rule(rows, last, last).map(|rule| (frame.top, rule))
        });
        let Some((top, rule)) = rule else {
            return Self::whole(rows);
        };
        let rows = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                if i < top {
                    panel_cut(row, Some(rule)).to_string()
                } else {
                    row.clone()
                }
            })
            .collect();
        Self {
            rows: Cow::Owned(rows),
            rule: Some(rule),
        }
    }

    /// `rows` whole, no panel's rule beside them: the transcript of a screen whose reader reads
    /// no side panel ([`crate::reader::ScreenReader::transcript`]'s default).
    #[must_use]
    pub const fn whole(rows: &'a [String]) -> Self {
        Self {
            rows: Cow::Borrowed(rows),
            rule: None,
        }
    }

    /// The screen's rows as the transcript's readers read them: each row above the composer's
    /// top rule cut at the panel's rule, the rest whole; with no rule, the screen whole. As many
    /// rows as the screen.
    #[must_use]
    pub fn rows(&self) -> &[String] {
        &self.rows
    }

    /// The cells the rows above the composer are laid out in: beside the panel, the rule's
    /// column, as wide as Claude Code lays those rows out and wraps them (the measured 2.1.316
    /// captures wrap the transcript's prose at the rule; [`status_line_end`] reads a status line
    /// so), and the right edge its hints there stand against; with no panel, `width`, the
    /// composer's or the screen's, as the reader measures it.
    pub(crate) fn room(&self, width: usize) -> usize {
        self.rule.unwrap_or(width)
    }

    /// How many cells short of the screen's widest row ([`crate::question::screen_width`]) the
    /// rows above the composer end, [`crate::wrap::said_lines`]'s margin: beside the panel, the
    /// cells from its rule to that edge; with none, none.
    pub(crate) fn margin(&self) -> usize {
        self.rule.map_or(0, |rule| {
            crate::question::screen_width(&self.rows).saturating_sub(rule)
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

/// The last row of the status line row `i` opens, short of row `until`, in a pane `width` cells
/// wide ([`line_end_in`]). Beside the side panel the line is read on its rows cut at the panel's
/// rule ([`panel_cut`]) in a pane as wide as the rule's column: Claude Code lays the rows it
/// draws beside the panel out that wide and wraps them there (the measured 2.1.316 captures wrap
/// the transcript's prose and the spinner's tip at the rule), and the wrap readers take no row
/// that draws the rule.
fn status_line_end(rows: &[String], i: usize, until: usize, width: usize) -> Option<usize> {
    if !crate::text::trim(&rows[i]).starts_with(is_spinner_glyph) {
        return None;
    }
    let Some(rule) = panel_rule(rows, i, i) else {
        return line_end_in(rows, i, until, width);
    };
    let cut: Vec<String> = rows[i..until.max(i + 1)]
        .iter()
        .map(|row| panel_cut(row, Some(rule)).to_string())
        .collect();
    line_end_in(&cut, 0, cut.len(), rule).map(|end| i + end)
}

/// The status row's line, wrapped in a narrow pane.
fn line_end_in(rows: &[String], i: usize, until: usize, width: usize) -> Option<usize> {
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

/// The status row, walking up from the composer's top rule: the first row of its line, cut at the
/// side panel's rule beside the line as the line is ([`panel_cut`]).
pub fn status_row(rows: &[String]) -> Option<&str> {
    let frame = composer_frame(rows)?;
    let block = status_block(rows, frame.top);
    let i = block.status?;
    Some(panel_cut(&rows[i], panel_rule(rows, i, block.end)))
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

/// WHETHER CLAUDE CODE'S PROMPT-HISTORY SEARCH (Ctrl-R) IS OPEN: a row of the footer, the rows under
/// the composer's bottom rule down to the first blank one, opens with its words, `search prompts:`
/// or, its query matching nothing, `no matching prompt:`. Measured on 2.1.318
/// (`claude-2.1.318-history-search*`): they head the mode row, in place of `-- INSERT --` in vim
/// mode, which is the footer's first row, or its second under a status line
/// (`claude-2.1.318-history-search-under-statusline`); the composer's row shows the old prompt the
/// search matched, plain as typed text, or what stood there before it (nothing, or a grey
/// suggestion), the cursor stands after the query on the search's row, keys type into the query, and
/// Enter sends the match. In a narrow window the label wraps in its own column beside the query
/// ([`search_label_at`]), each row at the label's indent: by whole words at 60 and 44 columns
/// (`search` / `prompts:`, `no matching` / `prompt:`) and at 34 (`no` / `matching` / `prompt:`),
/// within its words at 26 (`searc` / `h pro` / `mpts:`) and 22 (`sear` / `ch` / `prom` / `pts:`),
/// and, its column two cells wide at 14 and 12, with the space between two of its words a blank row
/// of its own (`se` / `ar` / `ch` / blank / `pr` / `om` / `pt` / `s:`, `no` / blank / `ma` / …), which
/// ends no footer the label goes on under
/// (`claude-2.1.318-history-search{,-no-match}-{60,44,26,12}col`, `-no-match-{34,22,14}col`,
/// `-14col`, `-narrow-14col`). Only the footer is read: a person's draft is drawn between the rules
/// (a row after its first indented, so never a rule), and the transcript above them.
#[must_use]
pub fn history_search(rows: &[String]) -> bool {
    composer_frame(rows).is_some_and(|f| {
        let under: Vec<&str> = rows.iter().skip(f.bottom + 1).map(String::as_str).collect();
        let footer = under
            .iter()
            .take_while(|row| !row.trim().is_empty())
            .count();
        (0..footer).any(|at| {
            [
                crate::anchor!("history.search"),
                crate::anchor!("history.no_match"),
            ]
            .into_iter()
            .any(|label| search_label_at(&under[at..], label))
        })
    })
}

/// Whether the search's `label` is spelled from `rows[0]` on: its letters, the spaces between its
/// words aside, are the leading words of `rows[0]` and of each row after it at the same indent, each
/// row giving at least one, until none is owed ([`label_spelled`]). Wrapped in its own column, the
/// label leaves a row by a whole word or, its column narrower than a word, within one, and a column
/// two cells wide gives the space between two words a blank row of its own; the words after its
/// part on a row (the query, the mode) are read only where they could still be the label's, and
/// every split is tried, so a query that starts with the label's next letters (`p`) does not hide
/// it.
fn search_label_at(rows: &[&str], label: &str) -> bool {
    let letters: String = label.split_whitespace().collect();
    // What is owed where a word of the label starts, past its first: the bytes of its letters
    // from there on.
    let words: Vec<&str> = label.split_whitespace().collect();
    let starts: Vec<usize> = (1..words.len())
        .map(|i| words[i..].iter().map(|w| w.len()).sum())
        .collect();
    rows.first().is_some_and(|first| {
        label_spelled(
            rows,
            &letters,
            &Spell {
                at: indent_of(first),
                starts: &starts,
            },
        )
    })
}

/// How [`label_spelled`] reads the label's column: the indent `at` every row of it stands at, and
/// what is owed (`starts`, in bytes) where one of its words starts, past its first, which one blank
/// row may stand before.
struct Spell<'a> {
    at: usize,
    starts: &'a [usize],
}

/// Whether `owed` is spelled by the leading words of `rows[0]`, then, where it is not yet whole, by
/// those of the rows after it, each at the indent `spell.at` and giving at least one word, one blank
/// row standing before a word of the label (`spell.starts`).
fn label_spelled(rows: &[&str], owed: &str, spell: &Spell<'_>) -> bool {
    let Some((row, below)) = rows.split_first() else {
        return false;
    };
    if indent_of(row) != spell.at {
        return false;
    }
    let mut rest = owed;
    for word in row.split_whitespace() {
        let Some(after) = rest.strip_prefix(word) else {
            return false;
        };
        rest = after;
        if rest.is_empty() || label_spelled(below, rest, spell) {
            return true;
        }
        if spell.starts.contains(&rest.len())
            && below.split_first().is_some_and(|(blank, under)| {
                blank.trim().is_empty() && label_spelled(under, rest, spell)
            })
        {
            return true;
        }
    }
    false
}

/// The bytes of `row`'s leading whitespace.
fn indent_of(row: &str) -> usize {
    row.len() - crate::text::trim_start(row).len()
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
pub(crate) const STATUS_GLYPHS: &[char] = &['✔', '✓', '✘', '✗', '⚠', 'ℹ'];

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
pub(crate) fn is_hint(row: &str, width: Option<usize>) -> bool {
    let lead = leading_spaces(row);
    let end = crate::text::trim_end(row).chars().count();
    end > lead && (lead >= HINT_COLUMN || (lead >= 6 && width.is_some_and(|w| end + 3 >= w)))
}

/// The last row the worker said (see [`last_said_index`]), its words up to the side panel's rule
/// ([`Transcript`]): beside the panel, the row's padding, the rule and the panel's text are none
/// of them, so a question's `?` ends the row.
pub fn last_said_row(rows: &[String]) -> Option<&str> {
    let transcript = Transcript::of(rows);
    let i = last_said_in(rows, &transcript)?;
    Some(panel_cut(&rows[i], transcript.rule))
}

/// The index of the last thing said above the composer, each row read by its words up to the side
/// panel's rule and its room the rule's ([`Transcript`]): read whole, a row beside the panel's own
/// `───` reads as a separator and an indented row whose panel text runs to the screen's edge as a
/// hint ([`is_hint`]), neither as said.
pub fn last_said_index(rows: &[String]) -> Option<usize> {
    last_said_in(rows, &Transcript::of(rows))
}

/// The line the last said row ([`last_said_index`]) ends: that row with the rows Ink wrapped its
/// line from in a pane `width` cells wide ([`crate::line_start`]; `None`, the row alone),
/// each trimmed and joined by a space. Each row is read up to the side panel's rule and the wrap in
/// its room ([`Transcript`]), so beside the panel the line holds none of the rows' padding, the
/// rule or the panel's text.
#[must_use]
pub fn last_said_line(rows: &[String], width: Option<usize>) -> Option<String> {
    let transcript = Transcript::of(rows);
    let last = last_said_in(rows, &transcript)?;
    let said = &transcript.rows;
    let first = width.map_or(last, |w| {
        crate::wrap::line_start(said, last, transcript.room(w))
    });
    Some(
        said[first..=last]
            .iter()
            .map(|r| crate::text::trim(r))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// [`last_said_index`] on `rows`, whose transcript ([`Transcript::of`]) the caller holds.
pub(crate) fn last_said_in(rows: &[String], transcript: &Transcript<'_>) -> Option<usize> {
    let (end, width) = match composer_frame(rows) {
        Some(f) => (
            status_block(rows, f.top).status.unwrap_or(f.top),
            Some(transcript.room(crate::text::trim_end(&rows[f.bottom]).chars().count())),
        ),
        None => (composer_index(rows).unwrap_or(rows.len()), None),
    };
    last_said_before(&transcript.rows, end, width)
}

/// The index of the last thing said above row `end` (`width`: the composer's, or beside the side
/// panel its rule's column ([`Transcript::room`]), so that its right-edge hints are not read as
/// said), passing over the vendor's own `⏺` notice paragraphs ([`vendor_paragraph`]). A `/goal`
/// echo's cause is read from above the echo's own row this way: a person's `❯` prompt above the
/// echo is a row of the transcript, never the composer ([`composer_index`]) to read above.
fn last_said_before(rows: &[String], end: usize, width: Option<usize>) -> Option<usize> {
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
pub(crate) fn vendor_paragraph(rows: &[String], i: usize) -> Option<usize> {
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

/// Where the transcript ends on the screen, by position: under a done status line, at a live one,
/// and with none above what Claude Code parks between the transcript and the composer (a blank
/// row, a survey, a tip, a hint against the right edge), each row read on the transcript, cut at
/// the side panel's rule and against its room ([`Transcript`]), so a row blank up to the rule is a
/// blank row.
pub fn transcript_end(rows: &[String]) -> usize {
    transcript_end_in(rows, &Transcript::of(rows))
}

/// [`transcript_end`] on `rows`, whose transcript ([`Transcript::of`]) the caller holds.
pub(crate) fn transcript_end_in(rows: &[String], transcript: &Transcript<'_>) -> usize {
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
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let said: &[String] = &transcript.rows;
    let mut end = frame.top;
    let parked = crate::survey::parked_span(said);
    while end > 0
        && (is_parked_above_composer(&said[end - 1], width)
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
    crate::wall::wall(rows)
        .filter(|w| w.kind.reads_limited())
        .map(|w| (w.message, w.reset))
}

/// The notice the worker's last turn ended on, placed as [`crate::wall::wall`] places it, each row
/// of it over the composer read up to the side panel's rule in its room ([`Transcript`]): read
/// whole, its message carried each row's padding, the rule and the panel's text after it, and a
/// line Ink wrapped at the rule was read as its first row.
pub(crate) fn notice(rows: &[String]) -> Option<Wall> {
    let transcript = Transcript::of(rows);
    let mut found = notice_placed(rows, &transcript)?;
    if let Some(frame) = composer_frame(rows) {
        let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
        let (said, room) = (&*transcript.rows, transcript.room(width));
        let above = found.row + 1..frame.top;
        let below = frame.bottom + 1..rows.len();
        if let Some(toast) = above
            .map(|i| line_at(said, i, frame.top, room).1)
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

/// [`notice`] on `rows`, its rows over the composer read on their transcript ([`Transcript`]),
/// the footer's whole: the side panel is drawn beside none of the footer's rows.
fn notice_placed(rows: &[String], transcript: &Transcript<'_>) -> Option<Wall> {
    let last = last_said_in(rows, transcript);
    let said: &[String] = &transcript.rows;
    if let Some(frame) = composer_frame(rows) {
        let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
        let from = last.map_or(0, |i| i + 1);
        if let Some(found) = footer_notice(rows, frame.bottom + 1, width)
            .or_else(|| banner_notice(said, from, frame.top, transcript.room(width)))
        {
            return Some(crate::wall::named_by_the_turn(rows, transcript, found));
        }
    }
    let last = last?;
    let width = transcript.room(crate::question::screen_width(rows));
    if let Some(echo) = goal_echo_open(said, last)
        && let Some(prev) = last_said_before(&said[..echo], echo, None)
        && let Some(mut cause) = notice_at(said, prev, width)
        && let text = goal_echo_text(said, echo, last)
        && echo_names_the_cause_of(&text, cause.kind)
    {
        cause.message = format!("{} \u{2014} {text}", cause.message);
        return Some(cause);
    }
    notice_at(said, last, width)
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

/// The wall the last thing said (ending at `last`) is, by placement, its rows wrapped in a pane
/// `width` cells wide.
fn notice_at(rows: &[String], last: usize, width: usize) -> Option<Wall> {
    if let Some(found) = error_row_notice(rows, last, width) {
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
    let kind = classify_wall(&lines[0])?;
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

/// The rows the vendor's `⏺` notice on rows `open..=end`, drawn in a pane `width` cells wide,
/// would take at 80 columns.
fn notice_rows_at_80(rows: &[String], open: usize, end: usize, width: usize) -> Option<usize> {
    use crate::question::cells;
    const ROW_AT_80: usize = 80 - NOTICE_MARGIN;
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

/// The vendor's error row the last turn ended on, in a pane `width` cells wide.
fn error_row_notice(rows: &[String], last: usize, width: usize) -> Option<Wall> {
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
        || notice_rows_at_80(rows, open, end, width).is_some_and(|n| n <= ERROR_ROWS);
    let remedy = short && (text.contains(" · ") || text.contains(" ∙ "));
    if !(head.starts_with("API Error") || remedy) {
        return None;
    }
    let kind = classify_wall(&text)?;
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

/// The last thing said, where claude code draws its own notices, its rows read up to the side
/// panel's rule in its room ([`Transcript`]).
#[must_use]
pub fn last_said_notice(rows: &[String]) -> Option<SaidNotice> {
    let transcript = Transcript::of(rows);
    let last = last_said_in(rows, &transcript)?;
    let width = transcript.room(crate::question::screen_width(rows));
    let rows: &[String] = &transcript.rows;
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
            || notice_rows_at_80(rows, open, end, width).is_some_and(|n| n <= ERROR_ROWS);
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
pub(crate) fn is_stop_hook_summary(row: &str) -> bool {
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
fn footer_notice(rows: &[String], from: usize, width: usize) -> Option<Wall> {
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
                .find_map(|item| classify_wall(item).map(|kind| (item, kind)))
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

/// A notice on a banner row under the last thing said, rows `from..top`, in a pane `width` cells
/// wide.
fn banner_notice(rows: &[String], from: usize, top: usize, width: usize) -> Option<Wall> {
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
        classify_wall(t).map(|kind| Wall {
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

    /// CLAUDE CODE'S PROMPT-HISTORY SEARCH IS READ ON THE FOOTER ALONE (measured on 2.1.318): open
    /// over its match, over nothing and over a grey suggestion, under a status line, where it heads
    /// the footer's second row, in vim mode, where it takes the mode's row, and in windows of 60, 44,
    /// 34, 26, 22, 14 and 12 columns, where its label wraps in its own column, by whole words or
    /// within them, and a column two cells wide gives the space between two words a blank row. A
    /// wrapped label is read only whole and leading its row: its rest gone, at another indent, other
    /// words in its place, a row between, another word before it, a blank row within a word or two
    /// blank rows, and it is closed; a query that starts with its next letters (`p`, `prompts`)
    /// leaves it open. The same
    /// screen with its footer row back is closed, and the search's words are read nowhere else: on a
    /// person's draft's first row or a later one, in the transcript, or past the footer's end.
    /// NEGATIVE CONTROLS: `history_search` reading the footer's first row alone, and the status
    /// line's screen reads the search closed; reading the words on any row, and a draft's later row
    /// reads it open; the label read only whole on one row (`fcf2daa1f8`'s parent), and every
    /// narrow screen reads closed; read by whole words (`fcf2daa1f8`), and the 26 and 22 column
    /// screens read closed; skipping a word that is not the label's, and a label after another word
    /// reads open; taking a row's words greedily, no split tried again, and the query `p` hides it;
    /// the footer ending at its first blank row, as before, and the 12-column match and the four
    /// frames with a blank row read closed; a blank row read anywhere in the label, and one within
    /// a word reads open.
    #[test]
    fn the_history_search_is_read_on_the_footer_alone() {
        use crate::prompt::fixtures::{
            HISTORY_SEARCH_2_1_318, HISTORY_SEARCH_12COL_2_1_318, HISTORY_SEARCH_14COL_2_1_318,
            HISTORY_SEARCH_14COL_LONG_QUERY_2_1_318, HISTORY_SEARCH_22COL_2_1_318,
            HISTORY_SEARCH_26COL_2_1_318, HISTORY_SEARCH_44COL_2_1_318,
            HISTORY_SEARCH_60COL_2_1_318, HISTORY_SEARCH_NO_MATCH_2_1_318,
            HISTORY_SEARCH_NO_MATCH_12COL_2_1_318, HISTORY_SEARCH_NO_MATCH_14COL_2_1_318,
            HISTORY_SEARCH_NO_MATCH_26COL_2_1_318, HISTORY_SEARCH_NO_MATCH_34COL_2_1_318,
            HISTORY_SEARCH_NO_MATCH_44COL_2_1_318, HISTORY_SEARCH_NO_MATCH_60COL_2_1_318,
            HISTORY_SEARCH_OVER_SUGGESTION_2_1_318, HISTORY_SEARCH_UNDER_STATUSLINE_2_1_318,
            screen as fx,
        };
        let open = fx(HISTORY_SEARCH_2_1_318);
        for (name, rows) in [
            ("its match", open.clone()),
            ("nothing", fx(HISTORY_SEARCH_NO_MATCH_2_1_318)),
            ("a suggestion", fx(HISTORY_SEARCH_OVER_SUGGESTION_2_1_318)),
            ("a status line", fx(HISTORY_SEARCH_UNDER_STATUSLINE_2_1_318)),
            ("its match, 60 columns", fx(HISTORY_SEARCH_60COL_2_1_318)),
            (
                "nothing, 60 columns",
                fx(HISTORY_SEARCH_NO_MATCH_60COL_2_1_318),
            ),
            ("its match, 44 columns", fx(HISTORY_SEARCH_44COL_2_1_318)),
            (
                "nothing, 44 columns",
                fx(HISTORY_SEARCH_NO_MATCH_44COL_2_1_318),
            ),
            (
                "nothing, 34 columns",
                fx(HISTORY_SEARCH_NO_MATCH_34COL_2_1_318),
            ),
            ("its match, 26 columns", fx(HISTORY_SEARCH_26COL_2_1_318)),
            (
                "nothing, 26 columns",
                fx(HISTORY_SEARCH_NO_MATCH_26COL_2_1_318),
            ),
            ("its match, 22 columns", fx(HISTORY_SEARCH_22COL_2_1_318)),
            ("its match, 14 columns", fx(HISTORY_SEARCH_14COL_2_1_318)),
            (
                "its match, 14 columns, a long query",
                fx(HISTORY_SEARCH_14COL_LONG_QUERY_2_1_318),
            ),
            (
                "nothing, 14 columns",
                fx(HISTORY_SEARCH_NO_MATCH_14COL_2_1_318),
            ),
            ("its match, 12 columns", fx(HISTORY_SEARCH_12COL_2_1_318)),
            (
                "nothing, 12 columns",
                fx(HISTORY_SEARCH_NO_MATCH_12COL_2_1_318),
            ),
        ] {
            assert!(history_search(&rows), "over {name}: {rows:#?}");
        }
        // A blank row stands in the label only where its column gives the space between two words
        // one: once, never within a word.
        let two_wide = fx(HISTORY_SEARCH_12COL_2_1_318);
        let label = composer_bottom(&two_wide).expect("the frame") + 1;
        assert_eq!(
            two_wide[label..label + 8]
                .iter()
                .map(|r| r.trim())
                .collect::<Vec<_>>(),
            ["se … ⏵⏵", "ar", "ch", "", "pr", "om", "pt", "s:"],
            "{two_wide:#?}"
        );
        let mut within = two_wide.clone();
        within.insert(label + 2, String::new());
        within.remove(label + 4);
        let mut twice = two_wide.clone();
        twice.insert(label + 3, String::new());
        for (name, rows) in [("within a word", within), ("twice", twice)] {
            assert!(!history_search(&rows), "a blank row {name}: {rows:#?}");
        }
        // A narrow window's wrapped label is read only whole: its rest leads the next footer row at
        // the same indent.
        let narrow = fx(HISTORY_SEARCH_NO_MATCH_44COL_2_1_318);
        let rest = composer_bottom(&narrow).expect("the frame") + 2;
        assert_eq!(narrow[rest].trim(), "prompt:", "{narrow:#?}");
        for (name, row) in [
            ("its rest gone", ""),
            ("its rest elsewhere", "    prompt:"),
            ("other words", "  history:"),
        ] {
            let mut rows = narrow.clone();
            rows[rest] = row.to_string();
            assert!(!history_search(&rows), "{name}: {rows:#?}");
        }
        let mut between = narrow.clone();
        between.insert(rest, "  history:".to_string());
        assert!(!history_search(&between), "a row between: {between:#?}");
        // Its first part leads its row: after another word it is not the search's.
        let mut led = narrow.clone();
        led[rest - 1] = "  ⏵⏵ no matching zzq".to_string();
        assert!(!history_search(&led), "after another word: {led:#?}");
        let mut led = open.clone();
        let mode = composer_bottom(&led).expect("the frame") + 1;
        led[mode] = format!("  lab {}", led[mode].trim_start());
        assert!(!history_search(&led), "after another word, whole: {led:#?}");
        // A query that starts with the label's next letters is the query's: every split is tried.
        let wrapped = fx(HISTORY_SEARCH_60COL_2_1_318);
        let first = composer_bottom(&wrapped).expect("the frame") + 1;
        assert!(wrapped[first].starts_with("  search "), "{wrapped:#?}");
        for query in ["p", "prompts", "prompts:"] {
            let mut rows = wrapped.clone();
            rows[first] = format!("  search  {query}  ⏵⏵ auto mode on");
            assert!(history_search(&rows), "query {query:?}: {rows:#?}");
        }
        let caret = composer_index(&open).expect("the composer");
        let footer = composer_bottom(&open).expect("the frame") + 1;
        let with = |at: usize, row: &str| {
            let mut rows = open.clone();
            rows[at] = row.to_string();
            rows
        };
        // Vim mode's, measured (2.1.318, `editorMode: vim`): the search takes the mode's row.
        assert!(history_search(&with(
            footer,
            "  search prompts: read  ⏵⏵ auto mode on (shift+tab to cycle)"
        )));
        let closed = with(footer, "  ⏵⏵ auto mode on (shift+tab to cycle)");
        let words = "search prompts: readme";
        let mut later_row = closed.clone();
        later_row.insert(caret + 1, format!("  {words}"));
        let mut past_footer = closed.clone();
        past_footer.splice(
            footer + 1..footer + 1,
            [String::new(), format!("  {words}")],
        );
        let mut said = closed.clone();
        said[caret - 2] = format!("⏺ {words}");
        let mut drafted = closed.clone();
        drafted[caret] = format!("❯ {words}");
        for (name, rows) in [
            ("closed", closed),
            ("a draft's first row", drafted),
            ("a draft's later row", later_row),
            ("the transcript", said),
            ("past the footer's end", past_footer),
        ] {
            assert!(composer_frame(&rows).is_some(), "{name}: {rows:#?}");
            assert!(!history_search(&rows), "{name}: {rows:#?}");
        }
    }

    /// Shell mode's box has a `!` caret, so it has no composer frame, and its footer opens with
    /// vim's `-- INSERT --` as the composer's does: `prompt_box_bottom` finds its bottom rule.
    /// Where a composer frame is on the screen, it is the composer's.
    #[test]
    fn the_prompt_box_bottom_is_shell_mode_s_where_no_composer_is() {
        let shell = crate::prompt::fixtures::screen(include_str!(
            "fixtures/claude-2.1.311-vim-insert-shell-mode-144col.txt"
        ));
        assert_eq!(
            composer_bottom(&shell),
            None,
            "shell mode has no `❯` composer"
        );
        let bottom = prompt_box_bottom(&shell).expect("shell mode's box");
        assert!(
            shell[bottom - 1].starts_with('!'),
            "{:?}",
            shell[bottom - 1]
        );
        assert!(
            shell[bottom + 1]
                .trim_start()
                .starts_with(crate::anchor!("vim.insert")),
            "{:?}",
            shell[bottom + 1]
        );
        for text in [
            include_str!("fixtures/claude-2.1.304-top-vim-insert-120col.txt"),
            include_str!("fixtures/claude-2.1.304-top-vim-normal-typed-120col.txt"),
        ] {
            let rows = crate::prompt::fixtures::screen(text);
            assert!(composer_bottom(&rows).is_some());
            assert_eq!(prompt_box_bottom(&rows), composer_bottom(&rows));
        }
        assert_eq!(prompt_box_bottom(&rows(&["no box here", ""])), None);
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

    /// A person's Esc is no question: after a declined approval box (2.1.316) and on the vendor's
    /// retry row (2.1.301), Claude Code writes `⎿  Interrupted · What should Claude do instead?`
    /// under what it cut short and returns to an idle composer. The last row said ends in `?`,
    /// but the question is the vendor's to the person: the worker is idle, and a turn a person
    /// stopped over its own background shells rests idle. An agent's own `?` still reads
    /// `question`.
    #[test]
    fn a_persons_esc_is_no_question() {
        for (name, text) in [
            (
                "claude-2.1.316-box-bash-declined-esc.txt",
                crate::prompt::fixtures::BOX_BASH_DECLINED_ESC_2_1_316,
            ),
            (
                "claude-2.1.301-wall-watchdog-midturn-esc.txt",
                crate::prompt::fixtures::WALL_WATCHDOG_MIDTURN_ESC_2_1_301,
            ),
        ] {
            let rows = crate::prompt::fixtures::screen(text);
            assert!(
                last_said_row(&rows).is_some_and(crate::turn::interrupt_notice),
                "{name}: the last row said is the vendor's notice"
            );
            assert!(crate::turn::interrupted(&rows), "{name}: interrupted");
            assert_eq!(worker_phase(&rows), Phase::Idle, "{name}");
        }
        let asked = screen(
            &[
                "⏺ Should I delete the old logs?",
                "",
                "✻ Brewed for 0s · done 10:46 AM",
                "",
            ],
            "  ⏸ manual mode on · ? for shortcuts",
        );
        assert_eq!(worker_phase(&asked), Phase::Question, "the agent's own ask");
        let shells = crate::prompt::fixtures::screen(crate::prompt::fixtures::IDLE_SHELLS_2_1_271);
        let said = shells
            .iter()
            .position(|x| x.ends_with("when they notify."))
            .expect("the last said row");
        let mut stopped = shells.clone();
        stopped.insert(
            said + 1,
            "  ⎿  Interrupted · What should Claude do instead?".to_string(),
        );
        assert!(
            last_said_row(&stopped).is_some_and(crate::turn::interrupt_notice),
            "the notice is the last row said"
        );
        assert_eq!(worker_phase(&stopped), Phase::Busy, "its shells run on");
        assert_eq!(background_rest(&stopped), Some(Phase::Idle));
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

    /// [`crate::prompt::fixtures::IDLE_COMPACTED_OVER_A_SHELL_2_1_312`] and the index of its row
    /// that opens `head`.
    fn compacted_over_a_shell(head: &str) -> (Vec<String>, usize) {
        let measured = crate::prompt::fixtures::screen(
            crate::prompt::fixtures::IDLE_COMPACTED_OVER_A_SHELL_2_1_312,
        );
        let at = measured
            .iter()
            .position(|r| r.starts_with(head))
            .unwrap_or_else(|| panic!("{head}"));
        (measured, at)
    }

    /// THE TURN A COMPACTION FOLLOWED STAYS OVER (2026-10-27): Claude Code compacted its idle
    /// conversation under a done row with a shell still running, and the notice, a row of the
    /// transcript, ended the status read, so the turn read as not over and the stall canary called
    /// the screen one this build does not recognise. The notice still ends the live zone:
    /// that frozen row says nothing of now, and the live footer alone says what runs.
    #[test]
    fn an_idle_compaction_leaves_the_turn_it_followed_over() {
        let (measured, _) = compacted_over_a_shell("✻ Worked for 23s");
        assert!(turn_over(&measured));
        assert!(done_row_drawn(&measured));
        assert!(turn_over_background(&measured));
        assert_eq!(
            busy_signal(&measured).map(|b| (b.zone, b.rule)),
            Some((Zone::Footer, "a shell running"))
        );
        assert_eq!(shells_running(&measured), Some(1));
        assert_eq!(worker_phase(&measured), Phase::Busy);
        assert_eq!(background_rest(&measured), Some(Phase::Idle));
        for fixture in [
            crate::prompt::fixtures::IDLE_COMPACTED_MEASURED,
            crate::prompt::fixtures::IDLE_COMPACTED_HINT_MEASURED,
        ] {
            let idle = crate::prompt::fixtures::screen(fixture);
            assert!(turn_over(&idle));
            assert!(!turn_over_background(&idle), "no work under it");
        }

        let inserted = |at: usize, row: &str| {
            let mut r = measured.clone();
            r.insert(at, row.to_string());
            r
        };
        let (_, done) = compacted_over_a_shell("✻ Worked for 23s");
        let (_, task) = compacted_over_a_shell("⏺ Task ");
        let (_, notice) = compacted_over_a_shell("⏺ Compacted while idle");
        let mut task_unread = measured.clone();
        task_unread[task] = "⏺ The task is still going.".to_string();
        for (name, rows) in [
            (
                "a person's message under the row",
                inserted(done + 1, "❯ what is left?"),
            ),
            (
                "a reply under the row",
                inserted(done + 1, "⏺ Nothing is left."),
            ),
            ("a row of no known shape among them", task_unread),
            (
                "a reply under the notice",
                inserted(notice + 1, "⏺ Nothing is left."),
            ),
            (
                "a live spinner under the notice",
                inserted(notice + 1, "✽ Baking… (3s)"),
            ),
        ] {
            assert!(!turn_over(&rows), "{name}");
            assert!(!turn_over_background(&rows), "{name}");
        }
    }

    /// A JOINED PILL COUNTS EACH WORK IT JOINS: Claude Code joins its counts into one footer item
    /// (`· 1 shell, 1 monitor ·` on 2.1.283,
    /// `claude-2.1.283-wall-session-wrapup-waiting-measured.txt`; `· 13 shells, 3 monitors` on
    /// 2.1.316, `claude-2.1.316-waiting-agents-and-workflows-turn-over-panel.txt`), and the footer
    /// read neither noun of it: where the footer is the one place a turn's work is read (its done
    /// row under an idle compaction's notice), a turn that left a shell and a monitor read as
    /// having left nothing, while the same screen counting `1 shell` alone left a shell. The
    /// control: a pill of work Claude Code runs in its own process names no shell or monitor.
    #[test]
    fn a_joined_pill_counts_each_work_it_joins() {
        let (measured, done) = compacted_over_a_shell("✻ Worked for 23s");
        let footer = measured
            .iter()
            .rposition(|r| r.starts_with("  ⏵⏵ bypass permissions on"))
            .expect("the mode row");
        let pill = |done_row: &str, mode_row: &str| {
            let mut rows = measured.clone();
            rows[done] = format!("✻ Worked for 23s · done 4:57 PM · {done_row}");
            rows[footer] = format!("  ⏵⏵ bypass permissions on · {mode_row}");
            rows
        };
        let both = pill("1 shell, 1 monitor still running", "1 shell, 1 monitor");
        assert_eq!(
            busy_signal(&both).map(|b| (b.zone, b.rule, b.soft)),
            Some((Zone::Footer, "a shell running", false))
        );
        for (name, rows, rule) in [
            ("a shell and a monitor", both, "a shell running"),
            (
                "the 2.1.316 counts",
                pill(
                    "13 shells, 3 monitors still running",
                    "13 shells, 3 monitors",
                ),
                "a shell running",
            ),
            (
                "a monitor joined after other work",
                pill("1 monitor still running", "1 task, 1 monitor"),
                "a monitor running",
            ),
        ] {
            assert_eq!(work_running(&rows), Some(rule), "{name}");
            assert!(turn_over_background(&rows), "{name}");
            assert_eq!(work_left(&rows), Some(rule), "{name}");
        }
        let own = pill("1 agent, 1 workflow still running", "1 agent, 1 workflow");
        assert_eq!(
            busy_signal(&own).map(|b| b.rule),
            Some("background work running")
        );
        assert_eq!(shell_or_monitor_left(&own), None);
        assert!(!turn_over_background(&own));
    }

    /// A COMPACTION UNDER ANOTHER IS NO TURN EITHER: a second idle compaction drawn under the first
    /// (with or without its own restored task) leaves the done row above both the turn's end, the
    /// shell it names the work the turn left, and the agent's reply its last words. The control:
    /// a reply between the two notices is a turn, and the first notice no longer follows the done
    /// row's.
    #[test]
    fn a_second_idle_compaction_leaves_the_same_turn_over() {
        let (measured, notice) = compacted_over_a_shell("⏺ Compacted while idle");
        let task =
            "⏺ Task \"Run lint and cross cells on the landed tree\" still running in background";
        let second = "⏺ Compacted while idle, before the prompt cache expired";
        let under = |rows: &[&str]| {
            let mut r = measured.clone();
            let at = notice + 1;
            r.splice(at..at, rows.iter().map(|row| (*row).to_string()));
            r
        };
        for rows in [
            under(&["", second]),
            under(&[
                "",
                task,
                "  ⎿  Skills restored (workflow-authoring)",
                "",
                second,
            ]),
        ] {
            assert!(crate::turn::idle_compacted(&rows));
            assert!(turn_over(&rows) && turn_over_background(&rows), "{rows:#?}");
            assert_eq!(work_left(&rows), Some("a shell running"));
            assert!(
                crate::turn::said_tail(&rows)
                    .is_some_and(|said| said.starts_with("Lint and the cross-OS")),
                "{:?}",
                crate::turn::said_tail(&rows)
            );
        }
        let replied = under(&["", "⏺ Nothing is left.", "", second]);
        assert!(!turn_over(&replied));
        assert_eq!(work_left(&replied), None);
    }

    /// THE WORK A RESTART IN PLACE WOULD END IS READ ONLY WHERE THE SCREEN NAMES IT LIVE
    /// ([`work_running`]): a workflow Claude Code waits on, a background agent a done row names
    /// still running, a shell or a monitor the turn left; never the progress row a workflow
    /// leaves drawn at its full bar once it ended, which [`background_wait`] still reads as a
    /// workflow running (measured on 2.1.304), nor a screen that names no work.
    #[test]
    fn the_work_a_restart_would_end_is_read_only_where_the_screen_names_it_live() {
        let outlived = crate::prompt::fixtures::screen(include_str!(
            "fixtures/claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt"
        ));
        assert_eq!(background_wait(&outlived), Some("a workflow running"));
        assert_eq!(
            work_running(&outlived),
            None,
            "a progress row outlives its work"
        );
        let done_at = outlived
            .iter()
            .position(|r| r.starts_with("✻ Brewed for 3m 20s"))
            .expect("the done row");
        let mut monitored = outlived.clone();
        monitored[done_at].push_str(" · 1 monitor still running");
        assert_eq!(
            work_running(&monitored),
            Some("a monitor still running"),
            "a monitor its done row names, under a progress row its workflow outlived"
        );
        let footer_at = outlived
            .iter()
            .position(|r| r.starts_with("  ⏵⏵ bypass permissions on"))
            .expect("the footer");
        let mut counted = outlived.clone();
        counted[footer_at] =
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · 1 monitor".to_string();
        assert_eq!(
            work_running(&counted),
            Some("a monitor running"),
            "a monitor its footer counts, under a progress row its workflow outlived"
        );
        let workflow = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert_eq!(
            work_running(&workflow),
            Some("waiting for a dynamic workflow")
        );
        let shell = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert_eq!(work_running(&shell), Some("a shell still running"));
        let done = "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running";
        let at = shell.iter().position(|r| r == done).expect("the done row");
        let mut monitor = with_footer(shell.clone(), IDLE_AUTO_FOOTER);
        monitor[at] = "✻ Crunched for 9m 27s · done 11:07 AM · 1 monitor still running".to_string();
        assert_eq!(work_running(&monitor), Some("a monitor still running"));
        let mut agents = shell.clone();
        agents[at] =
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell, 1 agent still running".to_string();
        assert_eq!(
            work_running(&agents),
            Some("background work still running"),
            "work of its own process, ahead of the shell"
        );
        let (compacted, _) = compacted_over_a_shell("✻ Worked for 23s");
        let footer = compacted
            .iter()
            .rposition(|r| r.contains("· 1 shell"))
            .expect("the footer");
        assert_eq!(work_running(&compacted), Some("a shell running"));
        let mut ended = compacted.clone();
        ended[footer] = "  ⏵⏵ bypass permissions on".to_string();
        assert_eq!(work_running(&ended), None, "the shell ended");
    }

    /// WHAT THE TURN LEFT IS NAMED ONLY WHERE THE SCREEN NAMES IT: a shell or a monitor under a
    /// done row, drawn live or left above an idle compaction, whichever the composer holds.
    /// Every other wait keeps its `None`, and with it the title as its witness: work of Claude
    /// Code's own process (the title runs over it), a progress row that outlives its workflow,
    /// no turn's end drawn, an active `/goal`'s check-ins, a wall, and a frozen row's claim the
    /// live footer no longer backs. Work of Claude Code's own process a done row names above the
    /// compaction is still read as that work ([`in_process_work`]), which a restart would end.
    #[test]
    fn the_work_a_turn_left_is_named_only_where_the_screen_names_it() {
        let (measured, done) = compacted_over_a_shell("✻ Worked for 23s");
        assert_eq!(work_left(&measured), Some("a shell running"));
        let caret = composer_index(&measured).expect("the caret");
        let mut empty = measured.clone();
        empty[caret] = "❯".to_string();
        assert_eq!(
            work_left(&empty),
            Some("a shell running"),
            "no composer text"
        );

        assert_eq!(
            in_process_work(&measured),
            None,
            "a shell is the process table's"
        );
        let row = "✻ Worked for 23s · done 4:57 PM · 1 shell, 2 agents still running";
        let mut agents = measured.clone();
        agents[done] = row.to_string();
        assert!(turn_over_background(&agents));
        assert_eq!(work_left(&agents), None, "agents left under a compaction");
        assert_eq!(
            in_process_work(&agents),
            Some("background work still running"),
            "a restart would end the agents the done row names"
        );
        let mut waited = measured.clone();
        waited[done] = "✻ Waiting for 1 background agent to finish".to_string();
        assert!(background_rest(&waited).is_some());
        assert_eq!(work_left(&waited), None, "a wait left under a compaction");
        let footer = measured
            .iter()
            .rposition(|r| r.contains("· 1 shell"))
            .expect("the footer");
        let mut ended = measured.clone();
        ended[footer] = "  ⏵⏵ bypass permissions on".to_string();
        assert!(turn_over(&ended));
        assert_eq!(work_left(&ended), None, "the shell ended, the row frozen");
        let mut goal = measured.clone();
        goal[footer] = format!(
            "{:<130}◎ /goal active (1d)",
            "  ⏵⏵ bypass permissions on · 1 shell"
        );
        assert!(turn_over_background(&goal) && crate::turn::goal_active(&goal));
        assert_eq!(
            work_left(&goal),
            None,
            "a goal runs on by its own check-ins"
        );

        let shell = waiter_capture(include_str!("fixtures/wait_bg4.out"));
        assert_eq!(work_left(&shell), background_wait(&shell));
        assert!(work_left(&shell).is_some());
        let done = "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running";
        let at = shell.iter().position(|r| r == done).expect("the done row");
        let mut monitor = with_footer(shell.clone(), IDLE_AUTO_FOOTER);
        monitor[at] = "✻ Crunched for 9m 27s · done 11:07 AM · 1 monitor still running".to_string();
        assert_eq!(work_left(&monitor), Some("a monitor still running"));
        let mut waiting = shell.clone();
        waiting[at] = "✻ Waiting for 1 background agent to finish".to_string();
        assert!(background_rest(&waiting).is_some());
        assert_eq!(work_left(&waiting), None, "a wait with no done row");
        let mut agents = shell.clone();
        agents[at] =
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell, 1 agent still running".to_string();
        assert!(background_rest(&agents).is_some());
        assert_eq!(work_left(&agents), None, "work of its own process");
        let mut bare = shell.clone();
        bare[at] = String::new();
        assert!(background_rest(&bare).is_some());
        assert_eq!(work_left(&bare), None, "no turn's end drawn");
        let workflow = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert!(background_wait(&workflow).is_some());
        assert_eq!(work_left(&workflow), None);
        let outlived = crate::prompt::fixtures::screen(include_str!(
            "fixtures/claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt"
        ));
        assert_eq!(background_wait(&outlived), Some("a workflow running"));
        assert!(turn_over(&outlived));
        assert_eq!(
            work_left(&outlived),
            None,
            "a progress row outlives its work"
        );
    }

    /// WHAT A TURN LEFT IS READ WHEREVER THE SCREEN NAMES IT, WHATEVER RULE COMES FIRST
    /// ([`turn_over_background`], [`work_left`], both by [`shell_or_monitor_left`]): a monitor
    /// still running, named by the done row or counted by the footer, under a workflow's progress
    /// row its workflow outlived (2.1.304 leaves it drawn at its full bar,
    /// `claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`), whose footer rule
    /// [`busy_signal`] reads ahead of a monitor's in either place. Both read that reader's one
    /// rule before, `a workflow running`, so the turn read as over no shell or monitor: the
    /// host's fact owed at such a break was never offered there, and the stall canary named it a
    /// screen this build does not read. The progress row alone names nothing the turn left.
    #[test]
    fn what_a_turn_left_under_a_progress_row_its_workflow_outlived_is_read_where_named() {
        let outlived = crate::prompt::fixtures::screen(include_str!(
            "fixtures/claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt"
        ));
        assert!(
            !turn_over_background(&outlived),
            "nothing the turn left runs"
        );
        assert_eq!(work_left(&outlived), None);
        let done_at = outlived
            .iter()
            .position(|r| r.starts_with("✻ Brewed for 3m 20s"))
            .expect("the done row");
        let footer_at = outlived
            .iter()
            .position(|r| r.starts_with("  ⏵⏵ bypass permissions on"))
            .expect("the footer");
        let mut monitored = outlived.clone();
        monitored[done_at].push_str(" · 1 monitor still running");
        let mut counted = outlived.clone();
        counted[footer_at] =
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · 1 monitor".to_string();
        for (name, rows, rule) in [
            (
                "the done row's monitor",
                &monitored,
                "a monitor still running",
            ),
            ("the footer's monitor", &counted, "a monitor running"),
        ] {
            assert_eq!(
                background_wait(rows),
                Some("a workflow running"),
                "{name}: the reader's one rule"
            );
            assert!(turn_over_background(rows), "{name}");
            assert_eq!(work_left(rows), Some(rule), "{name}");
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

    /// The screen of a live Claude Code 2.1.310 session whose turn ended (its transcript wrote
    /// `turn_duration` with `pendingWorkflowCount: 3`) while three dynamic workflows it started run
    /// on, captured read-only from the tab the cruise stall of 2026-10-24..27 was measured on: the
    /// phase stays busy (the work runs on and wakes the agent), and the turn's end rests idle.
    #[test]
    fn claude_2_1_310_turn_over_under_its_workflows_rests_idle() {
        use crate::prompt::fixtures::WAITING_WORKFLOWS_2_1_310 as TEXT;
        use crate::reader::{Program, identify};
        let r = crate::prompt::fixtures::screen(TEXT);
        let waiting = "✻ Waiting for 3 dynamic workflows to finish";
        let at = r
            .iter()
            .position(|x| x.starts_with(waiting))
            .expect("the waiting row");
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(
            signal(&r).as_deref(),
            Some("status row: waiting for a dynamic workflow")
        );
        assert!(background_only(&r));
        assert_eq!(background_rest(&r), Some(Phase::Idle));
        for reader in [identify(Some("claude"), &r), identify(Some("node"), &r)] {
            assert_eq!(reader.program(), Program::Claude);
            assert_eq!(reader.phase(&r), Phase::Busy, "the supervisor's break");
            assert_eq!(reader.background_rest(&r), Some(Phase::Idle));
        }

        // NEGATIVE CONTROLS: the same screen with a live turn on it is no turn end, whatever
        // background work its pills count.
        let row = |text: &str| {
            let mut s = r.clone();
            let (head, panel) = r[at].split_once('│').expect("the panel's rule");
            s[at] = format!("{text:<w$}│{panel}", w = head.chars().count());
            s
        };
        for live in [
            "· Wibbling… (4m 36s · ↓ 16.4k tokens)",
            "✢ Running… (12s · esc to interrupt)",
        ] {
            let busy = row(live);
            assert_eq!(worker_phase(&busy), Phase::Busy, "{live}");
            assert!(!background_only(&busy), "{live}");
            assert_eq!(background_rest(&busy), None, "{live}: a live turn");
            assert_eq!(identify(Some("claude"), &busy).background_rest(&busy), None);
        }
        // A message submitted under the wait opens the next turn.
        let top = composer_frame(&r).expect("the frame").top;
        let mut submitted = r.clone();
        submitted.insert(top, "❯ keep going".to_string());
        submitted.remove(0);
        assert_eq!(worker_phase(&submitted), Phase::Busy);
        assert_eq!(background_rest(&submitted), None, "the next turn has begun");
        // No other reader reads Claude Code's turn end.
        for program in [
            Program::Codex,
            Program::Agy,
            Program::Muse,
            Program::Generic,
        ] {
            assert_eq!(program.reader().background_rest(&r), None, "{program:?}");
        }
    }

    /// Two screens of the same live Claude Code 2.1.316 session, captured read-only on
    /// 2026-10-31 while aterm 0.147.0 published `agent=busy` over both: its turn over while two
    /// background agents and three dynamic workflows run on (Claude Code's agents panel drawn
    /// under the footer) rests idle, and the live turn it ran 17 minutes earlier over its own
    /// shells and agents (a spinner row) gives no rest.
    #[test]
    fn claude_2_1_316_turn_over_under_agents_and_workflows_rests_idle_and_its_live_turn_does_not() {
        use crate::prompt::fixtures::{
            INFERRING_OVER_BACKGROUND_2_1_316, WAITING_AGENTS_WORKFLOWS_2_1_316, screen,
        };
        use crate::reader::{Program, identify};
        let rest = screen(WAITING_AGENTS_WORKFLOWS_2_1_316);
        assert_eq!(
            rest.iter()
                .rposition(|x| x.starts_with('❯')
                    && x.ends_with("keep going, don't let yourself be blocked"))
                .expect("the composer's row"),
            47,
            "the cursor's row as captured"
        );
        assert_eq!(worker_phase(&rest), Phase::Busy);
        assert!(background_only(&rest));
        assert_eq!(background_rest(&rest), Some(Phase::Idle));
        let reader = identify(Some("claude"), &rest);
        assert_eq!(reader.program(), Program::Claude);
        assert_eq!(reader.phase(&rest), Phase::Busy, "the supervisor's break");
        assert_eq!(reader.background_rest(&rest), Some(Phase::Idle));

        // NEGATIVE CONTROL: the same session's live turn over its own background work.
        let live = screen(INFERRING_OVER_BACKGROUND_2_1_316);
        assert!(
            live.iter().any(|x| x.starts_with("✻ Inferring… (1m 35s")),
            "the spinner row"
        );
        assert_eq!(worker_phase(&live), Phase::Busy);
        assert!(!background_only(&live), "a live turn");
        assert_eq!(background_rest(&live), None);
        assert_eq!(identify(Some("claude"), &live).background_rest(&live), None);
    }

    /// The incident shape of a turn ended over two background shells it left running: its end
    /// rests idle, and a live turn over the same shells does not.
    #[test]
    fn a_turn_over_its_own_shells_rests_idle_and_a_live_one_does_not() {
        use crate::prompt::fixtures::IDLE_SHELLS_2_1_271 as TEXT;
        let r = crate::prompt::fixtures::screen(TEXT);
        assert_eq!(worker_phase(&r), Phase::Busy);
        assert_eq!(background_rest(&r), Some(Phase::Idle));
        let done = "✻ Cogitated for 3m 12s · done 9:29 PM · 2 shells still running";
        let at = r.iter().position(|x| x == done).expect("the done row");
        let mut footer_only = r.clone();
        footer_only[at] = "✻ Cogitated for 3m 12s · done 9:29 PM".to_string();
        assert_eq!(background_rest(&footer_only), Some(Phase::Idle));
        // Its last words a question: the turn ended asking.
        let said = r
            .iter()
            .position(|x| x.ends_with("when they notify."))
            .expect("the last said row");
        let mut asked = r.clone();
        asked[said] = asked[said].replace("when they notify.", "when they notify?");
        assert_eq!(worker_phase(&asked), Phase::Busy);
        assert_eq!(background_rest(&asked), Some(Phase::Question));
        let mut live = r.clone();
        live[at] = "✶ Pondering… (2m 5s · esc to interrupt)".to_string();
        assert_eq!(worker_phase(&live), Phase::Busy);
        assert_eq!(background_rest(&live), None, "a live turn over the shells");
        const BARE: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        let mut bare = with_footer(footer_only, BARE);
        assert_eq!(worker_phase(&bare), Phase::Idle);
        assert_eq!(
            background_rest(&bare),
            None,
            "idle already: nothing to rest"
        );
        bare.clear();
        assert_eq!(background_rest(&bare), None, "no screen");
    }

    /// Two background waits that are no turn end, as measured on Claude Code 2.1.283: a limit's
    /// notice drawn under the composer while the turn wraps up (it ended 41 minutes later), and a
    /// `/goal` that runs on through the wait by its own check-ins, its mark alone on its row or
    /// run into the update hint's (`✔ Update installed · Restart to update◎ /goal active (1d)`).
    /// Each rests once its marks are gone, so the marks are what decline it.
    #[test]
    fn a_turn_wrapping_up_or_a_goal_running_on_is_no_rest() {
        use crate::prompt::fixtures::{GOAL_ACTIVE_SUGGESTION, WRAPUP_WAITING_2_1_283, screen};
        let wrapup = screen(WRAPUP_WAITING_2_1_283);
        assert_eq!(worker_phase(&wrapup), Phase::Busy);
        assert!(background_only(&wrapup));
        assert!(
            limit_notice(&wrapup).is_some(),
            "the footer's notice is read"
        );
        assert!(
            crate::turn::goal_active(&wrapup),
            "its goal mark ends the update hint's row"
        );
        assert_eq!(background_rest(&wrapup), None, "a notice while it wraps up");
        let unnoticed: Vec<String> = wrapup
            .iter()
            .filter(|r| !r.contains("Usage limit reached"))
            .cloned()
            .collect();
        assert_eq!(worker_phase(&unnoticed), Phase::Busy);
        assert_eq!(background_rest(&unnoticed), None, "the goal declines it");
        let hint = "✔ Update installed · Restart to update";
        let unmarked = |rows: &[String]| -> Vec<String> {
            rows.iter()
                .map(|r| match r.split_once(hint) {
                    Some((lead, _)) => format!("{lead}{hint}"),
                    None => r.clone(),
                })
                .collect()
        };
        let goalless = unmarked(&wrapup);
        assert!(
            goalless.iter().any(|r| r.ends_with(hint)),
            "the update hint stays"
        );
        assert!(
            !crate::turn::goal_active(&goalless),
            "the update hint alone"
        );
        assert_eq!(background_rest(&goalless), None, "the notice declines it");
        assert_eq!(
            background_rest(&unmarked(&unnoticed)),
            Some(Phase::Idle),
            "CONTROL: the notice and the goal are what decline it"
        );

        let goal = screen(GOAL_ACTIVE_SUGGESTION);
        assert_eq!(worker_phase(&goal), Phase::Busy);
        assert!(background_only(&goal) && crate::turn::goal_active(&goal));
        assert_eq!(
            background_rest(&goal),
            None,
            "a goal runs on through its wait"
        );
        let goalless: Vec<String> = goal
            .iter()
            .map(|r| {
                if r.contains("◎ /goal active") {
                    String::new()
                } else {
                    r.clone()
                }
            })
            .collect();
        assert_eq!(worker_phase(&goalless), Phase::Busy);
        assert_eq!(
            background_rest(&goalless),
            Some(Phase::Idle),
            "CONTROL: the goal is what declines it"
        );
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
        for (row, cause, next, failed_wait) in [
            (
                "✻ Waiting for API response · will retry in 5s · check your network",
                RetryCause::Stalled,
                Some(5),
                None,
            ),
            (
                "✻ No response from the API after 30s · retrying, waiting up to 10m · attempt 1/2",
                RetryCause::NoResponse,
                None,
                Some(30),
            ),
            (
                "✶ No response from the API after 3m · retrying once, waiting up to 10m",
                RetryCause::NoResponse,
                None,
                Some(180),
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
                (r.row, r.cause, r.next_try_s, r.failed_wait_s),
                (2, cause, next, failed_wait),
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
            "✻ No response from the API after 30s · retrying, waiting up to 10m · attempt 1/2",
        )
        .expect("read");
        assert_eq!(
            r.guard,
            "^\\s*[^\\s⏺●⎿❯⏵◯]\\s+No.response.from.the.API.after.30s.·.retrying,.waiting.up.to.\
             10m.·.attempt.1/2\\s*(?:│.*)?$",
            "a no-response row stands as drawn while its retry waits: the guard holds all of it"
        );
        assert_eq!(wait_secs("1m…"), None, "a cut wait");
        assert_eq!(wait_secs("2d 3h"), Some(2 * 86_400 + 3 * 3600));
    }

    /// Claude Code 2.1.310's retry rows as the lab measured them (2026-10-24, the real binary on a
    /// local stub of the API, and for no route on an address the kernel refuses): each lead
    /// reads as its class with its countdown or the failed try's window, the no-response row with
    /// the hint the vendor wraps under it, and a no-response row's guard holds its whole text. A
    /// lead the vendor cut at a narrow width names no class, and reads as none.
    #[test]
    fn claude_code_2_1_310s_measured_retry_rows_read_by_class() {
        use crate::prompt::fixtures::{
            API_DROPPED_QUEUED_2_1_310, API_NO_RESPONSE_2_1_310, API_NO_RESPONSE_QUEUED_2_1_310,
            API_NO_RESPONSE_QUEUED_60_2_1_310, API_ROUTE_QUEUED_2_1_310,
            API_STALLED_QUEUED_2_1_310, API_UNREACHABLE_QUEUED_2_1_310, screen as measured,
        };
        for (name, text, cause, next, failed_wait, lead) in [
            (
                "no response",
                API_NO_RESPONSE_2_1_310,
                RetryCause::NoResponse,
                None,
                Some(13),
                "No response from the API after 13s · retrying once, waiting up to 10m",
            ),
            (
                "no response, queued",
                API_NO_RESPONSE_QUEUED_2_1_310,
                RetryCause::NoResponse,
                None,
                Some(13),
                "No response from the API after 13s · retrying once, waiting up to 10m",
            ),
            (
                "unreachable",
                API_UNREACHABLE_QUEUED_2_1_310,
                RetryCause::Unreachable,
                Some(19),
                None,
                "Can't reach the API server — check your internet or DNS (ENOTFOUND) · ",
            ),
            (
                "dropped",
                API_DROPPED_QUEUED_2_1_310,
                RetryCause::Unreachable,
                Some(17),
                None,
                "Connection dropped (ECONNRESET) · ",
            ),
            (
                "no route",
                API_ROUTE_QUEUED_2_1_310,
                RetryCause::Unreachable,
                Some(9),
                None,
                "No internet route — check your connection or VPN (EHOSTUNREACH) · ",
            ),
            (
                "stalled",
                API_STALLED_QUEUED_2_1_310,
                RetryCause::Stalled,
                Some(153),
                None,
                "Waiting for API response · will retry in ",
            ),
        ] {
            let rows = measured(text);
            assert_eq!(worker_phase(&rows), Phase::Busy, "{name}");
            let r = api_retry_row(&rows).unwrap_or_else(|| panic!("{name}: the retry row"));
            assert_eq!(
                (r.cause, r.next_try_s, r.failed_wait_s),
                (cause, next, failed_wait),
                "{name}"
            );
            assert!(rows[r.row].contains(&r.text), "{name}: {:?}", r.text);
            assert!(r.text.starts_with(lead), "{name}: {:?}", r.text);
        }
        let r = api_retry_row(&measured(API_NO_RESPONSE_QUEUED_2_1_310)).expect("read");
        assert_eq!(
            r.guard,
            "^\\s*[^\\s⏺●⎿❯⏵◯]\\s+No.response.from.the.API.after.13s.·.retrying.once,.waiting.up.\
             to.10m\\s*(?:│.*)?$"
        );
        let narrow = measured(API_NO_RESPONSE_QUEUED_60_2_1_310);
        assert_eq!(
            worker_phase(&narrow),
            Phase::Busy,
            "PRECONDITION: a busy read"
        );
        assert!(
            narrow
                .iter()
                .any(|row| row.contains("No response from the… · retrying once")),
            "PRECONDITION: the lead cut"
        );
        assert_eq!(api_retry_row(&narrow), None, "a cut lead names no class");
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

    /// A WAIT CLAUDE CODE CARRIES ABOVE ITS OWN WAKES ENDS WITH THE WORKFLOWS IT WAITS FOR: its
    /// `✻ Waiting for N dynamic workflow(s) to finish` stays the first glyph row above the
    /// composer where no done row is drawn after it (`showTurnDuration` off), and once a
    /// `⏺ Dynamic workflow "…" completed` row is drawn under it for each workflow it waits for
    /// and each one launched under it (`⏺ Workflow(…)`), nothing it names still runs (measured on
    /// 2.1.304: `claude-2.1.304-agents-panel-workflow-done-3m20s-120col.txt`, and the walled wake of
    /// `idle-after-limit-and-model-switch.txt`, each with its done row after it). The screen then
    /// reads no work running, at an idle point and under a limit's notice its wake drew alike;
    /// before, the stale wait read `background work still running` there for as long as it stood
    /// the first glyph row, and held every restart in place but the memory one. A wait for more
    /// workflows than completed, one a launch under it renews, and a wait for a background agent
    /// (whose completion row is unmeasured) still stand.
    #[test]
    fn a_wait_carried_above_its_wakes_ends_with_the_workflows_it_waits_for() {
        const LEFT: &str = "background work still running";
        let footer = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        let completed = "⏺ Dynamic workflow \"Second review pass\" completed · 5s";
        let carried = |waiting: &str, under: &[&str]| {
            let mut body = vec![waiting, ""];
            body.extend_from_slice(under);
            body.push("");
            screen(&body, footer)
        };
        let one = "✻ Waiting for 1 dynamic workflow to finish";
        let ended = carried(one, &[completed, "", "⏺ The review is in; nothing to fix."]);
        assert_eq!(busy_signal(&ended), None, "{ended:#?}");
        assert_eq!(in_process_work(&ended), None);
        assert_eq!(work_running(&ended), None);
        assert_eq!(worker_phase(&ended), Phase::Idle);
        let walled = carried(
            one,
            &[
                completed,
                "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
            ],
        );
        assert!(
            matches!(worker_phase(&walled), Phase::Limited { .. }),
            "{walled:#?}"
        );
        assert_eq!(
            in_process_work(&walled),
            None,
            "a walled wake ended the wait"
        );
        assert_eq!(work_running(&walled), None);
        for (name, waiting, under) in [
            (
                "a monitor's wake",
                one,
                vec!["⏺ Monitor event: \"lander\"", "", "⏺ Restarted it."],
            ),
            (
                "two waited, one completed",
                "✻ Waiting for 2 dynamic workflows to finish",
                vec![completed, "", "⏺ One pass is in."],
            ),
            (
                "a launch under it",
                one,
                vec![
                    "⏺ Workflow(Third review pass)",
                    "  ⎿  /workflows to view dynamic workflow runs",
                    "",
                    completed,
                    "",
                    "⏺ One pass is in.",
                ],
            ),
            (
                "an agent waited",
                "✻ Waiting for 1 background agent to finish",
                vec![completed, "", "⏺ The review is in."],
            ),
            (
                "an agent and a workflow waited",
                "✻ Waiting for 1 background agent and 1 dynamic workflow to finish",
                vec![completed, "", "⏺ The review is in."],
            ),
        ] {
            let r = carried(waiting, &under);
            assert_eq!(in_process_work(&r), Some(LEFT), "{name}: {r:#?}");
            assert_eq!(work_running(&r), Some(LEFT), "{name}");
            assert!(carried_wait_alone(&r), "{name}");
        }
    }

    /// A COMPLETED ROW WHOSE TITLE WRAPS STILL ENDS THE WAIT IT ANSWERS: Claude Code wraps a long
    /// `⏺ Dynamic workflow "…" completed` row at column 0, not at its text's column (measured on
    /// 2.1.302 at 175 columns, five rows of `claude-2.1.302-login-handoff-over-weekly-limit.txt` and
    /// `claude-2.1.302-login-interrupted-over-weekly-limit-continuing-shortly.txt`, and kept so in
    /// the retitled `claude-2.1.283-wall-session-goal-checkin-shell-measured.txt`), and
    /// [`waited_out`] read only the rows indented under it, so a workflow whose title wrapped was
    /// never counted as ended. Where no done row follows (`showTurnDuration` off), the wait carried
    /// above it then stood as work for as long as its row did, and held a lost line's restart and
    /// every restart but the memory one. Every measured wrapped completion now counts. The screen
    /// read whole is CONSTRUCTED from the measured rows: 2.1.302's from `✻ Waiting for 2 dynamic
    /// workflows to finish` through its second, wrapped completion, then a reply and a composer
    /// (the capture has a `/login` dialog in the composer's place and a done row under them). The
    /// controls: the wrapped completion cut to its first row (no ` completed` drawn) and a wait for
    /// one more workflow than completed still wait.
    #[test]
    fn a_completed_row_whose_title_wraps_still_ends_the_wait_it_answers() {
        use crate::prompt::fixtures::{
            LOGIN_HANDOFF_OVER_WEEKLY_LIMIT_2_1_302, LOGIN_INTERRUPTED_OVER_WEEKLY_LIMIT_2_1_302,
            screen as captured,
        };
        const LEFT: &str = "background work still running";
        let mut wraps = 0;
        for fixture in [
            LOGIN_HANDOFF_OVER_WEEKLY_LIMIT_2_1_302,
            LOGIN_INTERRUPTED_OVER_WEEKLY_LIMIT_2_1_302,
            include_str!("fixtures/claude-2.1.283-wall-session-goal-checkin-shell-measured.txt"),
        ] {
            let measured = captured(fixture);
            for (i, row) in measured.iter().enumerate() {
                let wrapped = row.starts_with("⏺ Dynamic workflow \"")
                    && !row.contains("\" completed")
                    && measured.get(i + 1).is_some_and(|next| {
                        next.contains("\" completed") && !next.starts_with(' ')
                    });
                if wrapped {
                    wraps += 1;
                    let one = "✻ Waiting for 1 dynamic workflow to finish";
                    assert!(waited_out(one, &measured, i, i + 2), "{row}");
                    assert!(
                        !waited_out(one, &measured, i, i + 1),
                        "cut to its first row: {row}"
                    );
                }
            }
        }
        assert_eq!(wraps, 6, "the measured column-0 wraps");

        let measured = captured(LOGIN_HANDOFF_OVER_WEEKLY_LIMIT_2_1_302);
        let from = measured
            .iter()
            .position(|r| r == "✻ Waiting for 2 dynamic workflows to finish")
            .expect("the wait");
        let wrapped = from
            + measured[from..]
                .iter()
                .position(|r| r.starts_with("⏺ Dynamic workflow \"Swap fix round 5"))
                .expect("the wrapped completion");
        let width = crate::question::screen_width(&measured);
        let carried = |waiting: &str, last: usize| {
            let mut r = vec![waiting.to_string()];
            r.extend_from_slice(&measured[from + 1..=last]);
            r.extend(rows(&[
                "",
                "⏺ Both lanes are in; nothing else is pending.",
                "",
            ]));
            r.extend([
                "─".repeat(width),
                "❯".to_string(),
                "─".repeat(width),
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string(),
            ]);
            r
        };
        let ended = carried(&measured[from], wrapped + 1);
        assert_eq!(busy_signal(&ended), None, "{ended:#?}");
        assert_eq!(in_process_work(&ended), None);
        assert_eq!(work_running(&ended), None);
        assert_eq!(worker_phase(&ended), Phase::Idle);
        for (name, r) in [
            ("cut to its first row", carried(&measured[from], wrapped)),
            (
                "one more waited",
                carried("✻ Waiting for 3 dynamic workflows to finish", wrapped + 1),
            ),
        ] {
            assert_eq!(in_process_work(&r), Some(LEFT), "{name}: {r:#?}");
            assert_eq!(work_running(&r), Some(LEFT), "{name}");
            assert!(carried_wait_alone(&r), "{name}");
        }
    }

    /// A WAIT FOR A BACKGROUND AGENT STANDS OVER AN AGENT'S FINISHED ROW ITS COUNT DOES NOT NAME
    /// (measured on 2.1.304, `claude-2.1.304-agents-panel-long-bash-4m57s-120col.txt`): a
    /// background agent's completion row is measured (`⏺ Agent "…" finished · 3m 20s`, the wake
    /// it opens a turn with, as a workflow's `completed` row is), but it does not answer the wait's
    /// count. There, `Waiting for 1 background agent to finish` is carried above the wake of
    /// `bg probe`, an agent the count had already let go when it said it waits on its own
    /// background work, while `long probe`, the one the wait counts, still runs (the agents panel
    /// draws it at `4m 57s`). One finished row under a wait for one agent is no proof that the
    /// agent it waits for ended, so the wait stays work a restart in place would end, unlike a
    /// wait for workflows alone ([`waited_out`]).
    #[test]
    fn a_wait_for_a_background_agent_stands_over_a_finished_row_its_count_does_not_name() {
        let measured = crate::prompt::fixtures::screen(include_str!(
            "fixtures/claude-2.1.304-agents-panel-long-bash-4m57s-120col.txt"
        ));
        assert!(
            measured
                .iter()
                .any(|r| r.starts_with("⏺ Agent \"bg probe\" finished")),
            "the finished row"
        );
        assert!(
            measured
                .iter()
                .any(|r| r.starts_with("  ◯ general-purpose  long probe")),
            "the agent the wait counts, still running"
        );
        const LEFT: &str = "background work still running";
        assert_eq!(in_process_work(&measured), Some(LEFT), "{measured:#?}");
        assert_eq!(work_running(&measured), Some(LEFT));
        assert!(carried_wait_alone(&measured));
    }

    /// [`in_process_work`] reads the work Claude Code runs in its own process off its status
    /// row's own words, whatever rule [`busy_signal`] reads first: a workflow's progress row and
    /// a shell count under the footer come ahead of the wait carried above a wake and of a done
    /// row's `… still running`. Shells and monitors (the process table's), a progress row alone,
    /// a message submitted under the wait and a screen with no composer are none.
    #[test]
    fn in_process_work_is_read_whatever_rule_comes_first() {
        const RUNNING: &str = "  ◯ review-pass  ▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱  1/2 · 3m02s · ↓ 12.4k tokens";
        const SHELL: &str = "  ⏵⏵ bypass permissions on · 1 shell · ← for agents · ↓ to manage";
        const BARE: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents";
        const LEFT: &str = "background work still running";
        let row = |mut r: Vec<String>| {
            r.push(RUNNING.to_string());
            r
        };
        let carried = |footer: &str| {
            screen(
                &[
                    "✻ Waiting for 1 dynamic workflow to finish",
                    "",
                    "⏺ Monitor event: \"lander\"",
                    "",
                    "⏺ Restarted it.",
                    "",
                ],
                footer,
            )
        };
        let done = |status: &str, footer: &str| screen(&["⏺ Done.", "", status, ""], footer);
        let agents = "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents still running";
        let mixed = "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents, 1 shell still running";
        for (name, r, first) in [
            ("carried", carried(BARE), LEFT),
            (
                "carried, workflow row",
                row(carried(BARE)),
                "a workflow running",
            ),
            ("carried, shell count", carried(SHELL), "a shell running"),
            ("done", done(agents, BARE), LEFT),
            (
                "done, workflow row",
                row(done(agents, BARE)),
                "a workflow running",
            ),
            ("done, shell count", done(agents, SHELL), "a shell running"),
            (
                "done, a shell with them",
                done(mixed, BARE),
                "a shell still running",
            ),
        ] {
            assert_eq!(background_wait(&r), Some(first), "{name}");
            assert_eq!(in_process_work(&r), Some(LEFT), "{name}");
        }
        let waiting = row(done("✻ Waiting for 1 dynamic workflow to finish", SHELL));
        assert_eq!(
            in_process_work(&waiting),
            Some("waiting for a dynamic workflow"),
            "the status row's own wait"
        );
        for status in [
            "✻ Brewed for 6m 7s · done 10:07 AM · 2 shells still running",
            "✻ Brewed for 6m 7s · done 10:07 AM · 1 shell, 1 monitor still running",
            "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents done",
            "✻ Brewed for 6m 7s · done 10:07 AM",
        ] {
            let r = row(done(status, SHELL));
            assert!(background_wait(&r).is_some(), "{status}");
            assert_eq!(in_process_work(&r), None, "{status}");
        }
        let bg = waiter_capture(include_str!("fixtures/wait_bg.out"));
        assert_eq!(in_process_work(&bg), Some("waiting for a dynamic workflow"));
        let top = composer_frame(&bg).expect("the frame").top;
        let mut typed = bg.clone();
        typed.insert(top, "❯ keep going".to_string());
        typed.remove(0);
        assert_eq!(
            in_process_work(&typed),
            None,
            "a message submitted under it"
        );
        let bare = rows(&["✻ Waiting for 1 dynamic workflow to finish", "$ "]);
        assert_eq!(in_process_work(&bare), None, "no composer");
    }

    /// Claude Code 2.1.310 and later draw a side panel to the right of the live zone: each row is
    /// padded out to the panel's rule `│`, and the panel's own text follows it (the measured
    /// 2.1.316 screen, whose diff panel heads its column with `1 file changed`). A done row's
    /// `… still running` is read up to that rule, by [`busy_signal`]'s soft rule and by
    /// [`in_process_work`] alike, and the panel's text is no item of the done row.
    #[test]
    fn a_done_row_beside_the_side_panel_is_read_up_to_its_rule() {
        use crate::prompt::fixtures::{WAITING_AGENTS_WORKFLOWS_2_1_316, screen as captured};
        const LEFT: &str = "background work still running";
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        const DIFF: &str = "1 file changed                                source: Current ▾";
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let at = panel
            .iter()
            .rposition(|x| x.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let bottom = composer_frame(&panel).expect("the frame").bottom;
        let (head, side) = panel[at].split_once('│').expect("the panel's rule");
        assert_eq!(side, "", "the waiting row's panel text as captured");
        let width = head.chars().count();
        // The status row drawn beside the panel, `side` after the rule; `quiet` swaps the
        // captured footer (its mode row's shell count, then its agents panel's workflow rows,
        // rules of their own that read first) for a bare one, so the status row is all that
        // reads.
        let beside = |text: &str, side: &str, quiet: bool| {
            let mut s = panel.clone();
            s[at] = format!("{text:<width$}│{side}");
            if quiet {
                s.truncate(bottom + 1);
                s.extend([String::new(), QUIET.to_string()]);
            }
            s
        };
        assert_eq!(
            in_process_work(&beside(
                "✻ Waiting for 2 background agents to finish",
                "",
                false
            )),
            Some("waiting for a background agent"),
            "the status row's own wait, beside the panel"
        );
        let agents = "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents still running";
        let mixed = "✻ Brewed for 6m 7s · done 10:07 AM · 2 agents, 1 shell still running";
        for side in ["", DIFF] {
            for (status, full_first, quiet_first) in [
                (agents, "a shell running", LEFT),
                (mixed, "a shell still running", "a shell still running"),
            ] {
                let full = beside(status, side, false);
                assert_eq!(background_wait(&full), Some(full_first), "{status} │{side}");
                assert_eq!(in_process_work(&full), Some(LEFT), "{status} │{side}");
                let quiet = beside(status, side, true);
                assert_eq!(
                    background_wait(&quiet),
                    Some(quiet_first),
                    "{status} │{side}"
                );
                assert_eq!(worker_phase(&quiet), Phase::Busy, "{status} │{side}");
                assert_eq!(in_process_work(&quiet), Some(LEFT), "{status} │{side}");
            }
        }
        // CONTROLS: shells alone are the process table's, a done row with nothing left running
        // rests, and the panel's text is never the done row's, whatever it says.
        let shells = beside(
            "✻ Brewed for 6m 7s · done 10:07 AM · 2 shells still running",
            "",
            false,
        );
        assert_eq!(in_process_work(&shells), None, "shells alone");
        for side in ["", DIFF, " · 2 agents still running"] {
            let done = beside("✻ Brewed for 6m 7s · done 10:07 AM", side, true);
            assert_eq!(background_wait(&done), None, "│{side}");
            assert_eq!(worker_phase(&done), Phase::Idle, "│{side}");
            assert_eq!(in_process_work(&done), None, "│{side}");
        }
    }

    /// THE SIDE PANEL'S TEXT IS NO WORD OF THE STATUS LINE, WHATEVER IT SAYS: the status line is
    /// read up to the panel's rule `│` ([`StatusBlock::line`]), as a done row's items were alone
    /// before, so no reader of it (the status row's busy rules, its shells and monitors still
    /// running, its `Waiting for …`, its shell count) reads the panel's text as the agent's. In
    /// every measured panel capture (2.1.310, two of 2.1.316) the panel draws its text beside the
    /// upper rows of the live zone and none beside the status row, so these screens are
    /// CONSTRUCTED from the measured 2.1.316 one: its status row replaced by a done row padded to
    /// the measured rule, the panel's text after it, and its footer bare. Read whole before, text
    /// there that said `2 shells still running` or `Waiting for 1 dynamic workflow` named work that
    /// held a lost line's restart while it stood, and an ellipsis read a live spinner, a turn
    /// running with no point. The wait carried above a wake reads its line the same way: an
    /// ellipsis beside it hid the wait, and the work it names, from every reader. CONTROL: the
    /// panel with no text beside the done row rests, and the carried wait beside none waits.
    #[test]
    fn the_side_panels_text_is_no_word_of_the_status_line() {
        use crate::prompt::fixtures::{WAITING_AGENTS_WORKFLOWS_2_1_316, screen as captured};
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        const DONE: &str = "✻ Worked for 3m 20s · done 4:57 PM";
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let at = panel
            .iter()
            .rposition(|x| x.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let (head, side) = panel[at].split_once('│').expect("the panel's rule");
        assert_eq!(side, "", "the status row's panel text as captured");
        let width = head.chars().count();
        let pad = |text: &str, side: &str| format!("{text:<width$}│{side}");
        let quiet = |mut s: Vec<String>| {
            let frame = composer_frame(&s).expect("the frame");
            s[frame.top + 1] = "❯".to_string();
            s.truncate(frame.bottom + 1);
            s.extend([String::new(), QUIET.to_string()]);
            s
        };
        let beside = |side: &str| {
            let mut s = panel.clone();
            s[at] = pad(DONE, side);
            quiet(s)
        };
        let rested = beside("");
        assert_eq!(busy_signal(&rested), None, "{rested:#?}");
        assert_eq!(worker_phase(&rested), Phase::Idle);
        assert_eq!(work_running(&rested), None);
        assert_eq!(shells_running(&rested), Some(0));
        for side in [
            "  turn left 2 shells still running under it",
            "  Waiting for 1 dynamic workflow to finish",
            "  the build is still going… check back later",
            "   · 5 shells · 1 monitor still running",
        ] {
            let r = beside(side);
            assert_eq!(status_line(&r).as_deref(), Some(DONE), "│{side}");
            assert_eq!(busy_signal(&r), None, "│{side}");
            assert_eq!(worker_phase(&r), Phase::Idle, "│{side}");
            assert_eq!(in_process_work(&r), None, "│{side}");
            assert_eq!(work_running(&r), None, "│{side}");
            assert_eq!(shells_running(&r), Some(0), "│{side}");
        }

        let carried = |side: &str| {
            let mut s = panel[..at].to_vec();
            s.extend([
                pad("✻ Waiting for 1 dynamic workflow to finish", side),
                pad("", ""),
                pad("⏺ Monitor event: \"lander\"", ""),
                pad("", ""),
                pad("⏺ Restarted it.", ""),
            ]);
            s.extend_from_slice(&panel[at + 1..]);
            quiet(s)
        };
        for side in ["", "  the build is still going… check back later"] {
            let r = carried(side);
            assert_eq!(
                in_process_work(&r),
                Some("background work still running"),
                "│{side}: {r:#?}"
            );
            assert_eq!(work_running(&r), Some("background work still running"));
            assert!(carried_wait_alone(&r), "│{side}");
        }
    }

    /// A `│` OF THE STATUS LINE'S OWN WORDS IS ONE OF THEM: the status line ends at the side
    /// panel's rule, the `│` drawn at the column the rows beside the line draw theirs
    /// ([`panel_rule`]), never at the first `│` it holds. The spinner row draws an in-progress
    /// todo's words, the model's text, so one may stand there; no measured capture draws one, so
    /// these screens are CONSTRUCTED from the measured 2.1.316 live turn: its spinner row's words
    /// replaced by ones holding a `│`, its footer bare, with its side panel as captured, with the
    /// panel's text beside the spinner row, and with every row cut at the panel's rule (no panel).
    /// Cut at its first `│` (found in the review of the stack review's fixes, 2026-11-04), the
    /// line read `✻ Fixing the`, a done row, so the live turn read as over and idle, the panel
    /// drawn or not. CONTROL: the measured spinner row, the panel beside it, reads up to the rule.
    #[test]
    fn a_rule_in_the_status_lines_own_words_is_one_of_them() {
        use crate::prompt::fixtures::{INFERRING_OVER_BACKGROUND_2_1_316, screen as captured};
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        const SPINNER: &str = "✻ Inferring… (1m 35s · ↓ 2.9k tokens)";
        const OWN: &str = "✻ Fixing the │ rule in the panel… (1m 35s · ↓ 2.9k tokens)";
        let live = captured(INFERRING_OVER_BACKGROUND_2_1_316);
        let at = live
            .iter()
            .position(|x| x.starts_with(SPINNER))
            .expect("the spinner row");
        let (head, side) = live[at].split_once('│').expect("the panel's rule");
        assert_eq!(side, "", "the spinner row's panel text as captured");
        let width = head.chars().count();
        let quiet = |mut s: Vec<String>| {
            let frame = composer_frame(&s).expect("the frame");
            s.truncate(frame.bottom + 1);
            s.extend([String::new(), QUIET.to_string()]);
            s
        };
        let measured = quiet(live.clone());
        assert_eq!(status_line(&measured).as_deref(), Some(SPINNER));
        assert_eq!(signal(&measured).as_deref(), Some("status row: spinner"));
        assert_eq!(worker_phase(&measured), Phase::Busy);

        let beside = |side: &str| {
            let mut s = live.clone();
            s[at] = format!("{OWN:<width$}│{side}");
            quiet(s)
        };
        let mut alone: Vec<String> = live
            .iter()
            .map(|r| {
                r.split_once('│')
                    .map_or(r.as_str(), |(left, _)| crate::text::trim_end(left))
                    .to_string()
            })
            .collect();
        alone[at] = OWN.to_string();
        for (name, r) in [
            ("no panel", quiet(alone)),
            ("beside the panel", beside("")),
            (
                "beside the panel's text",
                beside("  turn left 2 shells still running"),
            ),
        ] {
            assert_eq!(worker_phase(&r), Phase::Busy, "{name}");
            assert!(!done_row_drawn(&r), "{name}");
            assert_eq!(signal(&r).as_deref(), Some("status row: spinner"), "{name}");
            assert_eq!(status_line(&r).as_deref(), Some(OWN), "{name}: {r:#?}");
        }
    }

    /// THE STATUS ROW BESIDE THE SIDE PANEL IS READ UP TO ITS RULE, as the status line is
    /// ([`panel_cut`]): every measured panel capture (2.1.310's, both of 2.1.316's) pads its status
    /// row out to the panel's rule `│`, and [`status_row`] read the row whole, its padding and the
    /// rule with it, and the panel's text after it wherever the panel draws some there.
    #[test]
    fn the_status_row_beside_the_side_panel_is_read_up_to_its_rule() {
        use crate::prompt::fixtures::{
            INFERRING_OVER_BACKGROUND_2_1_316, WAITING_AGENTS_WORKFLOWS_2_1_316,
            WAITING_WORKFLOWS_2_1_310, screen as captured,
        };
        for fixture in [
            WAITING_WORKFLOWS_2_1_310,
            WAITING_AGENTS_WORKFLOWS_2_1_316,
            INFERRING_OVER_BACKGROUND_2_1_316,
        ] {
            let measured = captured(fixture);
            let line = status_line(&measured).expect("the status line");
            assert!(!line.contains('│'), "{line}");
            assert_eq!(status_row(&measured), Some(&*line), "{measured:#?}");
        }
    }

    /// THE API RETRY ROW BESIDE THE SIDE PANEL IS READ UP TO ITS RULE ([`panel_cut`]):
    /// [`api_retry_row`] read the status row whole, so the rule the panel pads it out to, and any
    /// text the panel draws after it, ran into the row's last item: no retry row read beside the
    /// panel, and a turn the Mac slept through in the vendor's retry got no Esc there. No capture
    /// draws a retry row beside the panel, so these screens are CONSTRUCTED from the measured
    /// 2.1.316 live turn: its spinner row replaced by each measured 2.1.310 retry row that fits
    /// left of the rule (the no-response, dropped and stalled rows), padded to the measured rule,
    /// with no panel text beside it (as captured) and with the panel's measured heading. Each
    /// reads as the same row read in its own capture, with the same guard, which holds the row
    /// beside the panel and with none; a no-response row's guard still holds its whole text, so
    /// no row whose words run on past it.
    #[test]
    fn the_api_retry_row_beside_the_side_panel_is_read_up_to_its_rule() {
        use crate::prompt::fixtures::{
            API_DROPPED_QUEUED_2_1_310, API_NO_RESPONSE_2_1_310, API_STALLED_QUEUED_2_1_310,
            INFERRING_OVER_BACKGROUND_2_1_316, screen as captured,
        };
        const SPINNER: &str = "✻ Inferring… (1m 35s · ↓ 2.9k tokens)";
        const DIFF: &str = "1 file changed                                source: Current ▾";
        let live = captured(INFERRING_OVER_BACKGROUND_2_1_316);
        let at = live
            .iter()
            .position(|x| x.starts_with(SPINNER))
            .expect("the spinner row");
        let (head, side) = live[at].split_once('│').expect("the panel's rule");
        assert_eq!(side, "", "the spinner row's panel text as captured");
        let width = head.chars().count();
        let mut read = 0;
        for fixture in [
            API_NO_RESPONSE_2_1_310,
            API_DROPPED_QUEUED_2_1_310,
            API_STALLED_QUEUED_2_1_310,
        ] {
            let alone = captured(fixture);
            let own = api_retry_row(&alone).expect("PRECONDITION: the measured retry row");
            let row = crate::text::trim_end(&alone[own.row]);
            assert!(row.chars().count() < width, "PRECONDITION: {row} fits");
            for side in ["", DIFF] {
                let mut r = live.clone();
                r[at] = format!("{row:<width$}│{side}");
                let beside = api_retry_row(&r).unwrap_or_else(|| panic!("{row} │{side}: {r:#?}"));
                assert_eq!(
                    (
                        beside.row,
                        beside.cause,
                        beside.next_try_s,
                        beside.failed_wait_s
                    ),
                    (at, own.cause, own.next_try_s, own.failed_wait_s),
                    "{row} │{side}"
                );
                assert_eq!(beside.text, own.text, "{row} │{side}");
                assert_eq!(beside.guard, own.guard, "{row} │{side}");
                let guard = aterm_regex::Regex::new(&beside.guard).expect("the guard compiles");
                assert!(guard.is_match(&r[at]), "{row} │{side}: {}", beside.guard);
                assert!(guard.is_match(&alone[own.row]), "{row}: with no panel");
                if own.cause == RetryCause::NoResponse {
                    let on = format!("{row} · attempt 2/2");
                    assert!(!guard.is_match(&on), "{on}");
                    assert!(
                        !guard.is_match(&format!("{on:<width$}│{side}")),
                        "{on} │{side}"
                    );
                }
                assert_eq!(status_row(&r), Some(row), "{row} │{side}");
                assert_eq!(worker_phase(&r), Phase::Busy, "{row} │{side}");
                read += 1;
            }
        }
        assert_eq!(read, 6, "each retry row, beside the panel's text and none");
    }

    /// A STATUS LINE INK WRAPS BESIDE THE SIDE PANEL IS READ WHOLE: beside the panel Claude Code
    /// lays its rows out as wide as the panel's rule's column, and wraps there (the measured
    /// 2.1.316 captures wrap the transcript's prose and the spinner's tip at the rule), so a status
    /// line longer than that wraps at the rule, its rest at the text's column as a done row wraps
    /// in a narrow pane (measured on 2.1.299 at 26 and 34 columns). Those rows carry the rule, and
    /// the wrap readers take no row that draws a box glyph and measured the room by the screen's
    /// width, so the line read as its first row alone: a done row naming agents, shells and
    /// monitors still running read `… still` and no work ran, which a restart in place would end.
    /// No measured status line is long enough to wrap beside the panel, so the screen is
    /// CONSTRUCTED from the measured 2.1.316 one: its status row replaced by a done row whose words
    /// join 2.1.283's measured done row and 2.1.316's measured pill counts with the `2 agents` this
    /// crate's tests construct, wrapped at the measured rule, its footer bare. CONTROL: the same
    /// words cut to its first row (no wrap drawn) name no work, as they say none.
    #[test]
    fn a_status_line_wrapped_beside_the_side_panel_is_read_whole() {
        use crate::prompt::fixtures::{WAITING_AGENTS_WORKFLOWS_2_1_316, screen as captured};
        const LEFT: &str = "background work still running";
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        const DONE: &str = "✻ Sautéed for 1d 2h 58m · done 12:19 PM · 2 agents, 13 shells, 3 monitors \
                            still running";
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let at = panel
            .iter()
            .rposition(|x| x.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let width = panel[at]
            .split_once('│')
            .expect("the rule")
            .0
            .chars()
            .count();
        assert!(DONE.chars().count() > width, "PRECONDITION: it wraps");
        let split = DONE
            .char_indices()
            .take(width - 1)
            .filter(|&(_, c)| c == ' ')
            .last()
            .map(|(i, _)| i)
            .expect("a space to wrap at");
        let (first, rest) = (&DONE[..split], &DONE[split + 1..]);
        let wrapped = |rows: &[&str]| {
            let mut s = panel[..at].to_vec();
            s.extend(rows.iter().map(|row| format!("{row:<width$}│")));
            s.extend_from_slice(&panel[at + 1..]);
            let frame = composer_frame(&s).expect("the frame");
            s[frame.top + 1] = "❯".to_string();
            s.truncate(frame.bottom + 1);
            s.extend([String::new(), QUIET.to_string()]);
            s
        };
        let r = wrapped(&[first, &format!("  {rest}")]);
        assert_eq!(status_line(&r).as_deref(), Some(DONE), "{r:#?}");
        assert_eq!(status_row(&r), Some(first));
        assert_eq!(in_process_work(&r), Some(LEFT));
        assert_eq!(work_running(&r), Some(LEFT));
        assert_eq!(background_wait(&r), Some("a monitor still running"));
        let cut = wrapped(&[first]);
        assert_eq!(status_line(&cut).as_deref(), Some(first));
        assert_eq!(work_running(&cut), None, "{cut:#?}");
    }

    /// THE SIDE PANEL'S TEXT IS NO HINT: the rows between the status line and the composer's top
    /// rule, where Claude Code draws its right-aligned hints (`Still working`, [`still_working`]),
    /// are read up to the side panel's rule ([`panel_cut`]), as the status line is. Read whole
    /// before, a `Still working` the panel drew beside such a row (a diff of a file holding those
    /// words) read as a live turn: the measured 2.1.316 turn over under its own background work
    /// gave no rest, and a turn over read busy, for as long as the panel showed it. No capture
    /// draws panel text beside those rows, nor the `Still working` hint at all, so these screens
    /// are CONSTRUCTED from the measured 2.1.316 one: the panel's text beside the row under its
    /// status row, which is the one row above the top rule there, its status row as captured or a
    /// done row padded to the measured rule over a bare footer. CONTROL: the crate's constructed
    /// hint drawn left of the rule, right-aligned to it, still reads as a live turn, with the
    /// panel's text beside it or none.
    #[test]
    fn the_side_panels_text_is_no_hint() {
        use crate::prompt::fixtures::{WAITING_AGENTS_WORKFLOWS_2_1_316, screen as captured};
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        const DONE: &str = "✻ Worked for 3m 20s · done 4:57 PM";
        const SAYS: &str = "+        .any(|r| r.contains(\"Still working\"))";
        const HINT: &str = "Still working. Check in from your phone · /remote";
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let at = panel
            .iter()
            .rposition(|x| x.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let (head, side) = panel[at + 1].split_once('│').expect("the rule under it");
        assert_eq!((crate::text::trim(head), side), ("", ""), "as captured");
        let width = head.chars().count();
        assert_eq!(composer_frame(&panel).map(|f| f.top), Some(at + 2));
        let under = |r: &[String], row: String| {
            let mut s = r.to_vec();
            s[at + 1] = row;
            s
        };
        let says = format!("{:width$}│{SAYS}", "");
        assert_eq!(background_rest(&panel), Some(Phase::Idle), "as captured");
        let rests = under(&panel, says.clone());
        assert!(background_only(&rests), "{rests:#?}");
        assert_eq!(background_rest(&rests), Some(Phase::Idle));

        let mut done = panel.clone();
        done[at] = format!("{DONE:<width$}│");
        let frame = composer_frame(&done).expect("the frame");
        done[frame.top + 1] = "❯".to_string();
        done.truncate(frame.bottom + 1);
        done.extend([String::new(), QUIET.to_string()]);
        assert_eq!(busy_signal(&done), None, "{done:#?}");
        let idle = under(&done, says);
        assert_eq!(busy_signal(&idle), None, "{idle:#?}");
        assert_eq!(worker_phase(&idle), Phase::Idle);

        for side in ["", SAYS] {
            let hint = format!("{HINT:>width$}│{side}");
            let live = under(&done, hint.clone());
            assert_eq!(
                signal(&live).as_deref(),
                Some("hint: Still working"),
                "│{side}"
            );
            assert_eq!(worker_phase(&live), Phase::Busy, "│{side}");
            let live = under(&panel, hint);
            assert!(!background_only(&live), "│{side}: a live turn");
            assert_eq!(background_rest(&live), None, "│{side}");
        }
    }

    /// THE SIDE PANEL'S TEXT IS NO WORD OF A COMPLETED WORKFLOW'S ROW: a `⏺ Dynamic workflow "…"`
    /// row's words, joined across the rows Claude Code wrapped it onto ([`wrapped_item`]), are
    /// read up to the side panel's rule ([`panel_cut`]), as the status line is, and end at a row
    /// blank up to it. Read whole before, a row blank but for the rule and the panel's text after
    /// it was no blank row, so the item's words ran on through the panel's text beside each row
    /// down to the next block, and a `" completed` the panel drew there (a diff of a test holding
    /// such a row) counted as completed a workflow no row said was: the wait carried above it
    /// ended, and the work it waits for read as none, which a restart in place would end. No
    /// capture draws a completion beside the panel, so these screens are CONSTRUCTED from the
    /// measured 2.1.316 one: under its rows, a carried wait for one workflow, a completion row
    /// (the hand-built one of `claude-2.1.283-wall-fable-goal-rate-limited-shell.txt`, the measured
    /// one of `idle-after-limit-and-model-switch.txt` with its title shortened to fit left of the
    /// rule), and that measured row with its title whole, wrapped at the measured rule at column 0,
    /// as 2.1.302 wraps one (`claude-2.1.302-login-handoff-over-weekly-limit.txt`), each padded to
    /// the rule, the panel's text beside the row under each or none, then a reply and the capture's
    /// own rows. Each completion ends the wait; the long one cut to
    /// its first row (no ` completed` drawn, as in
    /// `a_completed_row_whose_title_wraps_still_ends_the_wait_it_answers`) ends none.
    #[test]
    fn the_side_panels_text_is_no_word_of_a_completed_row() {
        use crate::prompt::fixtures::{WAITING_AGENTS_WORKFLOWS_2_1_316, screen as captured};
        const LEFT: &str = "background work still running";
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        const WAIT: &str = "✻ Waiting for 1 dynamic workflow to finish";
        const SHORT: &str = "⏺ Dynamic workflow \"Second review pass (read-only)\" completed · 5s";
        const LONG: &str = "⏺ Dynamic workflow \"Second review pass on the reworked \
                            cache-eviction change (read-only)\" completed · 5s";
        const SAYS: &str = "+  \"⏺ Dynamic workflow \\\"Review\\\" completed · 5s\",";
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let at = panel
            .iter()
            .rposition(|x| x.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let width = panel[at]
            .split_once('│')
            .expect("the rule")
            .0
            .chars()
            .count();
        let pad = |text: &str, side: &str| format!("{text:<width$}│{side}");
        let split = LONG
            .char_indices()
            .take(width + 1)
            .filter(|&(_, c)| c == ' ')
            .last()
            .map(|(i, _)| i)
            .expect("a space to wrap at");
        let (first, rest) = (&LONG[..split], &LONG[split + 1..]);
        assert!(first.chars().count() <= width && !first.contains(" completed"));
        let carried = |item: &[&str], side: &str| {
            let mut s = panel[..at].to_vec();
            s.extend([pad(WAIT, ""), pad("", "")]);
            s.extend(item.iter().map(|row| pad(row, "")));
            s.extend([
                pad("", side),
                pad("⏺ The review is in; nothing to fix.", ""),
            ]);
            s.extend_from_slice(&panel[at + 1..]);
            let frame = composer_frame(&s).expect("the frame");
            s[frame.top + 1] = "❯".to_string();
            s.truncate(frame.bottom + 1);
            s.extend([String::new(), QUIET.to_string()]);
            s
        };
        for side in ["", SAYS] {
            for item in [vec![SHORT], vec![first, rest]] {
                let r = carried(&item, side);
                assert_eq!(in_process_work(&r), None, "{item:?} │{side}: {r:#?}");
                assert_eq!(work_running(&r), None, "{item:?} │{side}");
                assert_eq!(worker_phase(&r), Phase::Idle, "{item:?} │{side}");
            }
            let cut = carried(&[first], side);
            assert_eq!(in_process_work(&cut), Some(LEFT), "│{side}: {cut:#?}");
            assert_eq!(work_running(&cut), Some(LEFT), "│{side}");
            assert!(carried_wait_alone(&cut), "│{side}");
        }
    }

    /// THE ROWS AN IDLE COMPACTION DREW BESIDE THE SIDE PANEL ARE READ UP TO ITS RULE: the walk up
    /// over them to the done row they stand under ([`crate::turn::idle_compaction_from`], which
    /// [`turn_end_line`] reads) reads the transcript's rows ([`Transcript`]), each cut at the rule
    /// [`panel_rule`] finds beside the row over the composer's top rule. Read whole before (found
    /// in the review of the KI-1101-3 panel fixes, 2026-11-04), a blank row beside the panel was no
    /// blank row and the restored `⏺ Task "…" still running in background` row ended in its padding
    /// and the rule, so the walk stopped under that row and no done row spoke for the turn:
    /// background agents a done row there names still running, which no process row names, were no
    /// work, and with no shell counted beside them nothing held a restart in place that would end
    /// them. No capture draws an idle compaction beside the panel, so the screens are CONSTRUCTED:
    /// the measured 2.1.316 capture's rows above its last waiting row (measured beside the panel),
    /// then the measured 2.1.312 capture's rows from its done row down to its composer, each
    /// wrapped at the measured rule as the 2.1.316 captures wrap a `⏺` row (at column 2) and a `⎿`
    /// row (at column 5) there ([`crate::prompt::fixtures::ink_wrap`]) and padded to it, with no
    /// panel text beside them or the panel's measured heading, and the 2.1.312 capture's composer
    /// and footer. Its done row names `1 shell, 2 agents still running` (as
    /// `the_work_a_done_row_names_above_an_idle_compaction_holds_the_restart` constructs it), or `2
    /// agents still running` over a footer whose `· 1 shell` is dropped, or is as captured (`· 1
    /// shell still running`), which names no work but the process table's. CONTROL: the same rows
    /// with no panel (each cut at the rule) read the same.
    #[test]
    fn the_rows_an_idle_compaction_drew_beside_the_side_panel_are_read_up_to_its_rule() {
        use crate::prompt::fixtures::{
            IDLE_COMPACTED_OVER_A_SHELL_2_1_312, WAITING_AGENTS_WORKFLOWS_2_1_316,
            screen as captured,
        };
        const LEFT: &str = "background work still running";
        const DONE: &str = "✻ Worked for 23s · done 4:57 PM";
        const DIFF: &str = "1 file changed                                source: Current ▾";
        let compacted = captured(IDLE_COMPACTED_OVER_A_SHELL_2_1_312);
        let done = compacted
            .iter()
            .position(|r| r.starts_with(DONE))
            .expect("the done row");
        let top = composer_frame(&compacted).expect("the frame").top;
        let pill = compacted.len() - 1;
        assert!(compacted[pill].ends_with(" · 1 shell"), "{compacted:#?}");
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let at = panel
            .iter()
            .rposition(|x| x.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let width = panel[at]
            .split_once('│')
            .expect("the rule")
            .0
            .chars()
            .count();
        let beside = |done_row: &str, shell: bool, side: &str| -> Vec<String> {
            let mut s = panel[..at].to_vec();
            for (i, row) in compacted.iter().enumerate().take(top).skip(done) {
                let row = if i == done { done_row } else { row.as_str() };
                s.extend(
                    wrapped_at_rule(row, width)
                        .iter()
                        .map(|r| format!("{r:<width$}│{side}")),
                );
            }
            s.extend_from_slice(&compacted[top..]);
            if !shell {
                let last = s.len() - 1;
                s[last] = compacted[pill].replace(" · 1 shell", "");
            }
            s
        };
        let alone = |r: &[String]| -> Vec<String> {
            r.iter()
                .map(|row| {
                    row.split_once('│')
                        .map_or(row.as_str(), |(left, _)| crate::text::trim_end(left))
                        .to_string()
                })
                .collect()
        };
        for side in ["", DIFF] {
            for (said, shell) in [
                ("1 shell, 2 agents still running", true),
                ("2 agents still running", false),
            ] {
                let agents = beside(&format!("{DONE} · {said}"), shell, side);
                assert!(
                    agents.iter().any(|r| r.starts_with("  \"idleCompaction\"")),
                    "PRECONDITION: the notice wraps at the rule: {agents:#?}"
                );
                for (name, r) in [
                    ("beside the panel", agents.clone()),
                    ("no panel", alone(&agents)),
                ] {
                    assert!(crate::turn::idle_compacted(&r), "{said}, {name} │{side}");
                    assert!(done_row_drawn(&r), "{said}, {name} │{side}: {r:#?}");
                    assert_eq!(in_process_work(&r), Some(LEFT), "{said}, {name} │{side}");
                    assert_eq!(unlisted_work(&r), Some(LEFT), "{said}, {name} │{side}");
                    assert_eq!(work_running(&r), Some(LEFT), "{said}, {name} │{side}");
                }
            }
            let shell = beside(&compacted[done], true, side);
            for (name, r) in [
                ("beside the panel", shell.clone()),
                ("no panel", alone(&shell)),
            ] {
                assert!(done_row_drawn(&r), "{name} │{side}");
                assert_eq!(in_process_work(&r), None, "{name} │{side}");
                assert_eq!(unlisted_work(&r), None, "{name} │{side}");
            }
        }
    }

    /// The measured 2.1.316 panel capture and the cell column of its rule.
    fn panel_capture_2_1_316() -> (Vec<String>, usize) {
        use crate::prompt::fixtures::{WAITING_AGENTS_WORKFLOWS_2_1_316, screen as captured};
        let panel = captured(WAITING_AGENTS_WORKFLOWS_2_1_316);
        let rule = panel[0]
            .split_once('│')
            .expect("the panel's rule")
            .0
            .chars()
            .count();
        (panel, rule)
    }

    /// `rows` with no side panel: each row cut at its first `│`, the panel's rule in these screens.
    fn no_panel(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.split_once('│')
                    .map_or(row.as_str(), |(left, _)| crate::text::trim_end(left))
                    .to_string()
            })
            .collect()
    }

    /// A QUESTION ASKED BESIDE THE SIDE PANEL READS AS A QUESTION: the last row the agent said
    /// ([`last_said_row`], which [`asked_in_prose`] reads for [`worker_phase`] and
    /// [`background_rest`]) is its words up to the panel's rule ([`Transcript`]). Read whole
    /// before (KI-1101-3), the row ended in its padding and the rule, or the panel's text after
    /// it, never in its `?`, so a turn that asks the person something read as `Idle`, a turn over
    /// (and the turn it ended over the agent's own background work rested as `Idle`), and the
    /// harness could take it as a turn to carry on rather than one waiting on its answer; where the
    /// panel's text is its own `───` rule, the question's row read as a separator and was no said
    /// row at all. No capture draws a question beside the panel, so these screens are CONSTRUCTED
    /// from the measured 2.1.316 capture: its last message's last row (`NEEDS YOU: an xAI account
    /// …`) replaced by a question padded to the measured rule, with no panel text after it, the
    /// panel's measured heading (its second row's) or its measured `───` (its sixth row's), under
    /// the capture's own waiting row (two background agents and three workflows) or a done row
    /// padded to the rule, the composer empty and the footer bare. CONTROL: the same rows with no
    /// panel (each cut at the rule) read the same, and the measured row, a statement, asks nothing.
    #[test]
    fn a_question_asked_beside_the_side_panel_reads_as_a_question() {
        const ASK: &str =
            "  Should I take the 71 rows myself rather than leave them to the ship step?";
        const DONE: &str = "✻ Worked for 3m 20s · done 4:57 PM";
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let (panel, rule) = panel_capture_2_1_316();
        let pad = |text: &str, side: &str| format!("{text:<rule$}│{side}");
        let said = panel
            .iter()
            .rposition(|r| r.starts_with("  NEEDS YOU: an xAI account"))
            .expect("the last said row");
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let heading = panel[1].split_once('│').expect("the heading").1;
        let ruled = panel[5].split_once('│').expect("the panel's rule row").1;
        assert!(ruled.starts_with("────") && heading.starts_with("1 file changed"));
        let screen = |row: &str, side: &str, done: bool| {
            let mut s = panel.clone();
            s[said] = pad(row, side);
            if done {
                s[at] = pad(DONE, "");
                let frame = composer_frame(&s).expect("the frame");
                s[frame.top + 1] = "❯".to_string();
                s.truncate(frame.bottom + 1);
                s.extend([String::new(), QUIET.to_string()]);
            }
            s
        };
        for side in ["", heading, ruled] {
            for done in [false, true] {
                let asks = screen(ASK, side, done);
                for (name, r) in [
                    ("beside the panel", asks.clone()),
                    ("no panel", no_panel(&asks)),
                ] {
                    let what = format!("{name}, done {done}, │{side}");
                    let (phase, rest) = if done {
                        (Phase::Question, None)
                    } else {
                        (Phase::Busy, Some(Phase::Question))
                    };
                    assert_eq!(background_rest(&r), rest, "{what}");
                    assert_eq!(worker_phase(&r), phase, "{what}");
                    assert_eq!(last_said_row(&r), Some(ASK), "{what}");
                    assert_eq!(last_said_index(&r), Some(said), "{what}");
                    let tail = crate::turn::said_tail(&r).expect("the last message");
                    assert!(tail.ends_with(ASK.trim_start()), "{what}: {tail}");
                    assert!(crate::turn::asks(&tail), "{what}");
                }
            }
        }
        let measured = panel[said].clone();
        let told = crate::text::trim_end(measured.split_once('│').expect("the rule").0);
        for done in [false, true] {
            let tells = screen(told, "", done);
            for (name, r) in [
                ("beside the panel", tells.clone()),
                ("no panel", no_panel(&tells)),
            ] {
                let what = format!("CONTROL {name}, done {done}");
                assert_eq!(last_said_row(&r), Some(told), "{what}");
                let (phase, rest) = if done {
                    (Phase::Idle, None)
                } else {
                    (Phase::Busy, Some(Phase::Idle))
                };
                assert_eq!(worker_phase(&r), phase, "{what}");
                assert_eq!(background_rest(&r), rest, "{what}");
            }
        }
    }

    /// THE NOTICE A TURN ENDED ON BESIDE THE SIDE PANEL IS READ UP TO ITS RULE: the notice
    /// [`notice`] places (and so [`limit_notice`], [`crate::wall::wall`] and the `Limited` phase)
    /// and the last said notice ([`last_said_notice`]) are read on the transcript's rows, each cut
    /// at the panel's rule and wrapped in its room ([`Transcript`]). Read whole before
    /// (KI-1101-3), the message carried each row's padding, the rule and the panel's text after
    /// it (four `│` in the limit's), and the `/goal` echo the last said notice reads ended in its
    /// padding and the rule. No capture draws a notice beside the panel, so these screens are
    /// CONSTRUCTED: the measured 2.1.316 panel capture's rows above its last waiting row, then the
    /// measured 2.1.283 weekly wall's rows from its completed workflow down to its done row, each
    /// wrapped at the measured rule as the 2.1.316 captures wrap a `⏺` row (at column 2) and a
    /// `⎿` row (at column 5) there ([`crate::prompt::fixtures::ink_wrap`]) and padded to it, with
    /// no panel text beside them or the panel's measured heading, then the 2.1.316 composer's
    /// rules around the 2.1.283 composer's and footer's rows. CONTROL: the same rows with no panel
    /// (each cut at the rule) read the same, and the wall reads as measured: its kind, its reset
    /// and its message.
    #[test]
    fn the_notice_a_turn_ended_on_beside_the_side_panel_is_read_up_to_its_rule() {
        use crate::prompt::fixtures::screen as captured;
        const MESSAGE: &str = "You've hit your weekly limit · resets 4pm (America/Los_Angeles) \
            Use your limit reset to reset it now: clau.de/reset /usage-credits to finish what \
            you’re working on. — Goal paused · usage limit reached · send a message after it \
            resets to continue";
        const ECHO: &str =
            "Goal paused · usage limit reached · send a message after it resets to continue";
        const RESET: &str = "4pm (America/Los_Angeles)";
        let wall = captured(include_str!(
            "fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
        ));
        let measured = limit_notice(&wall);
        assert_eq!(
            measured,
            Some((MESSAGE.to_string(), Some(RESET.to_string()))),
            "PRECONDITION: the wall as measured"
        );
        let from = wall
            .iter()
            .position(|r| r.starts_with("⏺ Dynamic workflow"))
            .expect("the completed workflow");
        let done = wall
            .iter()
            .position(|r| r.starts_with("✻ Sautéed"))
            .expect("the done row");
        let frame = composer_frame(&wall).expect("the frame");
        let (panel, rule) = panel_capture_2_1_316();
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let top = composer_frame(&panel).expect("the panel's frame").top;
        let heading = panel[1].split_once('│').expect("the heading").1;
        for side in ["", heading] {
            let mut beside = panel[..at].to_vec();
            for row in wall
                .iter()
                .take(done + 1)
                .skip(from)
                .chain([&String::new()])
            {
                beside.extend(
                    wrapped_at_rule(row, rule)
                        .iter()
                        .map(|r| format!("{r:<rule$}│{side}")),
                );
            }
            beside.extend([
                panel[top].clone(),
                wall[frame.top + 1].clone(),
                panel[top + 2].clone(),
                wall[frame.bottom + 1].clone(),
            ]);
            assert!(
                beside
                    .iter()
                    .any(|r| r.starts_with("  second and a third row of the ")),
                "PRECONDITION: the completed workflow wraps at the rule: {beside:#?}"
            );
            for (name, r) in [
                ("beside the panel", beside.clone()),
                ("no panel", no_panel(&beside)),
            ] {
                let what = format!("{name} │{side}");
                assert_eq!(limit_notice(&r), measured, "{what}: {r:#?}");
                let w = crate::wall::wall(&r).expect("the wall");
                assert_eq!(
                    (w.kind, w.message.as_str(), w.reset.as_deref()),
                    (WallKind::UsageWeekly, MESSAGE, Some(RESET)),
                    "{what}"
                );
                assert_eq!(
                    worker_phase(&r),
                    Phase::Limited {
                        message: MESSAGE.to_string(),
                        reset: Some(RESET.to_string()),
                    },
                    "{what}"
                );
                let said = last_said_notice(&r).expect("the last said notice");
                assert_eq!((said.text.as_str(), said.shaped), (ECHO, true), "{what}");
            }
        }
    }

    /// THE SIDE PANEL'S TEXT IS NO WORD OF THE LIMIT A CANCEL LEFT: the walks up the transcript
    /// that read the newest usage limit and what was served since it
    /// ([`crate::wall::since_limit`]), how Claude Code's own auto-continue ended ([`crate::wall::auto_continue_end`]), the limit a
    /// cancel left ([`crate::wall::left_wall`]) and the row such a reader read for want of the rows
    /// above ([`crate::wall::limit_words_above`]) read the transcript's rows, each cut at the
    /// panel's rule ([`Transcript`]), and wrap them in its room. Read whole before (KI-1101-3),
    /// each row's items, split at its wide gaps, took the panel's text after the padding as one of
    /// them: a limit notice the panel drew (a diff of a capture holding one) beside a row under
    /// the cancel or beside its done row read as the newest limit, its kind and reset the panel's,
    /// and the cancel's limit read as none; and the vendor's word that it went on by itself drawn
    /// there read as the auto-continue's end, so the cancel's limit read as none. No capture draws
    /// a limit beside the panel, so these screens are CONSTRUCTED: the measured 2.1.316 panel
    /// capture's rows above its last waiting row, then the measured 2.1.302 cancelled
    /// auto-continue's rows from its prompt down to its cancel and a blank row, each wrapped at
    /// the measured rule ([`wrapped_at_rule`]) and padded to it, with no panel text beside them or
    /// the measured 2.1.283 weekly notice's row or the measured 2.1.296 `⏺ Usage limit reset ·
    /// continuing automatically` beside one row, then the 2.1.316 composer's rules around the
    /// 2.1.302 composer's and footer's rows. CONTROL: the same rows in a pane as wide as the rule
    /// with no panel (each cut at the rule, the composer's rules that wide) read the same, and the
    /// limit as measured: its kind, reset and placement, its message the notice's and the
    /// countdown's (in the rule's room the wrap reader also joins the notice's `/upgrade …` row,
    /// as it does in a pane that wide with no panel). NEGATIVE CONTROL: the vendor's words drawn as
    /// a row of the transcript under the cancel end the auto-continue, and the cancel leaves no
    /// limit.
    #[test]
    fn the_side_panels_text_is_no_word_of_the_limit_a_cancel_left() {
        use crate::prompt::fixtures::{
            WALL_SONNET_BUCKET_AUTO_CONTINUE_CANCELLED_2_1_302, screen as captured,
        };
        use crate::wall::{
            AutoContinue, auto_continue_end, left_wall, limit_words_above, since_limit,
        };
        const NOTICE: &str = "You've hit your Sonnet limit · resets 10:06am (America/Los_Angeles)";
        const COUNTDOWN: &str =
            " — Usage limit reached · continuing automatically at 10:06am · esc to cancel";
        let weekly = captured(include_str!(
            "fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
        ));
        let weekly = weekly
            .iter()
            .find(|r| r.contains("You've hit your weekly limit"))
            .expect("the weekly notice");
        let fired = captured(crate::prompt::fixtures::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED);
        let fired = fired
            .iter()
            .find(|r| r.starts_with("⏺ Usage limit reset · continuing automatically"))
            .expect("the vendor's own continue");
        let cancelled = captured(WALL_SONNET_BUCKET_AUTO_CONTINUE_CANCELLED_2_1_302);
        let measured = (
            since_limit(&cancelled),
            auto_continue_end(&cancelled),
            limit_words_above(&cancelled),
            left_wall(&cancelled).expect("the measured cancel's limit"),
        );
        assert_eq!(
            measured.3.message,
            format!("{NOTICE}{COUNTDOWN}"),
            "PRECONDITION: the limit as measured"
        );
        let from = cancelled
            .iter()
            .position(|r| r.starts_with("❯ carry on with the task"))
            .expect("the prompt");
        let cancel = cancelled
            .iter()
            .position(|r| r.starts_with("⏺ Automatic continue cancelled"))
            .expect("the cancel");
        let done = cancelled
            .iter()
            .rposition(|r| r.starts_with("✻ Sautéed for 0s"))
            .expect("the done row");
        let frame = composer_frame(&cancelled).expect("the frame");
        let (panel, rule) = panel_capture_2_1_316();
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let top = composer_frame(&panel).expect("the panel's frame").top;
        let beside = |side: &str, by: usize, under: Option<&str>| {
            let mut s = panel[..at].to_vec();
            let rows = cancelled[from..=cancel]
                .iter()
                .map(String::as_str)
                .chain(under)
                .chain([""]);
            for (i, row) in (from..).zip(rows) {
                let side = if i == by { side } else { "" };
                s.extend(
                    wrapped_at_rule(row, rule)
                        .iter()
                        .map(|r| format!("{r:<rule$}│{side}")),
                );
            }
            s.extend([
                panel[top].clone(),
                cancelled[frame.top + 1].clone(),
                panel[top + 2].clone(),
                cancelled[frame.bottom + 1].clone(),
            ]);
            s
        };
        let narrow = |s: &[String]| {
            let mut v = no_panel(s);
            let frame = composer_frame(&v).expect("the frame");
            v[frame.top] = "─".repeat(rule);
            v[frame.bottom] = "─".repeat(rule);
            v
        };
        let mut misread = Vec::new();
        for (name, side, by) in [
            ("no panel text", "", usize::MAX),
            (
                "the weekly notice under the cancel",
                weekly.as_str(),
                cancel + 1,
            ),
            (
                "the vendor's continue under the cancel",
                fired.as_str(),
                cancel + 1,
            ),
            (
                "the weekly notice beside the done row",
                weekly.as_str(),
                done,
            ),
        ] {
            for (pane, r) in [
                ("beside the panel", beside(side, by, None)),
                (
                    "CONTROL as wide as the rule",
                    narrow(&beside(side, by, None)),
                ),
            ] {
                let left = left_wall(&r);
                let read = (
                    since_limit(&r),
                    auto_continue_end(&r),
                    limit_words_above(&r),
                    left.as_ref()
                        .map(|w| (w.kind, w.reset.clone(), w.placement)),
                );
                let wanted = (
                    measured.0.clone(),
                    measured.1,
                    measured.2,
                    Some((
                        measured.3.kind,
                        measured.3.reset.clone(),
                        measured.3.placement,
                    )),
                );
                let message = left.as_ref().map(|w| w.message.as_str());
                if read != wanted
                    || !message.is_some_and(|m| m.starts_with(NOTICE) && m.ends_with(COUNTDOWN))
                    || left != left_wall(&narrow(&beside("", usize::MAX, None)))
                {
                    misread.push(format!("{pane}, {name}: {read:?} {message:?}"));
                }
            }
        }
        assert!(misread.is_empty(), "{misread:#?}");
        let went_on = beside("", usize::MAX, Some(fired.as_str()));
        assert_eq!(
            (auto_continue_end(&went_on), left_wall(&went_on)),
            (Some(AutoContinue::Fired), None),
            "NEGATIVE CONTROL"
        );
    }

    /// THE TRANSCRIPT ENDS ABOVE THE ROWS BLANK UP TO THE SIDE PANEL'S RULE: with no status line
    /// over the composer, [`transcript_end`] walks up over what Claude Code parks between the
    /// transcript and the composer (a blank row, a survey, a tip, a hint against the right edge) on
    /// the transcript's rows, each cut at the panel's rule and against its room ([`Transcript`]).
    /// Read whole before (KI-1101-3), a row blank but for its padding and the rule was no blank
    /// row, so beside the panel the transcript ended under them, the rows of the worker's report
    /// ([`transcript_end`]'s, in aterm-agent) ran on through them, and only panel text reaching
    /// the screen's edge made such a row parked, as a hint against the composer's edge. No capture
    /// draws the live zone with no status line beside the panel, so these screens are CONSTRUCTED
    /// from the measured 2.1.316 waiting capture: its waiting row blank up to its rule, with no
    /// panel text beside the rows under the last said row, the panel's measured heading (it
    /// reaches the screen's edge) or its measured `crates/target-iceb` (it does not), or a hint
    /// against the rule over the composer. CONTROL: the same rows in a pane as wide as the rule
    /// with no panel (each cut at the rule, the composer's rules that wide) end there too, and the
    /// last said row and the person's stop read the same both ways.
    #[test]
    fn the_transcript_ends_above_the_rows_blank_up_to_the_side_panels_rule() {
        const HINT: &str = "◎ /goal active (3h)";
        let (panel, rule) = panel_capture_2_1_316();
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let said = last_said_index(&panel).expect("the last said row");
        let top = composer_frame(&panel).expect("the frame").top;
        assert!(
            said < at && (said + 1..top).all(|i| panel[i].ends_with('│')),
            "PRECONDITION: each row from the last said one to the composer is padded to the rule"
        );
        let heading = panel[1].split_once('│').expect("the heading").1;
        let path = panel[3].split_once('│').expect("the path").1;
        assert_eq!(
            path, "crates/target-iceb",
            "PRECONDITION: the panel's measured words"
        );
        let narrow = |s: &[String]| {
            let mut v = no_panel(s);
            v[top] = "─".repeat(rule);
            v[top + 2] = "─".repeat(rule);
            v
        };
        let mut misread = Vec::new();
        for (name, side, hint) in [
            ("no panel text", "", false),
            ("the panel's heading", heading, false),
            ("the panel's path", path, false),
            ("a hint against the rule", "", true),
        ] {
            let mut s = panel.clone();
            for row in &mut s[said + 1..top] {
                *row = format!("{:rule$}│{side}", "");
            }
            if hint {
                s[top - 1] = format!("{HINT:>edge$}  │", edge = rule - 2);
            }
            for (pane, r) in [
                ("beside the panel", s.clone()),
                ("CONTROL as wide as the rule", narrow(&s)),
            ] {
                let read = (
                    transcript_end(&r),
                    last_said_index(&r),
                    crate::turn::interrupted(&r),
                );
                if read != (said + 1, Some(said), false) {
                    misread.push(format!("{pane}, {name}: {read:?}"));
                }
            }
        }
        assert!(misread.is_empty(), "{misread:#?}");
    }

    /// A CARD PARKED OVER THE COMPOSER BESIDE THE SIDE PANEL IS PARKED: the cards Claude Code
    /// parks between the transcript and the composer (the rating cards,
    /// [`crate::survey::survey_block`], the drafts card, the ask cards and the rating's own
    /// aftermath, [`crate::survey::parked_span`]) are read on the transcript's rows, each cut at the
    /// panel's rule and laid out in its room ([`Transcript`]), so the readers that step over them
    /// (the status walk, [`transcript_end`], the last-said walk and so [`crate::turn::interrupted`])
    /// step over them beside the panel as with none, and [`survey_open`] reads the open rating card
    /// there. Read whole before (found in review, 2026-11-05), each card row ended in its padding
    /// and the rule, so beside the panel no card was found (the session survey's rows only by
    /// [`transcript_end`]'s own survey-row check): the transcript ended under every other card, a
    /// person's Esc above the plugin survey or the ask read as a turn that ended by itself (the
    /// last block was the card's head), and no rating card read as open. No capture draws a card
    /// beside the panel, so these screens are CONSTRUCTED from the
    /// measured 2.1.316 waiting capture: its rows under the last said row replaced by each card's
    /// rows as its fixture draws them over the composer (the real session survey of
    /// `survey-open.txt` and the measured 2.1.286 aftermath steps; the plugin survey, the transcript-share ask
    /// and the drafts card hand-built from the 2.1.284 render code), under no status line or a
    /// done row, with no stop above them or the measured Esc of the 2.1.301 watchdog capture
    /// (`⏺ Bash(sleep 12)` and its `⎿  Interrupted` row), each row padded to the rule. CONTROL:
    /// the same rows in a pane as wide as the rule with no panel (each cut at the rule, the
    /// composer's rules that wide) read the same.
    #[test]
    fn a_card_parked_over_the_composer_beside_the_side_panel_is_parked() {
        use crate::prompt::fixtures::{WALL_WATCHDOG_MIDTURN_ESC_2_1_301, screen as captured};
        const DONE: &str = "✻ Worked for 3m 20s · done 4:57 PM";
        let (panel, rule) = panel_capture_2_1_316();
        let said = last_said_index(&panel).expect("the last said row");
        let top = composer_frame(&panel).expect("the frame").top;
        let esc = {
            let r = captured(WALL_WATCHDOG_MIDTURN_ESC_2_1_301);
            let at = r
                .iter()
                .position(|x| x.starts_with("⏺ Bash(sleep 12)"))
                .expect("the stopped call");
            r[at..at + 3].to_vec()
        };
        assert!(
            esc[2].contains("Interrupted · What should Claude do instead?"),
            "PRECONDITION: the measured stop: {esc:#?}"
        );
        let card = |text: &str| {
            let r = captured(text);
            let done = r
                .iter()
                .rposition(|x| x.starts_with('✻') && x.contains(" · done "))
                .expect("the done row over the card");
            let below = composer_frame(&r).expect("the frame").top;
            r[done + 1..below]
                .iter()
                .filter(|x| leading_spaces(x) < 20)
                .cloned()
                .collect::<Vec<String>>()
        };
        let at_rule = |s: &[String]| {
            let mut v: Vec<String> = s
                .iter()
                .map(|row| {
                    if row.chars().nth(rule) == Some('│') {
                        crate::text::trim_end(&row.chars().take(rule).collect::<String>())
                            .to_string()
                    } else {
                        row.clone()
                    }
                })
                .collect();
            let frame = composer_frame(&v).expect("the frame");
            v[frame.top] = "─".repeat(rule);
            v[frame.bottom] = "─".repeat(rule);
            v
        };
        let mut misread = Vec::new();
        for (name, text, rating) in [
            (
                "the session survey",
                include_str!("fixtures/survey-open.txt"),
                true,
            ),
            (
                "the plugin survey",
                include_str!("fixtures/claude-2.1.284-survey-plugin.txt"),
                true,
            ),
            (
                "the rating's pending step",
                include_str!("fixtures/claude-2.1.286-survey-after-1-pending.txt"),
                false,
            ),
            (
                "the rating's thanks",
                include_str!("fixtures/claude-2.1.286-survey-after-1-thanks.txt"),
                false,
            ),
            (
                "the transcript-share ask",
                include_str!("fixtures/claude-2.1.284-share-ask-idle.txt"),
                false,
            ),
            (
                "the drafts card",
                include_str!("fixtures/claude-2.1.284-drafts-card.txt"),
                false,
            ),
        ] {
            let card = card(text);
            assert!(
                card.iter().any(|r| !crate::text::trim(r).is_empty())
                    && card.iter().all(|r| r.chars().count() < rule),
                "PRECONDITION: {name}'s rows fit beside the rule: {card:#?}"
            );
            for stopped in [false, true] {
                for status in [&[][..], &["", DONE][..]] {
                    let mut under: Vec<String> = Vec::new();
                    if stopped {
                        under.push(String::new());
                        under.extend(esc.iter().cloned());
                    }
                    under.extend(status.iter().map(ToString::to_string));
                    let end = said + 1 + under.len();
                    under.extend(card.iter().cloned());
                    let mut s = panel[..=said].to_vec();
                    s.extend(under.iter().map(|x| format!("{x:<rule$}│")));
                    s.extend(panel[top..].iter().cloned());
                    let what =
                        format!("{name}, stopped {stopped}, done row {}", !status.is_empty());
                    let control = at_rule(&s);
                    let read = |r: &[String]| {
                        (
                            transcript_end(r),
                            crate::turn::interrupted(r),
                            survey_open(r),
                            last_said_index(r),
                            worker_phase(r),
                        )
                    };
                    let wanted = (end, stopped, rating);
                    for (pane, r) in [
                        ("beside the panel", &s),
                        ("CONTROL as wide as the rule", &control),
                    ] {
                        let got = read(r);
                        if (got.0, got.1, got.2) != wanted || got != read(&control) {
                            misread.push(format!("{pane}, {what}: {got:?} wanted {wanted:?}"));
                        }
                    }
                }
            }
        }
        assert!(misread.is_empty(), "{misread:#?}");
    }

    /// A HINT OVER THE COMPOSER BESIDE THE SIDE PANEL STANDS AGAINST ITS RULE: the right-edge
    /// hint readers ([`crate::turn::goal_active`], [`crate::wall::memory_wall`],
    /// [`crate::credits::in_use`], [`crate::wall::login_expires_in`]) read a row over the
    /// composer on the transcript ([`Transcript`]), up to the panel's rule and against its room.
    /// The measured captures draw these hints on the row over the composer's top rule (the goal
    /// mark on 2.1.283's three and 2.1.302's, the credits notice on 2.1.300's full-screen ones),
    /// two cells short of the composer's edge, or in the footer; each measured panel capture draws
    /// the panel's rule on that row and on none under the composer. Read against the composer's
    /// width before (KI-1101-3), a hint drawn left of the rule was no hint, and the panel's own
    /// text reaching the screen's edge was read where a hint stands. No capture draws a hint beside
    /// the panel, so these screens are CONSTRUCTED from the measured 2.1.316 capture: the row over
    /// its composer's top rule replaced by each hint's measured words (the goal mark of
    /// `claude-goal-active-suggestion.txt`, the banner row of `memory-banner-busy.txt`, the notice
    /// of `claude-2.1.300-credits-in-use-fullscreen.txt`; the login's from its anchors, which no
    /// capture draws), ending two cells short of the measured rule and padded to it, with no panel
    /// text beside it or the panel's measured heading. CONTROL: the same rows in a pane as wide as
    /// the rule with no panel (each row cut at the rule, the composer's rules that wide) read the
    /// same; NEGATIVE CONTROL: the same words drawn as the panel's text, reaching the screen's
    /// edge beside a blank row, are no hint (the banner's are wider than the panel).
    #[test]
    fn a_hint_over_the_composer_beside_the_side_panel_stands_against_its_rule() {
        const GOAL: &str = "◎ /goal active (3h)";
        const MEMORY: &str =
            "Critical memory usage (140.4GB) — restart and resume with claude --continue";
        const CREDITS: &str = "Now using usage credits";
        const LOGIN: &str = "Your login expires in 2 days · run /login to renew";
        let (panel, rule) = panel_capture_2_1_316();
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let top = composer_frame(&panel).expect("the frame").top;
        assert_eq!(
            top,
            at + 2,
            "PRECONDITION: one row between the status row and the rule"
        );
        let heading = panel[1].split_once('│').expect("the heading").1;
        let hinted = |hint: &str, side: &str| {
            let mut s = panel.clone();
            s[at + 1] = format!("{hint:>edge$}  │{side}", edge = rule - 2);
            s
        };
        let narrow = |s: &[String]| {
            let mut v = no_panel(s);
            let frame = composer_frame(&v).expect("the frame");
            v[frame.top] = "─".repeat(rule);
            v[frame.bottom] = "─".repeat(rule);
            v
        };
        let read = |s: &[String]| {
            (
                crate::turn::goal_active(s),
                crate::wall::memory_wall(s).map(|w| w.message),
                crate::credits::in_use(s).is_some(),
                crate::wall::login_expires_in(s),
            )
        };
        for side in ["", heading] {
            for (hint, wanted) in [
                (GOAL, (true, None, false, None)),
                (MEMORY, (false, Some(MEMORY.to_string()), false, None)),
                (CREDITS, (false, None, true, None)),
                (LOGIN, (false, None, false, Some(2))),
            ] {
                let s = hinted(hint, side);
                assert_eq!(read(&s), wanted, "beside the panel: {hint} │{side}");
                assert_eq!(read(&narrow(&s)), wanted, "CONTROL {hint} │{side}");
            }
        }
        for hint in [GOAL, CREDITS, LOGIN] {
            let mut s = panel.clone();
            let room = panel[0].chars().count() - rule - 1;
            s[at + 1] = format!("{:rule$}│{hint:>room$}", "");
            assert_eq!(
                s[at + 1].chars().count(),
                panel[0].chars().count(),
                "PRECONDITION: the panel's text reaches the screen's edge"
            );
            assert_eq!(
                read(&s),
                (false, None, false, None),
                "NEGATIVE CONTROL {hint}"
            );
        }
    }

    /// `row` as the 2.1.316 panel captures wrap a transcript row at the panel's rule, `rule`
    /// cells: a `⏺` row's words at column 2, a `⎿` row's at column 5
    /// ([`crate::prompt::fixtures::ink_wrap`]); any other row fits as it is.
    fn wrapped_at_rule(row: &str, rule: usize) -> Vec<String> {
        let (lead, on) = if row.starts_with('⏺') {
            (2, "  ")
        } else if crate::text::trim_start(row).starts_with('⎿') {
            (5, "     ")
        } else {
            assert!(row.chars().count() <= rule, "PRECONDITION: {row} fits");
            return vec![row.to_string()];
        };
        let split = row.char_indices().nth(lead).map_or(row.len(), |(i, _)| i);
        let (head, text) = row.split_at(split);
        crate::prompt::fixtures::ink_wrap(text, rule - lead)
            .into_iter()
            .enumerate()
            .map(|(k, r)| format!("{}{r}", if k == 0 { head } else { on }))
            .collect()
    }

    /// A QUESTION DECLINED BESIDE THE SIDE PANEL IS THE PERSON'S STOP: [`crate::turn::interrupted`]
    /// (Claude Code's reader's `interrupted`, which tells a person's Esc from a turn's own end)
    /// reads the last block's head row on the transcript ([`Transcript`]), its words up to the
    /// panel's rule. Read whole before (found in the review of the KI-1101-3 said-row fixes,
    /// 2026-11-05), `⏺ User declined to answer questions` ended in its padding and the rule and
    /// was no longer the anchor `question.declined`, so a person's Esc on a question dialog read
    /// as no stop and the turn as one that ended by itself. No capture draws a declined question
    /// beside the panel, so these screens are CONSTRUCTED: the measured 2.1.316 panel capture's
    /// rows above its last waiting row, then the measured 2.1.282 Esc capture's rows from its
    /// declined row down to its done row and a blank row, each wrapped at the measured rule
    /// ([`wrapped_at_rule`]) and padded to it, with no panel text beside them or the panel's
    /// measured heading, then the 2.1.316 composer's rules around the 2.1.282 composer's and
    /// footer's rows. CONTROL: the same rows with no panel (each cut at the rule) read the same;
    /// NEGATIVE CONTROL: the agent's own `⏺ Done.` beside panel text that holds the declined
    /// words is no stop.
    #[test]
    fn a_question_declined_beside_the_side_panel_is_the_persons_stop() {
        use crate::prompt::fixtures::{QUESTION_DECLINED_ESC, screen as captured};
        use crate::reader::Program;
        const DECLINED: &str = "User declined to answer questions";
        let declined = captured(QUESTION_DECLINED_ESC);
        let from = declined
            .iter()
            .position(|r| r.starts_with('⏺') && r.ends_with(DECLINED))
            .expect("the declined row");
        let done = declined
            .iter()
            .position(|r| r.starts_with("✻ Crunched for 3s"))
            .expect("the done row");
        let frame = composer_frame(&declined).expect("the frame");
        let (panel, rule) = panel_capture_2_1_316();
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let top = composer_frame(&panel).expect("the panel's frame").top;
        let heading = panel[1].split_once('│').expect("the heading").1;
        let beside = |head: &str, side: &str| {
            let mut s = panel[..at].to_vec();
            for (i, row) in declined
                .iter()
                .enumerate()
                .take(done + 1)
                .skip(from)
                .chain([(usize::MAX, &String::new())])
            {
                let row = if i == from { head } else { row.as_str() };
                let side = if i == from { side } else { "" };
                s.extend(
                    wrapped_at_rule(row, rule)
                        .iter()
                        .map(|r| format!("{r:<rule$}│{side}")),
                );
            }
            s.extend([
                panel[top].clone(),
                declined[frame.top + 1].clone(),
                panel[top + 2].clone(),
                declined[frame.bottom + 1].clone(),
            ]);
            s
        };
        for side in ["", heading] {
            let stop = beside(&declined[from], side);
            assert!(
                stop.iter().any(|r| r.starts_with("     High contrast)")),
                "PRECONDITION: the question's row wraps at the rule: {stop:#?}"
            );
            for (name, r) in [
                ("beside the panel", stop.clone()),
                ("no panel", no_panel(&stop)),
            ] {
                let what = format!("{name} │{side}");
                assert!(crate::turn::interrupted(&r), "{what}: {r:#?}");
                assert!(Program::Claude.reader().interrupted(&r), "{what}");
                assert_eq!(worker_phase(&r), Phase::Idle, "{what}");
            }
        }
        let told = beside(&declined[from].replace(DECLINED, "Done."), DECLINED);
        assert!(!crate::turn::interrupted(&told), "NEGATIVE CONTROL");
    }

    /// A QUESTION BESIDE THE SIDE PANEL ON A ROW THAT HOLDS AN EMOJI READS AS A QUESTION: the cut
    /// at the panel's rule ([`panel_cut`]) finds the rule's `│` by its cell column, which counts
    /// each character in the cells aterm's grid draws it in ([`crate::question::cells`]); Claude
    /// Code pads each row by those cells (string-width, as the grid draws an emoji, two cells), so
    /// the rule stands at its column on a row that holds one. Counted one cell before (found in
    /// the review of the KI-1101-3 said-row fixes, 2026-11-05), `✅` (U+2705), `⭐` (U+2B50),
    /// `🚀` (U+1F680) and `🫡` (U+1FAE1) put the rule's `│` a cell short of its column, so the
    /// row was not cut, ended in its padding and the rule, and the question read as `Idle`. No
    /// capture draws an emoji beside the panel, so these screens are CONSTRUCTED from the measured
    /// 2.1.316 capture as [`a_question_asked_beside_the_side_panel_reads_as_a_question`] builds
    /// them: its last message's last row replaced by a question that holds an emoji, padded to
    /// the measured rule by the grid's widths (`aterm_grapheme::char_width`), under the capture's
    /// waiting row or a done row. CONTROL: the same rows with no panel read the same, and so does
    /// the question with CJK words, which were counted two cells before too.
    #[test]
    fn a_question_beside_the_side_panel_on_a_row_with_an_emoji_reads_as_a_question() {
        const DONE: &str = "✻ Worked for 3m 20s · done 4:57 PM";
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let (panel, rule) = panel_capture_2_1_316();
        let pad = |text: &str| {
            let drawn: usize = text.chars().map(aterm_grapheme::char_width).sum();
            format!("{text}{}│", " ".repeat(rule - drawn))
        };
        let said = panel
            .iter()
            .rposition(|r| r.starts_with("  NEEDS YOU: an xAI account"))
            .expect("the last said row");
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let screen = |row: &str, done: bool| {
            let mut s = panel.clone();
            s[said] = pad(row);
            if done {
                s[at] = pad(DONE);
                let frame = composer_frame(&s).expect("the frame");
                s[frame.top + 1] = "❯".to_string();
                s.truncate(frame.bottom + 1);
                s.extend([String::new(), QUIET.to_string()]);
            }
            s
        };
        for ask in [
            "  ✅ Tests and lint pass. Should I push to main?",
            "  ⭐ The bench is 4% faster. Should I record it?",
            "  🚀 The release is staged. Should I promote it?",
            "  🫡 Noted. Should I take the 71 rows myself?",
            "  日本語の見出しは直しました。Should I push to main?",
        ] {
            for done in [false, true] {
                let asks = screen(ask, done);
                assert!(
                    asks[said].ends_with('│'),
                    "PRECONDITION: padded to the rule"
                );
                for (name, r) in [
                    ("beside the panel", asks.clone()),
                    ("no panel", no_panel(&asks)),
                ] {
                    let what = format!("{name}, done {done}: {ask}");
                    let (phase, rest) = if done {
                        (Phase::Question, None)
                    } else {
                        (Phase::Busy, Some(Phase::Question))
                    };
                    assert_eq!(last_said_row(&r), Some(ask), "{what}");
                    assert_eq!(background_rest(&r), rest, "{what}");
                    assert_eq!(worker_phase(&r), phase, "{what}");
                }
            }
        }
    }

    /// A QUESTION BESIDE THE SIDE PANEL ON A ROW THAT HOLDS WHAT THE GRID DRAWS IN NO CELL OF ITS
    /// OWN READS AS A QUESTION: the rule's `│` is found by its cell column, counted as aterm's grid
    /// lays the row out ([`crate::question::cells`], the walk of `aterm_grapheme::GridColumns`,
    /// which aterm-core's `grid_columns_agree_with_what_the_grid_paints` holds to the painted
    /// grid): a variation selector, a zero-width joiner and a combining mark are stored in the
    /// cell before them, a VS16 widens a one-cell emoji to two (`❤️`), and the grid folds an emoji
    /// after a joiner, a skin tone after its base and a flag's second regional indicator into the
    /// cell before. Counted before as one cell or two a character, none of them zero (named in
    /// KI-1101-3, 2026-11-05), `✅️` (U+2705 U+FE0F), a ZWJ sequence, a skin tone, a flag, a
    /// keycap and a combining mark put the rule's `│` past its column, so the row was not cut,
    /// ended in its padding and the rule, and the question read as `Idle`. Counted then as the
    /// grid's write path lays a row out (found in review, 2026-11-05), a VS15 after a two-cell
    /// character (`✅︎`, `中︎`) put the rule a cell past its column, because the grid empties
    /// that cell's right half and reads it back as a space. No capture draws these beside the
    /// panel, so these screens are CONSTRUCTED from the measured 2.1.316 capture as
    /// [`a_question_beside_the_side_panel_on_a_row_with_an_emoji_reads_as_a_question`] builds
    /// them, each question padded to the measured rule by the grid's own layout
    /// (`aterm_grapheme::str_grid_columns`) and read as the grid reads that row back
    /// ([`crate::question::read_back`]). CONTROL: the same rows with no panel read the same, and
    /// so does `❤️` (U+2764 U+FE0F), a one-cell emoji its VS16 widens, which the per-character
    /// count put in its two cells too.
    #[test]
    fn a_question_beside_the_side_panel_on_a_row_the_grid_joins_reads_as_a_question() {
        const DONE: &str = "✻ Worked for 3m 20s · done 4:57 PM";
        const QUIET: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let (panel, rule) = panel_capture_2_1_316();
        let pad = |text: &str| {
            let drawn = aterm_grapheme::str_grid_columns(text);
            crate::question::read_back(&format!("{text}{}│", " ".repeat(rule - drawn)))
        };
        let said = panel
            .iter()
            .rposition(|r| r.starts_with("  NEEDS YOU: an xAI account"))
            .expect("the last said row");
        let at = panel
            .iter()
            .rposition(|r| r.starts_with("✻ Waiting for 2 background agents"))
            .expect("the waiting row");
        let screen = |row: &str, done: bool| {
            let mut s = panel.clone();
            s[said] = pad(row);
            if done {
                s[at] = pad(DONE);
                let frame = composer_frame(&s).expect("the frame");
                s[frame.top + 1] = "❯".to_string();
                s.truncate(frame.bottom + 1);
                s.extend([String::new(), QUIET.to_string()]);
            }
            s
        };
        let mut misread = Vec::new();
        for (ask, joined) in [
            (
                "  \u{2705}\u{FE0F} Tests and lint pass. Should I push to main?",
                true,
            ),
            (
                "  \u{1F469}\u{200D}\u{1F4BB} The review is done. Should I push to main?",
                true,
            ),
            (
                "  \u{2705}\u{FE0F} Tests pass, \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \
                 drawn whole. Should I push to main?",
                true,
            ),
            (
                "  \u{1F44D}\u{1F3FD} Agreed. Should I take the 71 rows myself?",
                true,
            ),
            (
                "  \u{1F1EF}\u{1F1F5} The locale is set. Should I push to main?",
                true,
            ),
            (
                "  1\u{FE0F}\u{20E3} The first step is done. Should I start the second?",
                true,
            ),
            (
                "  The re\u{301}sume\u{301} is updated. Should I push it?",
                true,
            ),
            (
                "  \u{2705}\u{FE0E} Tests and lint pass. Should I push to main?",
                true,
            ),
            (
                "  \u{4E2D}\u{FE0E} The heading is fixed. Should I push to main?",
                true,
            ),
            (
                "  \u{2764}\u{FE0F} Thanks for the review. Should I record it?",
                false,
            ),
        ] {
            let each: usize = ask
                .chars()
                .map(|c| aterm_grapheme::char_width(c).max(1))
                .sum();
            assert_eq!(
                aterm_grapheme::str_grid_columns(ask) != each,
                joined,
                "PRECONDITION: the grid lays {ask:?} out in other cells than a cell or two a \
                 character, or (the CONTROL) in as many"
            );
            for done in [false, true] {
                let asks = screen(ask, done);
                for (name, r) in [
                    ("beside the panel", asks.clone()),
                    ("no panel", no_panel(&asks)),
                ] {
                    let said = crate::question::read_back(ask);
                    let wanted = if done {
                        (Some(said.as_str()), None, Phase::Question)
                    } else {
                        (Some(said.as_str()), Some(Phase::Question), Phase::Busy)
                    };
                    let read = (last_said_row(&r), background_rest(&r), worker_phase(&r));
                    if read != wanted {
                        misread.push(format!("{name}, done {done}: {read:?}"));
                    }
                }
            }
        }
        assert!(misread.is_empty(), "{misread:#?}");
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

    /// EVERY WORK THE FOOTER'S PILL COUNTS IS WORK A RESTART IN PLACE WOULD END
    /// ([`work_running`]): Claude Code counts what still runs in its footer's pill, one label for
    /// each kind (2.1.286, read from its source: `1 Artifact comment monitor`, `1 MCP task`, `2
    /// background tasks`, `◇ 1 cloud session`; no capture draws one of them), and with no done row
    /// (`showTurnDuration` off) or one that names none of it, the pill is the one place the screen
    /// names that work. [`work_running`] read only a shell or a monitor counted alone there, so a
    /// question or a limit's notice, each read ahead of the soft busy, read at rest over the work,
    /// and a line lost there was restarted in place over it. The screens are `wait_bg17.out` with
    /// its monitor labels removed and its last reply turned into a question (CONSTRUCTED), each
    /// label in the footer: under a done row that names none, with no done row, and with no done
    /// row under a limit's notice (the measured Fable one). CONTROLS: the label on the done row,
    /// read before as well; the footer counting nothing, or a count of no work (`10 files
    /// changed`).
    #[test]
    fn every_work_the_footers_pill_counts_is_work_a_restart_would_end() {
        const FABLE: &str = "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";
        let said = "⏺ Two runs in, both binaries agree and every checksum verifies. Waiting for the next events.";
        let done_row = "✻ Cogitated for 2s · done 8:50 PM";
        let asked: Vec<String> = waiter_capture(include_str!("fixtures/wait_bg17.out"))
            .iter()
            .map(|row| {
                row.replace(" · 1 monitor still running", "")
                    .replace(" · 1 monitor ·", " ·")
                    .replace(
                        said,
                        "⏺ Two runs in, both binaries agree and every checksum verifies. Shall I \
                         compare the next runs too?",
                    )
            })
            .collect();
        let at = asked
            .iter()
            .rposition(|row| row == done_row)
            .expect("done row");
        let mut undone = asked.clone();
        undone.remove(at);
        let mut walled = undone.clone();
        walled.insert(at, FABLE.to_string());
        let footer = |rows: &[String], label: &str| {
            with_footer(
                rows.to_vec(),
                &format!("  ⏵⏵ auto mode on · {label} · ← for agents · ↓ to manage"),
            )
        };
        for (place, rows) in [("under a done row", &asked), ("with no done row", &undone)] {
            assert_eq!(worker_phase(rows), Phase::Question, "{place}");
            assert_eq!(work_running(rows), None, "{place}: nothing counted");
            let files = footer(rows, "10 files changed");
            assert_eq!(work_running(&files), None, "{place}: no work counted");
        }
        assert!(matches!(worker_phase(&walled), Phase::Limited { .. }));
        assert_eq!(work_running(&walled), None, "a limit over nothing counted");
        for label in [
            "1 Artifact comment monitor",
            "1 MCP task",
            "2 background tasks",
            "◇ 1 cloud session",
        ] {
            for (place, rows) in [
                ("under a done row", &asked),
                ("with no done row", &undone),
                ("under a limit's notice", &walled),
            ] {
                let r = footer(rows, label);
                let phase = worker_phase(&r);
                assert!(
                    matches!(phase, Phase::Question | Phase::Limited { .. }),
                    "{label} {place}: {phase:?}"
                );
                assert_eq!(
                    work_running(&r),
                    Some("background work running"),
                    "{label} {place}"
                );
            }
            let mut named = asked.clone();
            named[at] = format!("{done_row} · {label} still running");
            assert_eq!(
                work_running(&named),
                Some("background work still running"),
                "{label} on the done row"
            );
        }
    }

    /// THE WORK NO PROCESS ROW NAMES ([`unlisted_work`]) LEAVES THE SHELLS AND MONITORS TO THE
    /// PROCESS TABLE: any work the footer's pill counts (2.1.286's labels, read from its source; no
    /// capture draws one), alone or joined with a shell, but never a shell or a monitor, named on a
    /// done row or counted alone in the pill, which [`work_running`] reads too. The screens are
    /// `wait_bg17.out` (its done rows and pill name `1 monitor`) and, CONSTRUCTED from it, the same
    /// screen with the done rows' monitor dropped and the pill counting each item instead.
    #[test]
    fn the_work_no_process_row_names_leaves_the_shells_and_monitors_to_the_table() {
        let saved = waiter_capture(include_str!("fixtures/wait_bg17.out"));
        assert_eq!(work_running(&saved), Some("a monitor still running"));
        assert_eq!(unlisted_work(&saved), None, "its monitor");
        let pill = |count: &str| -> Vec<String> {
            saved
                .iter()
                .map(|row| {
                    row.replace(" · 1 monitor still running", "")
                        .replace("· 1 monitor ·", &format!("· {count} ·"))
                })
                .collect()
        };
        for (count, running) in [
            ("2 shells", "a shell running"),
            ("1 monitor", "a monitor running"),
            ("1 shell, 1 monitor", "a shell running"),
        ] {
            let r = pill(count);
            assert_eq!(unlisted_work(&r), None, "{count}");
            assert_eq!(work_running(&r), Some(running), "{count}");
        }
        for (count, running) in [
            ("1 MCP task", "background work running"),
            ("2 background tasks", "background work running"),
            ("◇ 1 cloud session", "background work running"),
            ("1 Artifact comment monitor", "background work running"),
            ("1 shell, 1 MCP task", "a shell running"),
        ] {
            let r = pill(count);
            assert_eq!(
                unlisted_work(&r),
                Some("background work running"),
                "{count}"
            );
            assert_eq!(work_running(&r), Some(running), "{count}");
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
        let read = notice_placed(&bucket, &Transcript::of(&bucket)).expect("a wall");
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
            let r = walled(row, footer);
            let read = notice_placed(&r, &Transcript::of(&r));
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
            let read = notice_placed(r, &Transcript::of(r)).expect("a wall");
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
            let read = notice_placed(r, &Transcript::of(r)).expect("a wall");
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
        let read = notice_placed(&own, &Transcript::of(&own)).expect("a wall");
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
