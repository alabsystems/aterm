// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Text content extraction and smart selection API.

use crate::grid::{Grid, Row, row_u16};
use crate::search::LineCells;

use super::Terminal;

#[inline]
fn push_cell_text(grid: &Grid, row: u16, col: u16, out: &mut String) {
    if let Some(cells) = grid.row_at_screen(row) {
        push_row_cell_text(grid, row, cells, col, out);
    }
}

/// [`push_cell_text`] with the screen row already resolved: `cells` is
/// `grid.row_at_screen(row)`.
// Keep per-cell emission inlined in both extraction loops.
#[expect(
    clippy::inline_always,
    reason = "measured 58-case row extraction: ordinary inlining regressed full nonblank controls; per-cell emission plus separate recording loops restored them"
)]
#[inline(always)]
fn push_row_cell_text(grid: &Grid, row: u16, cells: &Row, col: u16, out: &mut String) {
    let Some(cell) = cells.get(col) else {
        return;
    };
    if cells.is_cell_wide_continuation(col) {
        return;
    }
    if cell.is_complex() {
        if !grid.push_complex_char_at(row, col, out) {
            out.push('\u{FFFD}');
        }
    } else {
        let ch = cell.char();
        out.push(if ch == '\0' { ' ' } else { ch });
    }
    if cell.has_extras() {
        if let Some(extra) = grid.cell_extra(row, col) {
            extra.push_combining_text(out);
        }
    }
}

/// What a row extraction does with the blanks at the end of its range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailingBlanks {
    /// Drop trailing whitespace: the row ends a logical line (or the reader wants it trimmed), so the blanks after its text are layout.
    Trim,
    /// Keep every cell the program wrote, blanks included, and nothing past that: what a history [`Line`](aterm_scrollback::Line) stores for the row.
    Written,
}

/// Extract live-frame (screen-row, display_offset-independent) row text for an inclusive column range.
#[inline]
#[must_use]
pub(crate) fn visible_row_bounds_to_string(
    grid: &Grid,
    row: u16,
    start_col: u16,
    end_col: u16,
    trailing: TrailingBlanks,
) -> String {
    let mut line = String::new();
    push_visible_row_text(grid, row, start_col, end_col, trailing, None, &mut line);
    line
}

/// [`visible_row_bounds_to_string`] appended to `out`.
#[inline]
pub(crate) fn push_visible_row_bounds(
    grid: &Grid,
    row: u16,
    start_col: u16,
    end_col: u16,
    trailing: TrailingBlanks,
    out: &mut String,
) {
    push_visible_row_text(grid, row, start_col, end_col, trailing, None, out);
}

/// Live-frame row `row` as written (every cell up to `Row::len`, trailing blanks kept: [`TrailingBlanks::Written`]) with the cells its text was read from: each cell's bytes in the text, its column and its width, 2 for a wide glyph whose right half follows it.
#[inline]
#[must_use]
pub(crate) fn visible_row_written(grid: &Grid, row: u16) -> (String, LineCells) {
    let mut cells = LineCells::new();
    let mut text = String::new();
    push_visible_row_text(
        grid,
        row,
        0,
        grid.cols().saturating_sub(1),
        TrailingBlanks::Written,
        Some(&mut cells),
        &mut text,
    );
    (text, cells)
}

/// Peel only cells whose emitted text is certainly whitespace or empty. Complex
/// cells and extras retain the final string trim's complete Unicode semantics.
fn trimmed_plain_end(cells: &Row, start: u16, mut end: Option<u16>) -> Option<u16> {
    while let Some(col) = end.filter(|&col| start <= col) {
        let removable = cells.get(col).is_none_or(|cell| {
            let plain_blank = !cell.is_complex() && !cell.has_extras() && {
                let ch = cell.char();
                ch == '\0' || ch.is_whitespace()
            };
            plain_blank || cells.is_cell_wide_continuation(col)
        });
        if !removable {
            break;
        }
        end = col.checked_sub(1);
    }
    end
}

