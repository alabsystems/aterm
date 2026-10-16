// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Codex's screen grammar, MEASURED on codex 0.156.1 (2026-09-24, a private
//! headless aterm at 120×40), then on 0.157.0, 0.157.1, 0.158.0, 0.159.0,
//! 0.159.2 and 0.163.0, the managed build of 2026-10-16 (its walls, logins,
//! approval boxes, question, stream cuts and compaction captured under the
//! lab's stub of the Responses API at 144×50); fixtures in [`fixtures`], each
//! naming its provenance on line 1. [`crate::reader::CodexReader`] is this
//! module behind the reader trait; nothing here reads Claude Code's screen.
//!
//! **THE LAYOUT.** Codex draws its transcript as a column of blocks — inline
//! (`--no-alt-screen`) or, by default, on the ALTERNATE screen (0.163.0:
//! `modes` says `alt_screen=true`, measured in the lab 2026-10-16) — so the
//! screen is the transcript's tail with the live zone under it:
//!
//! ```text
//! › Fix the bug in calc.py …            a user message: `›` in column 0
//! • I'll inspect calc.py …              the agent: `•` in column 0,
//!   └ Read calc.py                        a tool row's output under `└`
//! • Working (6s • esc to interrupt)     the STATUS row, while a turn runs
//! › Ask Codex to do anything            the COMPOSER: `›` in column 0, the
//!                                         placeholder DIM, the cursor at col 2
//!   GPT-5.6-Luna low · /path…           the footer
//! ```
//!
//! **THE INPUT LINE'S MARK IS `›` OR `»`** ([`CARETS`]). Codex 0.158.0 draws
//! its composer as `» Ask Codex to do anything` — U+00BB, bold, the
//! placeholder still DIM at column 2 (measured 2026-09-28 on a live goal-mode
//! session, [`fixtures::GOAL_BUSY_0_158`]; the same mark was read on 0.157.1
//! at a paused goal that morning) — while the transcript's own user rows
//! still open with `›`. A reader that knows `›` alone finds no composer
//! there, so nothing that acts on an idle point (the supervisor, the live
//! upgrade, `Upgrade now`) ever runs on that tab.
//!
//! A turn ENDS on an end row under its last block — `  1:39 PM`, or `
//! Worked for 1m 55s · 1:43 PM` after a long one (from 0.159.0, `Worked for
//! <1s • 2:58 AM` after every one, [`END_ROW_MARKS`]) — or on a `■` row (`■
//! Conversation interrupted - …` after Esc, `■ <error>` when the turn
//! failed). While the final answer STREAMS, Codex draws neither the status
//! row nor an end row (measured: eight frames at 0.3 s): a user message with
//! no end under it is a turn in flight, and [`phase`] reads it busy.
//!
//! **THE BOXES** replace the composer and the footer. Each is a block of
//! rows at column 2 under at least two blank rows (single blanks separate
//! its fields), numbered options with the `›` cursor on one, and a footer of
//! key hints naming Enter and Esc:
//!
//! | box | title | options | footer |
//! |---|---|---|---|
//! | exec approval | `Would you like to run the following command?` | `Yes, proceed (y)` · `Yes, and don't ask again for commands that start with …` (p) · `No, and tell Codex what to do differently (esc)` | `Press enter to confirm or esc to cancel` |
//! | patch approval | `Would you like to make the following edits?` | `Yes, proceed (y)` · `Yes, and don't ask again for these files (a)` · `No, …` | the same |
//! | folder trust | `Folder access` | `Trust and continue` · `Quit` | `enter continue · esc quit` |
//! | plan | `Implement this plan?` | `Yes, implement this plan` · `Yes, clear context and implement` · `No, stay in Plan mode` | `enter select · esc back` |
//! | question | `Question 1/1 (1 unanswered)` | the model's answers, the recommended one `(Recommended)`, last `None of the above` | `tab to add notes \| enter to submit answer \| esc to interrupt`; its notes open, `tab or esc to clear notes \| enter to submit answer` |
//! | rate-limit nudge (0.163.0) | `Approaching rate limits` | `Switch to <model>` · `Keep current model` · `Keep current model (never show again)` | `enter select · esc back` |
//! | limit-increase banner (0.163.0, a workspace member) | `Usage limit reached` | `Yes (y)` · `No (default) (n)` | `enter select · esc back` |
//! | `/permissions` (0.163.0) | `Update Model Permissions` | `Ask for approval` … `Read Only`, `(current)` after one | `enter select · esc back` |
//! | full-access confirmation (0.163.0, a popup over the transcript) | `Enable full access?` | `Yes, continue anyway` · `Cancel` | `enter select · esc back` |
//! | `/model` model box (0.158.0) | `Select Model and Effort` | the models, `(current)` / `(default)` after a label | `enter select · esc back` |
//! | `/model` effort box (0.158.0) | `Select Reasoning Level for <model>` | `Low` … `Extra high`, `More reasoning…` | `enter default · s session · esc back` |
//! | `/model` advanced box (0.158.0) | `Advanced Reasoning` | `Max` · `Ultra` | `enter default · s session · esc back` |
//!
//! The effort boxes' Enter SAVES the choice as Codex's default for new
//! sessions and their `s` applies it to this conversation only (the footer's
//! own words, measured 2026-09-28); on `More reasoning…` Enter opens the
//! advanced box, and on `Ultra` the footer says `enter apply`.
//!
//! A DIGIT chooses at once, with no Enter (measured on the patch box, the
//! plan box and the question); Enter chooses the option under the cursor
//! (measured on the patch box and the question); the approval boxes' letter
//! (`y`) chose the one-shot allow (measured). The FOLDER GATE is the
//! exception: its footer says `enter continue`, and a digit changes nothing
//! on it (measured 2026-09-24, the integration's live check under the host:
//! `1` left the gate up, the screen not even redrawn) — it is chosen by the
//! cursor and Enter ([`Select::ArrowsEnter`]). The question dialog is drawn
//! at column 4 (`  › 1.`), every other box at column 0.
//!
//! **TYPING INTO THE COMPOSER.** Codex holds back an Enter that arrives
//! within a burst of typed characters and inserts it as a NEWLINE (its paste
//! guard): `send <text>` then `key enter` at once left the text unsubmitted
//! with an empty second row ([`fixtures::DRAFT`]). A submit must come after
//! the burst — `turn submit=guarded:^› …` submitted every time (measured).
//!
//! **WALLS** are `■` rows, read from the 0.156.1 binary's strings and
//! MEASURED on 0.163.0 under the lab's stub (every plan's usage wall, a
//! workspace's credits and spend cap, the 401s, a cut stream, a 500 and a
//! 502): [`WALLS`]. On 0.163.0 a usage wall is followed at once by the
//! rate-limit nudge — or, for a workspace member, the limit-increase banner
//! — which replaces the composer until a key dismisses it, then by the usage
//! notice right-aligned over the composer. Everything a wall row does not
//! name — a turn that ended on another error — is an ended turn with no
//! wall.

use crate::anchors::anchor_text;
use crate::phase::{Phase, leading_spaces};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::wall::{ApiCause, Placement, Wall, WallKind};

/// The agent's and the transcript's own row glyphs, in column 0: an agent
/// block, an error or interrupt, an approval echo (`✔ You approved codex to
/// run … this time`), the session card's corners. A `›` row followed by any
/// of these is a user message, never the composer.
const BLOCK_GLYPHS: &[char] = &['•', '■', '✔', '╭', '╰'];

/// The marks Codex opens its INPUT LINE with, in column 0: `›` (0.156.1 and
/// 0.157, measured) and `»` (0.158.0, measured 2026-09-28 — module header).
/// Only the input line is ever drawn with `»`; a user message in the
/// transcript opens with `›` on every build measured ([`is_user_row`]).
pub const CARETS: [char; 2] = ['›', '»'];

/// Whether `row` opens with one of the input line's marks ([`CARETS`]).
fn is_caret_row(row: &str) -> bool {
    // Compared on bytes, as every row of the screen is.
    const _: () = assert!(CARETS.len() == 2 && CARETS[0] == '›' && CARETS[1] == '»');
    let b = row.as_bytes();
    b.starts_with("›".as_bytes()) || b.starts_with("»".as_bytes())
}

/// The composer: the LAST row in column 0 opening with an input-line mark
/// ([`CARETS`]) with no transcript block under it (the slash popup's rows,
/// also `›` in column 0, are drawn ABOVE it) and its footer under it — the
/// last row on the screen, at column 2. A box's option row (`› 1. Yes,
/// proceed`) is not the composer. `None` while a box is up (it replaces the
/// composer), on a screen Codex left, and on a frame caught mid-redraw with
/// the rows under a user's message still blank: read as the composer, that
/// message makes the turn BEFORE it the last one and the session `idle`
/// mid-answer — the one reading that does, and the live probe's server
/// published `idle` for 220 ms mid-answer once (2026-09-24; the frame itself
/// was not caught). A `›` or `»` row of the TRANSCRIPT has the transcript's
/// blocks under it, and is never the composer.
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    if codex_box(rows).is_some() {
        return None;
    }
    let c = rows.iter().rposition(|r| is_caret_row(r))?;
    let below = &rows[c + 1..];
    let footer = below.iter().rfind(|r| !crate::text::trim(r).is_empty())?;
    (leading_spaces(footer) == 2
        && !below
            .iter()
            .any(|r| r.starts_with(BLOCK_GLYPHS) || crate::text::trim_start(r).starts_with('└')))
    .then_some(c)
}

/// The composer's text as `(caret row, lines)` — the caret row's text first
/// (its mark stripped; the dim placeholder when nothing is typed: the screen's
/// text cannot tell them apart, the cursor's column can — col 2 on the caret
/// row is the placeholder, measured), then every row down to the footer
/// less the one blank row that separates it. A newline in the draft is an
/// empty line: [`fixtures::DRAFT`] reads `["Fix the bug …", ""]`.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let c = composer(rows)?;
    let end = match footer_top(rows, c) {
        // The footer, and the blank row above it.
        Some(f) if f > c + 1 && crate::text::trim(&rows[f - 1]).is_empty() => f - 1,
        Some(f) => f,
        None => c + 1,
    };
    let caret = rows[c].strip_prefix(CARETS).unwrap_or(&rows[c]).trim();
    let mut lines = vec![caret.to_string()];
    lines.extend(
        rows[c + 1..end]
            .iter()
            .map(|r| crate::text::trim(r).to_string()),
    );
    Some((c, lines))
}

/// The mark the composer's row is drawn with NOW (`›` or `»`, [`CARETS`]):
/// what a guard on text typed into it anchors to, read off the screen it was
/// judged by — a guard spelled with the other mark would never match its
/// row, and the text would be left typed and unsent. `None` with no
/// composer on the screen.
#[must_use]
pub fn caret_on(rows: &[String]) -> Option<char> {
    rows[composer(rows)?].chars().next()
}

/// The first row of the footer under the composer at `c`: the last run of
/// non-blank rows on the screen, below the caret row. 0.156.1 draws one
/// row (`GPT-5.6-Luna low · <cwd>`); 0.157.0 draws two, the status row
/// (`GPT-5.6-Sol low · <cwd> · <thread title>`) over `← for agents · ? for
/// shortcuts` (measured 2026-09-25, [`fixtures::END_OF_TURN_TIP`]) — read
/// as the last row alone, its status row was the draft's text, and every
/// idle point read as a person typing. Codex draws one blank row between
/// the draft and the footer, so a draft's rows never join the run.
fn footer_top(rows: &[String], c: usize) -> Option<usize> {
    let last = (c + 1..rows.len())
        .rev()
        .find(|&i| !crate::text::trim(&rows[i]).is_empty())?;
    let mut top = last;
    while top > c + 1 && !crate::text::trim(&rows[top - 1]).is_empty() {
        top -= 1;
    }
    Some(top)
}

/// Whether `rows` are Codex's by their layout alone, for a session whose
/// program name says nothing (none, or `node` — an npm install of Codex runs
/// under its launcher): a [`composer`] row carrying the placeholder (an
/// empty composer, at idle and while a turn runs), or the status row above
/// it. Conservative: a composer holding a draft, with no turn running, is
/// not recognised, and names nothing.
pub(crate) fn is_codex_screen(rows: &[String]) -> bool {
    composer(rows).is_some_and(|c| {
        rows[c].contains(const { anchor_text("codex.composer.placeholder") })
            || rows[..c].iter().any(|r| is_status_row(r))
    })
}

/// Codex's status row while a turn runs: `• Working (6s • esc to
/// interrupt)` — a `•` block whose Esc hint's parenthesis ends the row, or
/// is followed only by the status line's own ` · ` items (0.158.0 draws `•
/// Waiting for background terminal (7m 43s • esc to interrupt) · 1
/// background terminal running · /ps to view · /stop to close` on ONE row,
/// MEASURED read-only on a live tab 2026-09-28: read as no status row, the
/// turn above it read ended and its session idle mid-turn). The header word
/// is the model's (`Working`, or a reasoning summary's title), so the row is
/// known by its hint.
fn is_status_row(row: &str) -> bool {
    let hint = const { anchor_text("codex.busy.interrupt") };
    row.starts_with("• ")
        && row.match_indices(hint).any(|(at, _)| {
            let rest = &row[at + hint.len()..];
            rest.strip_prefix(')').is_some_and(|tail| {
                crate::text::trim_end(tail).is_empty() || tail.starts_with(" · ")
            })
        })
}

/// A user message's first row: `›` in column 0, above the composer. Every
/// caller reads above a [`composer`], which is `None` while a box is up, so
/// no `›` row there is a box option: a message that opens on a numbered
/// list (`› 1. Write …`) is a message.
fn is_user_row(row: &str) -> bool {
    row.starts_with('›')
}

