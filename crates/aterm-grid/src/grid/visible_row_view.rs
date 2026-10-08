// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Display-offset-aware per-row read primitive shared by the render snapshot
//! and the text-extraction path.
//!
//! When the viewport is scrolled back (`display_offset > 0`), [`Grid::row`]
//! returns the historical cells, but the coordinate-keyed extras accessors
//! (`cell_render_data`, `complex_char_str_at`, `CellExtras::get`) read the live
//! extras map at the raw visible row. Scrolled-off extras live in
//! `ring_extras`, the lazy buffer or the tiered store, so reading the live map
//! would show emoji / CJK-SMP as U+FFFD, drop combining marks, truecolor and
//! hyperlinks, and misalign still-live rows during a partial scroll.
//!
//! This primitive resolves live-vs-history once per row and routes history
//! rows through the unified 3-tier materializer
//! ([`Grid::materialize_scrollback_row_full`]), so render and text paths read
//! correctly paired extras in every tier. At `display_offset == 0` it equals
//! the plain live read (`screen_row == visible_row`).

use std::sync::Arc;

use super::{Grid, HistoryEpoch, MaterializedRow};
use crate::extra_collection::CellRenderData;
use crate::{Cell, CellCoord, CellExtra, Row};

/// A single viewport row resolved against the current `display_offset`.
pub enum VisibleRowView<'a> {
    /// A still-live row. `row` already carries display-offset-mapped cells (from
    /// [`Grid::row`]); `screen_row` is the live-extras key (`visible_row -
    /// display_offset`), which equals `visible_row` when not scrolled.
    Live {
        grid: &'a Grid,
        screen_row: u16,
        row: &'a Row,
    },
    /// A scrolled-off history row, materialized (cells + paired extras) through
    /// the unified 3-tier reader. Shared, because a scrolled-back viewport
    /// reads the same row frame after frame and materialization is memoized
    /// per absolute row (see `viewport_row_cache`); the `Arc` lets a `&self`
    /// read hand out a memoized row without a borrow guard.
    History { mat: Arc<MaterializedRow> },
    /// No such row (out of range, or the history index was unavailable).
    Empty,
}

