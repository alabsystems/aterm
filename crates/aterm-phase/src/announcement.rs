// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's startup announcement under the launch card: the one notice its startup slot
//! shows (a credit offer, launch news, the auto-mode notice, ...), each row behind the quote bar
//! `▎` at column 0, with `  <n> more notice(s) hidden` under it when the vendor held others back.
//!
//! A notice that turns its accent bar off (the server's launch news, the saved-model nudge)
//! draws its rows at a 2-space indent with no bar, a blank row between its paragraphs. Such a
//! slot has no anchor of its own, so it is read by where it stands: the last notices at that
//! indent, one blank row between each block of them (its hidden count the last row), then blank
//! rows, then the composer's rule or the transcript's first row (a `!` shell command's too; a
//! hook's message, `⎿` at its head, stands straight under the slot with no blank row). One
//! blank row stands under it inline; in the fullscreen renderer, whose composer sits at the
//! screen's foot, every row down to the rule is blank until the first turn.
//!
//! Warnings (a status glyph at column 0, their hint rows at the indent) come before the slot,
//! one blank row over it, in one block or more (Claude Code sets a blank row over some warnings
//! of its own). A block opening on a status glyph, or holding any row off the indent, is the
//! warnings: none is ever the slot, and the slot under them is read past them. A first block
//! opening off the indent on anything else (a compaction's `✻` row over the files it restored)
//! is no notice, and nothing under it is read. Nor is the update notice ever the slot: one its
//! own reader refuses (its words wrapped or drifted) is still the vendor's notice and stays.
//!
//! The rows are text, so a slot is the vendor's shape, not yet its news: the emergency tip is
//! drawn at the indent over the slot as a paragraph of it would be, and an info notice (debug
//! mode, remote control, channels) takes the barred slot as an announcement does. Their inks
//! tell them apart, which the frame reads (`claude_card::announcement_inks`): a slot with its
//! bar off is read with the tip, and the frame keeps it off the news it covers.

use crate::notice::NoticePlace;
use crate::turn::{CardPlace, SAID};

/// The rows under the card (under its update notice, when one is placed) the first notice row,
/// the slot's or the warnings' over it, may stand within: the blank row the vendor leaves, and
/// one more.
const ANNOUNCEMENT_REACH: usize = 3;
/// The most rows a slot's news spans: the auto-mode notice wraps over 5 at 80 columns, 9 at 44.
/// A slot with its bar off spans the emergency tip's rows over that too ([`bare_slot`]).
const ANNOUNCEMENT_ROWS: usize = 16;
/// The most rows of one block the walk reads: the tallest slot and its hidden count, or the
/// warnings over the slot.
const BLOCK_ROWS: usize = ANNOUNCEMENT_ROWS + 1;
/// The quote bar every row of a barred slot opens on; a slot with its bar off opens on a
/// two-space indent instead.
pub const BAR: char = '\u{258E}';
/// The column the slot's words start at: after the bar and its space, or the indent.
const TEXT_COL: usize = 2;
/// The indent of the hidden-count row and of a notice with its bar off.
const INDENT: &str = "  ";

/// The announcement's bounds in rows and character columns ([`announcement_scan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnnouncementPlace {
    /// The slot's first row.
    pub top: usize,
    /// Number of the slot's rows from [`top`](Self::top), barred or at the indent.
    pub rows: usize,
    /// The column the slot's text starts at, after the bar and its space or at the indent: 2.
    pub text_col: usize,
    /// Column just past the widest of the slot's rows and its hidden-count row, trailing blanks
    /// trimmed.
    pub end: usize,
    /// The `  <n> more notice(s) hidden` row under the slot, where the vendor draws one.
    pub hidden: Option<usize>,
}

impl AnnouncementPlace {
    /// Rows the announcement spans from [`top`](Self::top), its hidden-count row included.
    #[must_use]
    pub const fn span(&self) -> usize {
        self.rows + if self.hidden.is_some() { 1 } else { 0 }
    }
}

/// Announcement scan result and the rows it read ([`announcement_scan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnnouncementScan {
    /// The first row read: under the card, or under its update notice.
    pub from: usize,
    /// The last row read (`None`: none was).
    pub read_to: Option<usize>,
    /// The last row read (`read_to`) is a transcript row or a rule: the walk met it before any
    /// slot, or under a slot with its bar off. A change there that leaves it one keeps the scan
    /// where it stands under a blank row; a hook's message straight under the slot
    /// ([`attached`]) is read again.
    pub stop: bool,
    /// The announcement, where it is the vendor's whole.
    pub place: Option<AnnouncementPlace>,
}

fn said(row: &str) -> bool {
    row.starts_with(SAID)
}

fn cells(row: &str) -> usize {
    crate::text::trim_end(row).chars().count()
}

