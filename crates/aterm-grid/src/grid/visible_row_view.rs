// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Display-offset-aware per-row read primitive shared by the render snapshot
//! and the text-extraction path.
//!
//! When the viewport is scrolled back (`display_offset > 0`), [`Grid::row`]
//! returns the historical CELLS (its `row_index` applies the offset) but the
//! coordinate-keyed extras accessors (`cell_render_data`, `complex_char_str_at`,
//! `CellExtras::get`) are display-offset-BLIND — they read the LIVE extras map at
//! the raw visible row. Scrolled-off extras were moved into `ring_extras` /
//! lazy buffer / tiered scrollback at scroll time, so the live map no longer holds
//! them: reading there strands every scrolled emoji / CJK-SMP as U+FFFD and drops
//! combining marks, truecolor RGB, and hyperlinks. It also MISALIGNS the still-live
//! bottom rows during a PARTIAL scroll — their cell comes from `visible_row -
//! display_offset` but their extras were read at the raw `visible_row`.
//!
//! This primitive resolves live-vs-history ONCE per row and routes history rows
//! through the unified 3-tier materializer ([`Grid::materialize_scrollback_row_full`]),
//! so both the render and text paths read correctly-paired extras across every tier.
//! At `display_offset == 0` it is byte-identical to the pre-existing live read
//! (`screen_row == visible_row`, `Live` arm forwards to the same accessors).

use std::borrow::Cow;
use std::sync::Arc;

use super::{Grid, HistoryEpoch, MaterializedRow};
use crate::extra_collection::CellRenderData;
use crate::{Cell, CellCoord, CellExtra, Row};

/// A single viewport row resolved against the current `display_offset`.
pub enum VisibleRowView<'a> {
    /// A still-live row. `row` already carries display-offset-mapped CELLS (from
    /// [`Grid::row`]); `screen_row` is the LIVE-extras key (`visible_row -
    /// display_offset`), which equals `visible_row` when not scrolled.
    Live {
        grid: &'a Grid,
        screen_row: u16,
        row: &'a Row,
    },
    /// A scrolled-off history row, materialized (cells + paired extras) through
    /// the unified 3-tier reader — SHARED, because a scrolled-back viewport
    /// reads the same row on frame after frame and the materialization is
    /// memoized per absolute row (see the `viewport_row_cache` module). The
    /// `Arc` is the
    /// price of handing out a memoized row from a `&self` read without a
    /// borrow guard that two live views could panic on; a refcount bump
    /// replaces a full `vec![Cell; cols]` + extras map + per-cluster `Arc<str>`
    /// rebuild.
    History { mat: Arc<MaterializedRow> },
    /// No such row (out of range, or the history index was unavailable).
    Empty,
}

