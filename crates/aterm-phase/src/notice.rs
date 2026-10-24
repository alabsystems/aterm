// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code update-notice placement under the launch card.

use crate::turn::{CardPlace, SAID};

const NOTICE_REACH: usize = 6;
const SUMMARY_ROWS: usize = 3;
const DETAILS_ROWS: usize = 2;
const INDENT: &str = "  ";

/// Update notice bounds in rows and character columns ([`update_notice_scan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoticePlace {
    /// The summary's first row: the notice's top.
    pub top: usize,
    /// Number of rows spanned from [`top`](Self::top).
    pub rows: usize,
    /// The column the notice's text starts at (the Box's padding: 2).
    pub text_col: usize,
    /// Column just past the notice's widest row with trailing blanks trimmed.
    pub end: usize,
}

/// Update notice scan result and row range ([`update_notice_scan`]).
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

fn said(row: &str) -> bool {
    row.starts_with(SAID)
}

fn at_indent(row: &str) -> Option<&str> {
    let text = row.strip_prefix(INDENT)?;
    text.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
        .then_some(text)
}

fn hard_broken(text: &str, rest: &str, box_cols: usize) -> bool {
    let cells = |s: &str| s.chars().count();
    let tail = text.rsplit(' ').next().unwrap_or(text);
    let head = rest.split(' ').next().unwrap_or(rest);
    cells(text) == box_cols && cells(tail) + cells(head) > box_cols
}

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

/// Scans `rows` below `card` on a `cols`-wide pane for Claude Code's update notice.
#[must_use]
pub fn update_notice_scan(rows: &[String], card: &CardPlace, cols: usize) -> NoticeScan {
    let updated = const { crate::anchor!("notice.updated") };
    let details = const { crate::anchor!("notice.updated_details") };
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

/// Returns whether `scan` remains valid after row indices `changed` were updated.
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
