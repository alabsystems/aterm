// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text selection to string conversion for `Terminal`.

use std::borrow::Cow;
use std::ops::Range;

use super::Terminal;
use super::content::{TrailingBlanks, push_visible_row_bounds, visible_row_bounds_to_string};
use crate::search::LineCells;
use crate::selection::SmartSelection;

const MAX_SELECTION_ROWS: usize = 1_000_000;

const MAX_SELECTION_BYTES: usize = 64 * 1024 * 1024;

const MAX_LOGICAL_LINE_ROWS: usize = 4096;

/// Per-line byte ceiling for scrollback text extraction.
pub(crate) const MAX_SCROLLBACK_LINE_SCAN_BYTES: usize = 1024 * 1024;

/// A stored line's full text (all columns, no trim) bounded to `max_scan_bytes`.
#[inline]
pub(crate) fn line_text_bounded(bytes: &[u8], max_scan_bytes: usize) -> Cow<'_, str> {
    let scan = if bytes.len() <= max_scan_bytes {
        bytes
    } else {
        &bytes[..max_scan_bytes]
    };
    String::from_utf8_lossy(scan)
}

fn scrollback_line_range_text(
    bytes: &[u8],
    max_scan_bytes: usize,
    cells_of: impl FnOnce(&str) -> LineCells,
    (start_col, end_col): (u16, u16),
    trailing: TrailingBlanks,
) -> String {
    let line = line_text_bounded(bytes, max_scan_bytes);
    let cells = cells_of(&line);
    let Some(range) = cells
        .bytes_in_columns(
            usize::from(start_col),
            usize::from(end_col).saturating_add(1),
        )
        .and_then(|bytes| line.get(bytes))
    else {
        return String::new();
    };
    match trailing {
        TrailingBlanks::Trim => range.trim_end().to_string(),
        TrailingBlanks::Written => range.to_string(),
    }
}

/// Append `row_text` to `result` without exceeding `max_bytes`, returning
/// `true` if the row was clamped (only a char-boundary-safe prefix was pushed).
#[inline]
fn push_row_capped(result: &mut String, row_text: &str, max_bytes: usize) -> bool {
    if result.len().saturating_add(row_text.len()) <= max_bytes {
        result.push_str(row_text);
        return false;
    }
    let budget = max_bytes.saturating_sub(result.len());
    let mut cut = budget.min(row_text.len());
    while cut > 0 && !row_text.is_char_boundary(cut) {
        cut -= 1;
    }
    result.push_str(&row_text[..cut]);
    true
}

/// The row/column span one selection walk needs, computed once by
/// [`Terminal::selection_to_string_capped`] and passed to whichever shape walk
/// runs, so the walks cannot drift apart in geometry.
#[derive(Clone, Copy)]
struct SelectionGeometry {
    /// Oldest terminal-relative row to visit (span-clamped to `-scrollback_lines`).
    first_row: i32,
    /// Newest terminal-relative row to visit (span-clamped to `rows - 1`).
    last_row: i32,
    /// Side-adjusted selection start row — the row whose text starts at `adj_start_col`.
    adj_start_row: i32,
    /// Side-adjusted selection end row — the row whose text stops at `adj_end_col`.
    adj_end_row: i32,
    /// Side-adjusted start column, inclusive.
    adj_start_col: u16,
    /// Side-adjusted end column, inclusive.
    adj_end_col: u16,
    /// `grid.cols()`; the caller has already rejected a zero-column grid.
    cols: u16,
}

struct SelectionRow<'a> {
    /// The stored line of a history row; `None` for a live row, or a history
    /// row that could not be read.
    history: Option<Cow<'a, aterm_scrollback::Line>>,
    /// Whether the row continues the one above (a soft wrap).
    continues: bool,
}