fn push_visible_row_text(
    grid: &Grid,
    row: u16,
    start_col: u16,
    end_col: u16,
    trailing: TrailingBlanks,
    cells: Option<&mut LineCells>,
    out: &mut String,
) {
    let last_col = grid.cols().saturating_sub(1);
    let end_col = end_col.min(last_col);

    let is_continuation =
        |col: u16| col <= last_col && grid.is_wide_continuation_at_screen(row, col);
    let (effective_start, _) = aterm_types::selection::glyph_cell_span(
        start_col,
        is_continuation(start_col.saturating_add(1)),
        is_continuation(start_col),
    );
    let (_, effective_end) = aterm_types::selection::glyph_cell_span(
        end_col,
        is_continuation(end_col.saturating_add(1)),
        is_continuation(end_col),
    );
    let end_col = effective_end.min(last_col);

    let base = out.len();
    if let Some(row_cells) = grid.row_at_screen(row) {
        // Beyond the written extent every character is an unwritten blank;
        // Trim would discard that suffix after constructing it.
        let mut end_col = row_cells
            .written_len()
            .checked_sub(1)
            .map(|last| end_col.min(last));
        if trailing == TrailingBlanks::Trim
            && let Some(bound) = end_col
        {
            end_col =
                trimmed_plain_end(row_cells, effective_start, end_col).map(|end| end.min(bound));
        }
        if let Some(end_col) = end_col.filter(|&end| effective_start <= end) {
            out.reserve(usize::from(end_col - effective_start) + 1);
            // Choose recording once per row; text-only extraction has no
            // metadata branch in its cell loop.
            if let Some(cells) = cells {
                for col in effective_start..=end_col {
                    let start = out.len();
                    push_row_cell_text(grid, row, row_cells, col, out);
                    let width = if row_cells.is_cell_wide_continuation(col.saturating_add(1)) {
                        2
                    } else {
                        1
                    };
                    cells.push(
                        start - base..out.len() - base,
                        usize::from(col.saturating_sub(effective_start)),
                        width,
                    );
                }
            } else {
                for col in effective_start..=end_col {
                    push_row_cell_text(grid, row, row_cells, col, out);
                }
            }
        }
    }

    if trailing == TrailingBlanks::Trim {
        let kept = base + out[base..].trim_end().len();
        out.truncate(kept);
    }
}

impl Terminal {
    /// Get visible content as string (for debugging/testing).
    #[inline]
    #[must_use]
    pub fn visible_content(&self) -> String {
        self.grid.visible_content()
    }