/// The marks Codex draws between an end row's `Worked for <time>` and its
/// clock: ` · ` (U+00B7) through 0.158.0, ` • ` (U+2022) from 0.159.0
/// (measured 2026-09-29: `  Worked for <1s • 2:58 AM`,
/// [`fixtures::RESUMED_DAEMON_0_159`]) — which draws that row for every
/// turn, a sub-second one's too, where 0.158.0 ended the same turns of the
/// same lab on a bare clock (read before and after one relaunch). A reader
/// that knew the first mark alone found no end under the last message of a
/// 0.159.0 screen, read every ended turn as running and the session busy for
/// good, and nothing that acts on an idle point ran there again: the live
/// upgrade's carry-on after `adopted`, the relaunch's, the supervisor's own
/// (every relaunched TUI of `targo --unverified lab codex-upgrade` stood at
/// `adopted` for its 600 s).
pub const END_ROW_MARKS: [&str; 2] = [" · ", " • "];

/// An end-of-turn row at `i`: the clock (`  1:39 PM`; a 24-hour `  13:39`
/// too, by construction), or `  Worked for 1m 55s · 1:43 PM` (`•` from
/// 0.159.0, [`END_ROW_MARKS`]), at column 2, with a blank row above it and
/// nothing under it but blank rows — and
/// 0.157.0's right-aligned tip ([`is_tip_row`]) — up to the next row in
/// column 0 — a message, the composer, a `•` or `■` block, a slash
/// command's echo (measured on every end). A bare clock the answer wrote
/// has the answer's row above it, or its next paragraph (column 2) under
/// it; one that is the answer's last row so far, with nothing under it, is
/// the one frame no reader can tell from an end.
fn is_end_row(rows: &[String], i: usize) -> bool {
    let row = &rows[i];
    if leading_spaces(row) != 2 || i == 0 || !crate::text::trim(&rows[i - 1]).is_empty() {
        return false;
    }
    if rows[i + 1..]
        .iter()
        .find(|r| !crate::text::trim(r).is_empty() && !is_tip_row(r) && !is_usage_notice_row(r))
        .is_some_and(|r| leading_spaces(r) != 0)
    {
        return false;
    }
    let t = crate::text::trim(row);
    let clock = match t.strip_prefix(const { anchor_text("codex.turn.worked") }) {
        // The clock follows the LAST mark on the row, whichever it is.
        Some(rest) => match END_ROW_MARKS
            .iter()
            .filter_map(|mark| rest.rsplit_once(mark))
            .max_by_key(|(head, _)| head.len())
        {
            Some((_, clock)) => clock,
            None => return false,
        },
        None => t,
    };
    let clock = clock
        .strip_suffix(" AM")
        .or_else(|| clock.strip_suffix(" PM"))
        .unwrap_or(clock);
    let Some((h, m)) = clock.split_once(':') else {
        return false;
    };
    (1..=2).contains(&h.len())
        && m.len() == 2
        && h.chars().chain(m.chars()).all(|c| c.is_ascii_digit())
}

/// 0.157.0's tip between a turn's end row and the composer: `Tip: Use
/// /skills to list available skills …`, drawn RIGHT-ALIGNED (column 93 of a
/// 160-column pane, measured 2026-09-25, [`fixtures::END_OF_TURN_TIP`]).
/// It is known by its word and its indent past the answer's column 2, so an
/// answer's own paragraph that opens with `Tip:` (column 2) is never one:
/// read as a row under the clock, the tip made every ended turn read as
/// running, and no Codex turn end was ever continued or answered.
fn is_tip_row(row: &str) -> bool {
    leading_spaces(row) > 2
        && crate::text::trim_start(row).starts_with(const { anchor_text("codex.tip") })
}

/// The mark Codex opens its own WARNING with: the usage notice drawn
/// right-aligned over the composer ([`is_usage_notice_row`]), and the
/// warning cell in column 0 of the transcript (`⚠ Heads up, you have less
/// than 25% of your 5h limit left. Run /status for a breakdown.`, measured on
/// 0.163.0 between a turn's answer and its end row,
/// [`fixtures::NEAR_LIMIT_5H_0_163`]) — never the agent's words.
const WARNING: char = '⚠';

/// 0.163.0's USAGE NOTICE between a turn's end (or its `■` wall) and the
/// composer: `⚠ 5h limit: 24% left · resets at 12:14 PM · /status`, drawn
/// RIGHT-ALIGNED (column 92 of a 144-column pane, measured 2026-10-16,
/// [`fixtures::NEAR_LIMIT_5H_0_163`], and `only <1% left` under every usage
/// wall once its nudge was dismissed). Known by its mark and its indent past
/// the answer's column 2, as the tip is ([`is_tip_row`]): read as a row
/// under the end row, it kept every turn that crossed a usage threshold
/// running — `busy`, its end never continued.
fn is_usage_notice_row(row: &str) -> bool {
    leading_spaces(row) > 2 && crate::text::trim_start(row).starts_with(WARNING)
}

/// A WARNING CELL of the transcript ([`WARNING`] in column 0): Codex's own
/// words, which end the block above them and are never the agent's.
fn is_warning_cell(row: &str) -> bool {
    row.starts_with(WARNING)
}

/// The last thing that ENDED a turn above `before`: an end row, or the head
/// of a `■` block.
fn last_end(rows: &[String], before: usize) -> Option<usize> {
    (0..before)
        .rev()
        .find(|&i| is_end_row(rows, i) || rows[i].starts_with('■'))
}

/// A `■` block from its head: the head without the glyph and its
/// continuation rows (which wrap to column 0, measured), joined with one
/// space, up to the first blank row — or the first TIP row ([`is_tip_line`]):
/// 0.157.1 draws its `Tip: Use /statusline …` right under a usage wall with
/// no blank between, and joined in, the wall's reset read `Oct 3rd 7:59 PM.
/// Tip: Use /statusline to configure which it…`, which no clock parses — the
/// loop waited 19 minutes, not until the reset (the owner's journal,
/// 2026-09-27 20:56). A background terminal's status line
/// ([`is_background_terminal_row`]) ends it too: it is the live zone's, at
/// column 2 where a wall's own rows wrap to column 0, and drawn with no blank
/// above it (as that tip was; not measured for this line) it read as the
/// reset's tail, `Sep 25th 3:05 PM. 1 background terminal running · …`. So
/// does a warning cell in column 0 ([`is_warning_cell`]). The rows joined are
/// [`end_block_rows`]'s.
fn block_text(rows: &[String], head: usize) -> String {
    let mut out = vec![rows[head].trim_start_matches('■').trim()];
    out.extend(
        rows[head + 1..end_block_rows(rows, head)]
            .iter()
            .map(|r| crate::text::trim(r)),
    );
    out.join(" ")
}

/// A tip Codex hangs on its `└` gutter under a block (`  └ Tip: Run /review
/// to get a code review of your current changes.`: under a status row on
/// 0.158.0, under the turn's answer on 0.163.0, both measured): Codex's own
/// row, never a tool's output or the agent's words. An answer's own paragraph
/// opening `Tip:` has no gutter, and is none.
fn is_tip_under(row: &str) -> bool {
    row.starts_with(' ')
        && crate::text::trim_start(row)
            .strip_prefix('└')
            .is_some_and(|t| {
                t.trim_start()
                    .starts_with(const { anchor_text("codex.tip") })
            })
}

/// One past the last row of the `■` block whose head is row `head`: the head
/// and its continuation rows (which wrap to column 0, measured), up to the
/// first blank row, the next block or caret row, a warning cell in column 0
/// ([`is_warning_cell`]), a TIP row (`Tip: …` however indented, or `└ Tip:
/// …`) or the background terminals' status line
/// ([`is_background_terminal_row`]) — never a row the block does not own: a
/// wall's reset read with the tip 0.157.1 drew right under it parsed as no
/// clock. The rows a wall's words are read from, and what a report keeps of
/// a `■` block (`aterm drive report --final` and `--messages`).
#[must_use]
pub fn end_block_rows(rows: &[String], head: usize) -> usize {
    rows.iter()
        .enumerate()
        .skip(head + 1)
        .find(|(_, r)| {
            crate::text::trim(r).is_empty()
                || r.starts_with(BLOCK_GLYPHS)
                || is_warning_cell(r)
                || is_caret_row(r)
                || is_tip_line(r)
                || is_background_terminal_row(r)
        })
        .map_or(rows.len(), |(i, _)| i)
}

/// A row that is Codex's own TIP, however it is indented: `Tip: …`, or
/// `└ Tip: …` under a status row (measured 0.158.0) — never a wall's words.
fn is_tip_line(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    let t = t.strip_prefix('└').map_or(t, str::trim_start);
    row.starts_with(' ') && t.starts_with(const { anchor_text("codex.tip") })
}

/// Where the last turn stands, read above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// The status row is up, or a user message has no end under it.
    Running,
    /// The row the last turn ended on.
    Ended(usize),
    /// No turn yet: the session's card ([`is_card_title`]) and no user
    /// message.
    Fresh,
    /// Nothing on the screen says: the message's head scrolled away.
    Unknown,
}

/// The session card's title row at `i`, as 0.158.0 on draws the card, with
/// no box: `  >_ OpenAI Codex (v0.163.0)` at column 2, over the card's
/// folder row at column 5 (measured on 0.158.0, 0.159.2 and 0.163.0; through
/// 0.157 the card's `╭` corner says it). A reader that knew the corner alone
/// read every fresh session of those builds as idle with no evidence, and
/// nothing that acts on evidence acted there: `aterm drive task` never
/// nudged one (measured 2026-10-16 on 0.163.0). The geometry is the test,
/// never the words alone: the same title in a tool's output (column 4, under
/// `└`) or quoted in an answer is no card, and a running turn read as a
/// fresh session would be typed into.
fn is_card_title(rows: &[String], i: usize) -> bool {
    let opens_at = |row: &str, col: usize| {
        let b = row.as_bytes();
        b.len() > col && b[..col].iter().all(|&c| c == b' ') && b[col] != b' '
    };
    rows[i]
        .strip_prefix("  >_ ")
        .is_some_and(|t| t.starts_with(const { anchor_text("codex.card.title") }))
        && rows.get(i + 1).is_some_and(|r| opens_at(r, 5))
}

/// A row of the SLASH POPUP over the composer at `c`: `› /logout  log out of
/// Codex` in column 0, nothing but blank rows and other matches between it
/// and the composer, whose own text is the `/…` being typed (measured on
/// 0.163.0 with `/log` typed, [`fixtures::SLASH_LOG_0_163`]). Read as a user
/// message, a match made the fresh session's turn run, `busy` with nothing
/// asked. A slash command the transcript echoes has its answer under it.
fn is_slash_match(rows: &[String], i: usize, c: usize) -> bool {
    let typed_slash = rows[c]
        .strip_prefix(CARETS)
        .is_some_and(|t| t.trim_start().starts_with('/'));
    typed_slash
        && rows[i].starts_with("› /")
        && rows[i + 1..c]
            .iter()
            .all(|r| crate::text::trim(r).is_empty() || r.starts_with("› /"))
}

/// The session card's title row ALONE and whole: `  >_ OpenAI Codex
/// (v0.163.0)` at column 2, nothing after its version's `)`. The host's
/// alt-screen archive keeps a frame Codex drew before the card's folder row
/// (measured in the lab on 0.163.0, 2026-10-16: the title archived over a
/// second copy of it, then the folder), where [`is_card_title`]'s geometry
/// cannot hold. Read only by [`transcript_head`], where a misread drops one
/// row from a report's views; never by the phase.
fn is_card_title_row(row: &str) -> bool {
    row.strip_prefix("  >_ ")
        .and_then(|t| t.strip_prefix(const { anchor_text("codex.card.title") }))
        .and_then(|v| crate::text::trim_end(v).strip_suffix(')'))
        .is_some_and(|v| {
            !v.is_empty()
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        })
}

fn turn(rows: &[String], composer: usize) -> Turn {
    let user = (0..composer)
        .rev()
        .find(|&i| is_user_row(&rows[i]) && !is_slash_match(rows, i, composer));
    let end = last_end(rows, composer);
    let from = user.max(end).map_or(0, |i| i + 1);
    if rows[from..composer].iter().any(|r| is_status_row(r)) {
        return Turn::Running;
    }
    match (user, end) {
        (Some(u), Some(e)) if u > e => Turn::Running,
        (Some(_), None) => Turn::Running,
        (_, Some(e)) => Turn::Ended(e),
        (None, None)
            if (0..composer).any(|i| rows[i].starts_with('╭') || is_card_title(rows, i)) =>
        {
            Turn::Fresh
        }
        (None, None) => Turn::Unknown,
    }
}

/// Codex's status line while a BACKGROUND TERMINAL a finished turn left runs
/// (measured 0.157.0 2026-09-26, over the composer after a unified-exec
/// `sleep` the turn left running: `  1 background terminal running · /ps to
/// view · /stop to close`): an indented row that OPENS with the count — a
/// person's message saying the words (`› 1 background terminal …`) is none.
#[must_use]
pub fn is_background_terminal_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    row.starts_with(' ')
        && t.split_whitespace()
            .next()
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        && (t.contains(" background terminal running")
            || t.contains(" background terminals running"))
}

/// WHAT KEEPS THIS SCREEN BUSY IS A BACKGROUND TERMINAL, AND NOTHING ELSE:
/// the status line says one runs ([`is_background_terminal_row`]), and
/// without that line the screen reads an ended turn at an idle composer,
/// authoritatively. `Some(the rule)` — the turn is over and the agent's own
/// work runs on — else `None`. The line sits under the turn's end row, where
/// only blank rows and a tip may (so [`phase`] reads the turn still
/// running); this read leaves [`phase`] as it is and names the break. A `■`
/// block ends the turn whatever is drawn under it, so [`phase`] reads a wall
/// under the line as that wall (a usage limit `Limited`), never busy.
#[must_use]
pub fn background_wait(rows: &[String]) -> Option<&'static str> {
    if !rows.iter().any(|r| is_background_terminal_row(r)) {
        return None;
    }
    let rest: Vec<String> = rows
        .iter()
        .filter(|r| !is_background_terminal_row(r))
        .cloned()
        .collect();
    (phase(&rest) == (Phase::Idle, true)).then_some("a background terminal running")
}

