// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE'S UPDATE NOTICE under its launch card — where it sits, read
//! by its own shape, never by its words alone.
//!
//! On the first launch of each new build (its `lastReleaseNotesSeen` older
//! than the build; `CLAUDE_CODE_FORCE_FIRST_LAUNCH=<older version>` forces
//! it) Claude Code draws, under the card, a Box with `paddingLeft: 2` and two
//! texts in a column (2.1.304's `ty`/`_a` render code, decoded from the
//! store binary), MEASURED on the real 2.1.304 in the lab (the
//! `claude-2.1.304-update-notice-*` captures) and in the 2.1.282 and 2.1.283
//! captures before it:
//!
//! ```text
//!
//!  ▐▛███▛█   Claude Code v2.1.304
//! ▝▜██████▀  Opus 5.5 · API Usage Billing
//!  ▝▝   ▝▝   ~/proj
//!
//!   Updated to latest. Got 66 features, 424 bugfixes, and 126 other changes.
//!   code.claude.com/docs/en/changelog for details
//! ```
//!
//! the bold SUMMARY (anchor `notice.updated` opens it: `Updated to latest.`
//! alone, or with ` Got <the build's changes>.`), then the dim (#999999)
//! DETAILS row (anchor `notice.updated_details`: the changelog link without
//! its `https://`, then ` for details`; no OSC 8 link on either row), both
//! at column 2, one blank row under the card: the fullscreen renderer draws
//! the card at row 1 under a row of top air and the summary at row 5, the
//! inline one the card at row 0 and the summary at row 4. On a narrow pane
//! both word-wrap at the Box's column: the summary (74 cells with its
//! indent at 2.1.304's counts) onto two rows below about 74 columns, the
//! details row (47 cells) below 47, onto `…changelog for` / `details`; from
//! 34 columns down the link alone is longer than the Box (its 33 cells, the
//! Box the pane's width less its padding) and the vendor breaks it INSIDE
//! the word, filling the row to the pane's edge (`…/changelo` / `g for
//! details` at 34, `…/chang` / `elog for details` at 31: wrap-ansi's hard
//! wrap, MEASURED in the `claude-2.1.304-update-notice-narrow-*` captures).
//! An aterm harness draws its own banner over the notice on the glass
//! (`aterm-gui`'s `claude_card`), so it must know exactly where it is and
//! never take some other row for it.
//!
//! [`update_notice_scan`] walks down from the card's last row: a row that
//! opens with a transcript glyph (`❯`, `⏺`, `●`: a person spoke or
//! something was said) ends the walk with no notice — what lies under it is
//! the conversation's, a quote of the notice included; a summary head at
//! the Box's column ([`INDENT`]) within [`NOTICE_REACH`] rows starts one,
//! and its continuation rows (at most [`SUMMARY_ROWS`] in all, each at the
//! Box's column) must end on the details row, whole — on one row, or
//! word-wrapped at the Box's column onto at most [`DETAILS_ROWS`], broken
//! between its words or, where a word is longer than the Box, inside it on
//! a row filled to the pane's edge — or there is no notice: a reworded row,
//! a details row broken anywhere else, a notice cut by the screen's bottom
//! edge. A reading refused leaves the vendor's notice bare, never a banner
//! drawn wrong.
//!
//! [`update_notice_unmoved`] knows, without a scan, when a later rebuild of
//! the rows reads as this one did: the scan is a function of the rows it
//! read and nothing else, and of its stop row only through that row's
//! first glyph, so a streamed `⏺` row or a keystroke in the composer keeps
//! the reading at no cost.

use crate::turn::{CardPlace, SAID};

/// How many rows under the card are searched for the summary's head: the
/// measured notice sits one row under the card (the logo's bottom margin),
/// and a few of the vendor's one-shot notices (`Ca`) may come between.
const NOTICE_REACH: usize = 6;

/// How many rows the summary may take before the details row: it wraps at
/// the Box's column on a narrow pane (73 and 74 cells in the captures; two
/// rows at 50 and at 44 columns, measured).
const SUMMARY_ROWS: usize = 3;

/// How many rows the details row may take: whole from 47 columns up, and
/// below that word-wrapped at the Box's column onto two (`…changelog for` /
/// `details`, measured at 46 and 44 columns; the link broken inside itself
/// from 34 columns down, `…/changelo` / `g for details`, measured at 34 to
/// 31).
const DETAILS_ROWS: usize = 2;

/// The vendor Box's `paddingLeft: 2`: every row of the notice opens with it,
/// and its text starts right after.
const INDENT: &str = "  ";

/// Where the update notice sits ([`update_notice_scan`]), in rows and CHAR
/// columns of the rows as passed (one char per cell on the window's live
/// text, so there a char index is the cell's column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoticePlace {
    /// The summary's first row: the notice's top.
    pub top: usize,
    /// How many rows the notice spans from [`top`](Self::top): the summary's
    /// (one, or more where it wrapped) and the details row's (one, or two
    /// where it wrapped).
    pub rows: usize,
    /// The column the notice's text starts at (the Box's padding: 2).
    pub text_col: usize,
    /// The column just past the notice's widest row, trailing blanks
    /// trimmed (74 on the 2.1.283 captures, 73 on the 2.1.282 ones).
    pub end: usize,
}

/// One reading of the rows for the notice ([`update_notice_scan`]), with
/// what it read: what lets a later rebuild keep it without a scan
/// ([`update_notice_unmoved`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoticeScan {
    /// The first row read: the row under the card.
    pub from: usize,
    /// The last row read (`None`: none was — the card's last row is the
    /// screen's).
    pub read_to: Option<usize>,
    /// The walk ended on a transcript row (`read_to`) before any summary.
    pub stop: bool,
    /// The notice, where it is the vendor's whole.
    pub place: Option<NoticePlace>,
}

