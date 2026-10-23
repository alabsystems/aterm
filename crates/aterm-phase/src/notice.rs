// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE'S UPDATE NOTICE under its launch card — where it sits, read by its own shape, never by its words alone.

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

/// Where the update notice sits ([`update_notice_scan`]), in rows and CHAR columns of the rows as passed (one char per cell on the window's live text, so there a char index is the cell's column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoticePlace {
    /// The summary's first row: the notice's top.
    pub top: usize,
    /// How many rows the notice spans from [`top`](Self::top): the summary's (one, or more where it wrapped) and the details row's (one, or two where it wrapped).
    pub rows: usize,
    /// The column the notice's text starts at (the Box's padding: 2).
    pub text_col: usize,
    /// The column just past the notice's widest row, trailing blanks trimmed (74 on the 2.1.283 captures, 73 on the 2.1.282 ones).
    pub end: usize,
}

/// One reading of the rows for the notice ([`update_notice_scan`]), with what it read: what lets a later rebuild keep it without a scan ([`update_notice_unmoved`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoticeScan {
    /// The first row read: the row under the card.
    pub from: usize,
    /// The last row read (`None`: none was — the card's last row is the screen's).
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
/// of its own.
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

/// Where Claude Code's update notice sits under the launch card at `card` on `rows` of a pane `cols` wide (module docs: the width tells the vendor's hard break of a word longer than its Box), and how far the walk read.
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

/// Whether [`update_notice_scan`] reads `rows` — under the same card, the pane as wide — as it read the rows they were before the rows `changed` names changed (the count of rows the same), known without a scan: every changed row lies above the first row the scan read, or below the last, or is the transcript row the walk stopped at and still opens with a transcript glyph.
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