/// The word `smart`'s rules find at column `col` of `text`, read with its grid cells.
#[inline]
#[must_use]
pub fn smart_word_in_cells(
    smart: &SmartSelection,
    text: &str,
    cells: &LineCells,
    col: usize,
) -> Option<(Range<usize>, (usize, usize))> {
    let byte = cells.byte_at_column(col)?;
    let (start, end) = smart.word_boundaries_at(text, byte)?;
    Some((start..end, cells.columns_of(text, start, end)))
}

impl Terminal {
    /// The selected text; `None` if there is no selection or it is empty.
    #[inline]
    #[must_use]
    pub fn selection_to_string(&self) -> Option<String> {
        self.selection_to_string_bounded().0
    }

    /// [`Self::selection_to_string`] and whether selected content is missing from either end.
    #[inline]
    #[must_use]
    pub fn selection_to_string_bounded(&self) -> (Option<String>, bool) {
        self.selection_to_string_capped(MAX_SELECTION_ROWS, MAX_SELECTION_BYTES)
    }

    fn selection_to_string_capped(
        &self,
        max_rows: usize,
        max_bytes: usize,
    ) -> (Option<String>, bool) {
        use crate::selection::SelectionType;

        let evicted = self.text_selection.truncated();
        let Some((adj_start_row, adj_start_col, adj_end_row, adj_end_col)) =
            self.text_selection.side_adjusted_bounds()
        else {
            return (None, evicted);
        };

        let mut result = String::new();
        let cols = self.grid.cols();
        if cols == 0 {
            return (None, false);
        }

        let visible_rows = i32::from(self.grid.rows());
        let history = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        let first_row = adj_start_row.max(-history);
        let last_row = adj_end_row.min(visible_rows - 1);

        let geom = SelectionGeometry {
            first_row,
            last_row,
            adj_start_row,
            adj_end_row,
            adj_start_col,
            adj_end_col,
            cols,
        };

        let (rows_emitted, truncated) = match self.text_selection.selection_type() {
            SelectionType::Block => {
                self.push_block_selection(&mut result, geom, max_rows, max_bytes)
            }
            _ => self.push_linear_selection(&mut result, geom, max_rows, max_bytes),
        };

        if truncated {
            aterm_log::warn!(
                "selection_to_string truncated at cap (rows_emitted={rows_emitted}, bytes={})",
                result.len()
            );
        }

        if result.is_empty() {
            (None, truncated || evicted)
        } else {
            (Some(result), truncated || evicted)
        }
    }

    fn push_block_selection(
        &self,
        result: &mut String,
        geom: SelectionGeometry,
        max_rows: usize,
        max_bytes: usize,
    ) -> (usize, bool) {
        let SelectionGeometry {
            first_row,
            last_row,
            adj_start_col,
            adj_end_col,
            ..
        } = geom;
        let mut rows_emitted = 0usize;
        let mut truncated = false;

        for row in first_row..=last_row {
            if rows_emitted >= max_rows || result.len() >= max_bytes {
                truncated = true;
                break;
            }
            rows_emitted += 1;
            if row > first_row {
                result.push('\n');
            }
            if let Some(line) = self.get_line_text(row, Some((adj_start_col, adj_end_col))) {
                if push_row_capped(result, &line, max_bytes) {
                    truncated = true;
                    break;
                }
            }
        }

        (rows_emitted, truncated)
    }

