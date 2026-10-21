// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Display-offset-aware per-row reads, shared by the render snapshot and text extraction.
//!
//! Scrolled back (`display_offset > 0`), [`Grid::row`] returns historical cells, but the
//! coordinate-keyed extras accessors (`cell_render_data`, `push_complex_char_at`,
//! `CellExtras::get`) read the live extras map at the raw visible row, while scrolled-off
//! extras live in `ring_extras`, the lazy buffer or the tiered store. [`VisibleRowView`]
//! resolves live-vs-history once per row and routes history rows through
//! [`Grid::materialize_scrollback_row_full`], so every tier reads correctly paired extras.

use std::sync::Arc;

use super::{Grid, HistoryEpoch, MaterializedRow};
use crate::extra_collection::CellRenderData;
use crate::{Cell, CellCoord, CellExtra, Row};

/// A single viewport row resolved against the current `display_offset`.
pub enum VisibleRowView<'a> {
    /// A still-live row: `row` holds display-offset-mapped cells (from [`Grid::row`]);
    /// `screen_row` is the live extras key, `visible_row - display_offset`.
    Live {
        grid: &'a Grid,
        screen_row: u16,
        row: &'a Row,
    },
    /// A scrolled-off history row, materialized (cells + paired extras). Shared: a
    /// scrolled-back viewport reads the same row every frame, so materialization is memoized
    /// per absolute row (`viewport_row_cache`) and handed out from `&self`.
    History { mat: Arc<MaterializedRow> },
    /// No such row (out of range, or the history index was unavailable).
    Empty,
}

/// Per-cell extras: the live probe or a materialized history cell's extras, behind one shape
/// for every tier.
#[derive(Clone, Copy)]
pub enum CellDataView<'a> {
    Live(CellRenderData<'a>),
    History(Option<&'a CellExtra>),
}

impl<'a> CellDataView<'a> {
    /// Foreground RGB overflow color, if any.
    #[must_use]
    pub fn fg_rgb(self) -> Option<[u8; 3]> {
        match self {
            Self::Live(r) => r.fg_rgb(),
            Self::History(e) => e.and_then(|x| x.fg_rgb()),
        }
    }

    /// Background RGB overflow color, if any.
    #[must_use]
    pub fn bg_rgb(self) -> Option<[u8; 3]> {
        match self {
            Self::Live(r) => r.bg_rgb(),
            Self::History(e) => e.and_then(|x| x.bg_rgb()),
        }
    }

    /// Base codepoint for a complex cell (the first scalar of the complex
    /// string). Mirrors [`CellRenderData::complex_char`] for both tiers.
    #[must_use]
    pub fn complex_base(self) -> Option<char> {
        match self {
            Self::Live(r) => r.complex_char(),
            Self::History(e) => e
                .and_then(|x| x.complex_char())
                .and_then(|s| s.chars().next()),
        }
    }

    /// The cell's [`CellExtra`] (underline color, hyperlink). For combining marks and cluster
    /// tails use [`marks`](Self::marks): live and history store them differently.
    #[must_use]
    pub fn cell_extra(self) -> Option<&'a CellExtra> {
        match self {
            Self::Live(r) => r.cell_extra(),
            Self::History(e) => e,
        }
    }

    /// The overlay marks for the render fold: combining diacritics, or the trailing scalars
    /// of an emoji cluster (ZWJ / skin tone / flag).
    ///
    /// A live cell keeps `(base, combining marks)` split, so this is `combining()`. A
    /// materialized cell folds the grapheme into its complex string (base = first scalar), so
    /// the marks are that string's tail plus any separately stored combining marks. Borrowed,
    /// so repainting cached history does not allocate; clone the iterator for a second pass.
    pub fn marks(self) -> impl Iterator<Item = char> + Clone + 'a {
        let extra = self.cell_extra();
        let complex = match self {
            Self::History(_) => extra
                .and_then(CellExtra::complex_char)
                .map_or("", |s| s.as_ref()),
            Self::Live(_) => "",
        };
        let mut tail = complex.chars();
        let _ = tail.next(); // The complex string starts with the base scalar.
        let combining = extra.map_or(&[][..], CellExtra::combining);
        tail.chain(combining.iter().copied())
    }
}

