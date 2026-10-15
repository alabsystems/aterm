// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Getter methods for [`Grid`] dimensions, cursor, subsystem accessors,
//! and damage control.
//!
//! O(1) forwarding to internal state — content queries that iterate over
//! grid data live in [`super::content_queries`].

use aterm_types::index::Dimensions;

use super::Grid;
use crate::Damage;
use crate::StyleTable;
use crate::extra_collection::CellRenderData;
use crate::{CellCoord, CellExtra, CellExtras};
#[cfg(test)]
use crate::{ExtendedStyle, StyleId};

/// Grid dimensions for `aterm_types::index::Dimensions`, so the Alacritty
/// bridge (and other consumers) can use `Dimensions` methods on `Grid` without
/// a foreign impl.
impl Dimensions for Grid {
    fn total_lines(&self) -> usize {
        self.storage.total_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.storage.visible_rows())
    }

    fn columns(&self) -> usize {
        usize::from(self.storage.cols())
    }
}

impl Grid {
    // -------------------------------------------------------------------------
    // Dimension getters
    // -------------------------------------------------------------------------

    /// Get the number of visible rows.
    #[must_use]
    #[inline]
    pub fn rows(&self) -> u16 {
        self.storage.visible_rows()
    }

    /// Get the number of columns.
    #[must_use]
    #[inline]
    pub fn cols(&self) -> u16 {
        self.storage.cols()
    }

    /// Get effective column count for the current cursor row.
    ///
    /// Returns `cols` for normal lines, `cols/2` for double-width (DECDWL) lines.
    /// Callers can cache this to avoid redundant ring-buffer lookups when
    /// multiple operations target the same row.
    #[must_use]
    #[inline]
    pub fn effective_cols_for_current_row(&self) -> u16 {
        self.storage.effective_cols_for_row(self.storage.cursor.row)
    }

    /// Get effective column count for an arbitrary row.
    ///
    /// Returns `cols` for normal lines, `cols/2` for double-width (DECDWL) lines.
    /// Used when the caller needs the effective width of a row other than the
    /// current cursor row (e.g., combining character wrap-back to the previous
    /// row).
    #[must_use]
    #[inline]
    pub fn effective_cols_for_row(&self, row: u16) -> u16 {
        self.storage.effective_cols_for_row(row)
    }

    /// Get total lines in buffer (visible + scrollback).
    #[must_use]
    #[inline]
    pub fn total_lines(&self) -> usize {
        self.storage.total_lines()
    }

    /// Get the display offset (scroll position).
    #[must_use]
    #[inline]
    pub fn display_offset(&self) -> usize {
        self.storage.display_offset()
    }

    /// Take the accumulated content scroll delta, resetting it to 0.
    #[inline]
    pub fn take_content_scroll_delta(&mut self) -> i32 {
        self.storage.take_content_scroll_delta()
    }

    /// Take the pending change to the monotonic absolute-row coordinate space.
    ///
    /// Terminal metadata consumers must apply splices in order. `Invalidate`
    /// means multiple non-composable changes accumulated before this call.
    #[inline]
    pub fn take_absolute_row_update(&mut self) -> Option<crate::AbsoluteRowUpdate> {
        self.storage.presentation.take_absolute_row_update()
    }

    /// Take the logical-row insertion retained for text-selection remapping.
    ///
    /// This is independent of [`Self::take_absolute_row_update`], because the
    /// parser may drain metadata updates before terminal post-processing.
    #[inline]
    pub fn take_selection_row_update(&mut self) -> Option<crate::AbsoluteRowUpdate> {
        self.storage.presentation.take_selection_row_update()
    }

    /// Force selection invalidation by setting `content_scroll_delta` to `i32::MAX`.
    ///
    /// Used when the entire grid content changes non-incrementally (e.g.,
    /// alternate screen buffer switch) and any active selection is stale.
    #[inline]
    pub fn force_selection_invalidation(&mut self) {
        self.storage.content_scroll_delta = i32::MAX;
        self.storage.coordinates_invalidated = true;
        // A wholesale coordinate change is not a row-band translation: poison the
        // band record so no host replays the batch's bands as if they explained it.
        self.storage.presentation.poison_row_band_moves();
        // Also record it on the selection damage lattice. `All`, since the whole
        // coordinate space is gone for these callers (ED 3, `clear_scrollback`,
        // RIS, a Kitty unscroll renumbering history), so no band describes it.
        self.storage.selection_damage = self
            .storage
            .selection_damage
            .union(crate::SelectionDamage::All);
    }

    /// Record that host-cached grid coordinates are stale, without claiming the
    /// content under any selection was destroyed.
    ///
    /// For the alt-screen switch: a buffer swap replaces every visible
    /// coordinate, and hosts caching them (cursor effects, the viewport row
    /// cache) translate rather than rebuild while
    /// `ContentScrollState::invalidation_epoch` is unchanged. But the outgoing
    /// screen's selection is parked across the switch, not destroyed, so
    /// recording `SelectionDamage::All` here would clear what the park keeps.
    #[inline]
    pub fn invalidate_host_coordinates(&mut self) {
        self.storage.coordinates_invalidated = true;
        // A buffer swap replaces every coordinate; the bands recorded so far (on
        // the OUTGOING grid) explain none of it.
        self.storage.presentation.poison_row_band_moves();
    }