    fn push_linear_selection(
        &self,
        result: &mut String,
        geom: SelectionGeometry,
        max_rows: usize,
        max_bytes: usize,
    ) -> (usize, bool) {
        let SelectionGeometry {
            first_row,
            last_row,
            adj_start_row,
            adj_end_row,
            adj_start_col,
            adj_end_col,
            cols,
        } = geom;
        let mut rows_emitted = 0usize;
        let mut truncated = false;
        let mut lookahead: Option<SelectionRow<'_>> = None;
        let mut line_start = result.len();

        for row in first_row..=last_row {
            if rows_emitted >= max_rows || result.len() >= max_bytes {
                truncated = true;
                break;
            }
            rows_emitted += 1;
            let current = lookahead.take().unwrap_or_else(|| self.selection_row(row));
            if row > first_row && !current.continues {
                result.push('\n');
                line_start = result.len();
            }

            let start_col = if row == adj_start_row {
                adj_start_col
            } else {
                0
            };
            let end_col = if row == adj_end_row {
                adj_end_col
            } else {
                cols - 1
            };
            let trailing = if row < last_row {
                let next = self.selection_row(row + 1);
                let continued = next.continues;
                lookahead = Some(next);
                if continued {
                    TrailingBlanks::Written
                } else {
                    TrailingBlanks::Trim
                }
            } else {
                TrailingBlanks::Trim
            };

            let line = if row < 0 {
                current
                    .history
                    .as_ref()
                    .map(|stored| self.history_range_text(stored, (start_col, end_col), trailing))
            } else {
                self.line_text(row, Some((start_col, end_col)), trailing)
            };
            if let Some(line) = line {
                if push_row_capped(result, &line, max_bytes) {
                    truncated = true;
                    break;
                }
            }
            if trailing == TrailingBlanks::Trim {
                let keep = line_start + result[line_start..].trim_end().len();
                result.truncate(keep);
            }
        }

        (rows_emitted, truncated)
    }