/// THE TURN ENDED UNDER A BACKGROUND TERMINAL, HOWEVER IT ENDED: the status
/// line says one runs ([`is_background_terminal_row`]), and without that
/// line the screen reads an ENDED turn, authoritatively — idle, a question
/// (its last words end in `?`: Codex often ends on an offer, a rejected push
/// on "Should I merge them and push again?"), or a wall (`Limited`; an API
/// error's `■` block reads idle or a question, its wall read as ever).
/// `Some(that phase)`, else `None` (no line, or a turn still running beside
/// the terminal). [`background_wait`] is its idle case — the break the
/// host's own background step may take; the supervisor's save-then-wait
/// switch takes as its point the ones [`phase`] reads busy, an end row's
/// idle or question. A `■` block's end (a wall, an API error) [`phase`]
/// reads as it is with the line under it, and it is an ordinary point.
#[must_use]
pub fn ended_under_background(rows: &[String]) -> Option<Phase> {
    if !rows.iter().any(|r| is_background_terminal_row(r)) {
        return None;
    }
    let rest: Vec<String> = rows
        .iter()
        .filter(|r| !is_background_terminal_row(r))
        .cloned()
        .collect();
    match phase(&rest) {
        (p @ (Phase::Idle | Phase::Question | Phase::Limited { .. }), true) => Some(p),
        _ => None,
    }
}

/// The phase, and whether it is evidence (module header): a box is a
/// prompt; the status row, or a user message with no end row under it, is
/// busy; an ended turn is limited (a usage wall, under a background
/// terminal's line too: [`background_wait`]), a question (the agent's last
/// words end in `?`) or idle; a fresh session is idle. A screen with no
/// composer and no box, or one whose last message's head has scrolled away
/// with no end in sight, is `idle` NOT authoritatively.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    if codex_box(rows).is_some() {
        return (Phase::Prompt, true);
    }
    let Some(c) = composer(rows) else {
        return (Phase::Idle, false);
    };
    match turn(rows, c) {
        Turn::Running => (Phase::Busy, true),
        Turn::Fresh => (Phase::Idle, true),
        Turn::Unknown => (Phase::Idle, false),
        Turn::Ended(_) => {
            if let Some(w) = wall(rows).filter(|w| w.kind.reads_limited()) {
                return (
                    Phase::Limited {
                        message: w.message,
                        reset: w.reset,
                    },
                    true,
                );
            }
            let asks = said_tail(rows).is_some_and(|t| crate::turn::asks(&t));
            (if asks { Phase::Question } else { Phase::Idle }, true)
        }
    }
}

/// No turn yet: the session card and no user message above the composer
/// (the fresh session [`phase`] reads idle as evidence).
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    composer(rows).is_some_and(|c| turn(rows, c) == Turn::Fresh)
}

/// The ended turn's `■` block, when it ended on one.
fn ended_on_block(rows: &[String]) -> Option<usize> {
    let c = composer(rows)?;
    match turn(rows, c) {
        Turn::Ended(e) if rows[e].starts_with('■') => Some(e),
        _ => None,
    }
}

/// Whether a PERSON stopped the last turn with Esc: it ended on `■
/// Conversation interrupted - …` — `… tell the model what to do
/// differently.` (measured on 0.156.1), `… use /feedback if something went
/// wrong` (the strings of 0.159.0's binary), so the head is the anchor —
/// read as the whole block: in a pane narrower than the sentence its head
/// row wraps. The next message is theirs.
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    ended_on_block(rows).is_some_and(|e| {
        block_text(rows, e).starts_with(const { anchor_text("codex.turn.interrupted") })
    })
}

/// Codex's wall rows by kind, from the 0.156.1 binary's strings (codex-rs
/// `core/src/error.rs`), first match wins. Read as a `■` block's text,
/// lowercased, `’` as `'`. Each phrase is one literal of the binary (the
/// drift canary looks for it there), so a status a row names is read from
/// the row ([`CodexWall::Status`]), never spelled into a phrase.
///
/// MEASURED on 0.163.0 (2026-10-16, the lab's stub of the Responses API):
/// an API key the provider refused ends the turn on `■ unexpected status 401
/// Unauthorized: Incorrect API key provided: …` ([`fixtures::AUTH_401_API_KEY_0_163`]),
/// a 502 on `■ unexpected status 502 Bad Gateway: …` with the body it quoted
/// wrapped under it, a 500 on Codex's own words for it, `■ We’re currently
/// experiencing high demand, which may cause temporary errors.` (no status
/// on the row: [`CodexWall::HighDemand`]), and a stream cut before
/// `response.completed` on `■ stream disconnected before completion: stream
/// closed before response.completed` ([`CodexWall::CutOff`]) — each after
/// Codex's own retries, drawn as `• Reconnecting... N/5 (… • esc to
/// interrupt)` over `└ <the error>` while the turn still runs.
pub const WALLS: &[(&str, CodexWall)] = &[
    // `You've hit your usage limit for <model>. Switch to another model now,
    // or try again at …`: one model's bucket.
    ("hit your usage limit for ", CodexWall::Model),
    // `You've hit your usage limit. Upgrade to Pro (…), visit … to purchase
    // more credits or try again at 3:05 PM.` — the purchase is advice; the
    // wall is the window, which the notice does not name.
    ("hit your usage limit", CodexWall::Usage),
    ("reached your usage limit", CodexWall::Usage),
    ("usage limit reached", CodexWall::Usage),
    ("spend cap", CodexWall::Spend),
    ("out of credits", CodexWall::Spend),
    ("workspace credit limit", CodexWall::Spend),
    ("quota exceeded", CodexWall::Spend),
    (
        "ran out of room in the model's context window",
        CodexWall::Context,
    ),
    ("access token could not be refreshed", CodexWall::Auth),
    ("please sign in again", CodexWall::Auth),
    ("selected model is at capacity", CodexWall::Overloaded),
    ("experiencing high demand", CodexWall::HighDemand),
    ("exceeded retry limit", CodexWall::Retry),
    ("unexpected status ", CodexWall::Status),
    ("stream disconnected before completion", CodexWall::CutOff),
];

/// What a [`WALLS`] row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexWall {
    Usage,
    Model,
    Spend,
    Context,
    Auth,
    Overloaded,
    /// The retries ran out (`exceeded retry limit, last status: 429 …`): an
    /// API error, retryable by the status it names.
    Retry,
    /// `unexpected status <n> <reason>: …`: the HTTP status the API
    /// answered with, read off the row — 401 is the login refused
    /// ([`WallKind::Auth`]), any other an API error retryable by it.
    Status,
    /// Codex's own words for an HTTP 500 (`We’re currently experiencing high
    /// demand, which may cause temporary errors.`, codex-rs's
    /// `InternalServerError`; measured on 0.163.0 with a 500 on every try):
    /// an API error of status 500.
    HighDemand,
    /// `stream disconnected before completion: …`: the reply was cut off
    /// before it was whole ([`ApiCause::CutOff`]) — unless the row names the
    /// status it was cut on (`last status: 503`), which is the API's answer.
    CutOff,
}

/// The HTTP status the vendor's rule retries: 408, 409, 429 and every 5xx,
/// and a row that names none.
fn retryable(code: Option<u16>) -> bool {
    code.is_none_or(|c| matches!(c, 408 | 409 | 429) || c >= 500)
}

/// The three digits after `after` in `lower` (`last status: 429 …`,
/// `unexpected status 502 …`).
fn status_after(lower: &str, after: &str) -> Option<u16> {
    lower
        .split_once(after)
        .and_then(|(_, s)| s.get(..3))
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
}

/// The wall the last turn ended on: a `■` block ([`WALLS`]) that is the
/// turn's end — the vendor's row under the last thing said, so
/// [`Placement::Banner`]. The reset is the notice's `try again at <time>`,
/// its year dropped (`Sep 24th, 2026 3:05 PM` → `Sep 24th 3:05 PM`, the form
/// a clock reads without a year).
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    let head = ended_on_block(rows)?;
    let message = block_text(rows, head);
    let lower = message.replace('’', "'").to_lowercase();
    let (_, tag) = WALLS.iter().find(|(phrase, _)| lower.contains(phrase))?;
    let server = |code: Option<u16>| WallKind::ApiError {
        code,
        retryable: retryable(code),
        cause: ApiCause::Server,
    };
    let kind = match tag {
        CodexWall::Usage => WallKind::UsageSession,
        CodexWall::Model => WallKind::ModelBucket { consent: false },
        CodexWall::Spend => WallKind::Spend,
        CodexWall::Context => WallKind::Context,
        CodexWall::Auth => WallKind::Auth,
        CodexWall::Overloaded => WallKind::Overloaded,
        CodexWall::Retry => server(status_after(&lower, "last status: ")),
        CodexWall::Status => match status_after(&lower, "unexpected status ") {
            Some(401) => WallKind::Auth,
            code => server(code),
        },
        CodexWall::HighDemand => server(Some(500)),
        CodexWall::CutOff => match status_after(&lower, "last status: ") {
            Some(code) => server(Some(code)),
            None => WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::CutOff,
            },
        },
    };
    Some(Wall {
        kind,
        reset: reset_of(&message),
        message,
        row: head,
        placement: Placement::Banner,
        under_call: false,
    })
}