/// A row of the slot: the bar at column 0, then a space and the notice's words, or nothing (a
/// blank line inside the notice).
fn barred(row: &str) -> bool {
    let Some(rest) = row.strip_prefix(BAR) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(' ')
}

/// A row at the indent, with words: a row of a notice with its bar off, or its hidden count.
fn indented(row: &str) -> bool {
    row.strip_prefix(INDENT)
        .and_then(|text| text.chars().next())
        .is_some_and(|c| !c.is_whitespace())
}

/// A warning's first row: one of Claude Code's status glyphs at column 0.
fn warning(row: &str) -> bool {
    row.starts_with(crate::phase::STATUS_GLYPHS)
}

/// What the vendor draws under its notices: the composer's rule or the transcript's first row,
/// a `!` shell command's or an [`attached`] one among them.
fn edge(row: &str) -> bool {
    said(row) || row.starts_with("! ") || attached(row) || crate::phase::is_rule(row)
}

/// A transcript row the vendor draws with no margin, straight under the notices as under a
/// turn: a hook's message or an attachment, `⎿` at its head. No notice draws one.
fn attached(row: &str) -> bool {
    crate::text::trim_start(row).starts_with('\u{23BF}')
}

/// The update notice's first row at the indent.
fn updated(row: &str) -> bool {
    let updated = const { crate::anchor!("notice.updated") };
    row.strip_prefix(INDENT)
        .is_some_and(|text| text.starts_with(updated))
}

/// The slot's first row: the bar, a space, and words.
fn opens(row: &str) -> bool {
    row.strip_prefix(BAR)
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|text| text.chars().next())
        .is_some_and(|c| !c.is_whitespace())
}

/// `  <n> more notice hidden` or `  <n> more notices hidden`.
fn hidden_count(row: &str) -> bool {
    let more = const { crate::anchor!("notice.more") };
    let hidden = const { crate::anchor!("notice.hidden") };
    let Some(text) = row.strip_prefix(INDENT) else {
        return false;
    };
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let Some(rest) = text[digits..].strip_prefix(more) else {
        return false;
    };
    let rest = rest.strip_prefix('s').unwrap_or(rest);
    rest.strip_prefix(hidden)
        .is_some_and(|tail| crate::text::trim_end(tail).is_empty())
}

/// Scans `rows` under `card` (under its update `notice`, when one is placed) for Claude Code's
/// startup announcement.
#[must_use]
pub fn announcement_scan(
    rows: &[String],
    card: &CardPlace,
    notice: Option<&NoticePlace>,
) -> AnnouncementScan {
    let from = match notice {
        Some(notice) => notice.top.saturating_add(notice.rows),
        None => card.top.saturating_add(card.rows),
    };
    let mut scan = AnnouncementScan {
        from,
        read_to: None,
        stop: false,
        place: None,
    };
    let reach = rows.len().min(from.saturating_add(ANNOUNCEMENT_REACH));
    for top in from..reach {
        scan.read_to = Some(top);
        let row = &rows[top];
        if edge(row) {
            scan.stop = true;
            return scan;
        }
        if cells(row) == 0 {
            continue;
        }
        scan.place = slot_from(rows, top, &mut scan);
        return scan;
    }
    scan
}

/// The slot of the notices that open on `top`, a row neither blank nor the edge: a barred slot
/// there or under the blocks of notices there, one blank row over each; else the slot with its
/// bar off the last of them end, every block of it at the indent and none over it the warnings or
/// the update notice ([`bare_slot`]).
fn slot_from(
    rows: &[String],
    top: usize,
    scan: &mut AnnouncementScan,
) -> Option<AnnouncementPlace> {
    if opens(&rows[top]) {
        return barred_slot(rows, top, scan);
    }
    if !warning(&rows[top]) && !indented(&rows[top]) {
        return None;
    }
    let (mut at, mut run) = (top, None);
    loop {
        let gap = block(rows, at, scan)?;
        let notice = rows[at..gap].iter().all(|row| indented(row)) && !updated(&rows[at]);
        run = notice.then(|| run.unwrap_or(at));
        if attached(&rows[gap]) {
            return bare_slot(rows, run?, gap, scan);
        }
        let next = rows.get(gap + 1)?;
        scan.read_to = Some(gap + 1);
        if opens(next) {
            return barred_slot(rows, gap + 1, scan);
        }
        if edge(next) || (!warning(next) && !indented(next)) {
            return bare_slot(rows, run?, gap, scan);
        }
        at = gap + 1;
    }
}