impl<'a> VisibleRowView<'a> {
    /// Effective column count of this row (matches `Row::len` / `MaterializedRow::len`).
    #[must_use]
    pub fn len(&self) -> u16 {
        match self {
            Self::Live { row, .. } => row.len(),
            Self::History { mat } => mat.len(),
            Self::Empty => 0,
        }
    }

    /// How many columns the row's text spans ([`Row::written_len`]): a
    /// recorded early-wrap hole is not text, also when BCE put it inside
    /// `len`, live or painted on a history row.
    #[must_use]
    pub fn written_len(&self) -> u16 {
        match self {
            Self::Live { row, .. } => row.written_len(),
            Self::History { mat } => mat.written_len(),
            Self::Empty => 0,
        }
    }

    /// Whether the row has no occupied columns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this row is a soft-wrap continuation of the previous line; `false` for
    /// `Empty`.
    #[must_use]
    pub fn is_wrapped(&self) -> bool {
        match self {
            Self::Live { row, .. } => row.is_wrapped(),
            Self::History { mat } => mat.is_wrapped(),
            Self::Empty => false,
        }
    }

    /// The cell at `col`, or `None` if out of range.
    #[must_use]
    pub fn cell(&self, col: u16) -> Option<Cell> {
        match self {
            Self::Live { row, .. } => row.get(col).copied(),
            Self::History { mat } => mat.cells.get(col as usize).copied(),
            Self::Empty => None,
        }
    }

    /// The occupied cell prefix (`len()` cells) as a contiguous slice, avoiding per-column
    /// `Live`/`History` enum dispatch in hot row-rendering loops.
    #[must_use]
    #[inline]
    pub fn cells_slice(&self) -> &[Cell] {
        match self {
            Self::Live { row, .. } => {
                let s = row.as_slice();
                let n = usize::from(row.len()).min(s.len());
                &s[..n]
            }
            Self::History { mat } => {
                let s = mat.cells.as_slice();
                let n = usize::from(mat.len()).min(s.len());
                &s[..n]
            }
            Self::Empty => &[],
        }
    }

    /// Whether the cell at `col` is the blank right half of a wide glyph, by
    /// `Row::is_cell_wide_continuation`'s rule: DECSCA protection shares bit 10 with
    /// WIDE_CONTINUATION, so a real continuation is not itself WIDE and sits right of a WIDE
    /// cell.
    #[must_use]
    pub fn is_wide_continuation(&self, col: u16) -> bool {
        self.cell(col)
            .is_some_and(|c| c.is_wide_continuation() && !c.is_wide())
            && col > 0
            && self.cell(col - 1).is_some_and(|c| c.is_wide())
    }

    /// The char at `col` as [`Grid::row_cols_into`] projects it: the lead char (a complex
    /// cell's base codepoint), `'\0'` at a wide continuation, `' '` for a blank or past the
    /// row.
    #[must_use]
    #[inline]
    pub fn col_char(&self, col: u16) -> char {
        match self.cell(col) {
            None => ' ',
            Some(_) if self.is_wide_continuation(col) => '\0',
            Some(cell) if cell.is_complex() => self
                .cell_data(col, cell)
                .complex_base()
                .unwrap_or('\u{FFFD}'),
            Some(cell) => {
                let c = cell.char();
                if c == '\0' { ' ' } else { c }
            }
        }
    }