    /// Read selection-space `row` for [`Self::push_linear_selection`]: its stored line when
    /// history (fetched once, here) and whether it continues the row above.
    #[inline]
    fn selection_row(&self, row: i32) -> SelectionRow<'_> {
        let history = if row < 0 {
            usize::try_from(-(i64::from(row)) - 1)
                .ok()
                .and_then(|rev_idx| self.grid.history_line_rev(rev_idx))
        } else {
            None
        };
        let continues = self.continuation_bit(row, history.as_deref());
        SelectionRow { history, continues }
    }

    /// Whether selection-space `row` continues the previous row (a soft wrap). The one place
    /// selection reads the wrap bit: [`Self::push_linear_selection`] (newlines in the copy)
    /// and [`Self::logical_line_span`] (click reach) must agree, or highlight and clipboard
    /// describe different text.
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "private row/line types prevent method-reference shorthand"
    )]
    fn continuation_bit(&self, row: i32, history_line: Option<&aterm_scrollback::Line>) -> bool {
        let visible_rows = i32::from(self.grid.rows());
        if row >= 0 && row < visible_rows {
            u16::try_from(row)
                .ok()
                .and_then(|idx| self.grid.row_at_screen(idx))
                .is_some_and(|r| r.is_wrapped())
        } else {
            row < 0
                && match history_line {
                    Some(line) => line.is_wrapped(),
                    None => usize::try_from(-(i64::from(row)) - 1)
                        .ok()
                        .and_then(|rev_idx| self.grid.history_line_rev(rev_idx))
                        .is_some_and(|l| l.is_wrapped()),
                }
        }
    }

    /// Whether selection-space `row` continues the previous row (soft wrap).
    #[inline]
    #[must_use]
    pub fn row_continues_previous(&self, row: i32) -> bool {
        self.continuation_bit(row, None)
    }

    /// Inclusive physical row span `(first, last)` of the logical line containing `row`.
    #[must_use]
    pub fn logical_line_span(&self, row: i32) -> (i32, i32) {
        let visible_rows = i32::from(self.grid.rows());
        let floor = -i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        if row < floor || row >= visible_rows {
            return (row, row);
        }
        let cap = i32::try_from(MAX_LOGICAL_LINE_ROWS).unwrap_or(i32::MAX);
        let up_stop = row.saturating_sub(cap).max(floor);
        let mut first = row;
        while first > up_stop && self.row_continues_previous(first) {
            first -= 1;
        }
        let down_stop = row.saturating_add(cap).min(visible_rows - 1);
        let mut last = row;
        while last < down_stop && self.row_continues_previous(last + 1) {
            last += 1;
        }
        (first, last)
    }

    /// Rows `first..=last` of a logical line (already clamped to its span) joined into one line.
    #[must_use]
    pub fn line_range_cells(&self, first: i32, last: i32) -> Option<(String, LineCells)> {
        let (mut text, mut cells) = self.drawn_line_cells(first)?;
        let cols = usize::from(self.grid.cols());
        let mut row_start = 0usize;
        for r in first.saturating_add(1)..=last {
            row_start = row_start.saturating_add(cols);
            let (row, row_cells) = self.drawn_line_cells(r).unwrap_or_default();
            cells.append_wrapped_row(&mut text, &row, &row_cells, row_start);
        }
        Some((text, cells))
    }

    /// [`Self::written_line_cells`] cut after the row's last cell: the text the row shows.
    #[inline]
    pub(crate) fn drawn_line_cells(&self, row: i32) -> Option<(String, LineCells)> {
        let (mut text, cells) = self.written_line_cells(row)?;
        let end = cells.end_byte();
        if text.is_char_boundary(end) {
            text.truncate(end);
        }
        Some((text, cells))
    }

    /// A line's text (visible or scrollback), trailing blanks trimmed.
    #[inline]
    pub fn get_line_text(&self, row: i32, col_range: Option<(u16, u16)>) -> Option<String> {
        self.line_text(row, col_range, TrailingBlanks::Trim)
    }

    /// Append live screen row `row`'s whole text to `out`.
    #[inline]
    pub fn push_line_text(&self, row: usize, out: &mut String) -> bool {
        let Ok(row) = u16::try_from(row) else {
            return false;
        };
        if row >= self.grid.rows() {
            return false;
        }
        if let Some(last_col) = self.grid.cols().checked_sub(1) {
            push_visible_row_bounds(&self.grid, row, 0, last_col, TrailingBlanks::Trim, out);
        }
        true
    }

    /// The text a row holds as written, trailing blanks included.
    #[inline]
    #[must_use]
    pub fn written_line_text(&self, row: i32) -> Option<String> {
        if row < 0 {
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let line = self.grid.history_line_rev(rev_idx)?;
            return Some(
                line_text_bounded(line.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned(),
            );
        }
        self.line_text(row, None, TrailingBlanks::Written)
    }

    fn line_text(
        &self,
        row: i32,
        col_range: Option<(u16, u16)>,
        trailing: TrailingBlanks,
    ) -> Option<String> {
        let visible_rows = i32::from(self.grid.rows());

        if row >= 0 && row < visible_rows {
            let row_idx = u16::try_from(row).ok()?;
            let cols = self.grid.cols();
            if cols == 0 && col_range.is_none() {
                return Some(String::new());
            }
            let (start_col, end_col) = col_range.unwrap_or((0, cols.saturating_sub(1)));
            Some(visible_row_bounds_to_string(
                &self.grid, row_idx, start_col, end_col, trailing,
            ))
        } else if row < 0 {
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let scrollback_line = self.grid.history_line_rev(rev_idx)?;
            let cols = self.grid.cols();
            if cols == 0 && col_range.is_none() {
                return Some(String::new());
            }
            let (start_col, end_col) = col_range.unwrap_or((0, cols.saturating_sub(1)));
            Some(self.history_range_text(&scrollback_line, (start_col, end_col), trailing))
        } else {
            None
        }
    }
}

impl Terminal {
    /// The text in columns `start_col..=end_col` of a history row whose stored line is
    /// `line`, read from the cells the row is drawn in.
    #[inline]
    fn history_range_text(
        &self,
        line: &aterm_scrollback::Line,
        cols: (u16, u16),
        trailing: TrailingBlanks,
    ) -> String {
        scrollback_line_range_text(
            line.as_bytes(),
            MAX_SCROLLBACK_LINE_SCAN_BYTES,
            |text| super::history_line_text_cells(&self.grid, line, text),
            cols,
            trailing,
        )
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
