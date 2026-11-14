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
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 36, "0.36");

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

/// Claude Code's window title says its REPL is at rest: the rest glyph (`title.rest`, `✳`), a
/// blank, then the title. A title that leads with no glyph says neither this nor
/// [`title_runs`].
#[must_use]
pub fn title_rests(title: &str) -> bool {
    let mut chars = title.chars();
    chars
        .next()
        .is_some_and(|c| const { crate::anchor!("title.rest") }.contains(c))
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
    /// The program's window title, where the screen shows no turn ([`busy_seen`]).
    Title,
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
            Zone::Title => "title",
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
    let i = glyph_above(rows, top)?;
    let end = status_line_end(rows, i, top, &|| crate::question::screen_width(rows)).unwrap_or(i);
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

/// An item ([`row_items`]) that counts background work under any label: a count whose noun
/// ([`counted_noun`]) is a kind of work, whatever words qualify it (`1 MCP task`, `1 Artifact
/// comment monitor`, `◇ 1 cloud session`, `2 background tasks`, `1 shell`): a shell, a monitor or
/// work the agent handed off ([`DELEGATED_NOUNS`]).
fn work_count(item: &str) -> bool {
    counted_noun(item)
        .is_some_and(|noun| noun == "shell" || noun == "monitor" || DELEGATED_NOUNS.contains(&noun))
}

/// The nouns of a count ([`counted_noun`]) that names work the agent handed off, which runs
/// Claude Code's title with its turn over (2.1.311: `delegatedActive`): a task, an agent, a
/// workflow, a session or a team. A shell or a monitor, qualified or not, is none.
const DELEGATED_NOUNS: [&str; 5] = ["task", "agent", "workflow", "session", "team"];

/// An item ([`row_items`]) that counts work the agent handed off: a count whose noun
/// ([`counted_noun`]) is one of [`DELEGATED_NOUNS`], whatever words qualify it (`1 MCP task`,
/// `2 background tasks`, `◇ 1 cloud session`, `1 local agent`); never a shell's or a monitor's
/// (`1 Artifact comment monitor`).
fn delegated_count(item: &str) -> bool {
    counted_noun(item).is_some_and(|noun| DELEGATED_NOUNS.contains(&noun))
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
/// of [`footer_count`], [`shells_running`], [`counts_work`], [`footer_work`] and
/// [`delegated_shown`].
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
/// and [`counted_noun`] (and through it [`work_count`] and [`delegated_count`]) share.
fn counted(item: &str) -> Option<(u32, &str)> {
    let item = item.strip_suffix(" still running").unwrap_or(item);
    let (count, label) = item.split_once(' ')?;
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((count.parse().ok()?, label))
}

/// The noun one count names ([`counted`]): its label's last word, without a plural `s`, whatever
/// words qualify it (`1 Artifact comment monitor`: `monitor`). The one noun reading
/// [`work_count`] and [`delegated_count`] share.
fn counted_noun(item: &str) -> Option<&str> {
    counted(item).map(|(_, label)| singular(label.rsplit_once(' ').map_or(label, |(_, last)| last)))
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

/// THE SCREEN SHOWS WORK THE AGENT HANDED OFF, which runs Claude Code's title with its turn over
/// (2.1.311: `delegatedActive`, a running local or remote agent, in-process teammate or local
/// workflow), so there its title says nothing of the turn ([`title_live`]): work Claude Code runs
/// in its own process ([`in_process_work`]: its status row's `Waiting for …`, the wait it carries
/// above its own wake, a done row's `… still running` naming more than shells and monitors counted
/// alone, though a done row ends the turn, which [`title_live`] reads ahead of this), a footer
/// item that counts a task, an agent, a workflow, a session or a team, whatever words qualify it
/// ([`delegated_count`]: `1 MCP task`, `2 background tasks`, `◇ 1 cloud session`), a workflow's
/// progress row under the footer ([`is_workflow_progress`]), or a running agent or workflow in
/// the agents panel drawn there (its row neither paused nor idle). A shell or a monitor in the
/// footer, counted alone or qualified (`1 Artifact comment monitor`), is no handed-off work: the
/// title rests over them ([`work_left`]). A progress row can outlive its workflow, and a counted
/// task or session need not run the title: the title then holds nothing, and the screen's own
/// reading stands. Past a message submitted under the status row neither [`in_process_work`] nor
/// a footer count is read ([`background_wait`]'s bounds): that message opens a turn, which runs
/// the title itself.
#[must_use]
pub fn delegated_shown(rows: &[String]) -> bool {
    use crate::agents_panel::{Bullet, Elapsed, RowKind, one_cell, parse_row};
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let footer = &rows[frame.bottom + 1..];
    let panel_runs = |row: &str| {
        parse_row(row, one_cell).is_some_and(|r| {
            matches!(r.kind, RowKind::Agent | RowKind::Workflow)
                && r.bullet.is_none_or(|(_, b)| b != Bullet::Paused)
                && r.elapsed.is_none_or(|(_, e)| e != Elapsed::Idle)
        })
    };
    in_process_work(rows).is_some()
        || (!turn_just_begun(rows)
            && footer
                .iter()
                .flat_map(|r| row_items(r))
                .any(delegated_count))
        || footer
            .iter()
            .any(|r| is_workflow_progress(r) || panel_runs(r))
}

/// CLAUDE CODE'S TITLE SAYS A TURN RUNS THAT ITS SCREEN DOES NOT SHOW: the title runs (`runs`,
/// [`crate::ScreenReader::runs`]), the composer's frame is drawn with no box over it, and the
/// screen shows no end of a turn and nothing else that runs the title. That is no done row
/// ([`done_row_drawn`]), no wall a turn ends on ([`crate::wall::turn_wall`], which holds every
/// [`limit_notice`]), no interrupt ([`crate::turn::interrupted`]), no handed-off work
/// ([`delegated_shown`]) and no live turn the screen already reads ([`no_live_turn`]). The
/// critical-memory banner ([`crate::wall::memory_wall`]) is no such end: Claude Code draws it over
/// a live turn too, and a reading under a running title keeps it as a hard busy does.
///
/// This is a reply drawn under a status line: the footer has no `esc to interrupt` and the
/// spinner row is gone once its first line is drawn (ATERM-KI-1027-3). `runs` of `None` (no
/// title, or one the program never set) and `Some(false)` are never live.
#[must_use]
pub fn title_live(rows: &[String], runs: Option<bool>) -> bool {
    runs == Some(true)
        && composer_frame(rows).is_some()
        && parse_prompt(rows).is_none()
        && !turn_over(rows)
        && no_live_turn(rows)
        && !delegated_shown(rows)
        && !crate::turn::interrupted(rows)
        && crate::wall::turn_wall(rows).is_none()
}

/// [`busy_signal`] read beside the title: a hard busy in [`Zone::Title`] (`title: running`)
/// where [`title_live`], else the screen's own.
#[must_use]
pub fn busy_seen(rows: &[String], runs: Option<bool>) -> Option<Busy> {
    if title_live(rows, runs) {
        Some(Busy::hard(Zone::Title, "running"))
    } else {
        busy_signal(rows)
    }
}

/// [`background_wait`] read beside the title: no break of the agent's own background work
/// where [`title_live`], since the title runs for a turn and not for a shell or a monitor.
#[must_use]
pub fn background_wait_seen(rows: &[String], runs: Option<bool>) -> Option<&'static str> {
    if title_live(rows, runs) {
        None
    } else {
        background_wait(rows)
    }
}

/// [`background_rest`] read beside the title: no rest where [`title_live`], since a turn the title
/// runs is no turn over under a shell or a monitor it left.
#[must_use]
pub fn background_rest_seen(rows: &[String], runs: Option<bool>) -> Option<Phase> {
    if title_live(rows, runs) {
        None
    } else {
        background_rest(rows)
    }
}

/// [`worker_phase`] read beside the title: an Idle or a Question is Busy where [`title_live`].
/// A box, a limit and a wall stand as read, as does every Busy.
#[must_use]
pub fn worker_phase_seen(rows: &[String], runs: Option<bool>) -> Phase {
    let phase = worker_phase(rows);
    if matches!(phase, Phase::Idle | Phase::Question) && title_live(rows, runs) {
        Phase::Busy
    } else {
        phase
    }
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

    /// [`Self::of`], falling back to the other hosted agents' composer rules.
    #[must_use]
    pub fn of_or_hosted(rows: &[String]) -> Self {
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
        if let Some((top, _, bottom)) = crate::kimi::composer_rows(rows) {
            return Self {
                frame: Some(Frame { top, bottom }),
                from: top,
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
    // The screen's width, measured only where a status line is read against it.
    let width = std::cell::OnceCell::new();
    let width = || *width.get_or_init(|| crate::question::screen_width(rows));
    for i in (0..top).rev() {
        if parked.is_some_and(|(first, last)| (first..=last).contains(&i)) {
            continue;
        }
        if let Some(end) = status_line_end(rows, i, top, &width) {
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

/// The last row of the status line row `i` opens, short of row `until`, in a pane `width()` cells
/// wide ([`line_end_in`]), measured only where that row opens one. Beside the side panel the line
/// is read on its rows cut at the panel's rule ([`panel_cut`]) in a pane as wide as the rule's
/// column: Claude Code lays the rows it draws beside the panel out that wide and wraps them there
/// (the measured 2.1.316 captures wrap the transcript's prose and the spinner's tip at the rule),
/// and the wrap readers take no row that draws the rule.
fn status_line_end(
    rows: &[String],
    i: usize,
    until: usize,
    width: &dyn Fn() -> usize,
) -> Option<usize> {
    if !crate::text::trim(&rows[i]).starts_with(is_spinner_glyph) {
        return None;
    }
    let Some(rule) = panel_rule(rows, i, i) else {
        return line_end_in(rows, i, until, width());
    };
    let cut: Vec<String> = rows[i..until.max(i + 1)]
        .iter()
        .map(|row| panel_cut(row, Some(rule)).to_string())
        .collect();
    line_end_in(&cut, 0, cut.len(), rule).map(|end| i + end)
}

/// The status row's line, wrapped in a narrow pane. A done row's line (`✻ Cooked for 9s · done
/// 3:33 PM`, [`is_done_shape`] as the transcript's box reads it) is read in that box
/// ([`crate::wrap::transcript_line_end`]), where the captures measure Claude Code drawing and
/// wrapping it. Any other status row's text-column wrap (a wait, a spinner whose parenthesis
/// wraps) keeps [`crate::wrap::line_end`]'s boxed room and its slack: no capture measures where
/// Claude Code wraps one, and that room joins every row the transcript's box would.
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
        let boxed = crate::wrap::transcript_line_end(rows, i, text, lead + 2, width, until);
        if is_done_shape(&crate::wrap::rejoined(&rows[i..=boxed])) {
            boxed
        } else {
            crate::wrap::line_end(rows, i, text, lead + 2, width, until)
        }
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

/// The line row `i` opens, read across the rows Ink wrapped it onto ([`crate::wrap`]): each line
/// of a `⎿` block's text in its box, which ends at the pane's edge
/// ([`crate::wrap::row_line_end`]).
pub(crate) fn line_at(rows: &[String], i: usize, until: usize, width: usize) -> (usize, String) {
    let end = if rows[i].starts_with(['⏺', '●']) {
        message_end(rows, i, until)
    } else {
        crate::wrap::row_line_end(rows, i, width, until)
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
#[path = "phase_tests.rs"]
mod tests;