    /// Record that retained history was discarded wholesale, without claiming
    /// a coordinate move.
    ///
    /// Selection damage is `All`: history that no longer exists cannot be
    /// named by a band. But ED 3 / `clear_scrollback` moves no live coordinate:
    /// `erase_scrollback` copies the visible rows to `new_rows[0..visible_rows]`
    /// and sets `ring_head = 0`, so with `display_offset` at 0 (guaranteed for
    /// the whole batch by the processing prologue) `GridStorage::row_index`
    /// names the same row before and after, and `absolute_row_counter` and
    /// `visible_rows` are untouched. Only `oldest_absolute_row()` moves, because
    /// history was evicted, which is not renumbering (that has its own
    /// `history_renumber_epoch`).
    ///
    /// So `invalidation_epoch` is not bumped (it would reset the GUI's cursor
    /// effects on every `clear`), and the `content_scroll_delta == i32::MAX`
    /// sentinel is not set either: its other job, clearing the selection, is
    /// done by `SelectionDamage::All`.
    ///
    /// Pinned by `discarding_scrollback_evicts_history_and_moves_no_live_coordinate`.
    #[inline]
    pub fn discard_history_selection(&mut self) {
        self.storage.selection_damage = self
            .storage
            .selection_damage
            .union(crate::SelectionDamage::All);
    }

    /// Record the scroll region's rows as selection damage.
    ///
    /// Shorthand for the line/column editing family (IL, DL, DECIC, DECDC, SL,
    /// SR, DECBI, DECFI), all confined to the scroll region. Using the whole
    /// region rather than each op's exact span may over-clear a selection in
    /// the untouched part of the region but never leaves a stale highlight;
    /// rows outside the region, including all of scrollback, are spared.
    pub(crate) fn damage_selection_scroll_region(&mut self) {
        // A line/column edit MOVES rows, so host coordinate caches must drop.
        self.storage.coordinates_invalidated = true;
        // A margined IL/DL or a column op (DECIC/DECDC/SL/SR/DECBI/DECFI) moves a
        // rectangle or cells within rows, which no row-band translation
        // describes, so poison the band record. Full-width IL/DL use
        // [`Self::damage_selection_scroll_region_as_band`] instead.
        self.storage.presentation.poison_row_band_moves();
        let (top, bottom) = (
            self.storage.scroll_region.top,
            self.storage.scroll_region.bottom,
        );
        self.damage_selection_visible_rows(top, bottom);
    }

    /// [`Self::damage_selection_scroll_region`] for the full-width IL/DL, also
    /// recording the move with numbers: rows `top..=bottom` (the cursor row
    /// through the region bottom) moved by `delta` (see [`crate::RowBandMove`]),
    /// so a host that translates bands keeps its marks on their lines.
    pub(crate) fn damage_selection_scroll_region_as_band(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
    ) {
        self.storage.coordinates_invalidated = true;
        self.storage
            .presentation
            .record_row_band_move(top, bottom, delta);
        self.damage_selection_visible_rows(top, bottom);
    }

    /// Drain this batch's accumulated selection damage (see [`SelectionDamage`]).
    ///
    /// [`SelectionDamage`]: crate::SelectionDamage
    pub fn take_selection_damage(&mut self) -> crate::SelectionDamage {
        self.storage.take_selection_damage()
    }

    /// Drain this batch's host-coordinate invalidation flag.
    pub fn take_coordinates_invalidated(&mut self) -> bool {
        self.storage.take_coordinates_invalidated()
    }

    /// Drain this batch's row-band move record (see [`crate::RowBandMoves`]),
    /// the numbers behind `take_coordinates_invalidated`. Read it in the same
    /// post-processing step as the flag: a non-empty, exact record means rows
    /// moved only as the listed bands say.
    pub fn take_row_band_moves(&mut self) -> crate::RowBandMoves {
        self.storage.presentation.take_row_band_moves()
    }

    /// Drain the most recent resize's revealed-history row shift (see
    /// `last_resize_row_shift`). Read from the active grid: `Terminal::resize`
    /// resizes both the active and the saved grid, so each records its own.
    pub fn take_last_resize_row_shift(&mut self) -> u16 {
        self.storage.take_last_resize_row_shift()
    }

    /// Drain what the most recent resize did to this grid's rows: every arm
    /// counted, trim/demote/push/reveal/append plus whether a column rewrap ran
    /// (see [`crate::ResizeShape`]). Independent of
    /// [`take_last_resize_row_shift`](Self::take_last_resize_row_shift), which
    /// stays the selection's one input: draining either leaves the other intact.
    /// Every resize overwrites the whole record, so a drain after each resize
    /// sees that resize alone.
    pub fn take_last_resize_shape(&mut self) -> crate::ResizeShape {
        self.storage.presentation.take_last_resize_shape()
    }

    /// Read the most recent resize's [`crate::ResizeShape`] without draining it.
    #[must_use]
    pub fn last_resize_shape(&self) -> crate::ResizeShape {
        self.storage.presentation.last_resize_shape
    }

    /// Record that visible rows `top..=bottom` had their content moved or
    /// rewritten, so a selection overlapping them (and only such a selection)
    /// is cleared.
    ///
    /// Converts through [`Grid::visible_to_absolute`], which does not read
    /// `display_offset`, so it is sound mid-batch while the processing
    /// prologue forces `display_offset` to 0 (`top_visible_absolute_row`
    /// subtracts it and would be wrong here).
    ///
    /// `moves_coordinates` says whether the op also moved rows (a scroll or
    /// line edit), as opposed to rewriting cells in place (an erase or
    /// rectangle op); it feeds `ContentScrollState::invalidation_epoch` (see
    /// `coordinates_invalidated`).
    pub fn damage_selection_visible_rows_ext(
        &mut self,
        top: u16,
        bottom: u16,
        moves_coordinates: bool,
    ) {
        if moves_coordinates {
            self.storage.coordinates_invalidated = true;
        }
        self.damage_selection_visible_rows(top, bottom);
    }

    pub fn damage_selection_visible_rows(&mut self, top: u16, bottom: u16) {
        let (top, bottom) = if top <= bottom {
            (top, bottom)
        } else {
            (bottom, top)
        };
        // Clamp to the visible screen before converting: a crafted DECSTBM can
        // leave a region `bottom` past a later shrink, and `visible_to_absolute`
        // would then name a row above `absolute_row_counter`. A band off screen
        // describes nothing.
        let last = self.storage.visible_rows.saturating_sub(1);
        let (top, bottom) = (top.min(last), bottom.min(last));
        let lo_abs = self.visible_to_absolute(top);
        let hi_abs = self.visible_to_absolute(bottom);
        debug_assert!(lo_abs <= hi_abs);
        self.damage_selection_absolute_rows(lo_abs, hi_abs);
    }

