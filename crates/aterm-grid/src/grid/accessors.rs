// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! O(1) [`Grid`] dimension, cursor, subsystem, and damage accessors (iterating queries live in
//! [`super::content_queries`]).

use aterm_types::index::Dimensions;

use super::Grid;
use crate::Damage;
use crate::extra_collection::CellRenderData;
use crate::{CellCoord, CellExtra, CellExtras};

/// `Dimensions` for `Grid`, so consumers need no foreign impl.
impl Dimensions for Grid {
    #[inline]
    fn total_lines(&self) -> usize {
        self.storage.total_lines()
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        usize::from(self.storage.visible_rows())
    }

    #[inline]
    fn columns(&self) -> usize {
        usize::from(self.storage.cols())
    }
}

impl Grid {
    /// Get the number of visible rows.
    #[must_use]
    #[inline]
    pub const fn rows(&self) -> u16 {
        self.storage.visible_rows()
    }

    /// Get the number of columns.
    #[must_use]
    #[inline]
    pub const fn cols(&self) -> u16 {
        self.storage.cols()
    }

    /// Get effective column count for the cursor row.
    #[must_use]
    #[inline]
    pub fn effective_cols_for_current_row(&self) -> u16 {
        self.storage.effective_cols_for_row(self.storage.cursor.row)
    }

    /// Get effective column count for an arbitrary row.
    #[must_use]
    #[inline]
    pub fn effective_cols_for_row(&self, row: u16) -> u16 {
        self.storage.effective_cols_for_row(row)
    }

    /// Get total lines in buffer (visible + scrollback).
    #[must_use]
    #[inline]
    pub const fn total_lines(&self) -> usize {
        self.storage.total_lines()
    }

    /// Get the display offset (scroll position).
    #[must_use]
    #[inline]
    pub const fn display_offset(&self) -> usize {
        self.storage.display_offset()
    }

    /// Take the accumulated content scroll delta, resetting it to 0.
    #[inline]
    pub const fn take_content_scroll_delta(&mut self) -> i32 {
        self.storage
            .cursor_state
            .presentation
            .take_content_scroll_delta()
    }

    /// Take the pending change to the monotonic absolute-row coordinate space.
    #[inline]
    pub const fn take_absolute_row_update(&mut self) -> Option<crate::AbsoluteRowUpdate> {
        self.storage
            .cursor_state
            .presentation
            .take_absolute_row_update()
    }

    /// Take the logical-row insertion retained for text-selection remapping.
    #[inline]
    pub const fn take_selection_row_update(&mut self) -> Option<crate::AbsoluteRowUpdate> {
        self.storage
            .cursor_state
            .presentation
            .take_selection_row_update()
    }

    /// Force selection invalidation (`content_scroll_delta = i32::MAX`) for a coordinate move.
    #[inline]
    pub fn force_selection_invalidation(&mut self) {
        self.storage.content_scroll_delta = i32::MAX;
        self.storage.coordinates_invalidated = true;
        self.storage.presentation.poison_row_band_moves();
        self.lose_anchors_renumbered();
        self.storage.selection_damage = self
            .storage
            .selection_damage
            .union(crate::SelectionDamage::All);
    }

    /// Record that host-cached grid coordinates are stale.
    #[inline]
    pub fn invalidate_host_coordinates(&mut self) {
        self.storage.coordinates_invalidated = true;
        self.storage.presentation.poison_row_band_moves();
        self.lose_anchors_renumbered();
    }

    /// Record that retained history was discarded wholesale (ED 3, `clear_scrollback`).
    #[inline]
    pub fn discard_history_selection(&mut self) {
        self.storage.selection_damage = self
            .storage
            .selection_damage
            .union(crate::SelectionDamage::All);
    }

    /// Record the scroll region's rows as selection damage, for the line/column editing family.
    pub(crate) fn damage_selection_scroll_region(&mut self) {
        self.storage.coordinates_invalidated = true;
        self.storage.presentation.poison_row_band_moves();
        let (top, bottom) = (
            self.storage.scroll_region.top,
            self.storage.scroll_region.bottom,
        );
        self.damage_selection_visible_rows(top, bottom);
    }

