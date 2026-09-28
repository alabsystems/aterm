// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Codex's screen grammar, MEASURED on codex 0.156.1 (2026-09-24, a private
//! headless aterm at 120×40; fixtures in [`fixtures`], each naming its
//! provenance on line 1). [`crate::reader::CodexReader`] is this module
//! behind the reader trait; nothing here reads Claude Code's screen.
//!
//! **THE LAYOUT.** Codex draws inline (no alternate screen), so the screen is
//! the transcript's tail with the live zone under it:
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
//! A turn ENDS on an end row under its last block — `  1:39 PM`, or `
//! Worked for 1m 55s · 1:43 PM` after a long one — or on a `■` row (`■
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
//! | question | `Question 1/1 (1 unanswered)` | the model's answers, the recommended one `(Recommended)`, last `None of the above` | `tab to add notes \| enter to submit answer \| esc to interrupt` |
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
//! **WALLS** are `■` rows, read from the 0.156.1 binary's strings (none was
//! reachable on the probe's account): [`WALLS`]. Everything a wall row does
//! not name — a turn that ended on another error — is an ended turn with no
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

/// The composer: the LAST `›` row in column 0 with no transcript block under
/// it (the slash popup's rows, also `›` in column 0, are drawn ABOVE it) and
/// its footer under it — the last row on the screen, at column 2. A box's
/// option row (`› 1. Yes, proceed`) is not the composer. `None` while a box
/// is up (it replaces the composer), on a screen Codex left, and on a frame
/// caught mid-redraw with the rows under a user's message still blank:
/// read as the composer, that message makes the turn BEFORE it the last one
/// and the session `idle` mid-answer — the one reading that does, and the
/// live probe's server published `idle` for 220 ms mid-answer once
/// (2026-09-24; the frame itself was not caught).
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    if codex_box(rows).is_some() {
        return None;
    }
    let c = rows.iter().rposition(|r| r.starts_with('›'))?;
    let below = &rows[c + 1..];
    let footer = below.iter().rfind(|r| !r.trim().is_empty())?;
    (leading_spaces(footer) == 2
        && !below
            .iter()
            .any(|r| r.starts_with(BLOCK_GLYPHS) || r.trim_start().starts_with('└')))
    .then_some(c)
}

/// The composer's text as `(caret row, lines)` — the caret row's text first
/// (the `›` stripped; the dim placeholder when nothing is typed: the screen's
/// text cannot tell them apart, the cursor's column can — col 2 on the caret
/// row is the placeholder, measured), then every row down to the footer
/// less the one blank row that separates it. A newline in the draft is an
/// empty line: [`fixtures::DRAFT`] reads `["Fix the bug …", ""]`.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let c = composer(rows)?;
    let end = match footer_top(rows, c) {
        // The footer, and the blank row above it.
        Some(f) if f > c + 1 && rows[f - 1].trim().is_empty() => f - 1,
        Some(f) => f,
        None => c + 1,
    };
    let caret = rows[c].strip_prefix('›').unwrap_or(&rows[c]).trim();
    let mut lines = vec![caret.to_string()];
    lines.extend(rows[c + 1..end].iter().map(|r| r.trim().to_string()));
    Some((c, lines))
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
        .find(|&i| !rows[i].trim().is_empty())?;
    let mut top = last;
    while top > c + 1 && !rows[top - 1].trim().is_empty() {
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
        rows[c].contains(anchor_text("codex.composer.placeholder"))
            || rows[..c].iter().any(|r| is_status_row(r))
    })
}

/// Codex's status row while a turn runs: `• Working (6s • esc to
/// interrupt)` — a `•` block whose row ends in the Esc hint's parenthesis.
/// The header word is the model's (`Working`, or a reasoning summary's
/// title), so the row is known by its tail.
fn is_status_row(row: &str) -> bool {
    row.starts_with("• ")
        && row
            .trim_end()
            .strip_suffix(')')
            .is_some_and(|r| r.ends_with(anchor_text("codex.busy.interrupt")))
}