    /// Record that absolute rows `lo_abs..=hi_abs` had their content moved or
    /// rewritten.
    ///
    /// The primitive under [`Grid::damage_selection_visible_rows`], for callers
    /// already holding absolute rows: ordinary output, whose start row can have
    /// scrolled away by the time the run ends (see
    /// [`Grid::damage_selection_output`]).
    pub(crate) fn damage_selection_absolute_rows(&mut self, lo_abs: u64, hi_abs: u64) {
        let (lo_abs, hi_abs) = if lo_abs <= hi_abs {
            (lo_abs, hi_abs)
        } else {
            (hi_abs, lo_abs)
        };
        self.storage.selection_damage = self
            .storage
            .selection_damage
            .union(crate::SelectionDamage::Band { lo_abs, hi_abs });
    }

    /// The absolute row the cursor is on, where the next printed character
    /// lands. Frame-invariant, so it survives later scrolls, letting output
    /// bracket a whole run with two reads.
    #[must_use]
    #[inline]
    fn cursor_absolute_row(&self) -> u64 {
        self.visible_to_absolute(self.storage.cursor.row)
    }

    /// Snapshot where output is about to land, for
    /// [`Grid::damage_selection_output`] to close over the print.
    #[must_use]
    pub fn output_damage_origin(&self) -> crate::OutputOrigin {
        let cursor_row = self.storage.cursor.row;
        // `Row::len()` is the last non-empty cell + 1, an O(1) field read.
        let occupied = self.row(cursor_row).map_or(0, crate::Row::len);
        crate::OutputOrigin {
            abs_row: self.visible_to_absolute(cursor_row),
            overwrites: self.storage.cursor.col < occupied,
        }
    }

    /// Record ordinary printed output that ran from `origin` to the cursor's
    /// current position as selection damage, so a bare `\rProgress: 90%`
    /// overwrite under a live highlight clears it.
    ///
    /// Kept cheap and from over-clearing:
    ///
    /// 1. Recorded per print call, not per glyph: the bulk ASCII/Unicode paths
    ///    pay it once per run, and the absolute frame lets one band cover a run
    ///    that wrapped and scrolled.
    /// 2. An append past the row's existing content on a single row replaces
    ///    nothing and records nothing. This is required: a top-anchored history
    ///    splice (Codex's `insert_history`) fills the blank row it created, and
    ///    the piecewise remap keeps a selection across that
    ///    (`EligibleSelectionUsesPiecewiseRemap`).
    /// 3. While an overwriting program stays on one row (progress bar,
    ///    spinner), it is one `Option<u64>` compare and an early return, since
    ///    `SelectionDamage::union` is monotone.
    ///
    /// Rows are recorded, not columns, so an overwrite in another column of a
    /// selected row still clears it (row granularity, failing safe). A run that
    /// wraps is recorded whole (rule 2 applies to its first row only).
    pub fn damage_selection_output(&mut self, origin: crate::OutputOrigin) {
        let to_abs = self.cursor_absolute_row();
        let single_row = origin.abs_row == to_abs;
        if single_row && !origin.overwrites {
            return;
        }
        if single_row && self.storage.last_output_damage_abs == Some(to_abs) {
            return;
        }
        self.storage.last_output_damage_abs = Some(to_abs);
        self.damage_selection_absolute_rows(origin.abs_row, to_abs);
    }

    // -------------------------------------------------------------------------
    // Cursor getters
    // -------------------------------------------------------------------------

    /// Get the cursor position.
    #[must_use]
    #[inline]
    pub fn cursor(&self) -> super::Cursor {
        self.storage.cursor()
    }

    /// Get cursor row.
    #[must_use]
    #[inline]
    pub fn cursor_row(&self) -> u16 {
        self.storage.cursor().row
    }

    /// Get cursor column.
    #[must_use]
    #[inline]
    pub fn cursor_col(&self) -> u16 {
        self.storage.cursor().col
    }

    /// Soft-wrap flag for a visible row: `true` if this row is a wrap
    /// continuation of the previous row. `None` for an out-of-range row.
    ///
    /// Resolved through [`visible_row_view`](Self::visible_row_view), so a
    /// scrolled-back row materialized from lazy/tiered scrollback reports its
    /// real wrap state, like `row_text`/`cell_text`. At `display_offset == 0`
    /// this equals reading `Grid::row` directly.
    #[must_use]
    #[inline]
    pub fn row_is_wrapped(&self, visible_row: u16) -> Option<bool> {
        match self.visible_row_view(visible_row) {
            super::VisibleRowView::Empty => None,
            view => Some(view.is_wrapped()),
        }
    }

    /// Logical length of a visible row (last non-empty cell + 1, 0 if blank).
    /// `None` for an out-of-range row.
    ///
    /// Resolved through [`visible_row_view`](Self::visible_row_view), so a
    /// scrolled-back history row reports its materialized length, like
    /// `row_text`/`cell_text`. At `display_offset == 0` this equals reading
    /// `Grid::row`.
    #[must_use]
    #[inline]
    pub fn row_len(&self, visible_row: u16) -> Option<u16> {
        match self.visible_row_view(visible_row) {
            super::VisibleRowView::Empty => None,
            view => Some(view.len()),
        }
    }

    /// Check if the cursor has a deferred wrap pending.
    #[must_use]
    #[inline]
    pub fn pending_wrap(&self) -> bool {
        self.storage.pending_wrap()
    }

