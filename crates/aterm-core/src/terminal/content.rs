// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Text content extraction and smart selection API.
//!
//! Methods for extracting text content from the terminal grid and
//! performing smart selection (URLs, paths, etc.) for triggers and UI.

use crate::grid::{Grid, Row, row_u16};

use super::Terminal;

pub(super) fn push_cell_text(grid: &Grid, row: u16, col: u16, out: &mut String) {
    // Live-frame (screen-row) reads: the extras lookups are keyed by the live
    // visible row, and callers pass terminal-relative rows (the renderer's
    // `sel_row = viewport_row - display_offset`). Display-mapped cells would
    // pair scrolled cells with live extras.
    if let Some(cells) = grid.row_at_screen(row) {
        push_row_cell_text(grid, row, cells, col, out);
    }
}

/// [`push_cell_text`] with the screen row already resolved: `cells` is
/// `grid.row_at_screen(row)`. A row-text walk resolves it once instead of
/// twice per cell.
fn push_row_cell_text(grid: &Grid, row: u16, cells: &Row, col: u16, out: &mut String) {
    let Some(cell) = cells.get(col) else {
        return;
    };
    // Context-aware check: Cell::is_wide_continuation() would misread
    // DECSCA-protected cells (PROTECTED shares bit 10). The same test as
    // `Grid::is_wide_continuation_at_screen`, on the row it resolves.
    if cells.is_cell_wide_continuation(col) {
        return;
    }
    if cell.is_complex() {
        // Use full string for text extraction (handles multi-char HashMap entries)
        if let Some(s) = grid.complex_char_str_at(row, col) {
            out.push_str(&s);
        } else {
            out.push('\u{FFFD}');
        }
    } else {
        let ch = cell.char();
        out.push(if ch == '\0' { ' ' } else { ch });
    }
    // Combining marks from CellExtra, gated on has_extras() so cells without
    // extras skip the map probe (the write path removes stale entries; the
    // flag check is the fast path).
    if cell.has_extras() {
        if let Some(extra) = grid.cell_extra(row, col) {
            for &combining in extra.combining() {
                out.push(combining);
            }
        }
    }
}

/// Extract live-frame (screen-row, display_offset-independent) row text for an
/// inclusive column range.
#[must_use]
pub(crate) fn visible_row_bounds_to_string(
    grid: &Grid,
    row: u16,
    start_col: u16,
    end_col: u16,
) -> String {
    // Clamp end_col to the grid width: `side_adjusted_bounds` uses u16::MAX as
    // an "entire row" sentinel.
    let last_col = grid.cols().saturating_sub(1);
    let end_col = end_col.min(last_col);

    // Both edges widen over a double-width glyph through `glyph_cell_span`, the
    // rule the highlight predicate uses: a CJK/emoji glyph is selected whole
    // whenever either cell is, so the copy and the paint cover the same cells.
    // Continuation checks are context-aware (DECSCA shares bit 10) and
    // screen-row keyed like `push_cell_text`. A cell one past `last_col` is not
    // a continuation, so an end at the row's edge stays there.
    let is_continuation =
        |col: u16| col <= last_col && grid.is_wide_continuation_at_screen(row, col);
    let (effective_start, _) = aterm_types::selection::glyph_cell_span(
        start_col,
        is_continuation(start_col.saturating_add(1)),
        is_continuation(start_col),
    );
    // Extending the end onto a continuation adds no text (the cluster lives on
    // the lead); it keeps one rule for both edges and the loop's span equal to
    // the painted span.
    let (_, effective_end) = aterm_types::selection::glyph_cell_span(
        end_col,
        is_continuation(end_col.saturating_add(1)),
        is_continuation(end_col),
    );
    let end_col = effective_end.min(last_col);

    // One byte per column is exact for ASCII and a lower bound otherwise, so a
    // plain row takes one allocation. Bounded by one row's width. Mirrors
    // `Grid::row_text_screen_into`.
    let mut line = String::with_capacity(
        usize::from(end_col.saturating_sub(effective_start)).saturating_add(1),
    );
    if let Some(cells) = grid.row_at_screen(row) {
        for col in effective_start..=end_col {
            push_row_cell_text(grid, row, cells, col, &mut line);
        }
    }

    let trimmed_len = line.trim_end().len();
    line.truncate(trimmed_len);
    line
}