    /// The text of visible row `row` (0 is the top), or `None` if out of bounds.
    #[inline]
    #[must_use]
    pub fn row_text(&self, row: usize) -> Option<String> {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            return None;
        }
        self.grid.row_text(row_u16(row))
    }

    /// Writes visible row `row`'s text into `out` (replacing its contents) and returns whether
    /// `row` was in bounds.
    #[inline]
    #[must_use]
    pub fn row_text_into(&self, row: usize, out: &mut String) -> bool {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return false;
        }
        self.grid.row_text_into(row_u16(row), out)
    }

    /// Fills `out` with visible row `row` as per-column chars and returns the row's fill length.
    #[inline]
    pub fn row_cols_into(&self, row: usize, out: &mut Vec<char>) -> u16 {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return 0;
        }
        self.grid.row_cols_into(row_u16(row), out)
    }

    /// Returns the combining-aware grapheme text of visible cell `(row, col)`.
    #[inline]
    #[must_use]
    pub fn cell_grapheme(&self, row: usize, col: usize) -> Option<String> {
        let r = u16::try_from(row).ok()?;
        let c = u16::try_from(col).ok()?;
        if usize::from(r) >= usize::from(self.grid.rows())
            || usize::from(c) >= usize::from(self.grid.cols())
        {
            return None;
        }
        let mut out = String::new();
        push_cell_text(&self.grid, r, c, &mut out);
        Some(out)
    }

    /// Returns the combining-aware grapheme text of display cell `(row, col)`.
    #[inline]
    #[must_use]
    pub fn display_cell_grapheme(&self, row: usize, col: usize) -> Option<String> {
        let r = u16::try_from(row).ok()?;
        let c = u16::try_from(col).ok()?;
        if usize::from(r) >= usize::from(self.grid.rows())
            || usize::from(c) >= usize::from(self.grid.cols())
        {
            return None;
        }
        let view = self.grid.visible_row_view(r);
        if view.is_wide_continuation(c) {
            return Some(String::new());
        }
        let mut out = String::new();
        if let Some(cell) = view.cell(c) {
            view.push_cell_text(c, cell, &mut out);
        }
        Some(out)
    }

    /// Every display column of `row` resolved once, as `(grapheme, is_wide)`, through a single [`Grid::visible_row_view`].
    #[must_use]
    pub fn display_row_grapheme_cells(&self, row: usize) -> Option<Vec<(String, bool)>> {
        let r = u16::try_from(row).ok()?;
        if usize::from(r) >= usize::from(self.grid.rows()) {
            return None;
        }
        let view = self.grid.visible_row_view(r);
        let cols = self.grid.cols();
        let mut out = Vec::with_capacity(usize::from(cols));
        for col in 0..cols {
            let wide = view.cell(col).is_some_and(|cell| cell.is_wide());
            let mut text = String::new();
            if !view.is_wide_continuation(col)
                && let Some(cell) = view.cell(col)
            {
                view.push_cell_text(col, cell, &mut text);
            }
            out.push((text, wide));
        }
        Some(out)
    }

    /// The text of a display-relative row, accounting for `display_offset`: display row 0 is a scrollback line while scrolled back.
    #[inline]
    #[must_use]
    pub fn display_row_text(&self, display_row: usize) -> Option<String> {
        if self.grid.display_offset() == 0 {
            return self.row_text(display_row);
        }
        self.get_line_text(self.display_terminal_row(display_row)?, None)
    }

    /// The terminal-relative row (negative is scrollback) display row
    /// `display_row` shows: `display_row - display_offset`.
    #[inline]
    fn display_terminal_row(&self, display_row: usize) -> Option<i32> {
        if display_row >= usize::from(self.grid.rows()) {
            return None;
        }
        let terminal_row = i64::try_from(display_row)
            .ok()?
            .checked_sub(i64::try_from(self.grid.display_offset()).ok()?)?;
        i32::try_from(terminal_row).ok()
    }

    /// How wide the grid writes an East Asian Ambiguous character (`→`, `─`, `°`): the `ambiguous_width` policy.
    #[inline]
    #[must_use]
    pub const fn ambiguous_width(&self) -> aterm_types::text_shaping::AmbiguousWidth {
        if self.modes.ambiguous_width_double {
            aterm_types::text_shaping::AmbiguousWidth::Double
        } else {
            aterm_types::text_shaping::AmbiguousWidth::Single
        }
    }

    /// Returns smart word boundaries `(start_col, end_col)` at `(row, col)` on a display-relative row.
    #[inline]
    #[must_use]
    pub fn smart_word_at(
        &self,
        row: usize,
        col: usize,
        smart: &crate::selection::SmartSelection,
    ) -> Option<(usize, usize)> {
        self.smart_word_text_at(row, col, smart)
            .map(|(_, start, end)| (start, end))
    }

    /// [`smart_word_at`](Self::smart_word_at) returning `(word, start_col, end_col)` extracted from the drawn cells.
    #[inline]
    #[must_use]
    pub fn smart_word_text_at(
        &self,
        row: usize,
        col: usize,
        smart: &crate::selection::SmartSelection,
    ) -> Option<(String, usize, usize)> {
        let (text, cells) = self.drawn_line_cells(self.display_terminal_row(row)?)?;
        let (bytes, (start, end)) = super::smart_word_in_cells(smart, &text, &cells, col)?;
        Some((text.get(bytes)?.to_owned(), start, end))
    }
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