/// Per-cell extras source: the live coordinated probe, or a materialized history
/// cell's extras. Both expose the SAME channels so the render / text leaves read
/// one shape regardless of tier. A materialized [`CellExtra`] is deliberately
/// stored in the same layout as a live one (complex string + separate combining
/// marks + RGB), so the History arm needs no reshaping.
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
    /// LIVE keeps `(base, combining marks)` split, so this is `combining()`.
    /// A MATERIALIZED cell folds the whole grapheme into its complex string
    /// (base = first scalar), so the marks are `complex_char.chars().skip(1)`
    /// (plus any separately-stored combining, normally none) — reconstructing
    /// the same `(base, marks)` pair the live path would produce.
    #[must_use]
    pub fn marks(self) -> Cow<'a, [char]> {
        match self {
            Self::Live(r) => r
                .cell_extra()
                .map_or(Cow::Borrowed(&[][..]), |e| Cow::Borrowed(e.combining())),
            Self::History(e) => match e.and_then(CellExtra::complex_char) {
                Some(s) => {
                    let mut v: Vec<char> = s.chars().skip(1).collect();
                    if let Some(x) = e {
                        v.extend_from_slice(x.combining());
                    }
                    Cow::Owned(v)
                }
                None => e.map_or(Cow::Borrowed(&[][..]), |x| Cow::Borrowed(x.combining())),
            },
        }
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
                // never take the render StyleId branch (which resolves against the
                // LIVE StyleTable). Guard the premise in debug builds.
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
    /// arm mirrors the historical `row_text_into` inner block verbatim but keyed
    /// on `screen_row`; at `display_offset == 0` (`screen_row == visible_row`) it
    /// is byte-identical.
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
                // Combining marks in CellExtra (HashMap). Guarded with has_extras()
                // + non-empty map exactly as the historical read, to skip stale
                // entries from overwritten cells (#7456).
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
    /// `display_offset == 0` always yields `Live` with `screen_row == visible_row`
    /// — byte-identical to reading `Grid::row` + the live extras accessors directly.
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

    /// The ABSOLUTE row number of the history row at `rev_idx` (0 = the newest
    /// scrolled-off line), or `None` when it cannot be computed exactly.
    ///
    /// `absolute_row_counter - visible_rows` is the absolute row of the TOP
    /// LIVE line, so the newest history line sits exactly one above it.
    /// Absolute numbers only ever count UP and are never reused, which is what
    /// lets the row memo key on them with no eviction bookkeeping at all: an
    /// evicted row's key can never be asked for again.
    ///
    /// Deliberately `checked_*` rather than `saturating_*`: saturation would
    /// collapse distinct rows onto 0 and alias two different rows onto one memo
    /// slot — the one arithmetic mistake here that produces a WRONG ROW instead
    /// of a slow one.
    fn history_absolute_row(&self, rev_idx: usize) -> Option<u64> {
        let top_live = self
            .storage
            .absolute_row_counter
            .checked_sub(u64::from(self.storage.visible_rows))?;
        top_live.checked_sub(u64::try_from(rev_idx).ok()?.checked_add(1)?)
    }

    /// The materialized history row at `rev_idx`: from the memo when the memo
    /// can prove it describes THIS history, else materialized once and
    /// memoized. `None` only when the row does not exist.
    ///
    /// This is the whole of SCR-1. Before it, a repaint of a motionless
    /// scrolled-back viewport rebuilt every visible row from the tier store on
    /// every presented frame — and the frame path has no damage gate, so the
    /// pill fade, the cursor blink, an effects frame and every mouse-move of a
    /// selection drag each paid a full viewport of materializations for zero
    /// new information.
    fn materialized_history_row(&self, rev_idx: usize, cols: u16) -> Option<Arc<MaterializedRow>> {
        let Some(abs_row) = self.history_absolute_row(rev_idx) else {
            // Unkeyable (the absolute counter has not advanced past the
            // viewport yet). Read straight through rather than guess a key —
            // an uncacheable row must never share a slot with a real one.
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
            // THE DEBUG NET (see the `viewport_row_cache` module docs): a stale
            // hit is a wrong glyph or colour on screen, the failure mode with
            // the worst signal-to-noise, so debug builds pay a full
            // re-materialize and compare it field-for-field. Compiled out
            // entirely in release. Second-order benefit: with this in place a
            // `cfg(test)` build performs exactly the same number of
            // `row_to_line` / materialize operations as before the memo
            // existed, so the crate's op-count tests keep measuring what they
            // always measured.
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
        // The memo borrow is taken INSIDE `lookup`/`store` and never held
        // across this materialize, so no read path can re-enter it under a live
        // borrow.
        let fresh = Arc::new(self.materialize_scrollback_row_full(rev_idx, cols)?);
        self.viewport_cache.store(epoch, abs_row, &fresh);
        Some(fresh)
    }

    /// The LIVE-frame twin of [`visible_row_view`](Self::visible_row_view): the row
    /// view at `screen_row` IGNORING `display_offset` (as if the viewport were at the
    /// bottom), so its cells + extras are always the live on-screen row. This is the
    /// offset-independent frame the socket introspection reads
    /// (`cell`/`screen`/`cells`) share with [`row_at_screen`](Self::row_at_screen) and
    /// the live text extraction — so a color/attr/wide read cannot pair with a
    /// scrolled-back row while the glyph comes from the live one. At
    /// `display_offset == 0` it is identical to `visible_row_view`.
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

    /// The live on-screen row at `screen_row`, IGNORING `display_offset` (as if
    /// the viewport were at the bottom) — the row reference behind
    /// [`row_text_screen_into`](Self::row_text_screen_into), for callers that
    /// need cell-level LIVE-frame reads (terminal-relative text extraction,
    /// whose extras lookups are keyed by the live visible row).
    #[must_use]
    pub fn row_at_screen(&self, screen_row: u16) -> Option<&Row> {
        self.storage.row_at_screen(screen_row)
    }

    /// LIVE-frame twin of [`is_wide_continuation_at`](Self::is_wide_continuation_at):
    /// same DECSCA-disambiguated continuation rule, keyed on the SCREEN row
    /// (offset-independent) so it pairs with [`row_at_screen`](Self::row_at_screen).
    #[must_use]
    #[inline]
    pub fn is_wide_continuation_at_screen(&self, screen_row: u16, col: u16) -> bool {
        self.storage
            .row_at_screen(screen_row)
            .is_some_and(|r| r.is_cell_wide_continuation(col))
    }

    /// Offset-INDEPENDENT read of the live on-screen row at `screen_row` (as if
    /// `display_offset == 0`), for absolute-frame callers (block command/output
    /// text extraction) that must NOT follow the scroll position. Unlike
    /// [`row_text_into`](Self::row_text_into) — which is display-offset-aware and
    /// returns the SCROLLED view — this always reads the live grid row + its live
    /// extras, so a user scrolled at read time cannot inject / duplicate history.
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

/// SCR-1 pins for the viewport row memo. Every one of these is a DIFFERENTIAL
/// or IDENTITY test — none of them assert "it is fast", because the memo's only
/// interesting property is that it is invisible.
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

    /// A repeat read of a MOTIONLESS scrolled-back viewport returns the SAME
    /// shared row — the stationary-repaint case, which was a full
    /// re-materialization per row per frame.
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

    /// The memo is keyed on ROW IDENTITY, not viewport position: after a
    /// one-line scroll the same history row is at a different viewport index
    /// and must still be the same shared row. This is the whole reason an
    /// overlapping wheel notch costs 3 rows instead of 24.
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

    /// A CONTENT change drops the whole memo: the next read must rebuild.
    /// Two-sided against the test above, which proves reads without a content
    /// change do NOT rebuild.
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

    /// Output ARRIVING while scrolled back: history grows while
    /// `display_offset` stays put, so every visible row's history index shifts
    /// by one and the content under the viewport slides UP by a row (row 0 is
    /// the OLDEST visible row, `rev_idx = display_offset - 1 - row`, so a new
    /// history line pushes the old row 0 off the top and lifts the old row 1
    /// into its place — `scroll_up` deliberately does NOT re-anchor the
    /// viewport, see `Grid::scroll_display`). The memo must follow the
    /// CONTENT, never the index: an index-keyed memo would still be showing
    /// the pre-arrival row 0 here.
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

    /// THE DIFFERENTIAL: every visible row of a scrolled-back viewport, read
    /// through the memo, is byte-identical to a fresh materialization of the
    /// same row — checked over a scrub that mixes hits and misses.
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
