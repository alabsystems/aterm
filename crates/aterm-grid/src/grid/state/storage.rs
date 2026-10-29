// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Storage- and history-oriented `Grid` state.

use std::ops::{Deref, DerefMut};

use aterm_scrollback::ScrollbackStorage;

use super::super::LineSize;
use super::super::ScrolledRowExtras;
use super::super::ring_extras::RingExtras;
use super::super::scroll_convert::LazyBuffer;
use super::super::{CellCoord, CellExtra};
use super::super::{HorizontalMargins, PageStore, Row, ScrollRegion};
use super::GridCursorState;

/// Ring capacity sentinel for unlimited retention on a ring-only grid.
pub(crate) const UNLIMITED_RING_SCROLLBACK: usize = usize::MAX - crate::MAX_GRID_ROWS as usize;

/// Effective tiered-scrollback settings captured while detached for off-thread reflow.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingScrollbackSettings {
    pub(crate) line_limit: Option<usize>,
    pub(crate) memory_budget: usize,
    pub(crate) line_limit_changed: bool,
    pub(crate) memory_budget_changed: bool,
}

/// Reader viewport target requested while history is detached for off-thread reflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetachedReaderAim {
    /// `rows` above the live bottom as of `at_counter`.
    Above { rows: usize, at_counter: u64 },
    /// `rows` below the top of the whole history.
    FromTop { rows: usize },
    /// `rows` above the live bottom inside attached rows as of `at_counter`.
    At { rows: usize, at_counter: u64 },
}

/// History rows a detach window owes its re-attach deficit fill (`Grid::settle_fill_debt`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FillDebt {
    /// Blank rows standing in for owed history rows.
    pub(crate) rows: usize,
    /// Trimmed rows (`Edge::Trimmed`) that stood in for owed history rows.
    pub(crate) trimmed: u16,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct GridStorage {
    /// Row storage (ring buffer) — before `pages` for drop order.
    pub rows: Vec<Row>,
    /// Page-backed storage for row cell data.
    pub pages: PageStore,
    /// Number of visible rows.
    pub visible_rows: u16,
    /// Number of columns.
    pub cols: u16,
    /// Maximum scrollback lines in ring buffer.
    pub max_scrollback: usize,
    /// Total lines in ring buffer (visible + scrollback).
    pub total_lines: usize,
    /// Rows scrolled back into history (0 = live).
    pub display_offset: usize,
    /// Ring buffer head index (oldest row).
    pub ring_head: usize,
    /// Tiered scrollback for long-term history.
    pub scrollback: Option<ScrollbackStorage>,
    /// Deferred lines staged between the tiered store and the ring.
    pub(crate) lazy_buffer: LazyBuffer,
    /// Whether the tiered store is detached for off-thread reflow ([`crate::grid::scrollback_offload`]).
    pub(crate) scrollback_detached_for_reflow: bool,
    /// Settings snapshot and pending mutations for a detached tiered store.
    pub(crate) pending_scrollback_settings: Option<PendingScrollbackSettings>,
    /// Deficit-fill debt accumulated during a detach window.
    pub(crate) fill_debt: FillDebt,
    /// Whether a width change during detach left `lazy_buffer` at a stale width.
    pub(crate) staged_width_stale: bool,
    /// Reader viewport aim requested past attached history during a detach window.
    pub(crate) detached_reader_aim: Option<DetachedReaderAim>,
    /// Whether an off-thread compression worker owns `lazy_buffer` draining.
    pub(crate) compress_offload_active: bool,
    /// Staged history lines dropped by flood backpressure caps.
    pub(crate) flood_truncated_lines: u64,
    /// Lines dropped by the detached-reflow-window cap in the current window.
    pub(crate) reflow_window_cap_dropped: u64,
    /// Generation counter bumped whenever off-screen scrollback is cleared.
    pub(crate) scrollback_clear_gen: u64,
    /// Epoch bumped whenever absolute rows are renumbered without an [`AbsoluteRowUpdate`](crate::AbsoluteRowUpdate).
    pub(crate) history_renumber_epoch: u64,
    /// Counter bumped when a rows-grow resize reveals history lines back onto the screen.
    pub(crate) history_reveal_gen: u64,
    /// Newest scrollback lines guaranteed present in a bounded replay step/keyframe chain.
    pub(crate) replay_carried_history: std::sync::atomic::AtomicUsize,
    /// Monotonic count of history pullbacks or deep rewraps that reached older than `replay_carried_history`.
    pub(crate) deep_history_pullback_ordinal: u64,
    /// Extracted extras (`CellExtras`) for rows in ring scrollback.
    pub(crate) ring_extras: RingExtras,
    /// Alt-screen rows-only shrink undo state.
    pub(crate) resize_undo: Option<Box<super::super::resize_undo::ResizeUndo>>,
    /// Native rows-only resize handback state.
    pub(crate) handback: super::super::resize_handback::HandBack,
    /// Active resize law for row layout.
    pub(crate) resize_law: super::super::resize_handback::ResizeLaw,
    /// Monotonic absolute row counter, for pins that survive scrollback eviction.
    pub absolute_row_counter: u64,
    /// Monotonic content generation bumped by every content-damage mutation.
    pub content_gen: u64,
    /// This grid's process-unique identity, fixed when it was built ([`Grid::instance`](crate::Grid::instance)).
    pub(crate) instance: u64,
    /// Generation bumped when a reader scroll primitive in [`crate::grid::scroll`] lands at `display_offset == 0`.
    pub(crate) reader_live_bottom_gen: u64,
    /// Autowrap serial bumped once per `advance_autowrap_line`.
    pub wrap_serial: u64,
    /// Fast-path flag set when any row uses DECDWL/DECDHL.
    pub any_double_width: bool,
    /// Fast-path flag: horizontal margins are narrower than the screen (DECLRMM).
    pub has_horizontal_margins: bool,
    /// Cursor- and region-oriented state layered under storage state.
    pub cursor_state: GridCursorState,
}