/// Per-cell extras source: the live coordinated probe, or a materialized
/// history cell's extras, exposing the same channels so render / text readers
/// see one shape in every tier. A materialized [`CellExtra`] uses the live
/// layout (complex string + separate combining marks + RGB).
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

    /// The cell's [`CellExtra`] (underline color, hyperlink). For combining
    /// marks / cluster tails use [`marks`](Self::marks) — the live and history
    /// representations differ (see there).
    #[must_use]
    pub fn cell_extra(self) -> Option<&'a CellExtra> {
        match self {
            Self::Live(r) => r.cell_extra(),
            Self::History(e) => e,
        }
    }

    /// The overlay marks for the render fold: combining diacritics, or the
    /// trailing scalars of an emoji cluster (ZWJ / skin-tone / flag), matching
    /// what the live cluster/combining classifier reads.
    ///
    /// A live cell keeps `(base, combining marks)` split, so this is
    /// `combining()`. A materialized cell folds the whole grapheme into its
    /// complex string (base = first scalar), so the marks are
    /// `complex_char.chars().skip(1)` plus any separately stored combining,
    /// giving the same `(base, marks)` pair. Both are borrowed, so repainting
    /// cached history does not allocate; clone the iterator for separate
    /// classification and output passes.
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

    /// Whether the row has no occupied columns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this row is a soft-wrap continuation of the previous line
    /// (matches `Row::is_wrapped` for live rows and the source `Line`'s wrap
    /// flag for materialized history rows). `false` for an empty/out-of-range
    /// row — callers distinguish that via the `Empty` variant.
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

    /// Whether the cell at `col` is the blank right half of a wide glyph.
    ///
    /// Same rule as `Row::is_cell_wide_continuation` (DECSCA-protected cells
    /// share bit 10 with WIDE_CONTINUATION, so a real continuation must not be
    /// itself WIDE and must sit immediately right of a WIDE main cell), applied
    /// uniformly to live and materialized rows.
    #[must_use]
    pub fn is_wide_continuation(&self, col: u16) -> bool {
        self.cell(col)
            .is_some_and(|c| c.is_wide_continuation() && !c.is_wide())
            && col > 0
            && self.cell(col - 1).is_some_and(|c| c.is_wide())
    }

    /// The per-column char at `col`, the projection [`Grid::row_cols_into`]
    /// builds a row from: the resolved lead char at its own column (a complex
    /// cell contributes its base codepoint), `'\0'` at a wide continuation,
    /// `' '` for a blank or a column past the row.
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

    /// The coordinated extras view for `col`. `cell` is passed so the live probe
    /// can flag-gate (and the history arm can assert its invariants).
    #[must_use]
    pub fn cell_data(&self, col: u16, cell: Cell) -> CellDataView<'_> {
        match self {
            Self::Live {
                grid, screen_row, ..
            } => CellDataView::Live(grid.cell_render_data(*screen_row, col, cell)),
            Self::History { mat } => {
                // Materialize bakes StyleId colors inline, so a history cell must
                // never take the StyleId branch (which reads the live StyleTable).
                debug_assert!(
                    !cell.uses_style_id(),
                    "materialized history cell must have colors baked inline, not a StyleId"
                );
                CellDataView::History(mat.get_extra(col))
            }
            Self::Empty => CellDataView::History(None),
        }
    }

    /// Append this cell's resolved text (complex string or plain char, then any
    /// combining marks) to `out`, reading extras from the correct tier. The Live
    /// arm is keyed on `screen_row`, which equals `visible_row` at
    /// `display_offset == 0`.
    pub fn push_cell_text(&self, col: u16, cell: Cell, out: &mut String) {
        match self {
            Self::Live {
                grid, screen_row, ..
            } => {
                if cell.is_complex() {
                    match grid.complex_char_str_at(*screen_row, col) {
                        Some(cs) => out.push_str(&cs),
                        None => out.push('\u{FFFD}'),
                    }
                } else {
                    let ch = cell.char();
                    out.push(if ch == '\0' { ' ' } else { ch });
                }
                // Combining marks in CellExtra (HashMap), guarded by has_extras()
                // and a non-empty map to skip stale entries of overwritten cells.
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
    /// Resolve a viewport row honoring `display_offset`: a [`Live`] row (cells +
    /// display-mapped extras key) when the viewport is not scrolled past it, else
    /// a [`History`] row materialized through the unified 3-tier reader.
    ///
    /// `display_offset == 0` always yields `Live` with
    /// `screen_row == visible_row`, equal to reading `Grid::row` and the live
    /// extras directly.
    ///
    /// [`Live`]: VisibleRowView::Live
    /// [`History`]: VisibleRowView::History
    #[must_use]
    pub fn visible_row_view(&self, visible_row: u16) -> VisibleRowView<'_> {
        let d = self.display_offset();
        if usize::from(visible_row) >= d {
            // Live row: `Grid::row` display-maps the CELLS; the extras live-map key
            // is `visible_row - display_offset`. Live implies d <= visible_row, so
            // d fits u16 and the subtraction cannot underflow.
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
            // History row: rev_idx 0 == newest scrolled-off line. Single tier-count-
            // independent handle — `try_get_history_line` splits the tiers internally.
            let rev_idx = d - 1 - usize::from(visible_row);
            match self.materialized_history_row(rev_idx, self.cols()) {
                Some(mat) => VisibleRowView::History { mat },
                None => VisibleRowView::Empty,
            }
        }
    }

    /// The absolute row number of the history row at `rev_idx` (0 = the newest
    /// scrolled-off line), or `None` when it cannot be computed exactly.
    ///
    /// `absolute_row_counter - visible_rows` is the absolute row of the top
    /// live line, so the newest history line sits one above it. Absolute
    /// numbers only count up and are never reused, so the row memo can key on
    /// them without eviction bookkeeping.
    ///
    /// `checked_*`, not `saturating_*`: saturation would alias distinct rows
    /// onto one memo slot, showing a wrong row.
    fn history_absolute_row(&self, rev_idx: usize) -> Option<u64> {
        let top_live = self
            .storage
            .absolute_row_counter
            .checked_sub(u64::from(self.storage.visible_rows))?;
        top_live.checked_sub(u64::try_from(rev_idx).ok()?.checked_add(1)?)
    }

    /// The materialized history row at `rev_idx`: from the memo when it
    /// provably describes this history, else materialized once and memoized.
    /// `None` only when the row does not exist.
    ///
    /// The frame path has no damage gate, so without this every repaint of a
    /// motionless scrolled-back viewport (cursor blink, effects frames, drag
    /// mouse-moves) would rematerialize every visible row.
    fn materialized_history_row(&self, rev_idx: usize, cols: u16) -> Option<Arc<MaterializedRow>> {
        let Some(abs_row) = self.history_absolute_row(rev_idx) else {
            // Unkeyable (the absolute counter has not passed the viewport yet):
            // read straight through; an uncacheable row never shares a slot.
            #[cfg(any(test, feature = "testing"))]
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
            // Debug check (see `viewport_row_cache`): a stale hit is a wrong
            // glyph on screen, so debug builds re-materialize every hit and
            // compare field-for-field. Compiled out in release. It also keeps a
            // `cfg(test)` build's `row_to_line` / materialize counts unchanged,
            // so op-count tests measure the same things.
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
        // The memo borrow is taken inside `lookup`/`store` and never held across
        // this materialize, so no read path can re-enter it under a live
        // borrow.
        //
        // Counted here, on the miss, not inside
        // `materialize_scrollback_row_full`: the debug check above
        // re-materializes every hit, so a counter one level down would count
        // every repaint in test builds. See `test_counters`
        // (`count_viewport_row_materialize`) and the aterm-core frame test.
        #[cfg(any(test, feature = "testing"))]
        super::count_viewport_row_materialize();
        let fresh = Arc::new(self.materialize_scrollback_row_full(rev_idx, cols)?);
        self.viewport_cache.store(epoch, abs_row, &fresh);
        Some(fresh)
    }

    /// The live-frame counterpart of [`visible_row_view`](Self::visible_row_view):
    /// the row view at `screen_row` ignoring `display_offset`, so its cells and
    /// extras are always the live on-screen row. Socket introspection
    /// (`cell`/`screen`/`cells`) reads this frame, as do
    /// [`row_at_screen`](Self::row_at_screen) and live text extraction, so a
    /// color/attr/wide read never pairs with a scrolled-back row. At
    /// `display_offset == 0` it equals `visible_row_view`.
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

    /// The live on-screen row at `screen_row`, ignoring `display_offset`: the
    /// row behind [`row_text_screen_into`](Self::row_text_screen_into), for
    /// cell-level live-frame reads (terminal-relative text extraction, whose
    /// extras lookups are keyed by the live visible row).
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

    /// Live-frame counterpart of
    /// [`is_wide_continuation_at`](Self::is_wide_continuation_at): the same
    /// DECSCA-disambiguated rule keyed on the screen row, pairing with
    /// [`row_at_screen`](Self::row_at_screen).
    #[must_use]
    #[inline]
    pub fn is_wide_continuation_at_screen(&self, screen_row: u16, col: u16) -> bool {
        self.storage
            .row_at_screen(screen_row)
            .is_some_and(|r| r.is_cell_wide_continuation(col))
    }

    /// Offset-independent read of the live on-screen row at `screen_row` (as if
    /// `display_offset == 0`), for absolute-frame callers (block command/output
    /// text extraction) that must not follow the scroll position. Unlike the
    /// display-offset-aware [`row_text_into`](Self::row_text_into), a user
    /// scrolled at read time cannot inject or duplicate history.
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
        let len = view.len();
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

/// Viewport row memo tests. All are differential or identity tests: the memo's
/// only interesting property is that it is invisible.
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

    /// The shared row behind a HISTORY viewport row (panics if the row is not
    /// history — every caller below has already scrolled far enough).
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

    /// A repeat read of a motionless scrolled-back viewport returns the same
    /// shared row.
    #[test]
    fn repeat_read_of_a_motionless_viewport_reuses_the_materialized_row() {
        let mut grid = grid_with_history(40);
        grid.scroll_display(10);
        let first = history_row(&grid, 0);
        let second = history_row(&grid, 0);
        assert!(
            Arc::ptr_eq(&first, &second),
            "a second read of an unchanged scrolled-back row re-materialized it"
        );
    }

    /// The memo is keyed on row identity, not viewport position: after a
    /// one-line scroll the same history row at a different index is still the
    /// same shared row.
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

    /// A content change drops the whole memo, so the next read rebuilds; reads
    /// without a content change do not (test above).
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

    /// Output arriving while scrolled back: history grows while
    /// `display_offset` stays put, so the content under the viewport slides up
    /// a row (row 0 is the oldest visible row, `rev_idx = display_offset - 1 -
    /// row`; `scroll_up` does not re-anchor the viewport, see
    /// `Grid::scroll_display`). The memo follows the content, not the index.
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

    /// Every visible row of a scrolled-back viewport read through the memo
    /// equals a fresh materialization, over a scrub mixing hits and misses.
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
            // Reverse at the ends so the scrub keeps re-reading rows it has
            // already built (hits) as well as new ones (misses).
            grid.scroll_display(-1);
        }
    }

    /// A row read at the live bottom is never a history row, so the memo is
    /// not even consulted — the unscrolled frame is untouched by all of this.
    #[test]
    fn an_unscrolled_viewport_reads_live_rows() {
        let grid = grid_with_history(40);
        assert!(
            matches!(grid.visible_row_view(0), VisibleRowView::Live { .. }),
            "an unscrolled viewport must read the live grid"
        );
    }
}
