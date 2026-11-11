// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A row as search reads it.

use std::borrow::Cow;

use aterm_scrollback::Line;

use super::Terminal;
use super::content::visible_row_written;
use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
use crate::grid::{Grid, stored_line_cells};
use crate::search::LineCells;

/// How far back [`Terminal::logical_origin`] looks for the row a soft-wrap run starts
/// on.
const MAX_LOGICAL_LINE_WALK: usize = 1024;

/// Rows of the terminal as search reads them ([`Terminal::read_search_rows`]).
#[derive(Debug, Default)]
pub struct SearchRows {
    /// Each row's text as written, trailing blanks kept.
    pub text: Vec<String>,
    /// The cells each row's text was read from.
    pub cells: Vec<LineCells>,
    /// Whether each row continues the row above it ([`Terminal::abs_row_continues`]).
    pub wrapped: Vec<bool>,
}

impl SearchRows {
    /// Room for `rows` rows.
    #[must_use]
    #[inline]
    pub fn with_capacity(rows: usize) -> Self {
        Self {
            text: Vec::with_capacity(rows),
            cells: Vec::with_capacity(rows),
            wrapped: Vec::with_capacity(rows),
        }
    }

    #[inline]
    fn push(&mut self, (text, cells): (String, LineCells), wrapped: bool) {
        self.text.push(text);
        self.cells.push(cells);
        self.wrapped.push(wrapped);
    }
}

/// The text of an absolute row ([`Terminal::abs_row_text_within`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbsRowText {
    /// The row's text as written, combining marks and clusters intact, control characters not yet collapsed.
    Text(String),
    /// Older than `oldest_absolute_row()`.
    Evicted,
    /// Below the live screen's bottom row (no such row).
    OutOfRange,
    /// Within retained history, but its stored line could not be read.
    Unreadable,
}

/// Where an absolute row stands ([`Terminal::abs_row_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbsRowAt {
    /// In the retained history, `rev_idx` lines above the live screen (0 = the newest scrolled-off line).
    History {
        /// Lines above the live screen: 0 is the newest scrolled-off line.
        rev_idx: usize,
    },
    /// On the live screen, at `row`.
    Screen {
        /// The live screen row, 0 at its top.
        row: usize,
    },
    /// Older than `oldest_absolute_row()`.
    Evicted,
    /// Below the live screen's bottom row (no such row).
    OutOfRange,
}

impl Terminal {
    /// [`written_line_text`](Self::written_line_text) with the grid cells its text was read from.
    #[must_use]
    pub fn written_line_cells(&self, row: i32) -> Option<(String, LineCells)> {
        if row < 0 {
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let line = self.grid.history_line_rev(rev_idx)?;
            return Some(history_row_written(&self.grid, &line));
        }
        let row = u16::try_from(row)
            .ok()
            .filter(|&row| row < self.grid.rows())?;
        Some(visible_row_written(&self.grid, row))
    }

    /// Append absolute rows `first..first + count` to `out`, exactly `count` of them.
    pub fn read_search_rows(&self, first: u64, count: usize, out: &mut SearchRows) {
        let grid = &self.grid;
        let oldest = grid.oldest_absolute_row();
        let history_end =
            oldest.saturating_add(u64::try_from(grid.scrollback_lines()).unwrap_or(u64::MAX));
        let end = first.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let rows_until = |bound: u64, from: u64| {
            usize::try_from(bound.min(end).saturating_sub(from)).unwrap_or(usize::MAX)
        };
        let mut abs = first;
        let evicted = rows_until(oldest, abs);
        for _ in 0..evicted {
            out.push(Default::default(), false);
        }
        abs = abs.saturating_add(u64::try_from(evicted).unwrap_or(u64::MAX));
        let in_history = rows_until(history_end, abs);
        if in_history > 0 {
            let start = usize::try_from(abs - oldest).unwrap_or(usize::MAX);
            let mut lines = grid.history_lines_from(start);
            for k in 0..in_history {
                let line = lines.next().flatten();
                let wrapped = (start > 0 || k > 0) && line.as_deref().is_some_and(Line::is_wrapped);
                out.push(history_row_for_search(grid, line), wrapped);
            }
            abs = abs.saturating_add(u64::try_from(in_history).unwrap_or(u64::MAX));
        }
        for _ in evicted.saturating_add(in_history)..count {
            let row = abs
                .checked_sub(history_end)
                .and_then(|visible| i32::try_from(visible).ok());
            out.push(
                row.and_then(|row| self.written_line_cells(row))
                    .unwrap_or_default(),
                self.abs_row_continues(abs),
            );
            abs = abs.saturating_add(1);
        }
    }

    /// Whether absolute row `abs` is a soft-wrap continuation.
    #[must_use]
    #[inline]
    pub fn abs_row_continues(&self, abs: u64) -> bool {
        abs > self.grid.oldest_absolute_row()
            && self
                .abs_selection_row(abs)
                .is_some_and(|row| self.row_continues_previous(row))
    }

    /// The absolute row the search index keys the logical line containing `abs` under.
    #[must_use]
    pub fn logical_origin(&self, abs: u64, floor: u64) -> Option<u64> {
        let mut origin = abs;
        for _ in 0..MAX_LOGICAL_LINE_WALK {
            if origin <= floor || !self.abs_row_continues(origin) {
                return Some(origin);
            }
            origin -= 1;
        }
        None
    }