impl Deref for GridStorage {
    type Target = GridCursorState;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.cursor_state
    }
}

impl DerefMut for GridStorage {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cursor_state
    }
}

impl GridStorage {
    /// Drain `range` from `rows` and recycle each row's cell allocation into `pages`.
    pub(crate) fn recycle_rows(&mut self, range: std::ops::Range<usize>) {
        for row in self.rows.drain(range) {
            // SAFETY: Every row in `GridStorage` was allocated from `self.pages` and drained from `self.rows`.
            unsafe { row.recycle(&mut self.pages) };
        }
    }

    /// Rotate the ring so its oldest retained row is at index `0` (`ring_head == 0`).
    #[inline]
    pub(crate) fn linearize_ring(&mut self) {
        if self.ring_head != 0 {
            self.rows.rotate_left(self.ring_head);
            self.ring_head = 0;
        }
    }

    /// Record `n` lines pushed onto the newest end of scrollback.
    #[inline]
    pub(crate) fn note_history_pushed(&self, n: usize) {
        if n == 0 {
            return;
        }
        let sb = self.scrollback_lines();
        let cur = self
            .replay_carried_history
            .load(std::sync::atomic::Ordering::Relaxed);
        let next = cur.saturating_add(n).min(sb);
        self.replay_carried_history
            .store(next, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record `n` lines popped from `sb_before` scrollback lines onto the viewport, advancing `deep_history_pullback_ordinal` when the pop reaches past the carried suffix.
    #[inline]
    pub(crate) fn note_history_popped(&mut self, sb_before: usize, n: usize) {
        let carried_before = self
            .replay_carried_history
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(sb_before);
        self.note_history_popped_after(carried_before, n);
    }

    /// Reconcile `n` popped lines against a pre-captured `carried_before` suffix.
    #[inline]
    pub(crate) fn note_history_popped_after(&mut self, carried_before: usize, n: usize) {
        if n == 0 {
            return;
        }
        if n > carried_before {
            self.deep_history_pullback_ordinal = self.deep_history_pullback_ordinal.wrapping_add(1);
        }
        let rem = carried_before
            .saturating_sub(n)
            .min(self.scrollback_lines());
        self.replay_carried_history
            .store(rem, std::sync::atomic::Ordering::Relaxed);
    }

    /// Reset the carried history suffix when scrollback is cleared.
    #[inline]
    pub(crate) fn note_history_cleared(&self) {
        self.replay_carried_history
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Clamp the carried history suffix after a bounded step (`max_lines`) or retention shrink.
    #[inline]
    pub(crate) fn clamp_replay_carried_history(&self, max_lines: usize) {
        let cap = self.scrollback_lines().min(max_lines);
        let cur = self
            .replay_carried_history
            .load(std::sync::atomic::Ordering::Relaxed);
        if cur > cap {
            self.replay_carried_history
                .store(cap, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Seed the carried history suffix from a keyframe bounded to `max_lines`.
    #[inline]
    pub(crate) fn seed_replay_carried_history(&self, max_lines: usize) {
        let cap = self.scrollback_lines().min(max_lines);
        self.replay_carried_history
            .store(cap, std::sync::atomic::Ordering::Relaxed);
    }

    /// Reconcile `replay_carried_history` right after width-reflowed scrollback is restored.
    #[inline]
    pub(crate) fn reconcile_replay_carried_after_width_restore(
        &mut self,
        sb_before: usize,
        carried_before: usize,
    ) {
        if self.scrollback_detached_for_reflow {
            return;
        }
        if carried_before == sb_before {
            self.replay_carried_history.store(
                self.scrollback_lines(),
                std::sync::atomic::Ordering::Relaxed,
            );
        } else {
            self.deep_history_pullback_ordinal = self.deep_history_pullback_ordinal.wrapping_add(1);
            self.replay_carried_history
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Reconcile `replay_carried_history` at the end of a non-width-reflow resize pass.
    #[inline]
    pub(crate) fn reconcile_replay_carried_after_resize(
        &mut self,
        sb_before: usize,
        carried_before: usize,
    ) {
        if self.scrollback_detached_for_reflow {
            return;
        }
        let sb_after = self.scrollback_lines();
        if sb_after >= sb_before {
            let next = carried_before
                .saturating_add(sb_after - sb_before)
                .min(sb_after);
            self.replay_carried_history
                .store(next, std::sync::atomic::Ordering::Relaxed);
        } else {
            self.note_history_popped_after(carried_before, sb_before - sb_after);
        }
    }

    /// Mark a single cell as content-damaged and bump `content_gen`.
    #[inline]
    pub(crate) fn mark_content_cell(&mut self, row: u16, col: u16) {
        self.damage.mark_cell(row, col);
        self.content_gen += 1;
    }

    /// Mark both cells of a wide glyph as content-damaged and bump `content_gen`.
    #[inline]
    pub(crate) fn mark_content_wide_cell(&mut self, row: u16, col: u16) {
        self.damage.mark_wide_cell(row, col);
        self.content_gen += 1;
    }

    /// Mark a single row as content-damaged and bump `content_gen`.
    #[inline]
    pub(crate) fn mark_content_row(&mut self, row: u16) {
        self.damage.mark_row(row);
        self.content_gen += 1;
    }

    /// Mark row range `[start, end)` as content-damaged and bump `content_gen`.
    #[inline]
    pub(crate) fn mark_content_rows(&mut self, start: u16, end: u16) {
        self.damage.mark_rows(start, end);
        self.content_gen += 1;
    }

    /// Mark the whole screen as content-damaged and bump `content_gen`.
    #[inline]
    pub(crate) fn mark_content_full(&mut self) {
        self.damage.mark_full();
        self.content_gen += 1;
    }

    /// Mark scroll damage for `n` scrolled rows and bump `content_gen`.
    #[inline]
    pub(crate) fn mark_content_scroll(&mut self, visible_rows: u16, n: usize) {
        self.presentation.mark_scroll_damage(visible_rows, n);
        self.content_gen += 1;
    }

    /// Map `visible_row` to its physical ring index, or `None` if in tiered scrollback.
    #[inline]
    pub(crate) fn row_index(&self, visible_row: u16) -> Option<usize> {
        debug_assert!(
            !self.rows.is_empty(),
            "row_index: ring buffer has zero rows"
        );

        let ring_scrollback = self.total_lines.saturating_sub(self.visible_rows as usize);
        let base = ring_scrollback + usize::from(visible_row);

        if self.display_offset == 0 {
            return Some((self.ring_head + base) % self.rows.len());
        }

        if self.display_offset > base {
            return None;
        }

        let absolute_row = base - self.display_offset;
        Some((self.ring_head + absolute_row) % self.rows.len())
    }

    /// Pre-computed base ring index for visible rows when `display_offset == 0`.
    #[inline]
    pub(crate) fn row_index_base(&self) -> usize {
        debug_assert_eq!(
            self.display_offset, 0,
            "row_index_base requires display_offset == 0"
        );
        let ring_scrollback = self.total_lines.saturating_sub(self.visible_rows as usize);
        (self.ring_head + ring_scrollback) % self.rows.len()
    }

    /// Shift visible rows in `[top, bottom]` up by `n` by swapping `Row` slots.
    pub(crate) fn shift_visible_rows_up(&mut self, top: usize, bottom: usize, n: usize) {
        if n == 0 || top + n > bottom {
            return;
        }
        let rows_len = self.rows.len();
        let base = self.row_index_base();
        let mut lo_phys = (base + top) % rows_len;
        let mut hi_phys = (base + top + n) % rows_len;
        for _ in top..=(bottom - n) {
            if lo_phys != hi_phys {
                self.rows.swap(lo_phys, hi_phys);
            }
            lo_phys += 1;
            if lo_phys >= rows_len {
                lo_phys = 0;
            }
            hi_phys += 1;
            if hi_phys >= rows_len {
                hi_phys = 0;
            }
        }
    }

    /// Shift visible rows in `[top, bottom]` down by `n` by swapping `Row` slots.
    pub(crate) fn shift_visible_rows_down(&mut self, top: usize, bottom: usize, n: usize) {
        if n == 0 || top + n > bottom {
            return;
        }
        let rows_len = self.rows.len();
        let base = self.row_index_base();
        let mut hi_phys = (base + bottom) % rows_len;
        let mut lo_phys = (base + bottom - n) % rows_len;
        for _ in (top..=(bottom - n)).rev() {
            if lo_phys != hi_phys {
                self.rows.swap(lo_phys, hi_phys);
            }
            if hi_phys == 0 {
                hi_phys = rows_len - 1;
            } else {
                hi_phys -= 1;
            }
            if lo_phys == 0 {
                lo_phys = rows_len - 1;
            } else {
                lo_phys -= 1;
            }
        }
    }

    /// Push extracted extras into `ring_extras`, boxing non-empty entries.
    #[inline]
    pub(crate) fn push_ring_extras(&mut self, extracted: ScrolledRowExtras) {
        self.ring_extras.push_extracted(extracted);
    }

    #[must_use]
    #[inline]
    pub(crate) const fn visible_rows(&self) -> u16 {
        self.visible_rows
    }

    #[must_use]
    #[inline]
    pub(crate) const fn cols(&self) -> u16 {
        self.cols
    }

    #[must_use]
    #[inline]
    pub(crate) const fn total_lines(&self) -> usize {
        self.total_lines
    }

    #[must_use]
    #[inline]
    pub(crate) const fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// Return the visible row at `visible_row`, or `None` if out of bounds or in tiered scrollback.
    #[must_use]
    #[inline]
    pub(crate) fn row(&self, visible_row: u16) -> Option<&Row> {
        if visible_row >= self.visible_rows {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        self.rows.get(idx)
    }

    /// Return the ring row at `visible_row` without clamping to `visible_rows`.
    #[inline]
    pub(crate) fn row_past_viewport(&self, visible_row: u16) -> Option<&Row> {
        if self.rows.is_empty() {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        self.rows.get(idx)
    }

    /// Return the live on-screen row at `screen_row`, ignoring `display_offset`.
    #[inline]
    pub(crate) fn row_at_screen(&self, screen_row: u16) -> Option<&Row> {
        if screen_row >= self.visible_rows || self.rows.is_empty() {
            return None;
        }
        let ring_scrollback = self.total_lines.saturating_sub(self.visible_rows as usize);
        let base = ring_scrollback + usize::from(screen_row);
        self.rows.get((self.ring_head + base) % self.rows.len())
    }

    /// Get a mutable row by visible row index.
    #[inline]
    pub(crate) fn row_mut(&mut self, visible_row: u16) -> Option<&mut Row> {
        if visible_row >= self.visible_rows {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        self.rows.get_mut(idx)
    }

    /// Return a mutable visible row and its effective column count in a single ring lookup.
    #[inline]
    pub(crate) fn row_mut_with_effective_cols(
        &mut self,
        visible_row: u16,
    ) -> Option<(&mut Row, u16)> {
        if visible_row >= self.visible_rows {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        debug_assert!(
            idx < self.rows.len(),
            "A1 violated: row_index returned out-of-bounds idx {idx} (len {})",
            self.rows.len()
        );
        let cols = self.cols.max(1);
        let effective_cols = if self.any_double_width {
            let is_double = {
                // SAFETY: `idx < self.rows.len()` is asserted above and `rows` length is unchanged.
                let row = unsafe { self.rows.get_unchecked(idx) };
                matches!(
                    row.line_size(),
                    LineSize::DoubleWidth
                        | LineSize::DoubleHeightTop
                        | LineSize::DoubleHeightBottom
                )
            };
            if is_double {
                let half = cols / 2;
                if half == 0 { 1 } else { half }
            } else {
                cols
            }
        } else {
            cols
        };
        debug_assert!(
            idx < self.rows.len(),
            "A1 lexical lemma broken before &mut use"
        );
        // SAFETY: `idx < self.rows.len()` is asserted above and `rows` length is unchanged.
        Some((unsafe { self.rows.get_unchecked_mut(idx) }, effective_cols))
    }

    #[inline]
    pub(crate) fn cell_extra_mut(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.set_cell_has_extras_flag(row, col, true);
        self.extras_mut().get_or_create(CellCoord::new(row, col))
    }

    /// Insert and return a fresh [`CellExtras`] entry for a cell whose `HAS_EXTRAS` flag is already set.
    #[inline]
    pub(crate) fn cell_extra_mut_preflagged(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.extras_mut().fresh(CellCoord::new(row, col))
    }

    #[inline]
    pub(crate) fn remove_cell_extra(&mut self, row: u16, col: u16) -> bool {
        let removed = self.extras_mut().remove(CellCoord::new(row, col));
        if removed {
            self.set_cell_has_extras_flag(row, col, false);
        }
        removed
    }

    /// Synchronize `HAS_EXTRAS` cell flags across all visible rows from their extras maps.
    pub(crate) fn sync_all_extras_flags(&mut self) {
        for row in &mut self.rows {
            for cell in row.iter_mut() {
                cell.set_has_extras(false);
            }
        }
        let row_count = self.rows.len();
        if row_count == 0 {
            return;
        }
        let ring_head = self.ring_head;
        let total_lines = self.total_lines;
        let visible_rows = self.visible_rows as usize;
        let display_offset = self.display_offset;
        let ring_scrollback = total_lines.saturating_sub(visible_rows);
        let extras = &self.cursor_state.presentation.extras;
        for (coord, _) in extras.iter() {
            let base = ring_scrollback + usize::from(coord.row);
            if display_offset > base {
                continue;
            }
            let absolute_row = base - display_offset;
            let idx = (ring_head + absolute_row) % row_count;
            if let Some(cell) = self.rows.get_mut(idx).and_then(|r| r.get_mut(coord.col)) {
                cell.set_has_extras(true);
            }
        }
    }

    #[inline]
    fn set_cell_has_extras_flag(&mut self, row: u16, col: u16, has_extras: bool) {
        if let Some(idx) = self.row_index(row)
            && let Some(cell) = self.rows.get_mut(idx).and_then(|r| r.get_mut(col))
        {
            cell.set_has_extras(has_extras);
        }
    }

    #[inline]
    pub(crate) fn row_is_double_width(&self, row: u16) -> bool {
        self.row(row)
            .map(|r| {
                matches!(
                    r.line_size(),
                    LineSize::DoubleWidth
                        | LineSize::DoubleHeightTop
                        | LineSize::DoubleHeightBottom
                )
            })
            .unwrap_or(false)
    }

    /// Effective column count for a given row, halved for double-width rows.
    #[inline]
    pub(crate) fn effective_cols_for_row(&self, row: u16) -> u16 {
        let cols = self.cols.max(1);
        if !self.any_double_width {
            return cols;
        }
        if self.row_is_double_width(row) {
            let half = cols / 2;
            if half == 0 { 1 } else { half }
        } else {
            cols
        }
    }

    #[inline]
    pub(crate) fn max_col_for_row(&self, row: u16) -> u16 {
        self.effective_cols_for_row(row).saturating_sub(1)
    }

    #[inline]
    pub(crate) fn clamp_col_for_row(&self, row: u16, col: u16) -> u16 {
        col.min(self.max_col_for_row(row))
    }

    /// Apply new viewport dimensions and clamp cursor-owned state to fit; the resize then places
    /// the cursor's next write and its pending wrap (`Grid::place_next_write`).
    pub(crate) fn resize_viewport_state(&mut self, new_rows: u16, new_cols: u16) {
        if new_cols != self.cols {
            // Only grow tab_stops, never shrink, preserving custom stops beyond
            // the current width across a shrink+grow (as xterm does). Tab ops
            // bounds-check via max_col, so a larger array is safe.
            if usize::from(new_cols) > self.tab_stops.len() {
                let old_len = self.tab_stops.len();
                self.tab_stops.resize(usize::from(new_cols), false);
                if !self.tab_defaults_suppressed {
                    for col in old_len..self.tab_stops.len() {
                        self.tab_stops[col] = crate::is_default_tab_stop(col);
                    }
                }
            }
        }

        self.visible_rows = new_rows;
        self.cols = new_cols;

        let row = self.cursor.row.min(new_rows.saturating_sub(1));
        let col = self.clamp_col_for_row(row, self.cursor.col);
        self.set_cursor_position(row, col);

        self.reset_scroll_region();
        self.reset_horizontal_margins();
    }

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub const fn scroll_region(&self) -> ScrollRegion {
        self.cursor_state.scroll_region()
    }

    /// Set the scroll region (DECSTBM).
    #[inline]
    pub const fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.cursor_state
            .set_scroll_region(top, bottom, self.visible_rows);
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub const fn reset_scroll_region(&mut self) {
        self.cursor_state.reset_scroll_region(self.visible_rows);
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub const fn horizontal_margins(&self) -> HorizontalMargins {
        self.cursor_state.horizontal_margins()
    }

    /// Set horizontal margins (DECSLRM, VT420+).
    #[inline]
    pub const fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        self.cursor_state
            .set_horizontal_margins(left, right, self.cols);
        self.has_horizontal_margins = !self.cursor_state.horizontal_margins().is_full(self.cols);
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub const fn reset_horizontal_margins(&mut self) {
        self.cursor_state.reset_horizontal_margins(self.cols);
        self.has_horizontal_margins = false;
    }
}

#[cfg(test)]
mod tests {
    use crate::LineSize;
    use crate::grid::Grid;

    fn make_storage(rows: u16, cols: u16) -> Grid {
        Grid::new(rows, cols)
    }

    /// Every row accessor refuses a row past the screen.
    #[test]
    fn row_accessors_refuse_rows_past_the_screen() {
        let mut g = make_storage(5, 10);
        for row in [5, 100] {
            assert!(g.storage.row(row).is_none(), "row {row}");
            assert!(g.storage.row_mut(row).is_none(), "row {row}");
            assert!(
                g.storage.row_mut_with_effective_cols(row).is_none(),
                "row {row}"
            );
        }
    }

    /// DECLRMM margins: a valid pair is set and flagged, an empty one (and a
    /// reset) returns to the full width with the flag lowered.
    #[test]
    fn horizontal_margins_set_reject_and_reset() {
        let mut g = make_storage(24, 80);
        g.storage.set_horizontal_margins(10, 70);
        let margins = g.storage.horizontal_margins();
        assert_eq!((margins.left, margins.right), (10, 70));
        assert!(g.storage.has_horizontal_margins);
        g.storage.reset_horizontal_margins();
        assert!(g.storage.horizontal_margins().is_full(80));
        assert!(!g.storage.has_horizontal_margins);
        g.storage.set_horizontal_margins(50, 50);
        assert!(g.storage.horizontal_margins().is_full(80));
    }

    /// A row's addressable width halves on every double-width or
    /// double-height line size, columns clamp to it, and `set_row_line_size`
    /// maintains `any_double_width` across transitions.
    #[test]
    fn line_size_halves_the_addressable_columns() {
        for (size, cols) in [
            (LineSize::SingleWidth, 80),
            (LineSize::DoubleWidth, 40),
            (LineSize::DoubleHeightTop, 40),
            (LineSize::DoubleHeightBottom, 40),
        ] {
            let mut g = make_storage(5, 80);
            assert_eq!(
                g.set_row_line_size(2, size),
                size != LineSize::SingleWidth,
                "{size:?}: changed flag"
            );
            assert_eq!(
                g.has_any_double_width(),
                size != LineSize::SingleWidth,
                "{size:?}: any_double_width"
            );
            assert_eq!(g.storage.effective_cols_for_row(2), cols, "{size:?}");
            assert_eq!(g.storage.max_col_for_row(2), cols - 1, "{size:?}");
            assert_eq!(g.storage.effective_cols_for_row(0), 80, "{size:?}: row 0");
            for (col, want) in [(cols / 2, cols / 2), (cols - 1, cols - 1), (200, cols - 1)] {
                assert_eq!(
                    g.storage.clamp_col_for_row(2, col),
                    want,
                    "{size:?} col {col}"
                );
            }
            if size != LineSize::SingleWidth {
                assert!(g.set_row_line_size(2, LineSize::SingleWidth));
                assert!(!g.has_any_double_width());
                assert_eq!(g.storage.effective_cols_for_row(2), 80);
            }
        }
        let mut g = make_storage(5, 80);
        assert!(!g.set_row_line_size(99, LineSize::DoubleWidth));
        assert!(!g.has_any_double_width());
    }

    /// A shift by zero rows, or by more than the range holds, moves nothing.
    #[test]
    fn degenerate_row_shifts_move_nothing() {
        let mut g = make_storage(5, 10);
        label_rows(&mut g);
        let before = first_cells(&g);
        for (top, bottom, n) in [(0, 4, 0), (0, 2, 5)] {
            g.storage.shift_visible_rows_up(top, bottom, n);
            assert_eq!(first_cells(&g), before, "up ({top}, {bottom}, {n})");
            g.storage.shift_visible_rows_down(top, bottom, n);
            assert_eq!(first_cells(&g), before, "down ({top}, {bottom}, {n})");
        }
    }

    /// Every visible row's first cell, top to bottom: the snapshot a no-op
    /// shift must leave unchanged.
    fn first_cells(g: &Grid) -> Vec<Option<char>> {
        (0..g.storage.visible_rows)
            .map(|r| {
                g.storage
                    .row(r)
                    .and_then(|row| row.get(0).map(|c| c.char()))
            })
            .collect()
    }

    /// A distinct letter at column 0 of every visible row, so a shift that
    /// moved any row would show up in [`first_cells`].
    fn label_rows(g: &mut Grid) {
        for (r, ch) in (0..g.storage.visible_rows).zip('A'..) {
            g.set_cursor(r, 0);
            g.write_char(ch);
        }
    }

    /// Every ring row's slice identity — `(cell pointer, cells length, page id)`,
    /// the tuple PROOF_CARRYING_PERFORMANCE.md A3 names — as a sorted multiset.
    fn ring_slice_identities(g: &Grid) -> Vec<(usize, usize, usize)> {
        let mut v: Vec<(usize, usize, usize)> = g
            .storage
            .rows
            .iter()
            .map(|r| (r.as_slice().as_ptr() as usize, r.cells_len(), r.page_id()))
            .collect();
        v.sort_unstable();
        v
    }

    /// A3, bounded and exhaustive: with the ring full and its head at every
    /// rotation, every `shift_visible_rows_up/down(top, bottom, n)` a 6-row
    /// screen admits keeps the multiset of row slices and allocates nothing
    /// from the page store.
    #[test]
    fn shift_visible_rows_permutes_row_slices_and_allocates_nothing_at_capacity() {
        const ROWS: u16 = 6;
        const SCROLLBACK: usize = 4;
        let capacity = usize::from(ROWS) + SCROLLBACK;
        let mut shifts = 0usize;
        for rotate in 0..capacity {
            let mut g = Grid::with_scrollback(ROWS, 8, SCROLLBACK);
            for line in 0..(capacity + rotate) {
                g.write_char(char::from(b'a' + (line % 26) as u8));
                g.carriage_return();
                g.line_feed();
            }
            assert_eq!(g.storage.display_offset, 0);
            assert_eq!(
                g.storage.total_lines,
                g.storage.rows.len(),
                "steady state: the ring is full (rotate {rotate})"
            );
            assert_eq!(g.storage.rows.len(), capacity, "rotate {rotate}");
            let identities = ring_slice_identities(&g);
            let allocations = g.storage.pages.stats().allocations;
            for top in 0..usize::from(ROWS) {
                for bottom in top..usize::from(ROWS) {
                    for n in 0..=usize::from(ROWS) {
                        for down in [false, true] {
                            if down {
                                g.storage.shift_visible_rows_down(top, bottom, n);
                            } else {
                                g.storage.shift_visible_rows_up(top, bottom, n);
                            }
                            shifts += 1;
                            assert_eq!(
                                ring_slice_identities(&g),
                                identities,
                                "rotate {rotate}: shift {} ({top}, {bottom}, {n}) changed the \
                                 multiset of row slices",
                                if down { "down" } else { "up" }
                            );
                            assert_eq!(
                                g.storage.pages.stats().allocations,
                                allocations,
                                "rotate {rotate}: a shift allocated"
                            );
                        }
                    }
                }
            }
        }
        assert!(shifts > 0);
    }

    #[test]
    fn scrollback_line_limit() {
        let mut g = make_storage(24, 80);
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
        g.storage.attach_scrollback(sb);
        g.storage.set_store_line_limit(Some(500));
        assert_eq!(
            g.storage.scrollback_line_limit(),
            Some(500 + g.storage.max_scrollback)
        );
    }
}