/// The slot whose barred rows open on `top`, and the hidden count under them.
fn barred_slot(
    rows: &[String],
    top: usize,
    scan: &mut AnnouncementScan,
) -> Option<AnnouncementPlace> {
    let mut end = cells(&rows[top]);
    let mut under = top + 1;
    loop {
        let row = rows.get(under)?;
        scan.read_to = Some(under);
        if !barred(row) {
            break;
        }
        if under - top >= ANNOUNCEMENT_ROWS {
            return None;
        }
        end = end.max(cells(row));
        under += 1;
    }
    let hidden = hidden_count(&rows[under]).then_some(under);
    if let Some(at) = hidden {
        end = end.max(cells(&rows[at]));
    }
    Some(AnnouncementPlace {
        top,
        rows: under - top,
        text_col: TEXT_COL,
        end,
        hidden,
    })
}

/// The row under the block of rows opening on `top`, blank or [`attached`]: at most
/// [`BLOCK_ROWS`] rows, none another edge.
fn block(rows: &[String], top: usize, scan: &mut AnnouncementScan) -> Option<usize> {
    let mut at = top;
    loop {
        let row = rows.get(at)?;
        scan.read_to = Some(at);
        if cells(row) == 0 || attached(row) {
            return Some(at);
        }
        if edge(row) || at - top >= BLOCK_ROWS {
            return None;
        }
        at += 1;
    }
}

/// The slot with its bar off: the blocks `top..gap` at the indent, the last row its hidden count
/// where it is one, standing over the edge at `gap` or under the blank rows from it. Past
/// [`ANNOUNCEMENT_ROWS`] rows it is read only as a first block (the tip) over at most that many.
fn bare_slot(
    rows: &[String],
    top: usize,
    gap: usize,
    scan: &mut AnnouncementScan,
) -> Option<AnnouncementPlace> {
    let mut below = gap;
    loop {
        let row = rows.get(below)?;
        scan.read_to = Some(below);
        if cells(row) != 0 {
            break;
        }
        below += 1;
    }
    if !edge(&rows[below]) {
        return None;
    }
    scan.stop = true;
    let hidden = (gap - top >= 2 && hidden_count(&rows[gap - 1])).then_some(gap - 1);
    let last = hidden.unwrap_or(gap);
    let news = rows[top..last]
        .iter()
        .position(|row| cells(row) == 0)
        .map_or(top, |blank| top + blank + 1);
    if last - top > ANNOUNCEMENT_ROWS && (news == top || last - news > ANNOUNCEMENT_ROWS) {
        return None;
    }
    Some(AnnouncementPlace {
        top,
        rows: last - top,
        text_col: TEXT_COL,
        end: rows[top..gap]
            .iter()
            .map(|row| cells(row))
            .max()
            .unwrap_or(0),
        hidden,
    })
}

/// Tracks the announcement after the launch card above it has scrolled off. `kept` are the rows
/// from under the card through the slot's last row as a scan under the card read them, and
/// `slot` the announcement among them (its rows counted from `kept`'s first). While `rows` open
/// on the rows `kept` ends with (the ones above gone off the screen's top, none of the rest
/// changed), returns the slot's rows still on screen: its top cut where rows went, its
/// hidden-count row kept.
#[must_use]
pub fn announcement_scrolled(
    rows: &[String],
    kept: &[String],
    slot: &AnnouncementPlace,
) -> Option<AnnouncementPlace> {
    if slot.top.checked_add(slot.span()) != Some(kept.len()) {
        return None;
    }
    (0..kept.len()).find_map(|gone| {
        let left = &kept[gone..];
        if rows.len() < left.len() || rows[..left.len()] != *left {
            return None;
        }
        let top = slot.top.saturating_sub(gone);
        let shown = slot.rows.saturating_sub(gone.saturating_sub(slot.top));
        Some(AnnouncementPlace {
            top,
            rows: shown,
            hidden: slot.hidden.map(|_| top + shown),
            ..*slot
        })
    })
}

/// Returns whether `scan` remains valid after row indices `changed` were updated.
#[must_use]
pub fn announcement_unmoved(rows: &[String], scan: &AnnouncementScan, changed: &[usize]) -> bool {
    let Some(last) = scan.read_to else {
        return true;
    };
    changed
        .iter()
        .all(|&j| j < scan.from || j > last || (scan.stop && j == last && stops(rows, j)))
}

/// Whether the edge a scan stopped on at `at` stops a fresh scan there too: any edge under a
/// blank row. An [`attached`] row straight under the slot, the one edge that stands there, is
/// read again.
fn stops(rows: &[String], at: usize) -> bool {
    let over = at.checked_sub(1).and_then(|up| rows.get(up));
    rows.get(at).is_some_and(|row| edge(row)) && over.is_some_and(|up| cells(up) == 0)
}

#[cfg(test)]
#[path = "announcement_tests.rs"]
mod tests;