    /// Whether absolute row `abs` carries the soft-wrap continuation flag (including the oldest retained row, unlike [`abs_row_continues`](Self::abs_row_continues)).
    #[inline]
    pub(crate) fn abs_row_wrap_flag(&self, abs: u64) -> bool {
        self.abs_selection_row(abs)
            .is_some_and(|row| self.row_continues_previous(row))
    }

    /// The exclusive end of the soft-wrap run absolute row `abs` belongs to.
    pub(crate) fn logical_run_end(&self, abs: u64, stop: u64) -> u64 {
        let mut end = abs.saturating_add(1);
        for _ in 0..MAX_LOGICAL_LINE_WALK {
            if end >= stop || !self.abs_row_continues(end) {
                return end.min(stop);
            }
            end = end.saturating_add(1);
        }
        stop
    }

    /// Text of absolute row `abs` as written, trailing blanks kept.
    #[must_use]
    pub fn abs_row_text_within(&self, abs: u64, max_bytes: usize) -> AbsRowText {
        let grid = &self.grid;
        let Some(rel) = abs.checked_sub(grid.oldest_absolute_row()) else {
            return AbsRowText::Evicted;
        };
        let Ok(rel) = usize::try_from(rel) else {
            return AbsRowText::OutOfRange;
        };
        let scrollback = grid.scrollback_lines();
        if rel < scrollback {
            return grid
                .get_history_line(rel)
                .map_or(AbsRowText::Unreadable, |line| {
                    AbsRowText::Text(line_text_bounded(line.as_bytes(), max_bytes).into_owned())
                });
        }
        let visible = rel - scrollback;
        if visible >= usize::from(self.rows()) {
            return AbsRowText::OutOfRange;
        }
        AbsRowText::Text(
            i32::try_from(visible)
                .ok()
                .and_then(|row| self.written_line_text(row))
                .unwrap_or_default(),
        )
    }

    /// Where absolute row `abs` (B-2's one coordinate space, `line`'s and `search`'s) stands.
    #[must_use]
    pub fn abs_row_at(&self, abs: u64) -> AbsRowAt {
        let grid = &self.grid;
        let Some(rel) = abs.checked_sub(grid.oldest_absolute_row()) else {
            return AbsRowAt::Evicted;
        };
        let Ok(rel) = usize::try_from(rel) else {
            return AbsRowAt::OutOfRange;
        };
        let scrollback = grid.scrollback_lines();
        if rel < scrollback {
            return AbsRowAt::History {
                rev_idx: scrollback - 1 - rel,
            };
        }
        let row = rel - scrollback;
        if row >= usize::from(self.rows()) {
            return AbsRowAt::OutOfRange;
        }
        AbsRowAt::Screen { row }
    }

    /// Text of absolute row `abs` as search reads it ([`abs_row_text_within`](Self::abs_row_text_within) at [`MAX_SCROLLBACK_LINE_SCAN_BYTES`]).
    #[must_use]
    #[inline]
    pub fn abs_row_text(&self, abs: u64) -> Option<String> {
        match self.abs_row_text_within(abs, MAX_SCROLLBACK_LINE_SCAN_BYTES) {
            AbsRowText::Text(text) => Some(text),
            AbsRowText::Evicted | AbsRowText::OutOfRange | AbsRowText::Unreadable => None,
        }
    }

    /// The selection-space row of absolute row `abs` (negative in history, `0..rows` on
    /// the live screen), `None` when `abs` is evicted or the row does not fit.
    #[inline]
    fn abs_selection_row(&self, abs: u64) -> Option<i32> {
        let rel = i128::from(abs.checked_sub(self.grid.oldest_absolute_row())?);
        let scrollback = i128::try_from(self.grid.scrollback_lines()).ok()?;
        i32::try_from(rel - scrollback).ok()
    }
}

/// A history line read for search from the dense history walk (`Grid::history_lines_from`).
#[inline]
pub(crate) fn history_row_for_search(
    grid: &Grid,
    line: Option<Cow<'_, Line>>,
) -> (String, LineCells) {
    line.map(|line| history_row_written(grid, &line))
        .unwrap_or_default()
}

/// A history row whose stored line is `line`, as search reads it: its text as
/// written, bounded by [`MAX_SCROLLBACK_LINE_SCAN_BYTES`] as
/// [`Terminal::written_line_text`] reads it, with the cells that text is drawn
/// in.
#[inline]
fn history_row_written(grid: &Grid, line: &Line) -> (String, LineCells) {
    let bytes = line.as_bytes();
    let text = line_text_bounded(bytes, MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned();
    let mut cells = history_line_text_cells(grid, line, &text);
    if bytes.len() > MAX_SCROLLBACK_LINE_SCAN_BYTES {
        cells.mark_truncated();
    }
    (text, cells)
}

/// The cells `text`, a read of a history row of `grid` whose stored line is `line`, is drawn in.
#[must_use]
#[inline]
pub(crate) fn history_line_text_cells(grid: &Grid, line: &Line, text: &str) -> LineCells {
    stored_line_text_cells(line, text, grid.cols())
}

/// The cells `text`, a read of stored history `line`, is drawn in on a row `cols` wide.
#[inline]
pub(crate) fn stored_line_text_cells(line: &Line, text: &str, cols: u16) -> LineCells {
    let mut cells = LineCells::new();
    for (bytes, col, width, count) in stored_line_cells(line, text, cols) {
        cells.push_repeated(
            bytes,
            usize::from(col),
            usize::from(width),
            usize::from(count),
        );
    }
    cells
}

#[cfg(test)]
#[path = "search_rows_tests.rs"]
mod tests;