/// Whether `row` opens with a transcript glyph ([`SAID`]).
fn said(row: &str) -> bool {
    row.starts_with(SAID)
}

/// Whether `row` has text right at the Box's column ([`INDENT`]): a notice
/// row's, or a continuation of its summary.
fn at_indent(row: &str) -> Option<&str> {
    let text = row.strip_prefix(INDENT)?;
    text.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
        .then_some(text)
}

/// Whether a row of the Box's text `text`, `rest` of its words wrapped onto
/// the next row with no space between, is the vendor's HARD break of a word
/// longer than the Box (`box_cols`, the pane's width less the Box's
/// padding): the row is filled to the Box's last column, and the word it
/// broke — `text`'s last and `rest`'s first, joined — does not fit on a row
/// of its own. wrap-ansi breaks such a word there and nowhere else; a word
/// that fits is wrapped whole, between words.
fn hard_broken(text: &str, rest: &str, box_cols: usize) -> bool {
    let cells = |s: &str| s.chars().count();
    let tail = text.rsplit(' ').next().unwrap_or(text);
    let head = rest.split(' ').next().unwrap_or(rest);
    cells(text) == box_cols && cells(tail) + cells(head) > box_cols
}

/// Whether `rows` from `d` hold `details` whole at the Box's column — on one
/// row, or wrapped onto at most [`DETAILS_ROWS`] (each row's text the next
/// of its words, broken at a space, or inside a word longer than the Box on
/// a row filled to it, [`hard_broken`]; the pane `cols` wide): the rows it
/// takes (`None`: refused), and either way the last row it read (`None`:
/// `d` is past the rows' end).
fn details_from(
    rows: &[String],
    d: usize,
    details: &str,
    cols: usize,
) -> (Option<usize>, Option<usize>) {
    let box_cols = cols.saturating_sub(INDENT.len());
    let mut want = details;
    let mut read = None;
    for k in 0..DETAILS_ROWS {
        let Some(row) = rows.get(d + k) else {
            return (None, read);
        };
        read = Some(d + k);
        let Some((text, rest)) = at_indent(row)
            .map(crate::text::trim_end)
            .and_then(|text| Some((text, want.strip_prefix(text)?)))
        else {
            return (None, read);
        };
        if rest.is_empty() {
            return (Some(k + 1), read);
        }
        want = match rest.strip_prefix(' ') {
            Some(rest) => rest,
            None if hard_broken(text, rest, box_cols) => rest,
            None => return (None, read),
        };
    }
    (None, read)
}

/// Where Claude Code's update notice sits under the launch card at `card`
/// on `rows` of a pane `cols` wide (module docs: the width tells the
/// vendor's hard break of a word longer than its Box), and how far the walk
/// read. Only the row under the card (`card.top + card.rows`) is read off
/// `card`, so a card scrolled up past the screen's top passes the place of
/// its rows left on it (`crate::turn::launch_card_scrolled`). Never panics,
/// on any rows, place and width.
#[must_use]
pub fn update_notice_scan(rows: &[String], card: &CardPlace, cols: usize) -> NoticeScan {
    let updated = const { crate::anchors::anchor_text("notice.updated") };
    let details = const { crate::anchors::anchor_text("notice.updated_details") };
    let from = card.top.saturating_add(card.rows);
    let mut scan = NoticeScan {
        from,
        read_to: None,
        stop: false,
        place: None,
    };
    let reach = rows.len().min(from.saturating_add(NOTICE_REACH));
    for r in from..reach {
        scan.read_to = Some(r);
        let row = &rows[r];
        if said(row) {
            scan.stop = true;
            return scan;
        }
        if !at_indent(row).is_some_and(|text| text.starts_with(updated)) {
            continue;
        }
        let width = |row: &String| crate::text::trim_end(row).chars().count();
        let mut end = width(row);
        for d in r + 1..=r + SUMMARY_ROWS {
            let Some(row) = rows.get(d) else {
                return scan;
            };
            let (taken, read) = details_from(rows, d, details, cols);
            scan.read_to = read.max(Some(d));
            if let Some(taken) = taken {
                let widest = rows[d..d + taken].iter().map(width).max().unwrap_or(0);
                scan.place = Some(NoticePlace {
                    top: r,
                    rows: d + taken - r,
                    text_col: INDENT.len(),
                    end: end.max(widest),
                });
                return scan;
            }
            if said(row) || at_indent(row).is_none() {
                return scan;
            }
            end = end.max(width(row));
        }
        return scan;
    }
    scan
}

/// Whether [`update_notice_scan`] reads `rows` — under the same card, the
/// pane as wide — as it read the rows they were before the rows `changed`
/// names changed (the count of rows the same), known without a scan: every changed row lies
/// above the first row the scan read, or below the last, or is the
/// transcript row the walk stopped at and still opens with a transcript
/// glyph. The scan reads its rows and nothing else, and its stop row only
/// through that glyph, so this is exact. Never panics, on any rows, scan
/// and indices.
#[must_use]
pub fn update_notice_unmoved(rows: &[String], scan: &NoticeScan, changed: &[usize]) -> bool {
    let Some(last) = scan.read_to else {
        return true;
    };
    changed.iter().all(|&j| {
        j < scan.from
            || j > last
            || (scan.stop && j == last && rows.get(j).is_some_and(|row| said(row)))
    })
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