/// `… try again at Sep 24th, 2026 3:05 PM.` → `Sep 24th 3:05 PM`.
fn reset_of(message: &str) -> Option<String> {
    let needle = const { anchor_text("codex.wall.retry_at") }.to_ascii_lowercase();
    // ASCII lowering keeps every byte offset of `message`.
    let at = message.to_ascii_lowercase().rfind(&needle)?;
    let rest = crate::text::trim(&message[at + needle.len()..]).trim_end_matches('.');
    let words: Vec<&str> = rest
        .split_whitespace()
        .filter(|w| {
            let w = w.trim_end_matches(',');
            !(w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|w| w.trim_end_matches(','))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The heads of Codex's own rows in an agent (`•`) block that are not the
/// agent's words: its tool calls and notices (measured: `Ran`, `Running`,
/// `Explored`, `Edited`, `Model changed to`, `Questions 1/1 answered`, and on
/// 0.163.0 `Context compacted · 0s`, `Permission selection requested: Full
/// Access` and `Unrecognized command '/login'. …`; the rest from the binary's
/// strings). A tool row is also known by the `└` output under it. Each is in
/// the managed build, its trailing space aside (the vendor draws it as a span
/// of its own): the drift canary looks for every one
/// (`every_literal_codex_anchor_is_in_the_store_codex`). 0.163.0 spells the
/// image row `Viewed image`; this table said `Viewed Image`, the spelling of
/// 0.149.0's binary, which 0.163.0's does not hold.
pub const TOOL_HEADS: &[&str] = &[
    "Ran ",
    "Running ",
    "Explored",
    "Exploring",
    "Edited ",
    "Added ",
    "Deleted ",
    "Called ",
    "Calling ",
    "Searched ",
    "Searching ",
    "Waited",
    "Waiting",
    "Viewed image",
    "Model changed to ",
    "Questions ",
    "Context compacted",
    "Permission selection requested",
    "Unrecognized command",
];

/// The agent's last words: the last `•` block of the last turn (above its
/// end row), the glyph and the continuation indent stripped, one line per
/// row — the visible rows from the top when the block's head scrolled away.
/// `None` when that block is a tool row, a notice or a `■` row, when the
/// turn has not ended, or when there is no message in it. A tip Codex hangs
/// under the message (`└ Tip: Use /feedback …`, measured on 0.163.0 under a
/// whole answer, 2026-10-16) is neither a tool's output nor the agent's
/// words: read as the first, the answer had no last words and a question it
/// ended on read `idle`. Nor is the prompt Codex PINS on the alternate
/// screen's top row over an answer whose head scrolled away (measured on
/// 0.163.0 in the lab, 2026-10-16: `› Write seventy paragraphs …` drawn over
/// paragraph 51, the answer drawn right under it): the answer's visible rows
/// are its last words. Read as their head, a screen read whole (a pane short
/// enough that the tail read reaches the top row) gave no last words and read
/// `idle` under a question. A person's message ends no turn on its own — a
/// `•` or `■` block stands between it and the end row — so a `›` row on the
/// top row with no block under it and a drawn row right under it is that copy.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let c = composer(rows)?;
    let Turn::Ended(end) = turn(rows, c) else {
        return None;
    };
    let head = (0..end)
        .rev()
        .find(|&i| rows[i].starts_with(BLOCK_GLYPHS) || is_user_row(&rows[i]))
        .filter(|&h| !(h == 0 && is_pinned_prompt(rows)));
    let mut out = Vec::new();
    let from = match head {
        Some(h) => {
            let body = rows[h].strip_prefix('•')?.trim();
            // A `└ Tip: …` under the message is Codex's (0.163.0 draws one
            // under an answer, measured), never a tool's output.
            let tool_under = rows[h + 1..end]
                .iter()
                .find(|r| !crate::text::trim(r).is_empty())
                .is_some_and(|r| crate::text::trim_start(r).starts_with('└') && !is_tip_under(r));
            if tool_under || TOOL_HEADS.iter().any(|t| body.starts_with(t)) {
                return None;
            }
            out.push(body);
            h + 1
        }
        // The message's head scrolled away: its visible rows are what the
        // agent ended on — never `None`, which reads as "nothing asked" —
        // under the prompt Codex pinned over them, when it did.
        None => usize::from(is_pinned_prompt(rows)),
    };
    // The agent's words end at Codex's own: a tip under them, or a warning
    // cell (`⚠ Heads up, …`) between them and the end row — read in, the
    // warning's full stop hid a question the agent ended on.
    let stop = (from..end)
        .find(|&i| is_warning_cell(&rows[i]) || is_tip_under(&rows[i]))
        .unwrap_or(end);
    out.extend(
        rows[from..stop]
            .iter()
            .map(|r| crate::text::trim(r))
            .filter(|t| !t.is_empty()),
    );
    (!out.is_empty()).then(|| out.join("\n"))
}

/// The screen's top row is a `›` row with a drawn row right under it: on the
/// alternate screen, the prompt Codex pins over the answer it shows
/// ([`said_tail`] says when that is the reading).
fn is_pinned_prompt(rows: &[String]) -> bool {
    rows.first().is_some_and(|r| is_user_row(r))
        && rows
            .get(1)
            .is_some_and(|r| !crate::text::trim(r).is_empty())
}

/// Codex's `<n>% context left` footer item, when it shows one. MEASURED on
/// 0.163.0 (2026-10-16): the DEFAULT footer draws none at any fill (6% to
/// 99.6% of the window, idle and mid-turn, [`fixtures::CTX_DEFAULT_0_163`]);
/// with the status line emptied (`tui.status_line=[]`) the key-hint row
/// draws its own, right-aligned (`? for shortcuts … 99% context left`,
/// [`fixtures::CTX_LEFT_0_163`]). A status line configured with the context
/// items draws `Context 52% left` in the status row instead, and `/status`
/// its `Context window: 11% left (…)` row: neither is this item.
#[must_use]
pub fn context_left(rows: &[String]) -> Option<u8> {
    let c = composer(rows)?;
    rows[c + 1..].iter().find_map(|r| {
        let (head, _) = r.split_once(const { anchor_text("codex.context.left") })?;
        let digits = head.len() - head.trim_start_matches(|c: char| !c.is_ascii_digit()).len();
        let tail = &head[digits..];
        let n = tail.rsplit(|c: char| !c.is_ascii_digit()).next()?;
        n.parse().ok()
    })
}

// ---------------------------------------------------------------------------
// The transcript, as a report reads it back.
// ---------------------------------------------------------------------------

/// What a row of Codex's TRANSCRIPT opens (module header's layout), for a
/// reader that splits the rows into blocks (`aterm drive report --final`,
/// `--messages`): every head is in column 0 but an end row's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    /// `›` and a space: a person's message (its wrapped rows at column 2).
    User,
    /// A `•` block that is the agent's own words.
    Said,
    /// A `•` block that is a tool call or one of Codex's notices
    /// (its tool heads, or `└` output right under it that is no tip), an
    /// approval's echo (`✔ You approved codex to run …`, `✗ You canceled the
    /// request to run …`), or a card: one with a `╭` corner, or the session
    /// card 0.158.0 on draws with no box (`  >_ OpenAI Codex (v0.163.0)` over
    /// its folder, read as the phase reads it to tell a fresh session, or that
    /// title row alone and whole, as the alt-screen archive can keep it), whose
    /// greeting, command list and art under it are Codex's, never the agent's
    /// words.
    Tool,
    /// The live zone: a running turn's status row (`• Working (6s • esc to
    /// interrupt)`), or an EMPTY composer — its mark and its placeholder
    /// (`› Ask Codex to do anything`) — that a frame left among the rows (the
    /// host's archive keeps the frame a reply taller than the screen replaced
    /// in one jump: measured on 0.163.0 in the lab, 2026-10-16), the footer
    /// under it carrying on its block. Never a person's message.
    Status,
    /// What ENDED a turn: an end row (`  1:39 PM`, `  Worked for 1s • 7:14
    /// AM`) or a `■` block (an interrupt, an error, a wall), whose own rows
    /// wrap to column 0.
    End,
}

/// The head row `i` of `rows` opens ([`Head`]), or `None` for a row that
/// carries on the block above it: a message's wrapped rows (column 2), a
/// tool's `└` output and its detail rows, a `■` block's wrapped rows
/// (column 0, measured), a card's `│` rows.
#[must_use]
pub fn transcript_head(rows: &[String], i: usize) -> Option<Head> {
    let row = rows[i].as_str();
    if is_empty_composer(row) {
        return Some(Head::Status);
    }
    if row
        .strip_prefix('›')
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    {
        return Some(Head::User);
    }
    if let Some(body) = row.strip_prefix('•') {
        if is_status_row(row) {
            return Some(Head::Status);
        }
        let body = body.trim();
        // A tip under a message (`└ Tip: Run /review …`, measured on 0.163.0
        // under a whole answer) is Codex's, not a tool's output.
        let output_under = rows[i + 1..]
            .iter()
            .find(|r| !crate::text::trim(r).is_empty())
            .is_some_and(|r| crate::text::trim_start(r).starts_with('└') && !is_tip_under(r));
        return Some(
            if output_under || TOOL_HEADS.iter().any(|t| body.starts_with(t)) {
                Head::Tool
            } else {
                Head::Said
            },
        );
    }
    if row.starts_with('■') {
        return Some(Head::End);
    }
    if row.starts_with(['✔', '✗', '╭']) || is_card_title(rows, i) || is_card_title_row(row) {
        return Some(Head::Tool);
    }
    is_end_row(rows, i).then_some(Head::End)
}

/// An EMPTY composer's row: an input-line mark ([`CARETS`]) and the
/// placeholder alone (`› Ask Codex to do anything`) — the live zone wherever
/// it is drawn, never a person's message. A composer holding a draft cannot
/// be told from a message by its row alone.
#[must_use]
pub fn is_empty_composer(row: &str) -> bool {
    is_caret_row(row)
        && crate::text::trim(row.trim_start_matches(CARETS))
            == const { anchor_text("codex.composer.placeholder") }
}

/// Where the transcript ends on a Codex screen, by POSITION: the first row
/// of its LIVE ZONE. A box's title (the box replaces the composer); else,
/// walking up from the composer over the blank rows, the rows hanging under
/// a status row (`└ Stream disconnected …`, `└ Making room to continue.`,
/// `└ Tip: …`) and the status rows themselves — a running turn's (`•
/// Working (6s • esc to interrupt)`, `• Reconnecting... 1/5 (0s • esc to
/// interrupt)`, measured on 0.163.0) and the background terminals' status
/// line ([`is_background_terminal_row`]) — the highest status row; else the
/// composer's row. The end row and every `■` block stay: they ENDED a turn.
/// With neither a box nor a composer (a screen Codex left, a frame caught
/// mid-redraw), one past the last non-blank row.
#[must_use]
pub fn transcript_end(rows: &[String]) -> usize {
    if let Some(p) = prompt(rows) {
        return p.span.0;
    }
    let Some(c) = composer(rows) else {
        return rows
            .iter()
            .rposition(|r| !crate::text::trim(r).is_empty())
            .map_or(0, |i| i + 1);
    };
    let mut end = c;
    let mut i = c;
    while i > 0 {
        let row = &rows[i - 1];
        let t = crate::text::trim(row);
        if is_status_row(row) || is_background_terminal_row(row) {
            i -= 1;
            end = i;
        } else if t.is_empty() || (row.starts_with(' ') && t.starts_with('└')) || is_tip_row(row)
        {
            i -= 1;
        } else {
            break;
        }
    }
    end
}

// ---------------------------------------------------------------------------
// The footer: the thread's model and effort, and its goal.
// ---------------------------------------------------------------------------

/// The footer's STATUS row, trimmed: the first row of the last run of
/// non-blank rows on the screen — the run under the composer — at column 2,
/// never its KEY-HINT row ([`key_hints_row`]), and carrying a ` · `
/// separator or a goal's state (`Pursuing goal (…)`, `Goal paused (/goal
/// resume)`, …) — `GPT-6-Astra medium · ~` over `? for shortcuts` at idle
/// (0.158.0, MEASURED), `GPT-6-Astra ultra · ~/pj · <thread title> · Main
/// [default] <padding> Pursuing goal (10d 3h 2m)` while a goal runs (0.158.0,
/// a live tab), `GPT-5.6-Luna low · <cwd>` alone (0.156.1). IN A NARROW PANE
/// Codex cuts the status row's LEFT side to keep the goal's state whole, and
/// the ` · ` goes with it (MEASURED 2026-09-28, 0.158.0 in a scratch
/// headless aterm: `gpt-5 default · /pri… Pursuing goal (21s)` at 45
/// columns, `gpt-5 default… Goal paused (/goal resume)` at 45 and `gpt-5 de…
/// Goal paused (/goal resume)` at 40, each over `← for agents · ? for
/// shortcuts …`): until that day the key-hint row under it, which keeps its
/// ` · `, was read as the status row, and the paused goal as none. Read
/// whatever the composer's caret (`›` at idle, `»` while a goal turn runs),
/// so the model and the goal read mid-turn too. `None` while a box is up —
/// it is drawn where the footer was — and on a screen whose last rows are no
/// footer.
#[must_use]
pub fn footer_status(rows: &[String]) -> Option<&str> {
    if codex_box(rows).is_some() {
        return None;
    }
    let last = rows
        .iter()
        .rposition(|r| !crate::text::trim(r).is_empty())?;
    let mut top = last;
    while top > 0 && !crate::text::trim(&rows[top - 1]).is_empty() {
        top -= 1;
    }
    let names_goal = |r: &str| GOAL_STATES.iter().any(|(text, _)| r.contains(text));
    rows[top..=last]
        .iter()
        .find(|r| {
            leading_spaces(r) == 2 && !key_hints_row(r) && (r.contains(" · ") || names_goal(r))
        })
        .map(|r| crate::text::trim(r))
}

/// Whether a footer row is Codex's KEY-HINT row, the one under its status
/// row: it opens with a key and ` for ` (`? for shortcuts`, `← for agents ·
/// ? for shortcuts`, cut to `← for agents · ? for shortc  ⚠ 1 · f2` in a
/// narrow pane — 0.158.0, measured), and it keeps its ` · ` however narrow
/// the pane is ([`footer_status`]).
fn key_hints_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    ["? for ", "\u{2190} for "].iter().any(|k| t.starts_with(k))
}

/// The footer's goal states, each by its anchor's text ([`goal_state`]),
/// looked up once at compile time.
const GOAL_STATES: [(&str, CodexGoal); 7] = [
    (anchor_text("codex.goal.pursuing"), CodexGoal::Pursuing),
    (anchor_text("codex.goal.paused"), CodexGoal::Paused),
    (anchor_text("codex.goal.limited"), CodexGoal::UsageLimited),
    (anchor_text("codex.goal.stalled"), CodexGoal::Stalled),
    (anchor_text("codex.goal.achieved"), CodexGoal::Achieved),
    (anchor_text("codex.goal.unmet"), CodexGoal::Unmet),
    (anchor_text("codex.goal.abandoned"), CodexGoal::Unmet),
];

/// The reasoning efforts Codex's footer names after the model, lowercased
/// (0.158.0's effort box: Low, Medium, High, Extra high, Max, Ultra; the
/// config's `minimal`, `xhigh`, `none`).
const EFFORTS: &[&str] = &[
    "extra high",
    "xhigh",
    "minimal",
    "low",
    "medium",
    "high",
    "max",
    "ultra",
    "none",
];

/// THE THREAD'S MODEL AND EFFORT as the footer's first ` · ` field shows
/// them: `GPT-6-Astra ultra` → `("GPT-6-Astra", Some("ultra"))`, the effort
/// lowercased; a field that ends in no effort word → the model alone. It is
/// the THREAD's own setting (the owner's `~/.codex/config.toml` said astra
/// at medium while the thread ran ultra, 2026-09-28), the display name the
/// `/model` picker lists. `None` where [`footer_status`] reads nothing.
#[must_use]
pub fn footer_model(rows: &[String]) -> Option<(String, Option<String>)> {
    let field = footer_status(rows)?.split(" · ").next()?.trim();
    let lower = field.to_lowercase();
    for e in EFFORTS {
        if let Some(model) = lower
            .strip_suffix(e)
            .filter(|m| m.ends_with(' '))
            .and_then(|m| field.get(..m.len()))
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            return Some((model.to_string(), Some((*e).to_string())));
        }
    }
    (!field.is_empty() && !field.contains("  ")).then(|| (field.to_string(), None))
}

/// Codex's GOAL, as the footer's right side names it (the 0.157.1 and
/// 0.158.0 binaries; `Pursuing goal (…)` measured on a live tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexGoal {
    /// `Pursuing goal (<elapsed>)`: Codex starts its own next turn within
    /// about 10 ms of each turn's end (measured in the owner's rollout), so
    /// nothing aterm types is needed to go on.
    Pursuing,
    /// `Goal paused (/goal resume)`.
    Paused,
    /// `Goal hit usage limits (/goal resume)`: the goal stopped at a usage
    /// wall; `/goal resume` goes on with it.
    UsageLimited,
    /// `Goal stalled (/goal resume)`.
    Stalled,
    /// `Goal achieved (<usage>)`, or `Goal achieved`: the goal is complete
    /// (the vendor's `Complete`).
    Achieved,
    /// `Goal unmet (<usage>)`, or `Goal abandoned`: the goal ran out of its
    /// token budget (the vendor's `BudgetLimited`).
    Unmet,
}

/// The goal state the footer's status row names ([`CodexGoal`]); `None`
/// when it names none (no goal — or, the vendor's `mode_indicator_line`
/// taking the place first, a collaboration mode drawn there instead) or no
/// footer is read ([`footer_status`]).
#[must_use]
pub fn goal_state(rows: &[String]) -> Option<CodexGoal> {
    let status = footer_status(rows)?;
    GOAL_STATES
        .into_iter()
        .find(|(text, _)| status.contains(text))
        .map(|(_, g)| g)
}

/// THE PAUSED GOAL'S BOX (`Resume paused goal?`, [`PromptKind::GoalResume`]),
/// read for the one decider that answers it — the live upgrade's resume of a
/// goal it paused for a move: `Some((row, resume))`, the FOCUSED option's row
/// (what a guarded key is pressed under) and whether that option is the
/// resume (`Resume goal`, anchor `codex.goal.resume.yes`: Codex focuses it
/// first); `None` for any other screen, a box of this title with no single
/// focused option among them.
#[must_use]
pub fn resume_box(rows: &[String]) -> Option<(usize, bool)> {
    let p = prompt(rows).filter(|p| p.kind == PromptKind::GoalResume)?;
    let focused = p.options.iter().find(|o| o.focused)?;
    Some((
        focused.row,
        focused.label == const { anchor_text("codex.goal.resume.yes") },
    ))
}