    /// [`Self::damage_selection_scroll_region`] for full-width IL/DL.
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
        self.anchors_band_move(top, bottom, i64::from(delta));
        self.damage_selection_visible_rows(top, bottom);
    }

    /// Drain this batch's accumulated selection damage (see [`SelectionDamage`]).
    ///
    /// [`SelectionDamage`]: crate::SelectionDamage
    #[inline]
    pub fn take_selection_damage(&mut self) -> crate::SelectionDamage {
        self.storage.take_selection_damage()
    }

    /// Drain this batch's host-coordinate invalidation flag.
    #[inline]
    pub fn take_coordinates_invalidated(&mut self) -> bool {
        self.storage.take_coordinates_invalidated()
    }

    /// Drain this batch's row-band move record (see [`crate::RowBandMoves`]).
    #[inline]
    pub fn take_row_band_moves(&mut self) -> crate::RowBandMoves {
        self.storage.presentation.take_row_band_moves()
    }

    /// Drain the most recent resize's revealed-history row shift (see `last_resize_row_shift`).
    #[inline]
    pub fn take_last_resize_row_shift(&mut self) -> u16 {
        self.storage.take_last_resize_row_shift()
    }

    /// Drain what the most recent resize did to this grid's rows (see [`crate::ResizeShape`]).
    #[inline]
    pub fn take_last_resize_shape(&mut self) -> crate::ResizeShape {
        self.storage.presentation.take_last_resize_shape()
    }

    /// Read the most recent resize's [`crate::ResizeShape`] without draining it.
    #[must_use]
    #[inline]
    pub const fn last_resize_shape(&self) -> crate::ResizeShape {
        self.storage.cursor_state.presentation.last_resize_shape
    }

    /// Record that visible rows `top..=bottom` had their content moved or rewritten.
    #[inline]
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
        let last = self.storage.visible_rows.saturating_sub(1);
        let (top, bottom) = (top.min(last), bottom.min(last));
        let lo_abs = self.visible_to_absolute(top);
        let hi_abs = self.visible_to_absolute(bottom);
        debug_assert!(lo_abs <= hi_abs);
        self.damage_selection_absolute_rows(lo_abs, hi_abs);
    }

    /// Record that absolute rows `lo_abs..=hi_abs` had their content moved or rewritten.
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

    /// The absolute row the cursor is on.
    #[must_use]
    #[inline]
    fn cursor_absolute_row(&self) -> u64 {
        self.visible_to_absolute(self.storage.cursor.row)
    }

    /// Snapshot where output is about to land.
    #[must_use]
    pub fn output_damage_origin(&self) -> crate::OutputOrigin {
        let cursor_row = self.storage.cursor.row;
        let occupied = self.row(cursor_row).map_or(0, crate::Row::len);
        crate::OutputOrigin {
            abs_row: self.visible_to_absolute(cursor_row),
            overwrites: self.storage.cursor.col < occupied,
        }
    }

    /// Record ordinary printed output that ran from `origin` to the cursor's current position.
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

    /// Get the cursor position.
    #[must_use]
    #[inline]
    pub const fn cursor(&self) -> super::Cursor {
        self.storage.cursor_state.cursor()
    }

    /// Get cursor row.
    #[must_use]
    #[inline]
    pub const fn cursor_row(&self) -> u16 {
        self.storage.cursor_state.cursor().row
    }

    /// Get cursor column.
    #[must_use]
    #[inline]
    pub const fn cursor_col(&self) -> u16 {
        self.storage.cursor_state.cursor().col
    }

    /// Soft-wrap flag for a visible row.
    #[must_use]
    #[inline]
    pub fn row_is_wrapped(&self, visible_row: u16) -> Option<bool> {
        match self.visible_row_view(visible_row) {
            super::VisibleRowView::Empty => None,
            view => Some(view.is_wrapped()),
        }
    }

    /// Logical length of a visible row (last non-empty cell + 1, 0 if blank).
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
    pub const fn pending_wrap(&self) -> bool {
        self.storage.cursor_state.pending_wrap()
    }

    /// Set the pending wrap flag directly.
    #[inline]
    pub const fn set_pending_wrap(&mut self, wrap: bool) {
        if wrap {
            self.storage.cursor_state.mark_pending_wrap();
        } else {
            self.storage.cursor_state.clear_pending_wrap();
        }
    }

    /// Resolve pending wrap: if a deferred wrap is active, perform the actual line advance now.
    #[inline]
    pub fn resolve_pending_wrap(&mut self) {
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }
    }

    /// Monotonic autowrap serial.
    #[must_use]
    #[inline]
    pub const fn wrap_serial(&self) -> u64 {
        self.storage.wrap_serial
    }

    /// Get damage state.
    #[must_use]
    #[inline]
    pub const fn damage(&self) -> &Damage {
        self.storage.cursor_state.presentation.damage()
    }

    /// Get mutable damage state.
    #[inline]
    pub const fn damage_mut(&mut self) -> &mut Damage {
        self.storage.cursor_state.presentation.damage_mut()
    }

    /// Monotonic content-generation counter.
    #[must_use]
    #[inline]
    pub const fn content_gen(&self) -> u64 {
        self.storage.content_gen
    }

    /// Get cell extras storage.
    #[must_use]
    #[inline]
    pub const fn extras(&self) -> &CellExtras {
        self.storage.cursor_state.presentation.extras()
    }

    /// Get mutable cell extras storage.
    #[inline]
    pub const fn extras_mut(&mut self) -> &mut CellExtras {
        self.storage.cursor_state.presentation.extras_mut()
    }

    /// Mark that the grid has at least one double-width row (decdwl/decdhl).
    #[inline]
    pub const fn mark_has_double_width(&mut self) {
        self.storage.any_double_width = true;
    }

    /// Whether the grid has (or recently had) any double-width rows.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn has_any_double_width(&self) -> bool {
        self.storage.any_double_width
    }

    /// Get extras for a specific cell.
    #[must_use]
    #[inline]
    pub fn cell_extra(&self, row: u16, col: u16) -> Option<&CellExtra> {
        self.storage.cell_extra(row, col)
    }

    /// Unified render/FFI lookup for a single cell's overflow data.
    #[must_use]
    #[inline]
    pub fn cell_render_data(&self, row: u16, col: u16, cell: crate::Cell) -> CellRenderData<'_> {
        self.storage.extras().render_data_for_cell(row, col, cell)
    }

    /// Get or create extras for a specific cell.
    #[inline]
    pub fn cell_extra_mut(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.storage.cell_extra_mut(row, col)
    }

    /// Drop the dense-RGB-ring fg/bg bytes of a `width`-cell span.
    #[inline]
    pub fn clear_rgb_ring_cells(&mut self, row: u16, col: u16, width: u16) {
        self.storage.extras.clear_rgb_ring_cell(row, col, width);
    }

    /// Make an inline-image ref a cell's only image.
    #[inline]
    pub fn set_cell_image(&mut self, row: u16, col: u16, image: crate::ImageRef) {
        self.storage.cell_extra_mut(row, col);
        let coord = CellCoord::new(row, col);
        self.storage.extras_mut().set_image(coord, image);
    }

    /// Stack an inline-image ref onto a cell's images in draw order.
    #[inline]
    pub fn stack_cell_image(&mut self, row: u16, col: u16, image: crate::ImageRef) {
        self.storage.cell_extra_mut(row, col);
        let coord = CellCoord::new(row, col);
        self.storage.extras_mut().stack_image(coord, image);
    }

    /// A fresh extras entry for a cell just written with its HAS_EXTRAS flag already set.
    #[inline]
    pub fn cell_extra_mut_preflagged(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.storage.cell_extra_mut_preflagged(row, col)
    }

    /// Store a complex char codepoint in the dense ring buffer.
    #[inline]
    pub fn set_complex_char_ring(&mut self, row: u16, col: u16, value: char) {
        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        self.storage
            .extras_mut()
            .set_complex_char_ring(row, col, value, visible_rows, cols);
    }

    /// Look up a complex char codepoint.
    #[inline]
    pub fn complex_char_at(&self, row: u16, col: u16) -> Option<char> {
        self.storage.extras().complex_codepoint_for(row, col)
    }

    /// Append a complex char's full text to `out`.
    #[inline]
    pub fn push_complex_char_at(&self, row: u16, col: u16, out: &mut String) -> bool {
        self.storage.extras().push_complex_char(row, col, out)
    }

    /// Look up fg RGB: ring buffer first (O(1)), then HashMap.
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
    #[inline]
    pub(crate) fn remove_cell_extra(&mut self, row: u16, col: u16) -> bool {
        self.storage.remove_cell_extra(row, col)
    }

    /// Enforce the hyperlink entry limit to prevent memory exhaustion.
    #[inline]
    pub fn enforce_hyperlink_limit(&mut self) {
        self.storage.extras_mut().enforce_hyperlink_limit();
    }

    /// Sync a row's HAS_EXTRAS cell flags from the extras map, setting and clearing.
    #[cfg(test)]
    pub fn sync_extras_flags_for_row(&mut self, row: u16, cols: u16) {
        self.storage.sync_extras_flags_for_row(row, cols);
    }

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub const fn scroll_region(&self) -> crate::ScrollRegion {
        self.storage.scroll_region()
    }

    /// Set the scroll region (DECSTBM).
    #[inline]
    pub const fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.storage.set_scroll_region(top, bottom);
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub const fn reset_scroll_region(&mut self) {
        self.storage.reset_scroll_region();
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub const fn horizontal_margins(&self) -> crate::HorizontalMargins {
        self.storage.horizontal_margins()
    }

    /// Whether DECSLRM margins are narrower than the full row.
    #[must_use]
    #[inline]
    pub const fn has_horizontal_margins(&self) -> bool {
        self.storage.has_horizontal_margins
    }

    /// Whether a run printed from the cursor must go glyph by glyph.
    #[must_use]
    #[inline]
    pub const fn cursor_outside_margin_span(&self) -> bool {
        if !self.storage.has_horizontal_margins {
            return false;
        }
        let margins = self.storage.horizontal_margins();
        let col = self.storage.cursor_state.cursor.col;
        col < margins.left || col > margins.right
    }

    /// Set horizontal margins (DECSLRM, VT420+).
    #[inline]
    pub const fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        self.storage.set_horizontal_margins(left, right);
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub const fn reset_horizontal_margins(&mut self) {
        self.storage.reset_horizontal_margins();
    }

    /// Get the current tab stops as a boolean slice (column-indexed).
    #[must_use]
    #[inline]
    pub fn tab_stops(&self) -> &[bool] {
        &self.storage.tab_stops
    }

    /// Whether TBC 3 has suppressed the every-8 defaults on this grid.
    #[must_use]
    #[inline]
    pub const fn tab_defaults_suppressed(&self) -> bool {
        self.storage.cursor_state.tab_defaults_suppressed
    }

    /// Replace the tab stops and the TBC 3 suppression flag that goes with them.
    pub fn restore_tab_stops(&mut self, stops: &[bool], defaults_suppressed: bool) {
        if stops.len() < usize::from(self.cols()) || stops.len() > usize::from(crate::MAX_GRID_COLS)
        {
            return;
        }
        self.storage.tab_stops.clear();
        self.storage.tab_stops.extend_from_slice(stops);
        self.storage.tab_defaults_suppressed = defaults_suppressed;
    }

    /// Get a reference to the scrollback storage.
    #[must_use]
    #[inline]
    pub const fn scrollback(&self) -> Option<&aterm_scrollback::ScrollbackStorage> {
        self.storage.scrollback()
    }

    /// Attach a scrollback storage backend.
    #[inline]
    pub fn attach_scrollback(
        &mut self,
        scrollback: impl Into<aterm_scrollback::ScrollbackStorage>,
    ) {
        self.storage.attach_scrollback(scrollback);
        self.cover_retained_rows();
        if !self.storage.scrollback_detached_for_reflow {
            return;
        }

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
        self.drain_lazy_buffer();
    }

    /// Get the scrollback line limit.
    #[must_use]
    #[inline]
    pub fn scrollback_line_limit(&self) -> Option<usize> {
        self.storage.scrollback_line_limit()
    }

    /// Get the effective tiered-store memory budget.
    #[must_use]
    #[inline]
    pub fn scrollback_memory_budget(&self) -> Option<usize> {
        self.storage.scrollback_memory_budget()
    }

    /// Set the retained scrollback line limit (`None` = unlimited).
    pub fn set_scrollback_line_limit(&mut self, limit: Option<usize>) {
        // The worker owns the store during an off-thread reflow. Keep the
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
            let before = self.scrollback_lines();
            let ring_cap = self.storage.max_scrollback;
            let store_limit = limit.map(|l| l.saturating_sub(ring_cap));
            if let Some(sb) = self.scrollback_mut() {
                sb.set_line_limit(store_limit);
            }
            if let Some(l) = limit
                && l < ring_cap
            {
                self.storage.max_scrollback = l;
                let excess = self.storage.ring_buffer_scrollback().saturating_sub(l);
                if excess > 0 {
                    self.evict_oldest_ring_scrollback(excess);
                }
            }
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
                self.enforce_history_link_budget();
                return Ok(());
            }
        }

        let before = self.scrollback_lines();
        let mut result = match self.storage.scrollback.as_mut() {
            Some(scrollback) => scrollback.set_memory_budget(budget),
            None => Ok(()),
        };
        if self.storage.scrollback.is_some() {
            self.drain_lazy_buffer();
            if let Some(scrollback) = self.storage.scrollback.as_mut() {
                let after_drain = scrollback.set_memory_budget(budget);
                if result.is_ok() {
                    result = after_drain;
                }
            }
        }
        self.enforce_history_link_budget();
        self.clamp_display_offset();
        if self.scrollback_lines() != before {
            self.storage.mark_content_full();
        }
        result
    }

    /// Monotonic count of history lines lost to truncation the user did not request.
    #[must_use]
    pub fn truncated_lines(&self) -> u64 {
        self.storage.flood_truncated_lines
            + self.storage.scrollback.as_ref().map_or(
                0,
                aterm_scrollback::ScrollbackStorage::pressure_evicted_lines,
            )
    }

    /// Monotonic count of history rows that kept their text but lost their inline image.
    #[must_use]
    pub fn image_rows_dropped_by_compression(&self) -> u64 {
        self.storage.scrollback.as_ref().map_or(
            0,
            aterm_scrollback::ScrollbackStorage::image_rows_dropped_by_compression,
        )
    }

    /// Number of lines in the ring buffer scrollback (not tiered).
    #[must_use]
    #[inline]
    pub const fn ring_buffer_scrollback(&self) -> usize {
        self.storage.ring_buffer_scrollback()
    }

    /// Clear damage after rendering.
    #[inline]
    pub fn clear_damage(&mut self) {
        let visible_rows = self.storage.visible_rows();
        self.storage.clear_damage(visible_rows);
    }

    /// Per-row content revisions for the visible rows, current as of this call.
    #[inline]
    pub fn row_revisions(&mut self) -> &[u64] {
        let visible_rows = self.storage.visible_rows();
        self.storage.presentation.fold_row_revisions(visible_rows);
        &self.storage.presentation.row_rev
    }

    /// Mark the cursor cell as damaged.
    #[inline]
    pub fn mark_cursor_damage(&mut self) {
        let cursor = self.storage.cursor();
        self.storage.damage.mark_cell(cursor.row, cursor.col);
    }

    /// Mark a cell's changed content, including grapheme extras, as damaged.
    #[inline]
    pub fn mark_content_cell(&mut self, row: u16, col: u16) {
        self.storage.mark_content_cell(row, col);
    }

    /// Damage the whole screen and bump `content_gen`, invalidating every content-keyed cache.
    #[cfg(test)]
    #[inline]
    pub fn mark_content_full(&mut self) {
        self.storage.mark_content_full();
    }

    /// Check if the grid needs a full redraw (tests).
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub const fn needs_full_redraw(&self) -> bool {
        self.storage.cursor_state.presentation.damage.is_full()
    }
}