/// A user message's first row: `›` in column 0, above the composer. Every
/// caller reads above a [`composer`], which is `None` while a box is up, so
/// no `›` row there is a box option: a message that opens on a numbered
/// list (`› 1. Write …`) is a message.
fn is_user_row(row: &str) -> bool {
    row.starts_with('›')
}

/// An end-of-turn row at `i`: the clock (`  1:39 PM`; a 24-hour `  13:39`
/// too, by construction), or `  Worked for 1m 55s · 1:43 PM`, at column 2,
/// with a blank row above it and nothing under it but blank rows — and
/// 0.157.0's right-aligned tip ([`is_tip_row`]) — up to the next row in
/// column 0 — a message, the composer, a `•` or `■` block, a slash
/// command's echo (measured on every end). A bare clock the answer wrote
/// has the answer's row above it, or its next paragraph (column 2) under
/// it; one that is the answer's last row so far, with nothing under it, is
/// the one frame no reader can tell from an end.
fn is_end_row(rows: &[String], i: usize) -> bool {
    let row = &rows[i];
    if leading_spaces(row) != 2 || i == 0 || !rows[i - 1].trim().is_empty() {
        return false;
    }
    if rows[i + 1..]
        .iter()
        .find(|r| !r.trim().is_empty() && !is_tip_row(r))
        .is_some_and(|r| leading_spaces(r) != 0)
    {
        return false;
    }
    let t = row.trim();
    let clock = match t.strip_prefix(anchor_text("codex.turn.worked")) {
        Some(rest) => match rest.rsplit_once(" · ") {
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
    leading_spaces(row) > 2 && row.trim_start().starts_with(anchor_text("codex.tip"))
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
/// space, up to the first blank row.
fn block_text(rows: &[String], head: usize) -> String {
    let mut out = vec![rows[head].trim_start_matches('■').trim()];
    for r in rows.iter().skip(head + 1) {
        let t = r.trim();
        if t.is_empty() || r.starts_with(BLOCK_GLYPHS) || r.starts_with('›') {
            break;
        }
        out.push(t);
    }
    out.join(" ")
}

/// Where the last turn stands, read above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// The status row is up, or a user message has no end under it.
    Running,
    /// The row the last turn ended on.
    Ended(usize),
    /// No turn yet: the session's card and no user message.
    Fresh,
    /// Nothing on the screen says: the message's head scrolled away.
    Unknown,
}

fn turn(rows: &[String], composer: usize) -> Turn {
    let user = (0..composer).rev().find(|&i| is_user_row(&rows[i]));
    let end = last_end(rows, composer);
    let from = user.max(end).map_or(0, |i| i + 1);
    if rows[from..composer].iter().any(|r| is_status_row(r)) {
        return Turn::Running;
    }
    match (user, end) {
        (Some(u), Some(e)) if u > e => Turn::Running,
        (Some(_), None) => Turn::Running,
        (_, Some(e)) => Turn::Ended(e),
        (None, None) if rows[..composer].iter().any(|r| r.starts_with('╭')) => Turn::Fresh,
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
    let t = row.trim_start();
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
/// running); this read leaves [`phase`] as it is and names the break.
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

/// The phase, and whether it is evidence (module header): a box is a
/// prompt; the status row, or a user message with no end row under it, is
/// busy; an ended turn is limited (a usage wall), a question (the agent's
/// last words end in `?`) or idle; a fresh session is idle. A screen with no
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
            let asks = said_tail(rows)
                .and_then(|t| t.lines().last().map(|l| l.trim_end().ends_with('?')))
                .unwrap_or(false);
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
/// Conversation interrupted - tell the model what to do differently.`
/// (measured), read as the whole block — in a pane narrower than the
/// sentence its head row wraps. The next message is theirs.
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    ended_on_block(rows)
        .is_some_and(|e| block_text(rows, e).starts_with(anchor_text("codex.turn.interrupted")))
}

/// Codex's wall rows by kind, from the 0.156.1 binary's strings (codex-rs
/// `core/src/error.rs`), first match wins. Read as a `■` block's text,
/// lowercased, `’` as `'`.
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
    ("experiencing high demand", CodexWall::Overloaded),
    ("exceeded retry limit", CodexWall::Retry),
    ("stream disconnected before completion", CodexWall::Retry),
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
    /// The retries ran out (`exceeded retry limit, last status: 429 …`) or
    /// the stream dropped: an API error, retryable by the status it names.
    Retry,
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
    let kind = match tag {
        CodexWall::Usage => WallKind::UsageSession,
        CodexWall::Model => WallKind::ModelBucket { consent: false },
        CodexWall::Spend => WallKind::Spend,
        CodexWall::Context => WallKind::Context,
        CodexWall::Auth => WallKind::Auth,
        CodexWall::Overloaded => WallKind::Overloaded,
        CodexWall::Retry => {
            let code = lower
                .split_once("last status: ")
                .and_then(|(_, s)| s.get(..3))
                .and_then(|s| s.parse::<u16>().ok());
            WallKind::ApiError {
                code,
                retryable: code.is_none_or(|c| matches!(c, 408 | 409 | 429) || c >= 500),
                cause: ApiCause::Server,
            }
        }
    };
    Some(Wall {
        kind,
        reset: reset_of(&message),
        message,
        row: head,
        placement: Placement::Banner,
    })
}

/// `… try again at Sep 24th, 2026 3:05 PM.` → `Sep 24th 3:05 PM`.
fn reset_of(message: &str) -> Option<String> {
    let needle = anchor_text("codex.wall.retry_at").to_ascii_lowercase();
    // ASCII lowering keeps every byte offset of `message`.
    let at = message.to_ascii_lowercase().rfind(&needle)?;
    let rest = message[at + needle.len()..].trim().trim_end_matches('.');
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
/// `Explored`, `Edited`, `Model changed to`, `Questions 1/1 answered`; the
/// rest from the binary's strings). A tool row is also known by the `└`
/// output under it.
const TOOL_HEADS: &[&str] = &[
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
    "Viewed Image",
    "Model changed to ",
    "Questions ",
];

/// The agent's last words: the last `•` block of the last turn (above its
/// end row), the glyph and the continuation indent stripped, one line per
/// row — the visible rows from the top when the block's head scrolled away.
/// `None` when that block is a tool row, a notice or a `■` row, when the
/// turn has not ended, or when there is no message in it.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let c = composer(rows)?;
    let Turn::Ended(end) = turn(rows, c) else {
        return None;
    };
    let head = (0..end)
        .rev()
        .find(|&i| rows[i].starts_with(BLOCK_GLYPHS) || is_user_row(&rows[i]));
    let mut out = Vec::new();
    let from = match head {
        Some(h) => {
            let body = rows[h].strip_prefix('•')?.trim();
            let tool_under = rows[h + 1..end]
                .iter()
                .find(|r| !r.trim().is_empty())
                .is_some_and(|r| r.trim_start().starts_with('└'));
            if tool_under || TOOL_HEADS.iter().any(|t| body.starts_with(t)) {
                return None;
            }
            out.push(body);
            h + 1
        }
        // The message's head scrolled away: its visible rows are what the
        // agent ended on — never `None`, which reads as "nothing asked".
        None => 0,
    };
    out.extend(
        rows[from..end]
            .iter()
            .map(|r| r.trim())
            .filter(|t| !t.is_empty()),
    );
    (!out.is_empty()).then(|| out.join("\n"))
}

/// Codex's `<n>% context left` footer item, when it shows one (from the
/// binary's strings; the probe's footer showed the model and the folder).
#[must_use]
pub fn context_left(rows: &[String]) -> Option<u8> {
    let c = composer(rows)?;
    rows[c + 1..].iter().find_map(|r| {
        let (head, _) = r.split_once(anchor_text("codex.context.left"))?;
        let digits = head.len() - head.trim_start_matches(|c: char| !c.is_ascii_digit()).len();
        let tail = &head[digits..];
        let n = tail.rsplit(|c: char| !c.is_ascii_digit()).next()?;
        n.parse().ok()
    })
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
        .rposition(|r| r.trim_start().starts_with('›') && option(r).is_some())?;
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
    let footer = (last + 1..(last + 4).min(rows.len())).find(|&i| is_hint_footer(&rows[i]))?;
    rows[footer + 1..]
        .iter()
        .all(|r| r.trim().is_empty())
        .then_some((first, footer))
}

/// A box footer: Enter and Esc named as keys, or 0.155.1's `Press enter to
/// continue`.
fn is_hint_footer(row: &str) -> bool {
    let t = row.trim().to_lowercase();
    t.starts_with("press enter") || (t.contains("enter ") && t.contains("esc "))
}

/// A wrapped option label's tail: a non-blank row indented past the
/// options' column that is not an option itself.
fn is_continuation(row: &str) -> bool {
    !row.trim().is_empty() && leading_spaces(row) >= 5 && option(row).is_none()
}

/// `› 1. Trust and continue` / `  2. Quit` / `  › 1. subtract (Recommended)
/// Uses a clear …` → `(1, "…")`, the whole text after the number.
fn option(row: &str) -> Option<(u8, String)> {
    let t = row.trim_start();
    let t = t.strip_prefix('›').map_or(t, str::trim_start);
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = t[digits.len()..].strip_prefix(". ")?;
    Some((digits.parse().ok()?, rest.trim().to_string()))
}

/// An option's label: its text up to the column gap before a description
/// (`subtract (Recommended)  Uses a clear …` → `subtract (Recommended)`),
/// less a trailing one-key shortcut (`Yes, proceed (y)` → `Yes, proceed`).
fn label_of(text: &str) -> (String, bool) {
    let (label, described) = match text.split_once("  ") {
        Some((l, _)) => (l.trim(), true),
        None => (text.trim(), false),
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
    let t = title.trim();
    if t.starts_with(anchor_text("codex.box.exec")) {
        PromptKind::Bash
    } else if t.starts_with(anchor_text("codex.box.patch")) {
        PromptKind::Edit
    } else if t == anchor_text("codex.trust.title") || t.starts_with("Do you trust the contents") {
        PromptKind::Trust
    } else if t == anchor_text("codex.plan.title") {
        PromptKind::PlanExit
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
    let once = anchor_text("codex.box.once").to_lowercase();
    match kind {
        PromptKind::Trust if l == "trust and continue" || l == "yes, continue" => Role::Trust,
        PromptKind::Trust if l == "quit" || l == "no, quit" => Role::Exit,
        PromptKind::Bash | PromptKind::Edit if l == once => Role::Once,
        // `(p)`: a prefix rule the TUI saves (`Approved command prefix
        // saved:`) — durable.
        PromptKind::Bash if l.starts_with(&anchor_text("codex.box.persist").to_lowercase()) => {
            Role::Persist
        }
        // `(a)`: these files, for the session.
        PromptKind::Edit if l.starts_with(&anchor_text("codex.box.session").to_lowercase()) => {
            Role::Session
        }
        PromptKind::Bash | PromptKind::Edit | PromptKind::PlanExit if l.starts_with("no,") => {
            Role::Deny
        }
        // The plain first yes: implement the plan as planned, in this thread.
        PromptKind::PlanExit if l == anchor_text("codex.plan.yes").to_lowercase() => Role::Once,
        // The question's answers are the model's own; its last row asks for
        // the person's own words instead, as Claude Code's free-text row does.
        PromptKind::Question if l == anchor_text("codex.question.none").to_lowercase() => {
            Role::Other
        }
        PromptKind::Question => Role::Answer,
        _ => Role::Other,
    }
}

/// The box's rows above its options: past the blank rows right above them
/// (one or two), up to under the next run of two blank rows (or a row in
/// column 0, or the top) — single blanks separate its fields, two separate
/// it from the transcript (measured on every box).
fn box_top(rows: &[String], first: usize) -> usize {
    let above = (0..first)
        .rev()
        .find(|&i| !rows[i].trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut blanks = 0;
    for i in (0..above).rev() {
        let r = &rows[i];
        if r.trim().is_empty() {
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
    let at = (top..first).find(|&i| rows[i].trim().starts_with(name))?;
    let inline = rows[at].trim()[name.len()..].trim();
    if !inline.is_empty() {
        return Some(inline.to_string());
    }
    let block: String = rows[at + 1..first]
        .iter()
        .map(|r| r.trim())
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
    let title_row = (top..first).find(|&i| !rows[i].trim().is_empty())?;
    let head_off_screen = top == 0 && title_row == 0;
    let title = rows[title_row].trim().to_string();
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
                focused: row.trim_start().starts_with('›'),
                row: i,
            });
            described_last = described;
        } else if is_continuation(row)
            && !described_last
            && let Some(o) = options.last_mut()
        {
            // A wrapped label; under a described option, the description's.
            o.label = format!("{} {}", o.label, label_of(row.trim()).0);
        }
    }
    let numbered = options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_some_and(|n| usize::from(n) == k + 1));
    let focus = options.iter().filter(|o| o.focused).count();
    if numbered && focus == 1 {
        // A box with its head cut is read by its labels alone: the approval
        // boxes' one-shot allow names them (module header's table).
        let roles_of = if head_off_screen
            && options
                .iter()
                .any(|o| o.label == anchor_text("codex.box.once"))
        {
            PromptKind::Bash
        } else {
            kind
        };
        for o in &mut options {
            o.role = role_of(roles_of, &o.label);
        }
    }

    let (command_rows, description, path) = match kind {
        PromptKind::Bash => {
            let at = (top..first).find(|&i| rows[i].trim_start().starts_with("$ "));
            let cmd: Vec<String> = at.map_or_else(Vec::new, |at| {
                rows[at..first]
                    .iter()
                    .map(|r| r.trim())
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
            let reason = field(rows, top, first, "Reason:").unwrap_or_default();
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
                .map(|r| r.trim())
                .find(|t| t.starts_with('/') || t.starts_with('~'))
                .map(str::to_string),
        ),
        PromptKind::Question => {
            let question: Vec<&str> = rows[title_row + 1..first]
                .iter()
                .map(|r| r.trim())
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
        head_off_screen,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

/// The footer's Esc: `esc quit` exits Codex (the trust gate), `esc back`
/// leaves the dialog (the plan box), `esc to cancel` refuses (an approval
/// box's option 3 carries `(esc)`), `esc to interrupt` stops the whole turn
/// (the question) — no refusal of the box; any other verb leaves the
/// dialog, as Claude Code's reader reads one. `None` when no Esc is named
/// (0.155.1's `Press enter to continue`).
fn cancel_of(footer: &str) -> Option<Cancel> {
    let lower = footer.trim().to_lowercase();
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
    /// HAND-BUILT: a turn that ended on the usage-limit row.
    pub const HIT_LIMIT: &str = include_str!("fixtures/codex-0.156.1-usage-limit.txt");
    /// 0.157.0: the folder-trust gate at the top of a 45-row pane.
    pub const TRUST_TALL_PANE: &str = include_str!("fixtures/codex-0.157.0-trust-tall-pane.txt");
    /// 0.157.0: an ended turn, the right-aligned tip under its end row, and
    /// the two-row footer (the last 40 rows of a 45-row pane).
    pub const END_OF_TURN_TIP: &str = include_str!("fixtures/codex-0.157.0-end-of-turn-tip.txt");
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