    /// The extras view for `col`; `cell` lets the live probe flag-gate.
    #[must_use]
    pub fn cell_data(&self, col: u16, cell: Cell) -> CellDataView<'_> {
        match self {
            Self::Live {
                grid, screen_row, ..
            } => CellDataView::Live(grid.cell_render_data(*screen_row, col, cell)),
            Self::History { mat } => CellDataView::History(mat.get_extra(col)),
            Self::Empty => CellDataView::History(None),
        }
    }

    /// Append this cell's text (complex string or plain char, then any combining marks) to
    /// `out`, reading extras from the right tier.
    pub fn push_cell_text(&self, col: u16, cell: Cell, out: &mut String) {
        match self {
            Self::Live {
                grid, screen_row, ..
            } => {
                if cell.is_complex() {
                    if !grid.push_complex_char_at(*screen_row, col, out) {
                        out.push('\u{FFFD}');
                    }
                } else {
                    let ch = cell.char();
                    out.push(if ch == '\0' { ' ' } else { ch });
                }
                // Combining marks, gated on HAS_EXTRAS so an overwritten cell's stale
                // entry is skipped.
                if cell.has_extras()
                    && !grid.storage.extras.is_empty()
                    && let Some(extra) = grid.storage.extras.get(CellCoord::new(*screen_row, col))
                {
                    for &mark in extra.combining() {
                        out.push(mark);
                    }
                }
            }
            Self::History { mat } => {
                let ex = mat.get_extra(col);
                if cell.is_complex() {
                    match ex.and_then(CellExtra::complex_char) {
                        Some(s) => out.push_str(s),
                        None => out.push('\u{FFFD}'),
                    }
                } else {
                    let ch = cell.char();
                    out.push(if ch == '\0' { ' ' } else { ch });
                }
                if let Some(ex) = ex {
                    for &mark in ex.combining() {
                        out.push(mark);
                    }
                }
            }
            Self::Empty => {}
        }
    }
}