// ---------------------------------------------------------------------------
// The boxes.
// ---------------------------------------------------------------------------

/// A Codex choice box, as `(first option row, footer row)`: an option row
/// carrying the `›` cursor (`› 1. <label>` in column 0, or at column 2 in the
/// question dialog), the options under it, and within three rows of the
/// last one a footer of key hints — one naming Enter and Esc (`enter
/// continue · esc quit`, `Press enter to confirm or esc to cancel`, `tab to
/// add notes | enter to submit answer | esc to interrupt`, all 0.156.1), or
/// `Press enter to continue` (0.155.1) — that is the screen's last row: a
/// box is the live zone, drawn in place of the composer (measured on every
/// box). A user message that opens on a numbered list has the composer
/// under it.
pub(crate) fn codex_box(rows: &[String]) -> Option<(usize, usize)> {
    let cursor = rows
        .iter()
        .rposition(|r| crate::text::trim_start(r).starts_with('›') && option(r).is_some())?;
    let first = (0..=cursor)
        .rev()
        .take_while(|&i| option(&rows[i]).is_some() || is_continuation(&rows[i]))
        .filter(|&i| option(&rows[i]).is_some())
        .last()
        .unwrap_or(cursor);
    let mut last = cursor;
    while last + 1 < rows.len()
        && (option(&rows[last + 1]).is_some() || is_continuation(&rows[last + 1]))
    {
        last += 1;
    }
    // The question's notes field, open between its options and its footer.
    let last = notes_field(rows, last).map_or(last, |(_, end)| end);
    let footer = (last + 1..(last + 4).min(rows.len())).find(|&i| is_hint_footer(&rows[i]))?;
    let below = &rows[footer + 1..];
    (below.iter().all(|r| crate::text::trim(r).is_empty())
        || leading_spaces(&rows[cursor]) > POPUP_INDENT && is_composer_zone(below))
    .then_some((first, footer))
}

/// How far past column 0 a box's cursor row is drawn when the box is a
/// POPUP over the transcript rather than the live zone: 0.163.0's `Enable
/// full access?` confirmation (`/permissions`, `3. Full Access`, Enter) is
/// drawn centred, its cursor row at column 36 of a 144-column pane, with the
/// composer and its footer still drawn under it (measured 2026-10-16,
/// [`fixtures::FULL_ACCESS_CONFIRM_0_163`]). Every box the live zone draws
/// puts its cursor at column 0, or 2 for the question's.
const POPUP_INDENT: usize = 4;

/// Whether `rows` are the composer's zone and nothing else: blank rows, one
/// input line ([`CARETS`] in column 0) and the footer at column 2 under it —
/// what a popup box leaves drawn under itself ([`POPUP_INDENT`]). Read as
/// no box, that popup's rows under the last end row read the turn as
/// running, and its `1. Yes, continue anyway` stood as `busy`.
fn is_composer_zone(rows: &[String]) -> bool {
    let mut lines = rows.iter().filter(|r| !crate::text::trim(r).is_empty());
    lines.next().is_some_and(|r| is_caret_row(r)) && lines.all(|r| leading_spaces(r) == 2)
}

/// The question dialog's NOTES FIELD, open under the options ending at
/// `last`, as `(its first row, its last row)`: one blank row, then `  ›
/// <text>` at column 2 — the placeholder `Add notes` while it is empty
/// ([`QuestionNotes`]) — and any rows its text wraps onto (indented past
/// column 2; not measured: the one text typed was one row). Measured on
/// 0.163.0 (2026-10-16): Tab on `› 3. None of the above` opens it
/// ([`fixtures::QUESTION_NOTES_OPEN_0_163`]) and its footer becomes `tab or
/// esc to clear notes | enter to submit answer`. Read as no box, the dialog
/// with its field open was no box at all: the user's message above it read
/// as the composer, and the session `idle` mid-question.
fn notes_field(rows: &[String], last: usize) -> Option<(usize, usize)> {
    let at = last + 2;
    let row = rows.get(at)?;
    let opens = crate::text::trim(rows.get(last + 1)?).is_empty()
        && leading_spaces(row) == 2
        && option(row).is_none()
        && crate::text::trim(row)
            .strip_prefix('›')
            .is_some_and(|t| t.is_empty() || t.starts_with(' '));
    if !opens {
        return None;
    }
    let mut end = at;
    while rows
        .get(end + 1)
        .is_some_and(|r| !crate::text::trim(r).is_empty() && leading_spaces(r) > 2)
    {
        end += 1;
    }
    Some((at, end))
}

/// The question's notes field as read ([`question_notes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionNotes {
    /// The field's first row (`  › <text>`): what a key typed into it is
    /// guarded on.
    pub row: usize,
    /// Its text, the `›` stripped and wrapped rows joined with one space;
    /// empty while the placeholder (anchor `codex.question.notes`) is drawn.
    pub text: String,
}

/// THE OPEN NOTES FIELD of the question on `rows` (`notes_field`); `None`
/// when no question box is up or its field is closed. A note goes with
/// `None of the above` when Enter submits it (`answer: None of the above`
/// over `note: <text>`, measured; the answer Codex sends is `["None of the
/// above", "user_note: <text>"]`). Esc in the field, or Tab, CLEARS the text
/// and closes the field, the focus staying on `None of the above` (measured:
/// Tab reopened it empty); a digit on the dialog answers at once, notes
/// closed or not reached.
#[must_use]
pub fn question_notes(rows: &[String]) -> Option<QuestionNotes> {
    let p = prompt(rows).filter(|p| p.kind == PromptKind::Question)?;
    let last = p.options.last()?.row;
    let last = (last..p.span.1)
        .take_while(|&i| i == last || is_continuation(&rows[i]))
        .last()
        .unwrap_or(last);
    let (at, end) = notes_field(rows, last)?;
    let mut text = crate::text::trim(&rows[at])
        .trim_start_matches('›')
        .trim()
        .to_string();
    if text == const { anchor_text("codex.question.notes") } {
        text.clear();
    }
    for r in &rows[at + 1..=end] {
        text.push(' ');
        text.push_str(crate::text::trim(r));
    }
    Some(QuestionNotes { row: at, text })
}

/// A box footer: Enter and Esc named as keys, or 0.155.1's `Press enter to
/// continue`.
fn is_hint_footer(row: &str) -> bool {
    let t = crate::text::trim(row).to_lowercase();
    t.starts_with("press enter") || (t.contains("enter ") && t.contains("esc "))
}

/// A wrapped option label's tail: a non-blank row indented past the
/// options' column that is not an option itself.
fn is_continuation(row: &str) -> bool {
    !crate::text::trim(row).is_empty() && leading_spaces(row) >= 5 && option(row).is_none()
}

/// `› 1. Trust and continue` / `  2. Quit` / `  › 1. subtract (Recommended)
/// Uses a clear …` → `(1, "…")`, the whole text after the number.
fn option(row: &str) -> Option<(u8, String)> {
    let t = crate::text::trim_start(row);
    let t = t.strip_prefix('›').map_or(t, str::trim_start);
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = t[digits.len()..].strip_prefix(". ")?;
    Some((digits.parse().ok()?, crate::text::trim(rest).to_string()))
}

/// An option's label: its text up to the column gap before a description
/// (`subtract (Recommended)  Uses a clear …` → `subtract (Recommended)`),
/// less a trailing one-key shortcut (`Yes, proceed (y)` → `Yes, proceed`).
fn label_of(text: &str) -> (String, bool) {
    let (label, described) = match text.split_once("  ") {
        Some((l, _)) => (crate::text::trim(l), true),
        None => (crate::text::trim(text), false),
    };
    let label = match label.rsplit_once(" (") {
        Some((head, key))
            if key.strip_suffix(')').is_some_and(|k| {
                (1..=3).contains(&k.len()) && k.chars().all(|c| c.is_ascii_lowercase())
            }) =>
        {
            head
        }
        _ => label,
    };
    (label.to_string(), described)
}

/// The box's kind, by its title (module header).
fn kind_of(title: &str) -> PromptKind {
    let t = crate::text::trim(title);
    if t.starts_with(const { anchor_text("codex.box.exec") }) {
        PromptKind::Bash
    } else if t.starts_with(const { anchor_text("codex.box.patch") }) {
        PromptKind::Edit
    } else if t == const { anchor_text("codex.trust.title") }
        || t.starts_with("Do you trust the contents")
    {
        PromptKind::Trust
    } else if t == const { anchor_text("codex.plan.title") } {
        PromptKind::PlanExit
    } else if t == const { anchor_text("codex.nudge.title") } {
        PromptKind::RateNudge
    } else if t == const { anchor_text("codex.pick.model") }
        || t.starts_with(const { anchor_text("codex.pick.effort") })
        || t == const { anchor_text("codex.pick.advanced") }
    {
        PromptKind::ModelPick
    } else if t == const { anchor_text("codex.goal.resume.title") } {
        PromptKind::GoalResume
    } else if t
        .strip_prefix("Question ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.split_once('/'))
        .is_some_and(|(a, b)| a.parse::<u8>().is_ok() && b.parse::<u8>().is_ok())
    {
        PromptKind::Question
    } else {
        PromptKind::Other
    }
}

/// What choosing `label` does in a box of `kind`, by the vendor's words
/// (module header's table). A kind this reader has not measured assigns no
/// roles.
fn role_of(kind: PromptKind, label: &str) -> Role {
    let l = label.to_lowercase();
    let once = const { anchor_text("codex.box.once") }.to_lowercase();
    match kind {
        PromptKind::Trust if l == "trust and continue" || l == "yes, continue" => Role::Trust,
        PromptKind::Trust if l == "quit" || l == "no, quit" => Role::Exit,
        PromptKind::Bash | PromptKind::Edit if l == once => Role::Once,
        // `(p)`: a prefix rule the TUI saves (`Approved command prefix
        // saved:`) — durable.
        PromptKind::Bash
            if l.starts_with(&const { anchor_text("codex.box.persist") }.to_lowercase()) =>
        {
            Role::Persist
        }
        // `(a)`: these files, for the session.
        PromptKind::Edit
            if l.starts_with(&const { anchor_text("codex.box.session") }.to_lowercase()) =>
        {
            Role::Session
        }
        PromptKind::Bash | PromptKind::Edit | PromptKind::PlanExit if l.starts_with("no,") => {
            Role::Deny
        }
        // The plain first yes: implement the plan as planned, in this thread.
        PromptKind::PlanExit if l == const { anchor_text("codex.plan.yes") }.to_lowercase() => {
            Role::Once
        }
        // The question's answers are the model's own; its last row asks for
        // the person's own words instead, as Claude Code's free-text row does.
        PromptKind::Question
            if l == const { anchor_text("codex.question.none") }.to_lowercase() =>
        {
            Role::Other
        }
        PromptKind::Question => Role::Answer,
        // The rate-limit nudge: its plain keep is the refusal that settles
        // nothing; the keep that never shows it again writes Codex's config
        // — a standing setting; its switch is a model change, matched by
        // label in the decider.
        PromptKind::RateNudge if l == const { anchor_text("codex.nudge.keep") }.to_lowercase() => {
            Role::Deny
        }
        PromptKind::RateNudge if l == const { anchor_text("codex.nudge.never") }.to_lowercase() => {
            Role::Persist
        }
        // The paused goal's box: leaving it paused settles nothing; its
        // resume is matched by its label in the decider (the upgrade's own
        // resume of a goal it paused, `resume_box`).
        PromptKind::GoalResume
            if l == const { anchor_text("codex.goal.resume.leave") }.to_lowercase() =>
        {
            Role::Deny
        }
        _ => Role::Other,
    }
}

/// Whether the rows between a box's title and its options ask to REQUEST A
/// LIMIT INCREASE of the workspace's owner (anchor `codex.banner.request`):
/// the `Usage limit reached` banner a workspace MEMBER is drawn when the
/// workspace is out of credits or the member is at the spend cap the owner
/// set (`Request a limit increase from your owner to continue using codex.
/// Request increase?` over `1. Yes (y)` and `› 2. No (default) (n)`,
/// measured on 0.162.1 and on 0.163.0, [`fixtures::WALL_MEMBER_CREDITS_0_163`]).
/// It replaces the composer under the wall, so the turn-end policy types
/// nothing while it stands; its `Yes` notifies the owner. Read only in a box
/// of no known kind: a question or a command's reason holding the words is
/// the model's.
fn is_request_banner(rows: &[String]) -> bool {
    rows.iter()
        .any(|r| r.contains(const { anchor_text("codex.banner.request") }))
}

/// An option's role on the limit-increase banner ([`is_request_banner`]):
/// its `No` (`No (default)`) is the refusal that settles nothing
/// ([`Role::Deny`]); its `Yes` asks a person — the workspace's owner — for
/// money, and has no role any decider presses ([`Role::Other`]), whatever
/// it is labelled.
fn banner_role(label: &str) -> Role {
    let l = label.trim().to_lowercase();
    let refuses = l
        .strip_prefix("no")
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()));
    if refuses { Role::Deny } else { Role::Other }
}

/// The box's rows above its options: past the blank rows right above them
/// (one or two), up to under the next run of two blank rows (or a row in
/// column 0, or the top) — single blanks separate its fields, two separate
/// it from the transcript (measured on every box).
fn box_top(rows: &[String], first: usize) -> usize {
    let above = (0..first)
        .rev()
        .find(|&i| !crate::text::trim(&rows[i]).is_empty())
        .map_or(0, |i| i + 1);
    let mut blanks = 0;
    for i in (0..above).rev() {
        let r = &rows[i];
        if crate::text::trim(r).is_empty() {
            blanks += 1;
            if blanks == 2 {
                return i + 2;
            }
            continue;
        }
        blanks = 0;
        if leading_spaces(r) == 0 {
            return i + 1;
        }
    }
    0
}