    /// Set the pending wrap flag directly.
    ///
    /// Used by DECRC (cursor restore) to restore the saved wrap-next state.
    /// Normal cursor operations clear pending_wrap automatically.
    #[inline]
    pub fn set_pending_wrap(&mut self, wrap: bool) {
        if wrap {
            self.storage.mark_pending_wrap();
        } else {
            self.storage.clear_pending_wrap();
        }
    }

    /// Resolve pending wrap: if a deferred wrap is active, perform the actual
    /// line advance now. Call this before writing characters.
    #[inline]
    pub fn resolve_pending_wrap(&mut self) {
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }
    }

    /// Monotonic autowrap serial: the emulator's wrap fact.
    ///
    /// Advances by exactly one each time an autowrap line advance resolves
    /// (`advance_autowrap_line`), so a change since the last read means a wrap
    /// happened; a parked [`pending_wrap`](Self::pending_wrap) is not yet a
    /// wrap, and hard newlines (`line_feed`) never advance it. Transient: see
    /// `GridStorage::wrap_serial` for why checkpoint/restore and main/alt swaps
    /// do not carry it.
    #[must_use]
    #[inline]
    pub fn wrap_serial(&self) -> u64 {
        self.storage.wrap_serial
    }

    // -------------------------------------------------------------------------
    // Subsystem accessors
    // -------------------------------------------------------------------------

    /// Get damage state.
    #[must_use]
    #[inline]
    pub fn damage(&self) -> &Damage {
        self.storage.damage()
    }

    /// Get mutable damage state.
    #[inline]
    pub fn damage_mut(&mut self) -> &mut Damage {
        self.storage.damage_mut()
    }

    /// Monotonic content-generation counter.
    ///
    /// Advances by one on every content mutation (cell/line/scrollback change)
    /// but not on a pure viewport scroll (`scroll_display`). Caches (search
    /// index, peer sessions) compare it in O(1) to detect change. Starts
    /// nonzero, so `0` is a usable "never observed" sentinel.
    #[must_use]
    #[inline]
    pub fn content_gen(&self) -> u64 {
        self.storage.content_gen
    }

    /// Get cell extras storage.
    #[must_use]
    #[inline]
    pub fn extras(&self) -> &CellExtras {
        self.storage.extras()
    }

    /// Get mutable cell extras storage.
    #[inline]
    pub fn extras_mut(&mut self) -> &mut CellExtras {
        self.storage.extras_mut()
    }

    /// Get the style table.
    #[must_use]
    #[inline]
    pub fn styles(&self) -> &StyleTable {
        self.storage.styles()
    }

    /// Get mutable access to the style table.
    #[inline]
    #[cfg(test)]
    pub fn styles_mut(&mut self) -> &mut StyleTable {
        self.storage.styles_mut()
    }

    /// Intern an extended style with color type information.
    ///
    /// This preserves the original color type (default/indexed/rgb) for
    /// later conversion back to `PackedColors` format.
    #[inline]
    #[cfg(test)]
    pub fn intern_extended_style(&mut self, ext_style: ExtendedStyle) -> StyleId {
        self.storage.styles_mut().intern_extended(ext_style)
    }

    /// Mark that the grid has at least one double-width row (DECDWL/DECDHL).
    ///
    /// This enables the slow path in `effective_cols_for_row` that does a
    /// ring-buffer lookup to check each row's line_size. When false (the
    /// common case), the lookup is skipped entirely.
    #[inline]
    pub fn mark_has_double_width(&mut self) {
        self.storage.any_double_width = true;
    }

    /// Returns true if the grid has (or recently had) any double-width rows.
    ///
    /// This is the optimization flag checked by cursor operations to decide
    /// whether the expensive per-row `line_size` lookup is needed.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn has_any_double_width(&self) -> bool {
        self.storage.any_double_width
    }

    /// Get extras for a specific cell.
    #[must_use]
    #[inline]
    pub fn cell_extra(&self, row: u16, col: u16) -> Option<&CellExtra> {
        self.storage.cell_extra(row, col)
    }

    /// Unified render/FFI lookup for a single cell's overflow data.
    ///
    /// Collapses ring-buffer and HashMap access into one pass keyed by the
    /// cell's flags, avoiding repeated probes for complex chars, combining
    /// marks, and RGB overflow on hot paths.
    #[must_use]
    #[inline]
    pub fn cell_render_data(&self, row: u16, col: u16, cell: crate::Cell) -> CellRenderData<'_> {
        self.storage.extras().render_data_for_cell(row, col, cell)
    }

    /// Get or create extras for a specific cell.
    ///
    /// Sets the HAS_EXTRAS flag on the cell so the rendering path can skip
    /// hash probes for cells without extras.
    #[inline]
    pub fn cell_extra_mut(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.storage.cell_extra_mut(row, col)
    }

    /// Make an inline-image ref a cell's only image (iTerm2 OSC 1337 / sixel,
    /// which own the cells they cover).
    ///
    /// Goes through `CellExtras::set_image` so the collection's scroll-off scan
    /// gate is armed by the same call; otherwise the image would vanish when
    /// its row scrolls off.
    #[inline]
    pub fn set_cell_image(&mut self, row: u16, col: u16, image: crate::ImageRef) {
        // Set the HAS_EXTRAS bit first (the cell-level flag the render fast
        // paths gate on), exactly as `cell_extra_mut` would.
        self.storage.cell_extra_mut(row, col);
        let coord = CellCoord::new(row, col);
        self.storage.extras_mut().set_image(coord, image);
    }

    /// Stack an inline-image ref onto a cell's images in draw order (a Kitty
    /// placement, which layers over what the cell already shows); the same
    /// scan-gate contract as [`set_cell_image`](Self::set_cell_image).
    #[inline]
    pub fn stack_cell_image(&mut self, row: u16, col: u16, image: crate::ImageRef) {
        self.storage.cell_extra_mut(row, col);
        let coord = CellCoord::new(row, col);
        self.storage.extras_mut().stack_image(coord, image);
    }

    /// A fresh extras entry for a cell just written with its HAS_EXTRAS flag
    /// already set in its PackedColors: the write path's handout, which never
    /// adopts an entry already keyed to the cell (`CellExtras::fresh`).
    ///
    /// Skips the ring-buffer lookup that `cell_extra_mut` does to set the flag.
    #[inline]
    pub fn cell_extra_mut_preflagged(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.storage.cell_extra_mut_preflagged(row, col)
    }

    /// Store a complex char codepoint in the dense ring buffer (O(1) flat-array
    /// write, raw `char`, no Arc or refcounting).
    ///
    /// Use this on the non-BMP write path instead of `cell_extra_mut_preflagged`
    /// + `set_complex_char`.
    #[inline]
    pub fn set_complex_char_ring(&mut self, row: u16, col: u16, value: char) {
        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        self.storage
            .extras_mut()
            .set_complex_char_ring(row, col, value, visible_rows, cols);
    }

    /// Look up a complex char codepoint: ring buffer first (O(1)), then HashMap.
    ///
    /// Returns the first codepoint of the complex character. For single-emoji
    /// cells (ring path), this is the full character. For multi-char strings
    /// in the HashMap (combining sequences, ZWJ families), this returns only
    /// the base character. Use `complex_char_str_at` for the full string.
    #[inline]
    pub fn complex_char_at(&self, row: u16, col: u16) -> Option<char> {
        self.storage.extras().complex_codepoint_for(row, col)
    }

    /// Look up a complex char as full string: ring buffer (char→String) or HashMap (Arc<str>).
    ///
    /// More expensive than `complex_char_at` — allocates a String for each call.
    /// Use for text extraction (row_text, content export). For rendering where
    /// only the base codepoint is needed, use `complex_char_at`.
    #[inline]
    pub fn complex_char_str_at(&self, row: u16, col: u16) -> Option<String> {
        self.storage.extras().complex_char_str_for(row, col)
    }

    /// Look up fg RGB: ring buffer first (O(1)), then HashMap.
    ///
    /// Unified read method that transparently checks the dense RGB ring
    /// buffer before falling back to the CellExtras HashMap.
    #[inline]
    pub fn fg_rgb_at(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        self.storage.extras().fg_rgb_for(row, col)
    }

    /// Look up bg RGB: ring buffer first (O(1)), then HashMap.
    #[inline]
    pub fn bg_rgb_at(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        self.storage.extras().bg_rgb_for(row, col)
    }

    /// Remove extras for a single cell and clear its HAS_EXTRAS flag.
    ///
    /// Returns `true` if an entry was present and removed.
    #[inline]
    pub(crate) fn remove_cell_extra(&mut self, row: u16, col: u16) -> bool {
        self.storage.remove_cell_extra(row, col)
    }

    /// Enforce the hyperlink entry limit to prevent memory exhaustion.
    ///
    /// Evicts hyperlink data from the oldest entries when the extras map
    /// exceeds [`crate::extra_collection::MAX_HYPERLINK_ENTRIES`].
    /// Call after setting hyperlinks on cells.
    #[inline]
    pub fn enforce_hyperlink_limit(&mut self) {
        self.storage.extras_mut().enforce_hyperlink_limit();
    }

    /// Sync HAS_EXTRAS per-cell flags from the extras map for a given row.
    ///
    /// Bidirectional: sets the flag on cells with extras entries, clears it
    /// on cells without. Called after bulk extras operations (checkpoint
    /// restore, compaction) where per-cell flag maintenance was deferred.
    #[cfg(test)]
    pub fn sync_extras_flags_for_row(&mut self, row: u16, cols: u16) {
        self.storage.sync_extras_flags_for_row(row, cols);
    }

    // -------------------------------------------------------------------------
    // Scroll region / horizontal margins
    // -------------------------------------------------------------------------

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub fn scroll_region(&self) -> crate::ScrollRegion {
        self.storage.scroll_region()
    }

    /// Set the scroll region (DECSTBM).
    #[inline]
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.storage.set_scroll_region(top, bottom);
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub fn reset_scroll_region(&mut self) {
        self.storage.reset_scroll_region();
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub fn horizontal_margins(&self) -> crate::HorizontalMargins {
        self.storage.horizontal_margins()
    }

    /// Whether DECSLRM margins are narrower than the full row.
    ///
    /// Same answer as `!self.horizontal_margins().is_full(self.cols())`, read
    /// from a flag maintained by `set_horizontal_margins` /
    /// `reset_horizontal_margins`, because the bulk write path asks once per
    /// call to choose between a batched and a per-glyph writer.
    #[must_use]
    #[inline]
    pub fn has_horizontal_margins(&self) -> bool {
        self.storage.has_horizontal_margins
    }

    /// Whether a run printed from the cursor must go glyph by glyph: DECSLRM
    /// margins narrower than the row are set and the cursor is outside their
    /// span.
    ///
    /// The run writers (`write_ascii_blast`, `write_ascii_run_styled_packed`,
    /// `write_ascii_run_with_extras`, `write_cell_run`) derive a row's
    /// right-margin clamp once, from the column the row's batch starts at, and
    /// apply it only when that column is inside the span. The per-glyph writer
    /// re-derives it at every column (`advance_cursor_wrap_slow`), as xterm's
    /// `dotext` does: a run from left of the left margin wraps at the right
    /// margin as soon as it enters the span, and a run that wraps from right of
    /// the span lands on column 0, left of it again. A run that starts inside the
    /// span keeps one clamp throughout, since its wraps return to the left
    /// margin, so only these starts are routed to the per-glyph writer.
    ///
    /// Ruling 2026-10-10 (r15 C1): route the rare case (DECSLRM is set by
    /// vttest-style streams; aterm advertises no margins capability) to the
    /// per-glyph path, as the wide-run batchers already are under margins, rather
    /// than add a fourth copy of the clamp to the run writers. A pending wrap
    /// sits at the clamp it was armed against: inside the span it resolves to
    /// the left margin, outside it to column 0, so the cursor column decides
    /// both cases.
    #[must_use]
    #[inline]
    pub fn cursor_outside_margin_span(&self) -> bool {
        if !self.storage.has_horizontal_margins {
            return false;
        }
        let margins = self.storage.horizontal_margins();
        let col = self.storage.cursor.col;
        col < margins.left || col > margins.right
    }

    /// Set horizontal margins (DECSLRM, VT420+).
    #[inline]
    pub fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        self.storage.set_horizontal_margins(left, right);
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub fn reset_horizontal_margins(&mut self) {
        self.storage.reset_horizontal_margins();
    }

    /// Get the current tab stops as a boolean slice (column-indexed).
    ///
    /// Each element is `true` if a tab stop is set at that column.
    /// Used by checkpoint serialization to persist custom tab stops.
    #[must_use]
    #[inline]
    pub fn tab_stops(&self) -> &[bool] {
        &self.storage.tab_stops
    }

    /// Whether TBC 3 has suppressed the every-8 defaults on this grid: the part
    /// of the tab state that is not the array. It must cross every seam the
    /// array crosses; see [`Self::restore_tab_stops`].
    #[must_use]
    #[inline]
    pub fn tab_defaults_suppressed(&self) -> bool {
        self.storage.tab_defaults_suppressed
    }

    /// Replace the tab stops with the given boolean slice and the suppression
    /// flag that goes with them.
    ///
    /// Used by checkpoint deserialization and by the alt-screen switch, which
    /// shares tab state between the two screens per xterm. A narrowed grid
    /// keeps stops beyond its current width so a later grow restores them.
    /// Accepts only a vector covering every active column and within
    /// [`crate::MAX_GRID_COLS`]; anything else is a no-op.
    ///
    /// `defaults_suppressed` is required: TBC 3 ("clear all tab stops") is both
    /// the cleared array and the instruction that a later widen must not
    /// re-seed the every-8 defaults. Every seam moving tab state between grids
    /// (alt-screen switches, the update checkpoint) must pass it, or the next
    /// resize would resurrect stops the application erased.
    pub fn restore_tab_stops(&mut self, stops: &[bool], defaults_suppressed: bool) {
        if stops.len() < usize::from(self.cols()) || stops.len() > usize::from(crate::MAX_GRID_COLS)
        {
            return;
        }
        self.storage.tab_stops.clear();
        self.storage.tab_stops.extend_from_slice(stops);
        self.storage.tab_defaults_suppressed = defaults_suppressed;
    }

    // -------------------------------------------------------------------------
    // Scrollback
    // -------------------------------------------------------------------------

    /// Get a reference to the scrollback storage.
    #[must_use]
    #[inline]
    pub fn scrollback(&self) -> Option<&aterm_scrollback::ScrollbackStorage> {
        self.storage.scrollback()
    }

    /// Attach a scrollback storage backend.
    #[inline]
    pub fn attach_scrollback(
        &mut self,
        scrollback: impl Into<aterm_scrollback::ScrollbackStorage>,
    ) {
        self.storage.attach_scrollback(scrollback);
        // The store's history takes the keys below the screen's: a store that
        // holds more lines than the counter covers raises it, so the two maps
        // of an absolute row still name one row (`cover_retained_rows`).
        self.cover_retained_rows();
        if !self.storage.scrollback_detached_for_reflow {
            return;
        }

        // A reset/recovery path installed a replacement while the old worker is
        // unresolved. Untouched settings follow the replacement; dirty host
        // requests stay authoritative and are enforced now (the snapshot stays
        // live for last-writer-wins updates until finish/abort consumes it).
        let replacement_line_limit = self
            .storage
            .scrollback
            .as_ref()
            .and_then(aterm_scrollback::ScrollbackStorage::line_limit)
            .map(|limit| limit.saturating_add(self.storage.max_scrollback));
        let replacement_memory_budget = self
            .storage
            .scrollback
            .as_ref()
            .map(aterm_scrollback::ScrollbackStorage::memory_budget);
        debug_assert!(self.storage.pending_scrollback_settings.is_some());
        if let Some(settings) = self.storage.pending_scrollback_settings.as_mut() {
            if !settings.line_limit_changed {
                settings.line_limit = replacement_line_limit;
            }
            if !settings.memory_budget_changed
                && let Some(budget) = replacement_memory_budget
            {
                settings.memory_budget = budget;
            }
        }
        let settings = self.storage.pending_scrollback_settings;
        if let Some(settings) = settings {
            if settings.memory_budget_changed
                && let Err(error) = self.set_scrollback_memory_budget(settings.memory_budget)
            {
                aterm_log::warn!(
                    "replacement scrollback could not fully enforce deferred memory budget: {error}"
                );
            }
            if settings.line_limit_changed {
                self.set_scrollback_line_limit(settings.line_limit);
            }
        }
        // The replacement is authoritative now; do not leave window output
        // staged indefinitely while the original worker is slow or dead.
        self.drain_lazy_buffer();
    }

    /// Get the scrollback line limit.
    #[must_use]
    #[inline]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        self.storage.scrollback_line_limit()
    }

    /// Get the effective tiered-store memory budget, including a change
    /// deferred while the store is detached for off-thread reflow.
    #[must_use]
    #[inline]
    pub fn scrollback_memory_budget(&self) -> Option<usize> {
        self.storage.scrollback_memory_budget()
    }

    /// Set the retained scrollback line limit (`None` = unlimited).
    ///
    /// The limit is one total: on a tiered grid it caps ring + lazy + store
    /// together, not the store alone. The ring stays the fixed fast tier:
    ///   * `limit >= ring cap`: store limit = `limit - ring cap`;
    ///   * `limit < ring cap`: the ring is re-capped to `limit` (evicting its
    ///     oldest lines) and the store share is zero. The ring cap does not grow
    ///     back on a later raise; a raise widens only the store share.
    ///
    /// Staged lazy lines count against the store share (drained into the store
    /// here, and re-truncated at every steady-state drain), so
    /// `scrollback_lines() <= limit` at quiescent points; mid-batch overshoot
    /// is bounded by the lazy-drain threshold.
    ///
    /// Ring-only grids re-cap the ring itself: growth stays lazy (rows allocate
    /// as lines scroll off) and a shrink evicts the oldest lines immediately,
    /// like the store's `set_line_limit`.
    pub fn set_scrollback_line_limit(&mut self, limit: Option<usize>) {
        // The worker owns the store during an off-thread reflow. Keep the
        // newest request in the detach snapshot so the getter is truthful and
        // re-attach can replay it. If another path has already attached a
        // replacement store, apply below as well; replaying the same latest
        // value when the stale worker result arrives is harmless.
        if self.storage.scrollback_detached_for_reflow {
            debug_assert!(
                self.storage.pending_scrollback_settings.is_some(),
                "detached scrollback must retain its settings snapshot"
            );
            if let Some(settings) = self.storage.pending_scrollback_settings.as_mut() {
                settings.line_limit = limit;
                settings.line_limit_changed = true;
            }
            if self.storage.scrollback.is_none() {
                return;
            }
        }

        if self.storage.scrollback.is_some() {
            // Retained-set size before the truncation (the lazy-buffer drain
            // below only MOVES staged lines into the store, so a change here
            // means lines were really evicted).
            let before = self.scrollback_lines();
            // Unified split: the store's share is what remains after the
            // ring's share (its cap). Computed BEFORE any ring re-cap so the
            // `limit < ring cap` arm sees the original cap.
            let ring_cap = self.storage.max_scrollback;
            let store_limit = limit.map(|l| l.saturating_sub(ring_cap));
            // Drain staged lazy lines first (via scrollback_mut) so the store's
            // truncation sees the full history.
            if let Some(sb) = self.scrollback_mut() {
                sb.set_line_limit(store_limit);
            }
            // `limit < ring cap`: the ring alone must not retain more than the
            // total — re-cap it.
            if let Some(l) = limit
                && l < ring_cap
            {
                self.storage.max_scrollback = l;
                let excess = self.storage.ring_buffer_scrollback().saturating_sub(l);
                if excess > 0 {
                    self.evict_oldest_ring_scrollback(excess);
                }
            }
            // A shrink that evicted lines changed the retained set: re-clamp a
            // scrolled-back viewport and invalidate content-keyed caches (search
            // index, peer polls), as the ring arm's eviction does.
            if self.scrollback_lines() != before {
                self.clamp_display_offset();
                self.storage.mark_content_full();
            }
            return;
        }
        self.storage.max_scrollback = limit.unwrap_or(super::state::UNLIMITED_RING_SCROLLBACK);
        let excess = self
            .storage
            .ring_buffer_scrollback()
            .saturating_sub(self.storage.max_scrollback);
        if excess > 0 {
            self.evict_oldest_ring_scrollback(excess);
        }
    }

    /// Set the tiered scrollback store's byte budget.
    ///
    /// While the store is detached for off-thread reflow, records the newest
    /// request and reports success; re-attach applies it before staged output
    /// is drained. Ring-only grids have no byte-budgeted store, so this is a
    /// no-op there.
    ///
    /// The ring and staged rows' OSC 8 payload is held to a share of this
    /// budget ([`history_link_budget`](Self::history_link_budget)), enforced
    /// here too, so a temporary lower budget sheds it with the store.
    pub fn set_scrollback_memory_budget(
        &mut self,
        budget: usize,
    ) -> Result<(), aterm_scrollback::ScrollbackError> {
        let budget = budget.max(1);
        if self.storage.scrollback_detached_for_reflow {
            debug_assert!(
                self.storage.pending_scrollback_settings.is_some(),
                "detached scrollback must retain its settings snapshot"
            );
            if let Some(settings) = self.storage.pending_scrollback_settings.as_mut() {
                settings.memory_budget = budget;
                settings.memory_budget_changed = true;
            }
            if self.storage.scrollback.is_none() {
                // The history link allowance follows the deferred budget now:
                // the ring and staged rows are still here to shed.
                self.enforce_history_link_budget();
                return Ok(());
            }
        }

        let before = self.scrollback_lines();
        let mut result = match self.storage.scrollback.as_mut() {
            Some(scrollback) => scrollback.set_memory_budget(budget),
            None => Ok(()),
        };
        // Callers setting a budget also settle staged history (the
        // `scrollback_mut` contract). Done after installing the budget, so a
        // raise cannot evict those rows against the old lower cap.
        if self.storage.scrollback.is_some() {
            self.drain_lazy_buffer();
            // `drain_lazy_buffer` enforces internally but cannot return its
            // error. Re-run the idempotent setter so this API still reports an
            // enforcement failure caused specifically by newly drained rows.
            if let Some(scrollback) = self.storage.scrollback.as_mut() {
                let after_drain = scrollback.set_memory_budget(budget);
                if result.is_ok() {
                    result = after_drain;
                }
            }
        }
        // The uncompressed history's link allowance is a share of this budget
        // (`history_link_budget`): a lowered budget (the pressure shed's) strips
        // the oldest ring and staged rows' links to it.
        self.enforce_history_link_budget();
        self.clamp_display_offset();
        if self.scrollback_lines() != before {
            self.storage.mark_content_full();
        }
        result
    }

    /// Monotonic count of history lines lost to truncation the user did not
    /// request: flood-backpressure staged-line drops, detached-reflow-window
    /// cap drops (this grid) and memory-pressure store evictions (the attached
    /// store). Limit shrinks and ordinary retention eviction (including rows a
    /// flood pushes past the configured limit) are not counted.
    ///
    /// The out-of-band half of the loss signal, for `metrics`/`lines`. This
    /// grid's flood drops are also marked in band: one dim
    /// `— aterm dropped N lines here …` row per cut
    /// (`scroll_convert::FloodCut`), naming the missing lines the limit would
    /// still hold; each cut adds its peak count here. The store's own
    /// memory-pressure evictions take the oldest lines and leave no marker.
    /// It also counts the lines of a store block quarantined as undecodable,
    /// whose rows stay in [`scrollback_lines`](Self::scrollback_lines) as blank
    /// rows when older history sits before them, so `lines`' count and its
    /// `truncated=` can overlap.
    #[must_use]
    pub fn truncated_lines(&self) -> u64 {
        self.storage.flood_truncated_lines
            + self.storage.scrollback.as_ref().map_or(
                0,
                aterm_scrollback::ScrollbackStorage::pressure_evicted_lines,
            )
    }

    /// Monotonic count of history rows that kept their text but lost their
    /// inline image because the attached tiered store compressed the line (the
    /// row is left blank under the picture; no sentinel is injected).
    ///
    /// The ring tier never compresses, so pictures there are intact; a grid
    /// with no store never counts anything. See
    /// [`Scrollback::image_rows_dropped_by_compression`](aterm_scrollback::Scrollback::image_rows_dropped_by_compression).
    #[must_use]
    pub fn image_rows_dropped_by_compression(&self) -> u64 {
        self.storage.scrollback.as_ref().map_or(
            0,
            aterm_scrollback::ScrollbackStorage::image_rows_dropped_by_compression,
        )
    }

    /// Set the ring byte watermark budget: ring-only grids with an unlimited
    /// line limit have no other memory signal. With a budget set,
    /// [`ring_watermark_level`](Self::ring_watermark_level) reports Yellow at
    /// 80% and Red at 95%. `None` (default) disables it (Green). Advisory only:
    /// nothing is evicted.
    pub fn set_ring_byte_watermark(&mut self, budget: Option<usize>) {
        self.storage.ring_byte_watermark = budget;
    }

    /// Ring-byte watermark level against the configured budget
    /// ([`set_ring_byte_watermark`](Self::set_ring_byte_watermark)), computed
    /// from [`memory_used`](Self::memory_used) at query time, which includes
    /// staged lazy-buffer lines so the signal rises during a flood. A pure
    /// threshold compare (no hysteresis): Green below 80%, Yellow from 80%, Red
    /// from 95%. Green when no budget is configured.
    #[must_use]
    pub fn ring_watermark_level(&self) -> aterm_scrollback::WatermarkLevel {
        use aterm_scrollback::WatermarkLevel;
        let Some(budget) = self.storage.ring_byte_watermark else {
            return WatermarkLevel::Green;
        };
        let used = self.memory_used();
        // Same 80/95 thresholds as the tiered store's defaults; u128 products
        // cannot overflow for real byte counts.
        if used as u128 * 100 >= budget as u128 * 95 {
            WatermarkLevel::Red
        } else if used as u128 * 100 >= budget as u128 * 80 {
            WatermarkLevel::Yellow
        } else {
            WatermarkLevel::Green
        }
    }

    // -------------------------------------------------------------------------
    // Damage control
    // -------------------------------------------------------------------------

    /// Number of lines in the ring buffer scrollback (not tiered).
    #[must_use]
    #[inline]
    pub fn ring_buffer_scrollback(&self) -> usize {
        self.storage.ring_buffer_scrollback()
    }

    /// Clear damage after rendering.
    pub fn clear_damage(&mut self) {
        let visible_rows = self.storage.visible_rows();
        self.storage.clear_damage(visible_rows);
    }

    /// Per-row content revisions for the visible rows, current as of this call
    /// (the pending damage session is folded in first).
    ///
    /// `rev[r]` changes whenever visible row `r`'s content changed since an
    /// earlier read; `0` means no fold has vouched for the row, which a
    /// consumer must treat as unknown and answer by comparing content. See
    /// [`GridPresentationState::row_rev`](crate::state::GridPresentationState::row_rev)
    /// for the contract and the row-identity conditions to pin before
    /// comparing two reads.
    ///
    /// `&mut self` because the fold latches; the grid's content is untouched.
    pub fn row_revisions(&mut self) -> &[u64] {
        let visible_rows = self.storage.visible_rows();
        self.storage.presentation.fold_row_revisions(visible_rows);
        &self.storage.presentation.row_rev
    }

    /// Mark the cursor cell as damaged.
    pub fn mark_cursor_damage(&mut self) {
        let cursor = self.storage.cursor();
        self.storage.damage.mark_cell(cursor.row, cursor.col);
    }

    /// Mark a cell's changed content, including grapheme extras, as damaged.
    /// Unlike cursor-only damage, this also invalidates content-keyed readers.
    #[inline]
    pub fn mark_content_cell(&mut self, row: u16, col: u16) {
        self.storage.mark_content_cell(row, col);
    }

    /// Damage the whole screen and bump `content_gen`, invalidating every
    /// content-keyed cache (search index, cross-session polls).
    ///
    /// For callers that evict retained scrollback outside a Grid mutation (e.g.
    /// `Terminal::set_memory_budget` driving the store's budget enforcer), like
    /// `set_scrollback_line_limit` does internally.
    #[cfg(test)]
    pub fn mark_content_full(&mut self) {
        self.storage.mark_content_full();
    }

    /// Check if the grid needs a full redraw (tests).
    #[cfg(test)]
    #[must_use]
    pub fn needs_full_redraw(&self) -> bool {
        self.storage.damage.is_full()
    }

    // -------------------------------------------------------------------------
    // Test-only convenience helpers
    // -------------------------------------------------------------------------

    /// Detach and return the scrollback storage, if any.
    #[cfg(test)]
    pub(crate) fn detach_scrollback(&mut self) -> Option<aterm_scrollback::ScrollbackStorage> {
        self.storage.scrollback.take()
    }
}