impl Terminal {
    // =========================================================================
    // Trigger evaluation helpers
    // =========================================================================

    /// Get visible content as string (for debugging/testing).
    #[must_use]
    pub fn visible_content(&self) -> String {
        self.grid.visible_content()
    }

    /// The text of visible row `row` (0 is the top), or `None` if out of
    /// bounds.
    #[must_use]
    pub fn row_text(&self, row: usize) -> Option<String> {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            return None;
        }
        self.grid.row_text(row_u16(row))
    }

    /// Append visible row `row`'s text into `out`, reusing its capacity, and
    /// report whether the row was in bounds: the non-allocating counterpart of
    /// [`row_text`](Self::row_text). `out` is cleared first; an out-of-range row
    /// leaves it empty and returns `false`. Used by the observation kernel's
    /// per-batch row scan.
    #[must_use]
    pub fn row_text_into(&self, row: usize, out: &mut String) -> bool {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return false;
        }
        self.grid.row_text_into(row_u16(row), out)
    }

    /// Fill `out` with visible row `row` as per-column chars and return the
    /// row's fill (one past the last non-blank column; `0` for an empty or
    /// out-of-range row).
    ///
    /// The column-indexed sibling of [`row_text_into`](Self::row_text_into), for
    /// per-frame row diffing: one `char` per grid column (the lead char at its
    /// column, `'\0'` at wide continuations, `' '` for blanks), so span math
    /// survives CJK/emoji. Clears and extends the caller's buffer.
    pub fn row_cols_into(&self, row: usize, out: &mut Vec<char>) -> u16 {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return 0;
        }
        self.grid.row_cols_into(row_u16(row), out)
    }

    /// The combining-aware grapheme text of visible cell `(row, col)`: the base
    /// character plus any complex-cluster string and combining marks, the same
    /// content the selection and `row_text`/`get_line_text` produce (NFD
    /// accents and ZWJ clusters included).
    ///
    /// A wide-continuation cell returns the empty string (its glyph belongs to
    /// the lead cell). `None` only for out-of-range coordinates; `Some("")`
    /// for a blank cell.
    #[must_use]
    pub fn cell_grapheme(&self, row: usize, col: usize) -> Option<String> {
        let mut out = String::new();
        self.cell_grapheme_into(row, col, &mut out).then_some(out)
    }

    /// The buffer-reusing twin of [`cell_grapheme`](Self::cell_grapheme): the
    /// same bytes appended to `out`, returning whether the coordinates were in
    /// range. Out of range appends nothing and returns `false`; a blank cell
    /// appends a space and a wide continuation nothing, both returning `true`.
    ///
    /// Both forms share `push_cell_text`. This one serves rows × cols sweeps
    /// (`aterm ctl screen`, `subscribe … cells`) with one allocation per row
    /// instead of per cell.
    pub fn cell_grapheme_into(&self, row: usize, col: usize, out: &mut String) -> bool {
        let (Ok(r), Ok(c)) = (u16::try_from(row), u16::try_from(col)) else {
            return false;
        };
        if usize::from(r) >= usize::from(self.grid.rows())
            || usize::from(c) >= usize::from(self.grid.cols())
        {
            return false;
        }
        push_cell_text(&self.grid, r, c, out);
        true
    }

    /// The combining-aware grapheme text of a display cell, accounting for
    /// `display_offset`: the per-cell counterpart of
    /// [`display_row_text`](Self::display_row_text), where row 0 is the top of
    /// the current viewport (a scrollback line when scrolled back). Cells and
    /// extras are resolved together through [`Grid::visible_row_view`], so a
    /// history row's clusters and marks stay paired with their cells. Equal to
    /// [`cell_grapheme`](Self::cell_grapheme) at `display_offset == 0`.
    ///
    /// A wide-continuation cell returns the empty string; `None` only for
    /// out-of-range coordinates.
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

    /// Every display column of `row` resolved once, as `(grapheme, is_wide)`,
    /// through a single [`Grid::visible_row_view`].
    ///
    /// The batched form of [`display_cell_grapheme`](Self::display_cell_grapheme):
    /// a scrolled-back row is materialized from scrollback, so per-cell reads
    /// would re-materialize it each time (O(cols²)). Hosts cache this and serve
    /// per-cell reads from it. `grapheme` is empty for a blank or
    /// wide-continuation cell; `is_wide` is the lead cell's flag. `None` only
    /// for an out-of-range row.
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

    /// The text of a display-relative row, accounting for `display_offset`:
    /// display row 0 is a scrollback line while scrolled back. Converts to a
    /// terminal-relative row and reads scrollback (negative) or the live grid.
    /// `None` if out of bounds.
    #[must_use]
    pub fn display_row_text(&self, display_row: usize) -> Option<String> {
        let offset = self.grid.display_offset();
        if offset == 0 {
            // Fast path: no scrollback scroll — display row == live grid row.
            return self.row_text(display_row);
        }

        // terminal_row = display_row - display_offset; negative is scrollback.
        let visible_rows = usize::from(self.grid.rows());
        if display_row >= visible_rows {
            return None;
        }

        // display_row < visible_rows (u16::MAX), offset <= scrollback_lines (bounded).
        // Both fit in i64; subtraction cannot overflow.
        #[allow(
            clippy::cast_possible_wrap,
            reason = "both values bounded well within i64 range"
        )]
        let terminal_row = (display_row as i64) - (offset as i64);

        // Clamp to i32 range for get_line_text.
        let terminal_row_i32 = i32::try_from(terminal_row).ok()?;

        self.get_line_text(terminal_row_i32, None)
    }

    // ========================================================================
    // Smart Selection API
    // ========================================================================

    /// Text of an absolute terminal row (the search index's coordinate), read
    /// from the same sources `build_search_index` indexes, so a snippet equals
    /// the line the match came from: history via the bounded
    /// `get_history_line`, visible rows via `get_line_text`. `None` when `abs`
    /// is evicted or past the live viewport. Feeds `search_summary`'s snippet.
    #[must_use]
    pub fn abs_row_text(&self, abs: u64) -> Option<String> {
        use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
        let grid = &self.grid;
        let oldest = grid.oldest_absolute_row();
        if abs < oldest {
            return None;
        }
        let rel = usize::try_from(abs - oldest).ok()?;
        let scrollback = grid.scrollback_lines();
        if rel < scrollback {
            return grid.get_history_line(rel).map(|l| {
                line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned()
            });
        }
        let visible_row = rel - scrollback;
        if visible_row >= usize::from(self.rows()) {
            return None;
        }
        let r = i32::try_from(visible_row).ok()?;
        self.get_line_text(r, None)
    }

    /// Smart word boundaries at a position on a display-relative row.
    ///
    /// Context-aware rules find semantic units (URLs, file paths, email
    /// addresses, git hashes, quoted strings), falling back to plain word
    /// boundaries. Reads scrollback when the viewport is scrolled back. `row`
    /// is display-relative (0 is the viewport's top). Returns `(start_col,
    /// end_col)`, or `None` on whitespace or out of bounds.
    #[must_use]
    pub fn smart_word_at(
        &self,
        row: usize,
        col: usize,
        smart: &crate::selection::SmartSelection,
    ) -> Option<(usize, usize)> {
        let text = self.display_row_text(row)?;
        smart.word_boundaries_at_column(&text, col)
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal::Terminal;

    // ---- Stale CellExtras cleanup on overwrite ------------------------------
    //
    // Through the VT byte stream: extras-bearing cells (hyperlink, combining
    // marks) overwritten by plain text leave no entry in the extras map and no
    // stale data for a later styled write on the same coordinate.

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

        // Overwrite all 4 linked cells with plain text (default-style
        // ASCII — the blast fast path).
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
        term.process("e\u{0301}".as_bytes()); // 'e' + combining acute
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
        // An RGB-styled write on an old hyperlink cell must not pick up the
        // stale entry's hyperlink.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\");
        assert!(term.hyperlink_at(0, 0).is_some());

        term.process(b"\r\x1b[58:2::255:0:0m\x1b[4mZ\x1b[0m"); // underline-colored Z

        assert_eq!(term.row_text(0).as_deref(), Some("Z"));
        assert_eq!(
            term.hyperlink_at(0, 0),
            None,
            "old hyperlink must not attach to the new styled char (#7456)"
        );
    }

    // ---- display_row_text across the history/live boundary ------------------

    /// display_row_text returns exactly what each display row shows when the
    /// viewport straddles the history/live boundary (live rows are not
    /// shifted again by display_offset).
    #[test]
    fn display_row_text_straddling_viewport_reads_each_displayed_row() {
        let mut term = Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        // Live viewport: line26..line29 + the blank cursor row.
        for (r, want) in ["line26", "line27", "line28", "line29", ""]
            .iter()
            .enumerate()
        {
            assert_eq!(term.display_row_text(r).as_deref(), Some(*want));
        }

        // Straddle: 2 history rows above 3 live rows.
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

        // Fully in history still reads correctly.
        term.grid_mut().scroll_display(24); // offset 26 = top of retention
        assert_eq!(term.display_row_text(0).as_deref(), Some("line0"));
        assert_eq!(term.display_row_text(4).as_deref(), Some("line4"));

        // Identity law: back at the live bottom, display == visible read.
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
        // Live viewport: line25, line26, line27, accent row, blank cursor row.
        assert_eq!(term.row_text(3).as_deref(), Some("acce\u{0301}nt"));

        term.grid_mut().scroll_display(1);
        // Display rows now: line24 | line25, line26, line27, accent row.
        assert_eq!(
            term.display_row_text(4).as_deref(),
            Some("acce\u{0301}nt"),
            "the live accent row keeps its combining mark while straddling"
        );
    }

    // ---- display_cell_grapheme (display-relative single-cell reads) ---------

    /// The display-relative cell reader tracks the scroll position like
    /// display_row_text (row 0 is the top displayed row), and equals
    /// cell_grapheme at display_offset == 0.
    #[test]
    fn display_cell_grapheme_reads_the_displayed_row() {
        let mut term = Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        // Identity at the live bottom.
        for r in 0..5 {
            for c in 0..8 {
                assert_eq!(
                    term.display_cell_grapheme(r, c),
                    term.cell_grapheme(r, c),
                    "offset-0 identity at ({r},{c})"
                );
            }
        }

        // Fully in history: display row 0 must spell the oldest line.
        term.scroll_to_top();
        let spelled: String = (0..5)
            .map(|c| term.display_cell_grapheme(0, c).unwrap())
            .collect();
        assert_eq!(spelled, "line0", "top display row spells the oldest line");

        // Straddling: history row above, live rows below, each display row's
        // cells spell that row's display_row_text.
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

        // Out-of-range coords stay None (the cell_grapheme contract).
        assert_eq!(term.display_cell_grapheme(999, 0), None);
        assert_eq!(term.display_cell_grapheme(0, 999), None);
    }

    /// A scrolled-off row's complex cluster and combining marks stay paired
    /// with their cells when read back per-cell from history (the ring extras
    /// materializer), and a wide lead's continuation reads as the empty spacer.
    #[test]
    fn display_cell_grapheme_keeps_history_extras_paired() {
        let mut term = Terminal::new(5, 20);
        term.process("\u{1F980}e\u{0301}x\r\n".as_bytes()); // 🦀 (wide) + é (combining) + x
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