/// The value of a `<Field>:` row in the box (`Reason: …`, `Description: …`),
/// or the rows under a bare `Destination:` up to a blank, joined with
/// nothing (a path wraps mid-token).
fn field(rows: &[String], top: usize, first: usize, name: &str) -> Option<String> {
    let at = (top..first).find(|&i| crate::text::trim(&rows[i]).starts_with(name))?;
    let inline = crate::text::trim(&rows[at])[name.len()..].trim();
    if !inline.is_empty() {
        return Some(inline.to_string());
    }
    let block: String = rows[at + 1..first]
        .iter()
        .map(|r| crate::text::trim(r))
        .take_while(|t| !t.is_empty())
        .collect();
    (!block.is_empty()).then_some(block)
}

/// The box on `rows`, read whole ([`PromptV2`]): its title and kind, the
/// exec box's command (`$ <cmd>` and the rows under it to a blank, in
/// [`PromptV2::command_rows`]) and reason, the patch box's destination, the
/// trust gate's folder, the question's text, the options with their roles
/// (all [`Role::Other`] unless they run `1.`, `2.`, … with the cursor on
/// exactly one), chosen by digit, and the footer's Esc.
///
/// A box whose rows run to the first row read with no blank above them
/// ([`box_top`] reached the top, and that row is the box's) has its head
/// OFF the rows read ([`PromptV2::head_off_screen`]): a box taller than the
/// pane, or one a `tail=` read cut (measured 2026-09-25 on 0.157.0,
/// [`fixtures::TRUST_TALL_PANE`]: the gate drawn at the top of a 45-row
/// pane, read from its last 40 rows, was titled by its body row `model
/// request. Continue only if you trust …` and escalated as a dialog of no
/// kind). Such a box is named by nothing — kind [`PromptKind::Other`], as
/// Claude Code's reader reads one — and its options' roles are the
/// approval boxes' when their one-shot allow is among them (its labels are
/// the exec and patch boxes' own), else none.
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    let (first, footer) = codex_box(rows)?;
    let top = box_top(rows, first);
    let title_row = (top..first).find(|&i| !crate::text::trim(&rows[i]).is_empty())?;
    let head_off_screen = top == 0 && title_row == 0;
    let title = crate::text::trim(&rows[title_row]).to_string();
    let kind = if head_off_screen {
        PromptKind::Other
    } else {
        kind_of(&title)
    };

    let mut options: Vec<Opt> = Vec::new();
    let mut described_last = false;
    for (i, row) in rows.iter().enumerate().take(footer).skip(first) {
        if let Some((n, text)) = option(row) {
            let (label, described) = label_of(&text);
            options.push(Opt {
                n: Some(n),
                label,
                role: Role::Other,
                focused: crate::text::trim_start(row).starts_with('›'),
                row: i,
            });
            described_last = described;
        } else if is_continuation(row)
            && !described_last
            && let Some(o) = options.last_mut()
        {
            // A wrapped label; under a described option, the description's.
            o.label = format!("{} {}", o.label, label_of(crate::text::trim(row)).0);
        }
    }
    let numbered = options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_some_and(|n| usize::from(n) == k + 1));
    let focus = options.iter().filter(|o| o.focused).count();
    // Only a box of no known kind: words the model wrote into a box it
    // asked (a question, a command's reason) never remake its roles.
    let banner =
        kind == PromptKind::Other && !head_off_screen && is_request_banner(&rows[title_row..first]);
    if numbered && focus == 1 {
        // A box with its head cut is read by its labels alone: the approval
        // boxes' one-shot allow names them (module header's table).
        let roles_of = if head_off_screen
            && options
                .iter()
                .any(|o| o.label == const { anchor_text("codex.box.once") })
        {
            PromptKind::Bash
        } else {
            kind
        };
        for o in &mut options {
            o.role = if banner {
                banner_role(&o.label)
            } else {
                role_of(roles_of, &o.label)
            };
        }
    }

    let (command_rows, description, path) = match kind {
        PromptKind::Bash => {
            let at = (top..first).find(|&i| crate::text::trim_start(&rows[i]).starts_with("$ "));
            let cmd: Vec<String> = at.map_or_else(Vec::new, |at| {
                rows[at..first]
                    .iter()
                    .map(|r| crate::text::trim(r))
                    .take_while(|t| !t.is_empty())
                    .enumerate()
                    .map(|(k, t)| {
                        if k == 0 {
                            t.trim_start_matches('$').trim_start().to_string()
                        } else {
                            t.to_string()
                        }
                    })
                    .collect()
            });
            let reason = reason_of(rows, top, first).unwrap_or_default();
            (cmd, reason, None)
        }
        PromptKind::Edit => (
            Vec::new(),
            field(rows, top, first, "Description:").unwrap_or_default(),
            field(rows, top, first, "Destination:"),
        ),
        PromptKind::Trust => (
            Vec::new(),
            String::new(),
            rows[title_row + 1..first]
                .iter()
                .map(|r| crate::text::trim(r))
                .find(|t| t.starts_with('/') || t.starts_with('~'))
                .map(str::to_string),
        ),
        PromptKind::Question => {
            let question: Vec<&str> = rows[title_row + 1..first]
                .iter()
                .map(|r| crate::text::trim(r))
                .filter(|t| !t.is_empty())
                .collect();
            (Vec::new(), question.join(" "), None)
        }
        _ => (Vec::new(), String::new(), None),
    };
    let command = match kind {
        PromptKind::Bash => command_rows.join(" "),
        _ => path.clone().unwrap_or_default(),
    };

    Some(PromptV2 {
        kind,
        title,
        command_rows,
        description_rows: 0,
        gutter: false,
        command,
        description,
        notes: Vec::new(),
        auto_deny: None,
        path,
        options,
        select: if kind == PromptKind::Trust {
            Select::ArrowsEnter
        } else {
            Select::Digits
        },
        cancel: cancel_of(&rows[footer]),
        question: None,
        span: (title_row, footer),
        footer: Some(crate::text::trim(&rows[footer]).to_string()),
        head_off_screen,
        question_dialog: None,
        // The exec box's Reason row is its reason block: what
        // [`PromptV2::owner_review`] reads an owner's prompt rule off.
        reason_rows: match kind {
            PromptKind::Bash => reason_of(rows, top, first)
                .map(|r| vec![format!("{REASON_HEAD}{r}")])
                .unwrap_or_default(),
            _ => Vec::new(),
        },
    })
}

/// The head of the exec box's reason row, `Reason: ` (0.156.1 on).
const REASON_HEAD: &str = "Reason: ";

/// The exec box's `Reason: …` field, its rows joined with one space up to
/// the blank row that ends it (a reason wider than the pane wraps; a rule's
/// `requires approval by policy` at its end must read whole). Drawn three
/// ways on 0.163.0 (measured 2026-10-16): the model's own justification of a
/// call it escalated (`Reason: Do you want to create probe.txt outside the
/// sandbox?`), none at all for one it escalated without a justification, and
/// an OWNER'S PROMPT RULE's (`prefix_rule(…, decision="prompt")` in
/// `$CODEX_HOME/rules/`): `` Reason: `/bin/zsh -lc 'touch probe.txt'`
/// requires approval by policy``, or `` … requires approval: <the rule's
/// justification>`` — which REPLACES the model's justification, and whose box
/// offers no `(p)`: `1. Yes, proceed (y)`, `2. No, …` ([`fixtures::BOX_RULE_0_163`]).
/// The row never names the rule, its file or its pattern.
fn reason_of(rows: &[String], top: usize, first: usize) -> Option<String> {
    let head = REASON_HEAD.trim_end();
    let at = (top..first).find(|&i| crate::text::trim(&rows[i]).starts_with(head))?;
    let mut parts = vec![crate::text::trim(&rows[at])[head.len()..].trim()];
    parts.extend(
        rows[at + 1..first]
            .iter()
            .map(|r| crate::text::trim(r))
            .take_while(|t| !t.is_empty() && !t.starts_with("$ ")),
    );
    let reason = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!reason.is_empty()).then_some(reason)
}