impl Grid {
    /// Resolve a viewport row against `display_offset`: [`Live`] when the viewport is not
    /// scrolled past it, else [`History`], materialized through the 3-tier reader. At
    /// `display_offset == 0` always `Live` with `screen_row == visible_row`.
    ///
    /// [`Live`]: VisibleRowView::Live
    /// [`History`]: VisibleRowView::History
    #[must_use]
    pub fn visible_row_view(&self, visible_row: u16) -> VisibleRowView<'_> {
        let d = self.display_offset();
        if usize::from(visible_row) >= d {
            // `Grid::row` display-maps the cells; the live extras key is
            // `visible_row - display_offset`, which cannot underflow here.
            let screen_row = visible_row.saturating_sub(u16::try_from(d).unwrap_or(0));
            match self.row(visible_row) {
                Some(row) => VisibleRowView::Live {
                    grid: self,
                    screen_row,
                    row,
                },
                None => VisibleRowView::Empty,
            }
        } else {
            // rev_idx 0 is the newest scrolled-off line.
            let rev_idx = d - 1 - usize::from(visible_row);
            match self.materialized_history_row(rev_idx, self.cols()) {
                Some(mat) => VisibleRowView::History { mat },
                None => VisibleRowView::Empty,
            }
        }
    }

    /// The absolute row number of the history row at `rev_idx` (0 = newest scrolled-off line),
    /// or `None` when it cannot be computed exactly: one above the top live line,
    /// `absolute_row_counter - visible_rows`. Absolute numbers only count up and are never
    /// reused, so the memo keys on them without eviction bookkeeping. `checked_*`, because
    /// saturation would alias distinct rows onto one memo slot.
    fn history_absolute_row(&self, rev_idx: usize) -> Option<u64> {
        let top_live = self
            .storage
            .absolute_row_counter
            .checked_sub(u64::from(self.storage.visible_rows))?;
        top_live.checked_sub(u64::try_from(rev_idx).ok()?.checked_add(1)?)
    }

    /// The materialized history row at `rev_idx`, from the memo when it provably describes
    /// this history, else materialized and memoized; `None` only when the row does not exist.
    /// The frame path has no damage gate, so without the memo every repaint of a motionless
    /// scrolled-back viewport (cursor blink, effects frames) would rematerialize every row.
    fn materialized_history_row(&self, rev_idx: usize, cols: u16) -> Option<Arc<MaterializedRow>> {
        let Some(abs_row) = self.history_absolute_row(rev_idx) else {
            // Unkeyable (the absolute counter has not passed the viewport yet): read through,
            // uncached.
            #[cfg(any(test, debug_assertions))]
            super::count_viewport_row_materialize();
            return self
                .materialize_scrollback_row_full(rev_idx, cols)
                .map(Arc::new);
        };
        let epoch = HistoryEpoch {
            content_gen: self.storage.content_gen,
            renumber: self.storage.history_renumber_epoch,
            cols,
            visible_rows: self.storage.visible_rows,
        };
        if let Some(hit) = self.viewport_cache.lookup(epoch, abs_row) {
            // A stale hit is a wrong glyph on screen, so debug builds re-materialize every hit
            // and compare (see `viewport_row_cache`).
            #[cfg(debug_assertions)]
            {
                let fresh = self.materialize_scrollback_row_full(rev_idx, cols);
                debug_assert!(
                    fresh.as_ref() == Some(&*hit),
                    "viewport row memo served a stale row for absolute row {abs_row} \
                     (rev_idx {rev_idx}, cols {cols})"
                );
            }
            return Some(hit);
        }
        // The memo borrow lives inside `lookup`/`store`, never across this materialize, so no
        // read path re-enters it. Counted here on the miss, not in
        // `materialize_scrollback_row_full`, which the debug check calls on every hit.
        #[cfg(any(test, debug_assertions))]
        super::count_viewport_row_materialize();
        let fresh = Arc::new(self.materialize_scrollback_row_full(rev_idx, cols)?);
        self.viewport_cache.store(epoch, abs_row, &fresh);
        Some(fresh)
    }

    /// [`visible_row_view`](Self::visible_row_view) ignoring `display_offset`: always the live
    /// on-screen row. Socket introspection (`cell`/`screen`/`cells`) and live text extraction
    /// read this frame, so a colour/attr/wide read never pairs with a scrolled-back row.
    #[must_use]
    pub fn screen_row_view(&self, screen_row: u16) -> VisibleRowView<'_> {
        match self.row_at_screen(screen_row) {
            Some(row) => VisibleRowView::Live {
                grid: self,
                screen_row,
                row,
            },
            None => VisibleRowView::Empty,
        }
    }

    /// The live on-screen row at `screen_row`, ignoring `display_offset`, for live-frame
    /// cell reads whose extras lookups key on the live visible row.
    #[must_use]
    pub fn row_at_screen(&self, screen_row: u16) -> Option<&Row> {
        self.storage.row_at_screen(screen_row)
    }

    /// The ring row at display index `visible_row` with no viewport bound: the
    /// row just past the viewport (`visible_row == rows`) whose DEC line size
    /// the incoming-row apron reads. `None` past the ring.
    #[must_use]
    pub fn row_past_viewport(&self, visible_row: u16) -> Option<&Row> {
        self.storage.row_past_viewport(visible_row)
    }

    /// [`is_wide_continuation_at`](Self::is_wide_continuation_at) keyed on the live screen
    /// row, pairing with [`row_at_screen`](Self::row_at_screen).
    #[must_use]
    #[inline]
    pub fn is_wide_continuation_at_screen(&self, screen_row: u16, col: u16) -> bool {
        self.storage
            .row_at_screen(screen_row)
            .is_some_and(|r| r.is_cell_wide_continuation(col))
    }

    /// The text of the live on-screen row at `screen_row`, ignoring `display_offset`, for
    /// absolute-frame callers (block command/output extraction): unlike
    /// [`row_text_into`](Self::row_text_into), a user scrolled back at read time cannot inject
    /// or duplicate history.
    pub fn row_text_screen_into(&self, screen_row: u16, out: &mut String) -> bool {
        out.clear();
        let Some(row) = self.storage.row_at_screen(screen_row) else {
            return false;
        };
        let view = VisibleRowView::Live {
            grid: self,
            screen_row,
            row,
        };
        let len = view.written_len();
        out.reserve(len as usize);
        for col in 0..len {
            let Some(cell) = view.cell(col) else {
                continue;
            };
            if view.is_wide_continuation(col) {
                continue;
            }
            view.push_cell_text(col, cell, out);
        }
        true
    }

    /// Allocating twin of [`row_text_screen_into`](Self::row_text_screen_into).
    #[must_use]
    pub fn row_text_screen(&self, screen_row: u16) -> Option<String> {
        let mut s = String::new();
        self.row_text_screen_into(screen_row, &mut s).then_some(s)
    }
}

/// Viewport row memo tests: differential or identity, since the memo's one property is that it
/// is invisible.
#[cfg(test)]
mod viewport_cache_tests {
    use std::sync::Arc;

    use super::VisibleRowView;
    use crate::Grid;

    /// A 3-row grid with `lines` numbered lines pushed into ring history.
    fn grid_with_history(lines: usize) -> Grid {
        let mut grid = Grid::new(3, 8);
        for i in 0..lines {
            grid.carriage_return();
            for ch in format!("L{i:03}").chars() {
                grid.write_char(ch);
            }
            grid.scroll_up(1);
        }
        grid
    }

