// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Text content extraction and smart selection API.

use crate::grid::{Grid, Row, row_u16};
use crate::search::LineCells;

use super::Terminal;

#[inline]
fn push_cell_text(grid: &Grid, row: u16, col: u16, out: &mut String) {
    // pair scrolled cells with live extras.
    if let Some(cells) = grid.row_at_screen(row) {
        push_row_cell_text(grid, row, cells, col, out);
    }
}

/// [`push_cell_text`] with the screen row already resolved: `cells` is
/// `grid.row_at_screen(row)`.
#[inline]
fn push_row_cell_text(grid: &Grid, row: u16, cells: &Row, col: u16, out: &mut String) {
    let Some(cell) = cells.get(col) else {
        return;
    };
    if cells.is_cell_wide_continuation(col) {
        return;
    }
    if cell.is_complex() {
        // Use full string for text extraction (handles multi-char HashMap entries)
        if !grid.push_complex_char_at(row, col, out) {
            out.push('\u{FFFD}');
        }
    } else {
        let ch = cell.char();
        out.push(if ch == '\0' { ' ' } else { ch });
    }
    if cell.has_extras() {
        if let Some(extra) = grid.cell_extra(row, col) {
            for &combining in extra.combining() {
                out.push(combining);
            }
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

/// [`visible_row_bounds_to_string`] appended to `out`, recording into `cells`,
/// when given, the cell each piece of the text was read from: its column counted
/// from the range's first, its bytes from where the append began.
fn push_visible_row_text(
    grid: &Grid,
    row: u16,
    start_col: u16,
    end_col: u16,
    trailing: TrailingBlanks,
    mut cells: Option<&mut LineCells>,
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
    out.reserve(usize::from(end_col.saturating_sub(effective_start)).saturating_add(1));
    if let Some(row_cells) = grid.row_at_screen(row) {
        let end_col = match trailing {
            TrailingBlanks::Trim => Some(end_col),
            TrailingBlanks::Written => row_cells
                .written_len()
                .checked_sub(1)
                .map(|last| end_col.min(last)),
        };
        if let Some(end_col) = end_col {
            for col in effective_start..=end_col {
                let start = out.len();
                push_row_cell_text(grid, row, row_cells, col, out);
                if let Some(cells) = cells.as_deref_mut() {
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

    /// Append visible row `row`'s text into `out`, reusing its capacity, and report whether the row was in bounds: the non-allocating counterpart of [`row_text`](Self::row_text).
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

    /// Fill `out` with visible row `row` as per-column chars and return the row's fill (one past the last non-blank column; `0` for an empty or out-of-range row).
    #[inline]
    pub fn row_cols_into(&self, row: usize, out: &mut Vec<char>) -> u16 {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return 0;
        }
        self.grid.row_cols_into(row_u16(row), out)
    }

    /// The combining-aware grapheme text of visible cell `(row, col)`: the base character plus any complex-cluster string and combining marks, the same content the selection and `row_text`/`get_line_text` produce (NFD accents and ZWJ clusters included).
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

    /// The combining-aware grapheme text of a display cell, accounting for `display_offset`: the per-cell counterpart of [`display_row_text`](Self::display_row_text), where row 0 is the top of the current viewport (a scrollback line when scrolled back).
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
            // Fast path: no scrollback scroll — display row == live grid row.
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

    /// Smart word boundaries `(start_col, end_col)` at `(row, col)` on a display-relative row (`0` = viewport top), matching URLs, file paths, emails, git hashes, and quoted strings before falling back to word boundaries.
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
mod tests {
    use crate::terminal::Terminal;

    #[test]
    fn plain_overwrite_removes_stale_hyperlink_extras_entries() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\");
        assert_eq!(
            term.hyperlink_at(0, 0),
            Some("https://example.com"),
            "linked text must carry the hyperlink"
        );
        assert_eq!(term.grid().extras().len(), 4, "one entry per linked cell");

        term.process(b"\rplain");

        assert_eq!(term.row_text(0).as_deref(), Some("plain"));
        assert_eq!(term.hyperlink_at(0, 0), None);
        assert_eq!(
            term.grid().extras().len(),
            0,
            "stale extras entries must be removed on overwrite (#7456)"
        );
    }

    #[test]
    fn plain_overwrite_removes_stale_combining_mark_entry() {
        let mut term = Terminal::new(24, 80);
        term.process("e\u{0301}".as_bytes());
        assert_eq!(term.row_text(0).as_deref(), Some("e\u{0301}"));
        assert_eq!(term.grid().extras().len(), 1, "combining mark entry");

        term.process(b"\rx");

        assert_eq!(
            term.row_text(0).as_deref(),
            Some("x"),
            "text API must show the plain char, not the stale mark"
        );
        assert_eq!(
            term.grid().extras().len(),
            0,
            "stale combining-mark entry must be removed on overwrite (#7456)"
        );
    }

    #[test]
    fn styled_overwrite_does_not_resurrect_stale_hyperlink() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\");
        assert!(term.hyperlink_at(0, 0).is_some());

        term.process(b"\r\x1b[58:2::255:0:0m\x1b[4mZ\x1b[0m");

        assert_eq!(term.row_text(0).as_deref(), Some("Z"));
        assert_eq!(
            term.hyperlink_at(0, 0),
            None,
            "old hyperlink must not attach to the new styled char (#7456)"
        );
    }

    /// display_row_text returns exactly what each display row shows when the
    /// viewport straddles the history/live boundary (live rows are not
    /// shifted again by display_offset).
    #[test]
    fn display_row_text_straddling_viewport_reads_each_displayed_row() {
        let mut term = Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        for (r, want) in ["line26", "line27", "line28", "line29", ""]
            .iter()
            .enumerate()
        {
            assert_eq!(term.display_row_text(r).as_deref(), Some(*want));
        }

        term.grid_mut().scroll_display(2);
        for (r, want) in ["line24", "line25", "line26", "line27", "line28"]
            .iter()
            .enumerate()
        {
            assert_eq!(
                term.display_row_text(r).as_deref(),
                Some(*want),
                "display row {r} of a straddling viewport"
            );
        }

        term.grid_mut().scroll_display(24);
        assert_eq!(term.display_row_text(0).as_deref(), Some("line0"));
        assert_eq!(term.display_row_text(4).as_deref(), Some("line4"));

        term.scroll_to_bottom();
        for r in 0..5 {
            assert_eq!(term.display_row_text(r), term.row_text(r));
        }
    }

    /// A straddling read keeps live-frame extras (combining marks) paired with
    /// their base cells.
    #[test]
    fn display_row_text_straddling_keeps_combining_marks_paired() {
        let mut term = Terminal::new(5, 20);
        for i in 0..28 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.process("acce\u{0301}nt\r\n".as_bytes());
        assert_eq!(term.row_text(3).as_deref(), Some("acce\u{0301}nt"));

        term.grid_mut().scroll_display(1);
        assert_eq!(
            term.display_row_text(4).as_deref(),
            Some("acce\u{0301}nt"),
            "the live accent row keeps its combining mark while straddling"
        );
    }

    /// The display-relative cell reader tracks the scroll position like
    /// display_row_text (row 0 is the top displayed row), and equals
    /// cell_grapheme at display_offset == 0.
    #[test]
    fn display_cell_grapheme_reads_the_displayed_row() {
        let mut term = Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        for r in 0..5 {
            for c in 0..8 {
                assert_eq!(
                    term.display_cell_grapheme(r, c),
                    term.cell_grapheme(r, c),
                    "offset-0 identity at ({r},{c})"
                );
            }
        }

        term.scroll_to_top();
        let spelled: String = (0..5)
            .map(|c| term.display_cell_grapheme(0, c).unwrap())
            .collect();
        assert_eq!(spelled, "line0", "top display row spells the oldest line");

        term.scroll_to_bottom();
        term.grid_mut().scroll_display(2);
        for r in 0..5 {
            let spelled: String = (0..6)
                .map(|c| term.display_cell_grapheme(r, c).unwrap())
                .collect();
            assert_eq!(
                spelled.trim_end(),
                term.display_row_text(r).unwrap().trim_end(),
                "straddling display row {r}"
            );
        }

        assert_eq!(term.display_cell_grapheme(999, 0), None);
        assert_eq!(term.display_cell_grapheme(0, 999), None);
    }

    /// A scrolled-off row's complex cluster and combining marks stay paired
    /// with their cells when read back per-cell from history (the ring extras
    /// materializer), and a wide lead's continuation reads as the empty spacer.
    #[test]
    fn display_cell_grapheme_keeps_history_extras_paired() {
        let mut term = Terminal::new(5, 20);
        term.process("\u{1F980}e\u{0301}x\r\n".as_bytes());
        for i in 0..10 {
            term.process(format!("fill{i}\r\n").as_bytes());
        }
        term.scroll_to_top();
        assert_eq!(
            term.display_cell_grapheme(0, 0).as_deref(),
            Some("\u{1F980}"),
            "history complex cluster stays on its lead cell"
        );
        assert_eq!(
            term.display_cell_grapheme(0, 1).as_deref(),
            Some(""),
            "wide continuation spacer reads as empty"
        );
        assert_eq!(
            term.display_cell_grapheme(0, 2).as_deref(),
            Some("e\u{0301}"),
            "history combining mark stays paired with its base"
        );
        assert_eq!(term.display_cell_grapheme(0, 3).as_deref(), Some("x"));
    }
}
