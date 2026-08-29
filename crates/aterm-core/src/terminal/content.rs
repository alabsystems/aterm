// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Text content extraction and smart selection API.
//!
//! Methods for extracting text content from the terminal grid and
//! performing smart selection (URLs, paths, etc.) for triggers and UI.

use crate::grid::{Grid, row_u16};

use super::Terminal;

fn push_cell_text(grid: &Grid, row: u16, col: u16, out: &mut String) {
    // LIVE-frame (screen-row) reads throughout: the extras lookups below are
    // keyed by the live visible row, and callers pass terminal-relative rows
    // (the renderer's selection contract, `sel_row = viewport_row -
    // display_offset`). The display-mapped `Grid::cell` here paired scrolled
    // cells with live extras and double-shifted `display_row_text` /
    // selection copies whenever the viewport straddled the history boundary.
    let Some(cell) = grid.row_at_screen(row).and_then(|r| r.get(col)) else {
        return;
    };
    // Context-aware check: Cell::is_wide_continuation() would false-positive
    // on DECSCA-protected cells (PROTECTED shares bit 10), hiding protected
    // text from the read API.
    if grid.is_wide_continuation_at_screen(row, col) {
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
    // Combining marks from CellExtra — gate on has_extras() so cells
    // without extras skip the HashMap probe. Stale entries from
    // overwritten cells are removed at the grid write path since #7456
    // was fixed (grid/write.rs `remove_stale_extras*`); the flag check
    // remains as the fast path and as defense in depth.
    if cell.has_extras() {
        if let Some(extra) = grid.cell_extra(row, col) {
            for &combining in extra.combining() {
                out.push(combining);
            }
        }
    }
}

/// Extract LIVE-frame (screen-row, display_offset-independent) row text for an
/// inclusive column range.
#[must_use]
pub(crate) fn visible_row_bounds_to_string(
    grid: &Grid,
    row: u16,
    start_col: u16,
    end_col: u16,
) -> String {
    // Clamp end_col to actual grid width. `side_adjusted_bounds` uses
    // u16::MAX as a sentinel for "entire row" when the end retreats to the
    // previous row; iterating up to 65535 wastes ~65K no-op cell lookups.
    let last_col = grid.cols().saturating_sub(1);
    let end_col = end_col.min(last_col);

    // Both edges widen over a double-width glyph through the SAME authority the
    // highlight predicate uses — `glyph_cell_span`: a CJK/emoji glyph is one
    // indivisible unit, selected whole whenever either of its cells is. An edge
    // that landed mid-glyph would otherwise make the copy and the paint describe
    // different runs of cells (#7526): a start on the continuation would drop the
    // lead's text under a painted highlight, and an end on the lead would take a
    // character whose right half was never highlighted.
    //
    // Context-aware continuation checks so DECSCA-protected cells are not
    // mistaken for continuations (shared bit 10). Screen-row keyed, like
    // `push_cell_text`. A cell one past `last_col` cannot be a continuation, so
    // an end already at the row's edge stays there.
    let is_continuation =
        |col: u16| col <= last_col && grid.is_wide_continuation_at_screen(row, col);
    let (effective_start, _) = aterm_types::selection::glyph_cell_span(
        start_col,
        is_continuation(start_col.saturating_add(1)),
        is_continuation(start_col),
    );
    // Extending the end onto a continuation adds no TEXT — the cluster lives on
    // the lead and `push_cell_text` yields nothing for the right half. It earns
    // its place as a seam, not as output: ONE rule governs both edges instead of
    // two that agree by coincidence, and the loop's span stays the painted span.
    let (_, effective_end) = aterm_types::selection::glyph_cell_span(
        end_col,
        is_continuation(end_col.saturating_add(1)),
        is_continuation(end_col),
    );
    let end_col = effective_end.min(last_col);

    // One byte per column is the exact ASCII size and a tight lower bound
    // otherwise, so a plain row lands in a single allocation instead of growing
    // by doubling from zero (~ceil(log2(bytes)) realloc+memcpy cycles per
    // extracted row). Both bounds are already inside `0..=last_col`, so this can
    // never exceed one row's width. Mirrors `Grid::row_text_screen_into`, which
    // reserves the same way before its identical per-column push loop.
    let mut line = String::with_capacity(
        usize::from(end_col.saturating_sub(effective_start)).saturating_add(1),
    );
    for col in effective_start..=end_col {
        push_cell_text(grid, row, col, &mut line);
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

    /// Get the text content of a specific visible row.
    ///
    /// Row 0 is the top visible row. Returns `None` if row is out of bounds.
    /// Useful for trigger evaluation on specific lines.
    #[must_use]
    pub fn row_text(&self, row: usize) -> Option<String> {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            return None;
        }
        self.grid.row_text(row_u16(row))
    }

    /// Append visible row `row`'s text into `out`, reusing its capacity, and
    /// report whether the row was in bounds.
    ///
    /// Bounds-checked, non-allocating counterpart of [`row_text`](Self::row_text):
    /// clears `out` then appends exactly what `row_text` would return for an
    /// in-bounds row (returning `true`); for an out-of-range row it leaves `out`
    /// cleared and returns `false`. The Observation Kernel's per-batch row scan
    /// (`observe_at`) refills a persistent scratch buffer through this so a
    /// `RowMatches` watcher does not heap-allocate one `String` per visible row
    /// on every processed batch.
    #[must_use]
    pub fn row_text_into(&self, row: usize, out: &mut String) -> bool {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return false;
        }
        self.grid.row_text_into(row_u16(row), out)
    }

    /// Fill `out` with visible row `row`'s content as PER-COLUMN chars and
    /// return the row's FILL (one past the last non-blank column; `0` for an
    /// empty or out-of-range row).
    ///
    /// The column-indexed sibling of [`row_text_into`](Self::row_text_into),
    /// for per-frame row DIFFING (the GUI's erase-poof probe): one `char` per
    /// grid column — the resolved lead char at its column, `'\0'` at wide
    /// continuations, `' '` for blanks — so span math survives CJK/emoji.
    /// Clear-then-extend into the caller's buffer (zero steady-state
    /// allocation on the probe path).
    pub fn row_cols_into(&self, row: usize, out: &mut Vec<char>) -> u16 {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return 0;
        }
        self.grid.row_cols_into(row_u16(row), out)
    }

    /// Fill `out` with at most the first `prefix_len` entries of visible row
    /// `row` using the same per-column projection as [`Self::row_cols_into`].
    ///
    /// The grid scan and destination growth are both bounded by `prefix_len`;
    /// a sparse row's implicit blank tail is left for the caller to pad when a
    /// fixed-width prefix is required.
    pub fn row_cols_prefix_into(&self, row: usize, prefix_len: usize, out: &mut Vec<char>) -> u16 {
        let rows = usize::from(self.grid.rows());
        if row >= rows {
            out.clear();
            return 0;
        }
        self.grid
            .row_cols_prefix_into(row_u16(row), prefix_len, out)
    }

    /// Get the combining-aware grapheme text of a single VISIBLE cell.
    ///
    /// Returns the resolved base character plus any complex-cluster string and
    /// trailing combining marks for visible-grid cell `(row, col)` — the SAME
    /// content the selection and `row_text`/`get_line_text` paths produce, so an
    /// introspecting reader of one cell never silently drops an NFD accent
    /// (`e`+U+0301) or a ZWJ emoji cluster (👨‍👩‍👧) the pixels and selection show.
    ///
    /// A wide-continuation cell (the blank right half of a CJK/emoji glyph)
    /// returns the empty string; its glyph belongs to the lead cell. Returns
    /// `None` only for an out-of-range row/col (so the caller can report a
    /// distinct "out of range"), and `Some("")` for a genuinely blank cell.
    #[must_use]
    pub fn cell_grapheme(&self, row: usize, col: usize) -> Option<String> {
        let mut out = String::new();
        self.cell_grapheme_into(row, col, &mut out).then_some(out)
    }

    /// The buffer-REUSING twin of [`cell_grapheme`](Self::cell_grapheme): the
    /// same bytes APPENDED to a caller-owned `String`, returning whether the
    /// coordinates were in range — the `Some`/`None` of the allocating form. An
    /// out-of-range cell appends nothing and returns `false`; every in-range
    /// cell returns `true`, including the blank one that appends a single space
    /// and the wide-continuation that appends nothing.
    ///
    /// Both forms route through the SAME `push_cell_text`, so byte-identity is
    /// by construction rather than by argument. This one exists for the rows ×
    /// cols sweeps: the agent-facing styled frame (`aterm ctl screen` /
    /// `subscribe … cells`) built a fresh `String` PER CELL — 1,920 heap
    /// allocations for a 24x80 screen, 10,000 for 50x200, all under the terminal
    /// lock and repeated on every poll. Appending into one per-row buffer makes
    /// that one allocation per row.
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

    /// Get the combining-aware grapheme text of a single DISPLAY cell,
    /// accounting for `display_offset` (scroll position into history).
    ///
    /// The display-relative sibling of [`cell_grapheme`](Self::cell_grapheme)
    /// — the per-cell counterpart of [`display_row_text`](Self::display_row_text):
    /// row 0 is the TOP of the CURRENT viewport, which maps to a scrollback
    /// line while the user is scrolled back. Cells and extras are resolved
    /// together through the unified [`Grid::visible_row_view`] resolver, so a
    /// history row's complex clusters / combining marks stay paired with their
    /// cells across every scrollback tier. At `display_offset == 0` it is
    /// byte-identical to `cell_grapheme`.
    ///
    /// A wide-continuation cell returns the empty string (its glyph belongs to
    /// the lead cell); `None` only for an out-of-range row/col.
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

    /// Every DISPLAY column of `row` resolved ONCE — `(grapheme, is_wide)` per
    /// column — through a single [`Grid::visible_row_view`].
    ///
    /// The batched form of [`display_cell_grapheme`](Self::display_cell_grapheme)
    /// plus a per-cell wide read: a scrolled-back row is a HISTORY row that
    /// `visible_row_view` materializes from scrollback, so resolving it per cell
    /// re-materializes the whole row on every access (O(cols²) for a host that
    /// walks the row cell-by-cell, e.g. the buffer facade's non-ASCII
    /// `translateToString`). This materializes the row ONCE (the `VisibleRowView`
    /// holds the materialized row) and reads each column O(1), so a host caches
    /// this and serves its per-cell reads from it. `grapheme` is empty for a
    /// blank or wide-continuation cell; `is_wide` is the lead cell's double-width
    /// flag. `None` only for an out-of-range row.
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

    /// Get the text content of a display-relative row, accounting for
    /// `display_offset` (scroll position into history).
    ///
    /// When `display_offset > 0`, display row 0 maps to a scrollback line,
    /// not live grid row 0. This method converts display-relative coordinates
    /// to terminal-relative coordinates and reads from the correct source
    /// (scrollback for negative terminal rows, live grid for non-negative).
    ///
    /// Returns `None` if the row is out of bounds.
    #[must_use]
    pub fn display_row_text(&self, display_row: usize) -> Option<String> {
        let offset = self.grid.display_offset();
        if offset == 0 {
            // Fast path: no scrollback scroll — display row == live grid row.
            return self.row_text(display_row);
        }

        // Convert display-relative row to terminal-relative (i32).
        // terminal_row = display_row - display_offset
        // Negative terminal_row = scrollback line.
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

    /// Text of an ABSOLUTE terminal row (the search index's native coordinate),
    /// reconstructed from the SAME sources `build_search_index` indexes so a
    /// snippet equals the line the match was found in: history rows via the
    /// bounded `get_history_line` read, visible rows via `get_line_text`.
    /// `None` when `abs` is below the oldest retained row (evicted) or past the
    /// live viewport. Feeds `search_summary`'s snippet field (fed E-1) without
    /// depending on the index's soon-to-be-dropped String cache (E-4).
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
            return grid
                .get_history_line(rel)
                .map(|l| line_text_bounded(l.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES));
        }
        let visible_row = rel - scrollback;
        if visible_row >= usize::from(self.rows()) {
            return None;
        }
        let r = i32::try_from(visible_row).ok()?;
        self.get_line_text(r, None)
    }

    /// Get smart word boundaries at a position on a display-relative row.
    ///
    /// This uses context-aware selection rules to identify semantic text units
    /// like URLs, file paths, email addresses, git hashes, quoted strings, etc.
    /// Falls back to basic word boundaries for plain text.
    ///
    /// When the terminal is scrolled into history (`display_offset > 0`),
    /// display row 0 corresponds to a scrollback line, not live grid row 0.
    /// This method correctly reads from scrollback when needed.
    ///
    /// # Arguments
    ///
    /// * `row` - The display-relative row index (0 is top of viewport)
    /// * `col` - The column position
    /// * `smart` - The smart selection engine with configured rules
    ///
    /// # Returns
    ///
    /// Returns `Some((start_col, end_col))` if a word/semantic unit is found,
    /// `None` if the position is on whitespace or out of bounds.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aterm_core::terminal::Terminal;
    /// use aterm_core::selection::SmartSelection;
    ///
    /// let terminal = Terminal::new(24, 80);
    /// let smart = SmartSelection::with_builtin_rules();
    /// let (row, col) = (5, 10);
    /// if let Some((start, end)) = terminal.smart_word_at(row, col, &smart) {
    ///     // Select from start to end column
    /// }
    /// ```
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

    // ---- Stale CellExtras cleanup on overwrite (#7456) ----------------------
    //
    // End-to-end proof through the VT byte stream: extras-bearing cells
    // (hyperlink, combining marks) overwritten by plain text must leave NO
    // entry in the extras map (memory leak) and NO stale data reachable by
    // a later styled write on the same coordinate.

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
        // The resurrection case: an RGB-styled write landing on an old
        // hyperlink cell used to `get_or_create` the stale entry and
        // attach the OLD hyperlink to the NEW character.
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

    /// display_row_text must return exactly what each DISPLAY row shows when
    /// the viewport STRADDLES the history/live boundary. Pre-fix, the live
    /// rows of a straddling viewport re-applied display_offset (get_line_text's
    /// positive arm read display-mapped cells), repeating the top history rows.
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

    /// A straddling read must keep live-frame extras (combining marks) PAIRED
    /// with their base cells: pre-fix the cells came from the display-mapped
    /// row while the extras stayed live-keyed, so the accent row read as a
    /// different row's text.
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

    /// The DISPLAY-relative cell reader must track the scroll position exactly
    /// like display_row_text: row 0 reads the top DISPLAYED row (a scrollback
    /// line when scrolled), not live screen row 0 — and it is byte-identical to
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

        // Fully in history: display row 0 must spell the OLDEST line.
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