    /// The shared row behind a history viewport row; panics on a live one.
    fn history_row(grid: &Grid, visible_row: u16) -> Arc<super::MaterializedRow> {
        match grid.visible_row_view(visible_row) {
            VisibleRowView::History { mat } => mat,
            _ => panic!("visible row {visible_row} is not a history row"),
        }
    }

    /// The row's text, read the way the render/text leaves read it.
    fn row_text(grid: &Grid, visible_row: u16) -> String {
        let view = grid.visible_row_view(visible_row);
        let mut out = String::new();
        for col in 0..view.len() {
            if let Some(cell) = view.cell(col) {
                view.push_cell_text(col, cell, &mut out);
            }
        }
        out
    }

    /// The memo keys on row identity, not viewport position: a row keeps its materialization
    /// across a one-line scroll.
    #[test]
    fn a_scrolled_row_keeps_its_materialization_at_its_new_viewport_index() {
        let mut grid = grid_with_history(40);
        grid.scroll_display(10);
        let before = history_row(&grid, 1);
        let text_before = row_text(&grid, 1);
        // One line further back: what was viewport row 1 is now viewport row 2.
        grid.scroll_display(1);
        let after = history_row(&grid, 2);
        assert_eq!(
            row_text(&grid, 2),
            text_before,
            "the row that moved down one viewport slot is not the same line"
        );
        assert!(
            Arc::ptr_eq(&before, &after),
            "an overlapping scroll re-materialized a row it had already built"
        );
    }

    /// A content change drops the whole memo.
    #[test]
    fn a_content_change_invalidates_every_memoized_row() {
        let mut grid = grid_with_history(40);
        grid.scroll_display(10);
        let before = history_row(&grid, 0);
        let text_before = row_text(&grid, 0);
        grid.mark_content_full();
        let after = history_row(&grid, 0);
        assert!(
            !Arc::ptr_eq(&before, &after),
            "a content change left stale rows in the memo"
        );
        assert_eq!(
            row_text(&grid, 0),
            text_before,
            "the rebuilt row does not match the row it replaced"
        );
    }

    /// Output arriving while scrolled back grows history under a fixed `display_offset`, so
    /// the content under the viewport slides up a row; the memo follows the content, not the
    /// index.
    #[test]
    fn a_line_arriving_while_scrolled_back_reshuffles_rows_correctly() {
        let mut grid = grid_with_history(40);
        grid.scroll_display(10);
        let text_row0 = row_text(&grid, 0);
        let text_row1 = row_text(&grid, 1);
        assert_ne!(text_row0, text_row1, "the fixture rows are not distinct");
        // One more line into history; the viewport does not move.
        grid.carriage_return();
        for ch in "NEW".chars() {
            grid.write_char(ch);
        }
        grid.scroll_up(1);
        assert_eq!(
            row_text(&grid, 0),
            text_row1,
            "the line that was at viewport row 1 is not at row 0 after one new line"
        );
        assert_ne!(
            row_text(&grid, 0),
            text_row0,
            "viewport row 0 still shows the pre-arrival line — a row was served by \
             INDEX across a history growth"
        );
    }

    /// Memoized rows equal a fresh materialization over a scrub mixing hits and misses.
    #[test]
    fn memoized_rows_match_a_fresh_materialize_across_a_scrub() {
        let mut grid = grid_with_history(60);
        grid.scroll_display(30);
        for _ in 0..20 {
            let offset = grid.display_offset();
            for r in 0..3u16 {
                if usize::from(r) >= offset {
                    continue;
                }
                let rev_idx = offset - 1 - usize::from(r);
                let fresh = grid
                    .materialize_scrollback_row_full(rev_idx, grid.cols())
                    .expect("history row exists");
                let cached = history_row(&grid, r);
                assert_eq!(
                    *cached, fresh,
                    "memoized viewport row {r} (rev_idx {rev_idx}) diverged from a \
                     fresh materialization"
                );
            }
            // One line toward live per pass: two of the three rows were built last pass
            // (hits), one is new (a miss).
            grid.scroll_display(-1);
        }
    }
}