/// The footer's Esc: `esc quit` exits Codex (the trust gate), `esc back`
/// leaves the dialog (the plan box), `esc to cancel` refuses (an approval
/// box's option 3 carries `(esc)`), `esc to interrupt` stops the whole turn
/// (the question) — no refusal of the box; any other verb leaves the
/// dialog, as Claude Code's reader reads one. `None` when no Esc is named
/// (0.155.1's `Press enter to continue`).
fn cancel_of(footer: &str) -> Option<Cancel> {
    let lower = crate::text::trim(footer).to_lowercase();
    let at = lower.find("esc ")?;
    let verb: String = lower[at + 4..]
        .split(['·', '|'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches("to ")
        .to_string();
    let effect = if verb.starts_with("quit") || verb.starts_with("exit") {
        CancelEffect::Exit
    } else if ["cancel", "reject", "deny"]
        .iter()
        .any(|k| verb.starts_with(k))
    {
        CancelEffect::Reject
    } else if verb.starts_with("interrupt") {
        CancelEffect::Interrupt
    } else {
        CancelEffect::Back
    };
    Some(Cancel {
        key: "esc".to_string(),
        verb,
        effect,
    })
}

/// Codex's screens, measured (and one hand-built from the binary), for this
/// crate's tests and a dependent crate's `#[cfg(test)]` code; not part of
/// the documented API. Read them with [`crate::prompt::fixtures::screen`].
#[doc(hidden)]
pub mod fixtures {
    /// The folder-trust gate (2026-09-23).
    pub const TRUST: &str = include_str!("fixtures/codex-0.156.1-trust.txt");
    /// A fresh session at its idle composer.
    pub const IDLE: &str = include_str!("fixtures/codex-0.156.1-idle.txt");
    /// Mid-turn: the status row over the composer.
    pub const BUSY: &str = include_str!("fixtures/codex-0.156.1-busy.txt");
    /// Mid-turn, the answer streaming: no status row, no end row.
    pub const STREAMING: &str = include_str!("fixtures/codex-0.156.1-streaming.txt");
    /// The same turn, ended.
    pub const END_OF_TURN: &str = include_str!("fixtures/codex-0.156.1-end-of-turn.txt");
    /// An exec approval box.
    pub const BOX_EXEC: &str = include_str!("fixtures/codex-0.156.1-box-exec.txt");
    /// A patch approval box.
    pub const BOX_PATCH: &str = include_str!("fixtures/codex-0.156.1-box-patch.txt");
    /// Plan mode's question dialog.
    pub const QUESTION: &str = include_str!("fixtures/codex-0.156.1-question.txt");
    /// Plan mode's `Implement this plan?` box.
    pub const PLAN: &str = include_str!("fixtures/codex-0.156.1-plan.txt");
    /// A turn stopped with Esc.
    pub const INTERRUPTED: &str = include_str!("fixtures/codex-0.156.1-interrupted.txt");
    /// `/status` after an ended turn.
    pub const STATUS: &str = include_str!("fixtures/codex-0.156.1-status.txt");
    /// A turn that ended asking the user a question.
    pub const QUESTION_END: &str = include_str!("fixtures/codex-0.156.1-question-end.txt");
    /// A draft left unsubmitted by the paste guard (cursor row 31, col 2).
    pub const DRAFT: &str = include_str!("fixtures/codex-0.156.1-draft.txt");
    /// After `/quit`: the resume line, then the shell.
    pub const EXIT: &str = include_str!("fixtures/codex-0.156.1-exit.txt");
    /// 0.163.0, MEASURED (it replaced 0.156.1's hand-built wall): a turn
    /// that ended on the usage-limit row — the weekly window of a Plus plan,
    /// its reset on another day (`try again at Oct 17th, 2026 1:14 PM.`), the
    /// row wrapped to column 0 after `purchase more`, Esc pressed once on the
    /// rate-limit nudge Codex drew under it, so the usage notice (`⚠ weekly
    /// limit: only <1% left · …`) stands right-aligned over the composer.
    pub const HIT_LIMIT: &str =
        include_str!("fixtures/codex-0.163.0-wall-weekly-plus-after-esc.txt");
    /// 0.157.0: the folder-trust gate at the top of a 45-row pane.
    pub const TRUST_TALL_PANE: &str = include_str!("fixtures/codex-0.157.0-trust-tall-pane.txt");
    /// 0.157.0: an ended turn, the right-aligned tip under its end row, and
    /// the two-row footer (the last 40 rows of a 45-row pane).
    pub const END_OF_TURN_TIP: &str = include_str!("fixtures/codex-0.157.0-end-of-turn-tip.txt");
    /// 0.163.0, MEASURED (it replaced the 0.157.1 hand-built box, whose
    /// switch read `Uses fewer credits for upcoming turns`): the rate-limit
    /// model nudge after a turn whose answer said 92% of the 5h window was
    /// used, the usage warning (`⚠ Heads up, …`) in the turn above it.
    pub const RATE_NUDGE: &str = include_str!("fixtures/codex-0.163.0-nudge-5h-92pct.txt");
    /// 0.158.0: a fresh session at its `›` composer, the two-row footer.
    pub const IDLE_158: &str = include_str!("fixtures/codex-0.158.0-idle.txt");
    /// 0.158.0: `/model`'s model box.
    pub const MODEL_PICK: &str = include_str!("fixtures/codex-0.158.0-model-pick.txt");
    /// 0.158.0: the effort box of the current model.
    pub const EFFORT_PICK: &str = include_str!("fixtures/codex-0.158.0-effort-pick.txt");
    /// 0.158.0: the same, the cursor on `More reasoning…`.
    pub const EFFORT_PICK_MORE: &str = include_str!("fixtures/codex-0.158.0-effort-pick-more.txt");
    /// 0.158.0: the effort box of a model that is not the current one.
    pub const EFFORT_PICK_OTHER: &str =
        include_str!("fixtures/codex-0.158.0-effort-pick-other.txt");
    /// 0.158.0: the advanced box (`Max`, `Ultra`), the cursor on `Max`.
    pub const ADVANCED_PICK: &str = include_str!("fixtures/codex-0.158.0-advanced-pick.txt");
    /// 0.158.0: the same, the cursor on `Ultra`.
    pub const ADVANCED_PICK_ULTRA: &str =
        include_str!("fixtures/codex-0.158.0-advanced-pick-ultra.txt");
    /// 0.158.0: `/goal pause` with no goal set — a `■` row that is no wall.
    pub const GOAL_PAUSE_NONE: &str = include_str!("fixtures/codex-0.158.0-goal-pause-none.txt");
    /// HAND-BUILT around MEASURED rows (0.158.0): a goal turn running, its
    /// status row's background-terminal tail, the `└ Tip:` row, the `»`
    /// composer and the footer's `Pursuing goal (…)`. Kept hand-built: no
    /// 0.163.0 capture runs a goal.
    pub const GOAL_BUSY: &str = include_str!("fixtures/codex-0.158.0-goal-busy.txt");
    /// 0.158.0: a goal turn running, its INPUT LINE drawn `»` (U+00BB), the
    /// placeholder dim at column 2 (a live 131x64 tab, read only; the
    /// transcript's words scrubbed).
    pub const GOAL_BUSY_0_158: &str = include_str!("fixtures/codex-0.158.0-goal-busy-live.txt");
    /// 0.158.0: a goal's NEXT turn already running a second after the last
    /// one's end row — its first message streaming under that row, with no
    /// status row and no user row — which the screen reads as an ended turn
    /// (a live 131x64 tab, read only; the transcript's words scrubbed).
    pub const GOAL_NEXT_TURN_0_158: &str =
        include_str!("fixtures/codex-0.158.0-goal-next-turn.txt");
    /// HAND-BUILT from the vendor's own render of it (0.158.0, the same in
    /// 0.157.1): the `Resume paused goal?` box `codex resume` draws over a
    /// paused goal, `Resume goal` focused. Kept hand-built: no 0.163.0
    /// capture pauses a goal.
    pub const GOAL_RESUME: &str = include_str!("fixtures/codex-0.158.0-goal-resume.txt");
    /// 0.157.1, MEASURED: a turn that ended on the usage-limit row, which
    /// names no reset (a loopback model's `usage_limit_reached`).
    pub const HIT_LIMIT_NO_RESET: &str =
        include_str!("fixtures/codex-0.157.1-usage-limit-no-reset.txt");
    /// 0.157.1, MEASURED: Plan mode's question dialog, as the supervisor
    /// answered it live (a loopback model's `request_user_input`).
    pub const QUESTION_0157: &str = include_str!("fixtures/codex-0.157.1-question.txt");
    /// 0.157.1, MEASURED: the answered question, the plan and the
    /// `Implement this plan?` box, as the supervisor pressed it live.
    pub const PLAN_0157: &str = include_str!("fixtures/codex-0.157.1-plan.txt");
    /// 0.157.1, MEASURED: a daemon-mode client the live upgrade relaunched,
    /// its conversation redrawn over a BARE composer (no placeholder).
    pub const RESUMED_DAEMON: &str = include_str!("fixtures/codex-0.157.1-resumed-daemon.txt");
    /// 0.159.0, MEASURED: a daemon-mode client the live upgrade relaunched,
    /// its conversation redrawn with every end row's clock after ` • `
    /// ([`super::END_ROW_MARKS`]), the two-row footer.
    pub const RESUMED_DAEMON_0_159: &str =
        include_str!("fixtures/codex-0.159.0-resumed-daemon.txt");
    /// 0.159.0, MEASURED: an inline (`--no-alt-screen`) client the live
    /// upgrade relaunched — a long answer's tail, its user message scrolled
    /// off, the end row's clock after ` • `, the one-row footer.
    pub const RESUMED_INLINE_0_159: &str =
        include_str!("fixtures/codex-0.159.0-resumed-inline.txt");
    /// 0.159.2, MEASURED: a notice typed as a turn (the live upgrade's, its
    /// wording) and answered by the usage wall — the last block on the
    /// screen, wrapped in a 120-column pane, naming no reset.
    pub const WALL_AFTER_NOTICE_0_159: &str =
        include_str!("fixtures/codex-0.159.2-wall-after-notice.txt");
    /// 0.159.2, MEASURED: `codex resume` of that conversation — the notice
    /// and the wall redrawn from the rollout as the last block, no warning
    /// item in the footer.
    pub const WALL_RESUMED_0_159: &str = include_str!("fixtures/codex-0.159.2-wall-resumed.txt");
    /// 0.159.2, MEASURED: a turn that ran a command and then met the usage
    /// wall, in a 120-column pane.
    pub const WALL_MID_TURN_0_159: &str = include_str!("fixtures/codex-0.159.2-wall-mid-turn.txt");
    /// 0.159.2, MEASURED: the same wall in a 100x30 pane, its row wrapped
    /// after `purchase more`.
    pub const WALL_MID_TURN_100X30_0_159: &str =
        include_str!("fixtures/codex-0.159.2-wall-mid-turn-100x30.txt");
    /// 0.159.2, MEASURED: the same wall in a 160x50 pane, its row on one line.
    pub const WALL_MID_TURN_160X50_0_159: &str =
        include_str!("fixtures/codex-0.159.2-wall-mid-turn-160x50.txt");
    /// 0.159.2, MEASURED: a fresh session at its idle composer — no turn, no
    /// wall (the startup logo's rows dropped).
    pub const IDLE_0_159: &str = include_str!("fixtures/codex-0.159.2-idle.txt");
    /// 0.163.0, MEASURED: a fresh session at its composer, the session card
    /// with no box (its title row at column 2) and the launch's art.
    pub const IDLE_0_163: &str = include_str!("fixtures/codex-0.163.0-idle.txt");
    /// 0.163.0, MEASURED: a turn's first frame, its status row up.
    pub const BUSY_0_163: &str = include_str!("fixtures/codex-0.163.0-busy.txt");
    /// 0.163.0, MEASURED: the same turn, ended (`• hi`, `Worked for 1s •`).
    pub const END_OF_TURN_0_163: &str = include_str!("fixtures/codex-0.163.0-end-of-turn.txt");
    /// 0.163.0, MEASURED: an exec approval box.
    pub const BOX_EXEC_0_163: &str = include_str!("fixtures/codex-0.163.0-box-exec.txt");
    /// 0.163.0, MEASURED: a patch approval box.
    pub const BOX_PATCH_0_163: &str = include_str!("fixtures/codex-0.163.0-box-patch.txt");
    /// 0.163.0, MEASURED: the exec box declined with Esc.
    pub const INTERRUPTED_0_163: &str = include_str!("fixtures/codex-0.163.0-interrupted.txt");

    // The 0.163.0 lab captures (2026-10-16): the real Codex under a private
    // headless aterm at 144x50, a scratch `CODEX_HOME`, and the lab's stub of
    // the Responses API (`crates/aterm-lab`) answering what each line 1 says.

    /// The usage wall of a Free plan (`Upgrade to Plus …`), the rate-limit
    /// nudge Codex draws under it.
    pub const WALL_5H_FREE_0_163: &str = include_str!("fixtures/codex-0.163.0-wall-5h-free.txt");
    /// The same after one Esc: the nudge gone, the usage notice (`⚠ 5h
    /// limit: only <1% left · …`) over the composer.
    pub const WALL_5H_FREE_AFTER_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-free-after-esc.txt");
    /// The usage wall of an answer that names no plan (`Try again at …`),
    /// the nudge under it.
    pub const WALL_5H_NOPLAN_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-noplan.txt");
    /// The same after one Esc.
    pub const WALL_5H_NOPLAN_AFTER_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-noplan-after-esc.txt");
    /// The usage wall of a Plus plan (`Upgrade to Pro …, visit … to purchase
    /// more credits or try again at …`, wrapped), the nudge under it.
    pub const WALL_5H_PLUS_0_163: &str = include_str!("fixtures/codex-0.163.0-wall-5h-plus.txt");
    /// The same after one Esc.
    pub const WALL_5H_PLUS_AFTER_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-plus-after-esc.txt");
    /// The Plus wall of an account that holds credits: the same row, no
    /// nudge and no usage notice.
    pub const WALL_5H_PLUS_CREDITS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-plus-credits.txt");
    /// The same after one Esc (`esc again to edit previous message`).
    pub const WALL_5H_PLUS_CREDITS_AFTER_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-plus-credits-after-esc.txt");
    /// The usage wall of a Pro plan (`Visit … to purchase more credits or
    /// try again at …`), the nudge under it.
    pub const WALL_5H_PRO_0_163: &str = include_str!("fixtures/codex-0.163.0-wall-5h-pro.txt");
    /// The same after one Esc.
    pub const WALL_5H_PRO_AFTER_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-pro-after-esc.txt");
    /// The usage wall of a Team plan (`To get more access now, send a
    /// request to your admin or try again at …`), the nudge under it.
    pub const WALL_5H_TEAM_0_163: &str = include_str!("fixtures/codex-0.163.0-wall-5h-team.txt");
    /// The same after one Esc.
    pub const WALL_5H_TEAM_AFTER_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-5h-team-after-esc.txt");
    /// The weekly wall of a Plus plan, its reset on another day, the nudge
    /// under it ([`HIT_LIMIT`] is the same after one Esc).
    pub const WALL_WEEKLY_PLUS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-weekly-plus.txt");
    /// A workspace MEMBER whose workspace is out of credits (`Your workspace
    /// is out of credits. Ask your workspace owner to refill …`), and under it
    /// the `Usage limit reached` banner whose `Yes` asks the owner for more
    /// (`Request increase?`), `No (default)` focused.
    pub const WALL_MEMBER_CREDITS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-reached-workspace-member-credits-depleted.txt");
    /// The same after one Esc: the banner gone, the rate-limit nudge drawn.
    pub const WALL_MEMBER_CREDITS_AFTER_ESC_0_163: &str = include_str!(
        "fixtures/codex-0.163.0-wall-reached-workspace-member-credits-depleted-after-esc.txt"
    );
    /// A workspace MEMBER at the spend cap the owner set (`You hit your
    /// spend cap set by the owner of your workspace. …`), the same banner
    /// under it.
    pub const WALL_MEMBER_SPEND_CAP_0_163: &str = include_str!(
        "fixtures/codex-0.163.0-wall-reached-workspace-member-usage-limit-reached.txt"
    );
    /// The same after one Esc: the rate-limit nudge drawn.
    pub const WALL_MEMBER_SPEND_CAP_AFTER_ESC_0_163: &str = include_str!(
        "fixtures/codex-0.163.0-wall-reached-workspace-member-usage-limit-reached-after-esc.txt"
    );
    /// A workspace OWNER out of credits (`Usage limit reached. You've
    /// reached your usage limit. Increase your limits …`), the nudge under it.
    pub const WALL_OWNER_CREDITS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-reached-workspace-owner-credits-depleted.txt");
    /// The same after one Esc.
    pub const WALL_OWNER_CREDITS_AFTER_ESC_0_163: &str = include_str!(
        "fixtures/codex-0.163.0-wall-reached-workspace-owner-credits-depleted-after-esc.txt"
    );
    /// A workspace OWNER at its usage limit: the same row, the nudge under it.
    pub const WALL_OWNER_USAGE_0_163: &str =
        include_str!("fixtures/codex-0.163.0-wall-reached-workspace-owner-usage-limit-reached.txt");
    /// The same after one Esc.
    pub const WALL_OWNER_USAGE_AFTER_ESC_0_163: &str = include_str!(
        "fixtures/codex-0.163.0-wall-reached-workspace-owner-usage-limit-reached-after-esc.txt"
    );
    /// 76% of the 5h window used: the usage warning (`⚠ Heads up, you have
    /// less than 25% of your 5h limit left. …`, column 0, in the turn) and
    /// the usage notice right-aligned under the turn's end row.
    pub const NEAR_LIMIT_5H_0_163: &str =
        include_str!("fixtures/codex-0.163.0-near-limit-5h-76pct.txt");
    /// The same for the weekly window.
    pub const NEAR_LIMIT_WEEKLY_0_163: &str =
        include_str!("fixtures/codex-0.163.0-near-limit-weekly-76pct.txt");
    /// 100% of the 5h window used by an account that holds credits, the
    /// turn answered: nothing is drawn of the limit or the credits.
    pub const CREDITS_IN_USE_0_163: &str =
        include_str!("fixtures/codex-0.163.0-credits-5h-100pct-answered.txt");
    /// `/status` of a ChatGPT login (Plus) holding credits.
    pub const STATUS_CHATGPT_0_163: &str =
        include_str!("fixtures/codex-0.163.0-status-card-chatgpt-credits.txt");
    /// `/status` of the same login with its 5h window used up and credits
    /// carrying the turn.
    pub const STATUS_CHATGPT_CREDITS_IN_USE_0_163: &str =
        include_str!("fixtures/codex-0.163.0-status-card-chatgpt-credits-in-use.txt");
    /// `/status` under an API-key provider (`Limits: data not available
    /// yet`).
    pub const STATUS_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-status-card-lab-provider.txt");
    /// A status line configured with the limit items, under a ChatGPT login
    /// (`GPT-6.1-Sol default · 5h 24% left · weekly 70% left`).
    pub const STATUS_LINE_LIMITS_CHATGPT_0_163: &str =
        include_str!("fixtures/codex-0.163.0-status-line-limit-items-chatgpt.txt");
    /// The same items under an API-key provider: the model alone.
    pub const STATUS_LINE_LIMITS_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-status-line-limit-items-lab.txt");
    /// An API key the provider refused: `■ unexpected status 401
    /// Unauthorized: Incorrect API key provided: …`.
    pub const AUTH_401_API_KEY_0_163: &str =
        include_str!("fixtures/codex-0.163.0-401-api-key-provider.txt");
    /// A ChatGPT login whose refresh token expired (`■ Your access token
    /// could not be refreshed because your refresh token has expired. …`).
    pub const AUTH_401_REFRESH_EXPIRED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-401-refresh-refresh-token-expired.txt");
    /// The same, the refresh token revoked.
    pub const AUTH_401_REFRESH_INVALIDATED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-401-refresh-refresh-token-invalidated.txt");
    /// The same, the refresh token already used.
    pub const AUTH_401_REFRESH_REUSED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-401-refresh-refresh-token-reused.txt");
    /// A 401 after `auth.json` was replaced by another account's (`… because
    /// you have since logged out or signed in to another account. Please sign
    /// in again.`).
    pub const AUTH_401_ACCOUNT_CHANGED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-401-account-changed.txt");
    /// The first screen of a session with no login: the sign-in choice
    /// (`> 1. Sign in with ChatGPT`, `Press enter to continue`).
    pub const SIGN_IN_0_163: &str = include_str!("fixtures/codex-0.163.0-signin-signed-out.txt");
    /// The folder-trust gate of a first run in an untrusted repository's
    /// subdirectory.
    pub const TRUST_0_163: &str = include_str!("fixtures/codex-0.163.0-first-run-folder-trust.txt");
    /// `/login` typed and entered: `• Unrecognized command '/login'. …` —
    /// 0.163.0 has no login slash command.
    pub const SLASH_LOGIN_0_163: &str =
        include_str!("fixtures/codex-0.163.0-slash-login-unrecognized.txt");
    /// `/log` typed: the slash popup's one match, `/logout`.
    pub const SLASH_LOG_0_163: &str = include_str!("fixtures/codex-0.163.0-slash-log.txt");
    /// `/logout` and Enter: Codex left for the shell, its resume hint
    /// printed.
    pub const LOGOUT_0_163: &str = include_str!("fixtures/codex-0.163.0-logout-exits.txt");
    /// `/exit` and Enter: the shell screen with the resume hint.
    pub const EXIT_0_163: &str = include_str!("fixtures/codex-0.163.0-exit-resume-hint.txt");
    /// An owner's PROMPT RULE (`prefix_rule(pattern=["touch"],
    /// decision="prompt")`): the exec box's Reason row `` `/bin/zsh -lc 'touch
    /// probe.txt'` requires approval by policy``, and two options, no `(p)`.
    pub const BOX_RULE_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-rule-prompt-touch.txt");
    /// The same rule with a justification: `… requires approval: owner rule:
    /// touch needs a person`.
    pub const BOX_RULE_JUSTIFIED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-rule-prompt-touch-justification.txt");
    /// The same rule over a call the model escalated with its own
    /// justification: the rule's Reason row, not the model's.
    pub const BOX_RULE_OVER_ESCALATION_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-rule-prompt-touch-escalated.txt");
    /// No rule, a call the model escalated with a justification: the Reason
    /// row is the model's words, three options with `(p)`.
    pub const BOX_ESCALATED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-norules-touch-escalated.txt");
    /// No rule, an escalated call with no justification: no Reason row.
    pub const BOX_ESCALATED_NO_REASON_0_163: &str = include_str!(
        "fixtures/codex-0.163.0-approval-norules-touch-escalated-no-justification.txt"
    );
    /// A command no rule names, escalated by the model, with the touch rule
    /// loaded: the model's Reason row, three options.
    pub const BOX_ESCALATED_UNNAMED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-escalated-unnamed-mkdir.txt");
    /// No rule, a plain call: no box is drawn (the command ran sandboxed).
    pub const NO_RULE_NO_BOX_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-norules-touch-no-box.txt");
    /// The touch rule loaded, a plain call of a command it does not name: no
    /// box.
    pub const RULE_UNNAMED_NO_BOX_0_163: &str =
        include_str!("fixtures/codex-0.163.0-approval-rule-unnamed-mkdir-no-box.txt");
    /// Plan mode's question dialog as drawn, option 1 focused.
    pub const QUESTION_0_163: &str = include_str!("fixtures/codex-0.163.0-question-dialog.txt");
    /// After Down Down: `› 3. None of the above` focused, the notes closed.
    pub const QUESTION_NONE_FOCUSED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-none-focused.txt");
    /// After Tab there: the notes field open and empty (`  › Add notes`),
    /// the footer `tab or esc to clear notes | enter to submit answer`.
    pub const QUESTION_NOTES_OPEN_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-open.txt");
    /// The notes field holding typed text (`  › use the long-term option`).
    pub const QUESTION_NOTES_TYPED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-typed.txt");
    /// Enter in the notes field: the answered question in the transcript
    /// (`answer: None of the above` / `note: …`), the turn ended.
    pub const QUESTION_NOTES_SUBMITTED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-submitted.txt");
    /// The digit 3 alone: the question answered at once, with no note.
    pub const QUESTION_DIGIT_SUBMITS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-digit3-submits.txt");
    /// Esc in the notes field holding text: the note cleared and the field
    /// closed, `None of the above` still focused.
    pub const QUESTION_NOTES_ESC_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-esc.txt");
    /// Tab again after that Esc: the field reopened empty.
    pub const QUESTION_NOTES_REOPENED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-reopen-after-esc.txt");
    /// Esc in the empty notes field: closed the same way.
    pub const QUESTION_NOTES_ESC_EMPTY_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-esc-empty.txt");
    /// Tab in the notes field holding text: cleared and closed, as Esc does.
    pub const QUESTION_NOTES_TAB_CLEARS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-notes-tab-clears.txt");
    /// Esc on the dialog with its notes closed: the turn interrupted.
    pub const QUESTION_INTERRUPTED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-question-esc-dialog.txt");
    /// A stream cut before `response.completed` (HTTP 200), mid-turn: the
    /// partial answer, then `• Reconnecting... 1/5 (0s • esc to interrupt)`
    /// over `└ Stream disconnected before completion: …`.
    pub const STREAM_CUT_RETRY_1_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-text-retry-1.txt");
    /// The same at the last retry, `5/5`.
    pub const STREAM_CUT_RETRY_5_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-text-retry-5.txt");
    /// The retries ran out: `■ stream disconnected before completion: stream
    /// closed before response.completed` under the partial answer.
    pub const STREAM_CUT_FINAL_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-final.txt");
    /// A stream cut before any text, mid-turn: the retry rows alone.
    pub const STREAM_CUT_NOTEXT_RETRY_1_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-notext-retry-1.txt");
    /// The same at `5/5`.
    pub const STREAM_CUT_NOTEXT_RETRY_5_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-notext-retry-5.txt");
    /// The retries ran out with no text: the `■` row alone under the message.
    pub const STREAM_CUT_NOTEXT_FINAL_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-notext-final.txt");
    /// Two cut answers, then a whole one: mid-turn at the retry `2/5`.
    pub const STREAM_CUT_RECOVER_RETRY_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-recover-retry-2.txt");
    /// The same turn ended on the whole answer, no `■` row.
    pub const STREAM_CUT_RECOVERED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-200-recovered.txt");
    /// HTTP 500 on every try, mid-turn: `• Reconnecting... 1/5` over `└
    /// We’re currently experiencing high demand, which may cause temporary
    /// errors.`
    pub const HIGH_DEMAND_RETRY_1_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-500-retry-1.txt");
    /// The same at `2/5`.
    pub const HIGH_DEMAND_RETRY_2_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-500-retry-2.txt");
    /// The 500's retries ran out: `■ We’re currently experiencing high
    /// demand, …`.
    pub const HIGH_DEMAND_FINAL_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-500-final.txt");
    /// HTTP 502 on every try, mid-turn: `└ Unexpected status 502 Bad
    /// Gateway: …` under the retry row.
    pub const STATUS_502_RETRY_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-502-retry-1.txt");
    /// The 502's retries ran out: `■ unexpected status 502 Bad Gateway: …`,
    /// the SSE body it quotes wrapped to column 0 under it.
    pub const STATUS_502_FINAL_0_163: &str =
        include_str!("fixtures/codex-0.163.0-stream-cut-502-final.txt");
    /// The default footer after a turn at 6% of the context: no `% context
    /// left` item.
    pub const CTX_DEFAULT_0_163: &str = include_str!("fixtures/codex-0.163.0-ctx-turn1-15k.txt");
    /// A turn at 89% of the context running: the default footer, no item.
    pub const CTX_BUSY_0_163: &str = include_str!("fixtures/codex-0.163.0-ctx-busy-89pct.txt");
    /// A pre-turn auto-compaction running: `• Compacting context (0s • esc
    /// to interrupt)` over `└ Making room to continue.`
    pub const COMPACTING_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-compacting-busy.txt");
    /// The turn after that compaction ended: `• Context compacted · 0s` above
    /// its answer, `⚠ 1 warning · f2 to view` at the key-hint row's end.
    pub const COMPACTED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-turn5-99pct-compacted.txt");
    /// f2 on that warning: the warnings view, no composer.
    pub const COMPACTION_WARNING_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-compaction-warning-f2.txt");
    /// `/status` at 89% of the context (`Context window: 11% left (230K used
    /// / 258K)`).
    pub const CTX_STATUS_CARD_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-status-card-89pct.txt");
    /// `tui.status_line=[]` (not the default): the footer's own `99% context
    /// left`, right-aligned on the key-hint row.
    pub const CTX_LEFT_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-nostatusline-15k.txt");
    /// The same at 99.6%: `0% context left` beside `⚠ 1 warning · f2 to
    /// view`.
    pub const CTX_LEFT_FULL_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-nostatusline-99pct.txt");
    /// The same mid-turn after a compaction: `100% context left`.
    pub const CTX_LEFT_BUSY_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-nostatusline-busy-95pct.txt");
    /// A status line configured with the context items (not the default):
    /// `Context 52% left · Context 48% used` in the status row.
    pub const CTX_STATUS_LINE_ITEMS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-ctx-statusline-configured-50pct.txt");
    /// `/model`'s model box.
    pub const MODEL_PICK_0_163: &str = include_str!("fixtures/codex-0.163.0-model-picker.txt");
    /// `/model`'s effort box for `GPT-6-Astra`.
    pub const EFFORT_PICK_0_163: &str =
        include_str!("fixtures/codex-0.163.0-model-effort-picker.txt");
    /// The effort chosen with `s`: `• Model changed to gpt-6-astra medium for
    /// this session only` under the last turn, the footer's new model.
    pub const MODEL_CHANGED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-model-changed-session.txt");
    /// `/permissions`: the `Update Model Permissions` menu.
    pub const PERMISSIONS_0_163: &str = include_str!("fixtures/codex-0.163.0-permissions-menu.txt");
    /// `3. Full Access` and Enter: the `Enable full access?` confirmation,
    /// drawn centred over the transcript with the composer still under it.
    pub const FULL_ACCESS_CONFIRM_0_163: &str =
        include_str!("fixtures/codex-0.163.0-permissions-full-access-confirm.txt");
    /// Full access applied: `• Permission selection requested: Full Access`
    /// under the last turn.
    pub const FULL_ACCESS_APPLIED_0_163: &str =
        include_str!("fixtures/codex-0.163.0-permissions-full-access-applied.txt");
    /// `codex resume <thread> -a on-request -s read-only`: the conversation
    /// redrawn before any turn.
    pub const RESUMED_0_163: &str = include_str!("fixtures/codex-0.163.0-resumed.txt");
    /// The same after one turn.
    pub const RESUMED_AFTER_TURN_0_163: &str =
        include_str!("fixtures/codex-0.163.0-resumed-after-turn.txt");
    /// `/permissions` after that resume: `Read Only (current)`.
    pub const RESUMED_PERMISSIONS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-resumed-permissions-menu.txt");
    /// `/permissions` after a resume with no `-a`/`-s`: `Full Access
    /// (current)`, the session's own choice kept.
    pub const RESUMED_NOFLAGS_PERMISSIONS_0_163: &str =
        include_str!("fixtures/codex-0.163.0-resumed-noflags-permissions-menu.txt");

    /// 0.163.0 IN THE LAB, MEASURED (the real Codex on the ALTERNATE screen,
    /// the lab's stub of the Responses API): Plan mode's question, raised by
    /// a scripted `request_user_input` call, over a transcript holding an
    /// ended turn, a declined exec box and the switch to Plan mode.
    pub const QUESTION_LAB_0_163: &str = include_str!("fixtures/codex-0.163.0-lab-question.txt");
    /// 0.163.0 IN THE LAB, MEASURED: that question answered by `aterm drive
    /// answer`'s digit 2 (`Questions 1/1 answered`, `answer: Long-term`), the
    /// next answer with a `└ Tip:` row under it, and its end row.
    pub const QUESTION_ANSWERED_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-lab-question-answered.txt");
    /// 0.163.0 IN THE LAB, MEASURED: a second question dismissed with Esc,
    /// the turn ended on `■ Conversation interrupted - use /feedback if
    /// something went wrong`.
    pub const QUESTION_ESC_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-lab-question-esc.txt");
    /// 0.163.0 IN THE LAB, MEASURED: a reply taller than the screen (60
    /// paragraphs in one answer) drawn in one frame — the user's row pinned
    /// on the screen's top row over paragraph 40, the first 39 never drawn —
    /// its end row, the composer and the footer.
    pub const TALL_REPLY_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-lab-tall-reply.txt");
    /// 0.163.0 IN THE LAB, MEASURED: a reply taller than the screen STREAMED
    /// in small deltas (70 paragraphs) — the turn's prompt PINNED on the
    /// screen's top row over paragraph 50, while its own row and paragraphs
    /// 1 to 49 went to the host's archive (`offscreen` `back=0`) — its end
    /// row, the composer and the footer.
    pub const PINNED_PROMPT_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-lab-pinned-prompt.txt");
    /// 0.163.0 IN THE LAB, MEASURED: the same, the reply ending on a question
    /// (`Shall I write the migration plan now?`), the prompt pinned over
    /// paragraph 51.
    pub const PINNED_QUESTION_LAB_0_163: &str =
        include_str!("fixtures/codex-0.163.0-lab-pinned-question.txt");
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
