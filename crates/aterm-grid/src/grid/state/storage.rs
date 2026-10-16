// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Storage- and history-oriented `Grid` state.

use std::ops::{Deref, DerefMut};

use aterm_scrollback::ScrollbackStorage;

use super::super::GenerationTracker;
use super::super::LineSize;
use super::super::ScrolledRowExtras;
use super::super::ring_extras::RingExtras;
use super::super::scroll_convert::LazyBuffer;
use super::super::{CellCoord, CellExtra};
use super::super::{HorizontalMargins, PageStore, Row, ScrollRegion};
use super::GridCursorState;

/// Ring cap meaning "unlimited retention" (`set_scrollback_line_limit(None)`
/// on a ring-only grid). Not `usize::MAX`: `scroll_up` computes ring capacity
/// as `visible_rows + max_scrollback`, and `visible_rows` is clamped to
/// `MAX_GRID_ROWS`, so this headroom keeps that sum overflow-free.
pub(crate) const UNLIMITED_RING_SCROLLBACK: usize = usize::MAX - crate::MAX_GRID_ROWS as usize;

/// Effective tiered-scrollback settings captured while the store is detached
/// for off-thread reflow, so getters stay truthful during the window. The
/// `changed` bits mark a host mutation to replay when the store returns (or,
/// for a line limit, onto the ring after an aborted reflow). A deferred raise
/// cannot resurrect history the worker already evicted under the detach-time
/// settings, as with an ordinary raise after an eviction.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingScrollbackSettings {
    pub(crate) line_limit: Option<usize>,
    pub(crate) memory_budget: usize,
    pub(crate) line_limit_changed: bool,
    pub(crate) memory_budget_changed: bool,
}

/// Where the reader asked the viewport to go, past what the attached rows
/// could show, while the history was away for an off-thread reflow. See
/// `Grid::note_detached_reader_motion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetachedReaderAim {
    /// `rows` above the live bottom (more than the attached history holds) as
    /// of `absolute_row_counter == at_counter`. Lines scrolling into history
    /// afterwards push the aim up with them, so it grows by the counter's rise
    /// before it is applied.
    Above { rows: usize, at_counter: u64 },
    /// `rows` below the top of the whole history (a `Top` gesture, then any
    /// motion from there).
    FromTop { rows: usize },
    /// Exactly `rows` above the live bottom (as of `at_counter`, grown the same
    /// way): where the reader came back to inside the attached rows after aiming
    /// past them (`rows == 0` for End, a keystroke's snap, or scrolling all the
    /// way down). The restore rule does not apply to it.
    At { rows: usize, at_counter: u64 },
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
    /// Tiered scrollback for long-term history (memory-only or disk-backed).
    pub scrollback: Option<ScrollbackStorage>,
    /// Deferred lines between the store and the ring
    /// (`tiered | lazy_buffer | ring`): `scroll_up` stages O(1) row snapshots
    /// here. Readers read them in place; they are promoted to the store past
    /// the drain threshold (inline, or by the compress worker) and before
    /// anything that rewrites the store (`scrollback_mut`, limit changes,
    /// reflow, unscroll).
    pub(crate) lazy_buffer: LazyBuffer,
    /// True while the tiered store is detached for an off-thread reflow
    /// ([`crate::grid::scrollback_offload`]): evicted rows still stage to
    /// `lazy_buffer` though `scrollback` is `None`, so output during the window
    /// reaches history.
    pub(crate) scrollback_detached_for_reflow: bool,
    /// Settings snapshot for the detached tiered store, plus mutations that
    /// arrived while the worker owned it. `None` outside a detach window.
    pub(crate) pending_scrollback_settings: Option<PendingScrollbackSettings>,
    /// The viewport's trailing-blank-row count when a width-reflow resize
    /// detached the store (`resize_offloading_scrollback`): the resize cannot
    /// bottom-anchor without the history, so the re-attach runs the deficit fill
    /// (`fill_viewport_deficit_from_history`) against it. Consumed by the first
    /// width-matched re-attach; cleared by erase, replacement and abort.
    pub(crate) pending_fill_target: Option<usize>,
    /// The reader's aim while the history is away: `Some` once a gesture asks
    /// for more than is attached, so the re-attach can land the reader there;
    /// `None` leaves the re-attach's restore rule standing. Recorded only by
    /// `scroll_display`, `scroll_to_top` and `scroll_to_bottom`; cleared at every
    /// detach, re-attach and abort.
    pub(crate) detached_reader_aim: Option<DetachedReaderAim>,
    /// Set once at session setup when an off-thread compression worker is
    /// attached: the reader's ingest path then leaves `should_drain` to the
    /// worker ([`Grid::drain_lazy_bounded`]), keeping LZ4 promotion off the PTY
    /// reader. Past the flood staging cap (`Grid::flood_overflow`) the reader
    /// drops its oldest staged lines instead: rows the limit would evict go
    /// silently, other loss is counted in `flood_truncated_lines`.
    pub(crate) compress_offload_active: bool,
    /// Staged history lines dropped by flood backpressure (the compress
    /// worker's staging cap and the detached-reflow-window cap) that the
    /// configured limit would have kept. Reported out of band (`metrics`,
    /// `lines`) and in band as one dim marker row per cut
    /// (`scroll_convert::FloodCut`). Retention eviction and limit shrinks are
    /// not counted.
    pub(crate) flood_truncated_lines: u64,
    /// Lines the detached-reflow-window cap dropped in the current window
    /// (`Grid::bound_detached_lazy_buffer`), zeroed when a window opens: the cap
    /// logs at the window's first drop and its total at close
    /// (`Grid::close_reflow_window_cap_run`), not once per line.
    pub(crate) reflow_window_cap_dropped: u64,
    /// Approximate ring byte budget the host sets so ring-only "unlimited"
    /// retention still reports memory pressure ([`Grid::ring_watermark_level`]).
    /// `None` (default): no watermark, the level reads Green.
    pub(crate) ring_byte_watermark: Option<usize>,
    /// Bumped whenever off-screen scrollback is erased (ED3, full reset). Sampled
    /// at detach for reflow and re-checked at re-attach: if it advanced, the
    /// reflowed pre-erase store is dropped rather than resurrected.
    pub(crate) scrollback_clear_gen: u64,
    /// Bumped whenever absolute rows are renumbered with no
    /// [`AbsoluteRowUpdate`](crate::AbsoluteRowUpdate): an absolute row starts
    /// naming different text while `base_y()` and the protected-footer revision
    /// stand still (every width reflow, the Kitty unscroll, the deficit
    /// pullback, the resize undo's hand-back, a `Grid::cover_retained_rows`
    /// counter raise, a flood cut that opens a hole, `Grid::attach_older_history`).
    /// Retention eviction and moves across the live/history boundary keep keys
    /// and do not bump it. Absolute-row-keyed caches over history (the search
    /// index) rebuild on any advance: nothing in the `(content_gen,
    /// absolute_row_revision, geometry)` key shows this change.
    pub(crate) history_renumber_epoch: u64,
    /// Rows-grow reveals that handed history lines back to the live screen
    /// (`Grid::resize`, via `adjust_row_count`). A reveal keeps keys (so
    /// `history_renumber_epoch` stands), but a revealed line can be rewritten and
    /// pushed back into history under the same key with other text, so the
    /// self-update's history export fences on this (`HistoryFence::reveal_gen`).
    pub(crate) history_reveal_gen: u64,
    /// Extras (hyperlinks, complex chars, combining marks, RGB colors) of ring
    /// scrollback rows, extracted from `CellExtras` as rows scroll off screen;
    /// oldest first. Boxed per row so a plain row costs 8 bytes (`None`) rather
    /// than 120 of empty Vec headers; running sums of what the boxes retain feed
    /// `Grid::memory_used` and the history link budget (`grid::ring_extras`).
    pub(crate) ring_extras: RingExtras,
    /// The alt-screen resize undo: what the last rows-only shrink(s) of a grid
    /// that keeps no history took off the screen, for a grow back with nothing
    /// drawn in between to hand back exactly (`grid::resize_undo`). `None`
    /// outside such a flap. Session-only: never checkpointed.
    pub(crate) resize_undo: Option<Box<super::super::resize_undo::ResizeUndo>>,
    /// Page-eviction generations, for detecting stale pins.
    pub generations: GenerationTracker,
    /// Monotonic absolute row counter, for pins that survive scrollback eviction.
    pub absolute_row_counter: u64,
    /// Monotonic content generation, bumped by every `mark_content_*` wrapper
    /// (every content mutation that marks damage) and not by a pure viewport
    /// change, so a search index or peer session detects content change in O(1)
    /// without invalidating on a scroll. Starts at `1` (`0` = never observed);
    /// read only by [`Grid::content_gen`].
    pub content_gen: u64,
    /// Bumped each time a reader-facing primitive in [`crate::grid::scroll`]
    /// (`scroll_display`, `scroll_to_top`, `scroll_to_bottom`,
    /// `scroll_to_absolute_row`) takes `display_offset` from non-zero to zero,
    /// and by nothing else. The [`crate::grid::scrollback_offload`] re-attach
    /// compares it with a detach-time sample to tell a reader's End from machine
    /// motion that writes the same 0 (clamps, output repins, erase resets, the
    /// rows-only re-anchor).
    pub(crate) reader_live_bottom_gen: u64,
    /// Autowrap serial, bumped once per `advance_autowrap_line` (the funnel
    /// every autowrap line advance goes through). A host diffing it learns the
    /// terminal wrapped, which tells a scrolled bottom-row wrap from `Home` at
    /// the last column. Not checkpointed and not migrated across main/alt swaps
    /// (each grid keeps its own), so a restore or buffer switch can give a host
    /// one spurious read. No render or parse path reads it.
    pub wrap_serial: u64,
    /// Fast-path flag: some row uses DECDWL/DECDHL. While false,
    /// `effective_cols_for_row` skips the row lookup. Sticky until a full-screen
    /// erase, `clear_line_attributes` or DECALN clears it.
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
    /// Remove rows after their content/extras have been preserved, returning
    /// their cell allocations to the same arena for future rows.
    pub(crate) fn recycle_rows(&mut self, range: std::ops::Range<usize>) {
        for row in self.rows.drain(range) {
            // SAFETY: every row in GridStorage is allocated by its own pages;
            // drain transfers ownership and removes all access through rows.
            unsafe { row.recycle(&mut self.pages) };
        }
    }

    /// Rotate the ring so its oldest retained row is physical row 0
    /// (`ring_head == 0`): Vec order is then logical order. Every edit that
    /// addresses ring rows by Vec position (an append, a drain, a splice at the
    /// front) runs this first; O(rows) when wrapped, free otherwise.
    ///
    /// A wrapped ring is not necessarily at its cap: a rows-only grow's reveal
    /// and a ring-only limit raise lift `visible_rows + max_scrollback` without
    /// adding a row, so `scroll_up`'s next append (`grow_scrollback_ring`) must
    /// linearize first or the new row lands mid-ring.
    #[inline]
    pub(crate) fn linearize_ring(&mut self) {
        if self.ring_head != 0 {
            self.rows.rotate_left(self.ring_head);
            self.ring_head = 0;
        }
    }

    // Content-mutation damage wrappers: each marks damage and bumps
    // `content_gen`. Viewport-only marks (`apply_display_offset_damage`) do not
    // come here. Plain `+= 1`, so a debug build panics rather than wrapping a
    // generation token.

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

    /// Mark a range of rows `[start, end)` as content-damaged and bump
    /// `content_gen`.
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

    /// Mark scroll damage (targeted rows for small scrolls, full for large) and
    /// bump `content_gen`. Used by the content-bearing `scroll_up` path where
    /// rows move into scrollback (`absolute_row_counter += rows`).
    #[inline]
    pub(crate) fn mark_content_scroll(&mut self, visible_rows: u16, n: usize) {
        self.presentation.mark_scroll_damage(visible_rows, n);
        self.content_gen += 1;
    }

    /// Visible row index → ring index, or `None` when the row is in tiered
    /// scrollback (`display_offset` past the ring).
    #[inline]
    pub(crate) fn row_index(&self, visible_row: u16) -> Option<usize> {
        debug_assert!(
            !self.rows.is_empty(),
            "row_index: ring buffer has zero rows"
        );

        let ring_scrollback = self.total_lines.saturating_sub(self.visible_rows as usize);
        let base = ring_scrollback + usize::from(visible_row);

        // Fast path: the live viewport, as on nearly every write.
        if self.display_offset == 0 {
            return Some((self.ring_head + base) % self.rows.len());
        }

        if self.display_offset > base {
            return None;
        }

        let absolute_row = base - self.display_offset;
        Some((self.ring_head + absolute_row) % self.rows.len())
    }

    /// Pre-computed base index for visible row → physical row when `display_offset == 0`.
    ///
    /// REQUIRES: `display_offset == 0`, `rows` non-empty.
    /// Usage: `(base + usize::from(visible_row)) % rows.len()`.
    #[inline]
    pub(crate) fn row_index_base(&self) -> usize {
        debug_assert_eq!(
            self.display_offset, 0,
            "row_index_base requires display_offset == 0"
        );
        let ring_scrollback = self.total_lines.saturating_sub(self.visible_rows as usize);
        (self.ring_head + ring_scrollback) % self.rows.len()
    }

    /// Shift rows up by `n` within visible range `[top, bottom]` by swapping
    /// `Row` structs (~24 bytes each) instead of copying cells (~640 bytes at 80
    /// cols). Afterwards `[bottom-n+1..=bottom]` hold the old top rows, which the
    /// caller clears.
    /// REQUIRES: `display_offset == 0`, `rows` non-empty, all indices in bounds.
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

    /// Shift rows down by `n` within visible range `[top, bottom]`, swapping
    /// `Row` structs from the bottom up. Afterwards `[top..top+n-1]` hold the
    /// old bottom rows, which the caller clears.
    /// REQUIRES: `display_offset == 0`, `rows` non-empty, all indices in bounds.
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

    /// Push extracted extras into ring_extras, boxing non-empty entries.
    #[inline]
    pub(crate) fn push_ring_extras(&mut self, extracted: ScrolledRowExtras) {
        self.ring_extras.push_extracted(extracted);
    }

    #[must_use]
    #[inline]
    pub(crate) fn visible_rows(&self) -> u16 {
        self.visible_rows
    }

    #[must_use]
    #[inline]
    pub(crate) fn cols(&self) -> u16 {
        self.cols
    }

    #[must_use]
    #[inline]
    pub(crate) fn total_lines(&self) -> usize {
        self.total_lines
    }

    #[must_use]
    #[inline]
    pub(crate) fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// Get a row by visible row index.
    ///
    /// Returns `None` if the row is out of the visible area or in tiered
    /// scrollback (display_offset exceeds ring buffer bounds).
    #[must_use]
    pub(crate) fn row(&self, visible_row: u16) -> Option<&Row> {
        if visible_row >= self.visible_rows {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        self.rows.get(idx)
    }

    /// The row at `visible_row` mapped through the ring without the viewport
    /// bound: the one-past-the-viewport read the incoming-row apron needs
    /// (`visible_row == visible_rows`). `None` beyond the ring (tiered
    /// history), exactly where [`row`](Self::row) answers `None`.
    pub(crate) fn row_past_viewport(&self, visible_row: u16) -> Option<&Row> {
        if self.rows.is_empty() {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        self.rows.get(idx)
    }

    /// The live on-screen row at `screen_row`, ignoring `display_offset` (as if
    /// the viewport were at the bottom). Absolute-frame readers (block text
    /// extraction) use this so the scroll position cannot shift which row they
    /// read. Equivalent to `row_index`'s `display_offset == 0` fast path.
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

    /// Get a mutable row and its effective column count in a single ring-buffer lookup.
    ///
    /// Avoids the double `row_index` computation that happens when calling
    /// `effective_cols_for_row` and `row_mut` separately in bulk write paths.
    #[inline]
    pub(crate) fn row_mut_with_effective_cols(
        &mut self,
        visible_row: u16,
    ) -> Option<(&mut Row, u16)> {
        if visible_row >= self.visible_rows {
            return None;
        }
        let idx = self.row_index(visible_row)?;
        // A1: `row_index` returns `(ring_head + base) % self.rows.len()`, so
        // `idx < self.rows.len()`
        // (crates/aterm-spec-models/proofs/ay/a1_row_index/row_index_in_bounds.smt2;
        // a zero `len` panics inside `row_index`), and `self.rows` is not resized
        // before the two `get_unchecked` uses below. This is the hottest lookup
        // (every bulk write), so they skip the bounds check; the debug_asserts are
        // a tripwire for an edit that resizes `self.rows` in between.
        debug_assert!(
            idx < self.rows.len(),
            "A1 violated: row_index returned out-of-bounds idx {idx} (len {})",
            self.rows.len()
        );
        let cols = self.cols.max(1);
        // Fast path: no DECDWL/DECDHL row, no line_size check.
        let effective_cols = if self.any_double_width {
            let is_double = {
                // SAFETY: idx < self.rows.len() (A1, asserted above); len unchanged.
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
        // Second tripwire, just before the `&mut` use.
        debug_assert!(
            idx < self.rows.len(),
            "A1 lexical lemma broken before &mut use"
        );
        // SAFETY: idx < self.rows.len() (A1, asserted above); len unchanged in this method.
        Some((unsafe { self.rows.get_unchecked_mut(idx) }, effective_cols))
    }

    #[inline]
    pub(crate) fn cell_extra_mut(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.set_cell_has_extras_flag(row, col, true);
        self.extras_mut().get_or_create(CellCoord::new(row, col))
    }

    /// A fresh extras entry for a cell just written with its HAS_EXTRAS flag
    /// already set (via `colors.with_extras_flag()` during the write step).
    ///
    /// Skips the `set_cell_has_extras_flag` ring-buffer lookup, and never
    /// adopts an entry already keyed to the cell (`CellExtras::fresh`): the
    /// write is new content.
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

    #[cfg(test)]
    pub(crate) fn sync_extras_flags_for_row(&mut self, row: u16, cols: u16) {
        let Some(idx) = self.row_index(row) else {
            return;
        };
        let rows = &mut self.rows;
        let extras = &self.cursor_state.presentation.extras;
        let Some(r) = rows.get_mut(idx) else {
            return;
        };
        for col in 0..cols {
            let has = extras.get(CellCoord::new(row, col)).is_some();
            if let Some(cell) = r.get_mut(col) {
                cell.set_has_extras(has);
            }
        }
    }

    /// Sync HAS_EXTRAS flags for all visible rows by iterating extras entries.
    ///
    /// Phase 1 clears all flags: O(rows * cols) bitwise ops, zero hash probes.
    /// Phase 2 sets flags for actual entries: O(E) row-index lookups.
    pub(crate) fn sync_all_extras_flags(&mut self) {
        for row in &mut self.rows {
            for cell in row.iter_mut() {
                cell.set_has_extras(false);
            }
        }
        // Split borrows: `extras` lives in `cursor_state.presentation`, apart
        // from `rows`, so the row index is computed inline.
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
        // Fast path: no DECDWL/DECDHL row, no ring lookup.
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

    /// Apply new viewport dimensions and clamp cursor-owned state to fit.
    pub(crate) fn resize_viewport_state(&mut self, new_rows: u16, new_cols: u16) {
        if new_cols != self.cols {
            // Only grow tab_stops, never shrink, preserving custom stops beyond
            // the current width across a shrink+grow (as xterm does). Tab ops
            // bounds-check via max_col, so a larger array is safe.
            if usize::from(new_cols) > self.tab_stops.len() {
                let old_len = self.tab_stops.len();
                self.tab_stops.resize(usize::from(new_cols), false);
                // Unless TBC 3 erased every stop and nothing has reset them
                // since: seeding every-8 defaults into widened columns would
                // resurrect stops the application cleared.
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

        // The saved cursor is not clamped: it names a cell by absolute
        // coordinates, the resize moves it only with its text, and DECRC and the
        // 1049 exit clamp what they read (as xterm's `CursorRestore` does). A slot
        // on a row a shrink trimmed stays past the grid, so a grow back restores
        // it onto that row (`Grid::lend_saved_cursor`).

        self.reset_scroll_region();
        self.reset_horizontal_margins();
        self.clear_pending_wrap();
    }

    /// Get the current scroll region.
    #[must_use]
    #[inline]
    pub fn scroll_region(&self) -> ScrollRegion {
        self.cursor_state.scroll_region()
    }

    /// Set the scroll region (DECSTBM).
    #[inline]
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.cursor_state
            .set_scroll_region(top, bottom, self.visible_rows);
    }

    /// Reset scroll region to full screen.
    #[inline]
    pub fn reset_scroll_region(&mut self) {
        self.cursor_state.reset_scroll_region(self.visible_rows);
    }

    /// Get the current horizontal margins (DECSLRM).
    #[must_use]
    #[inline]
    pub fn horizontal_margins(&self) -> HorizontalMargins {
        self.cursor_state.horizontal_margins()
    }

    /// Set horizontal margins (DECSLRM, VT420+).
    #[inline]
    pub fn set_horizontal_margins(&mut self, left: u16, right: u16) {
        self.cursor_state
            .set_horizontal_margins(left, right, self.cols);
        // Update fast-path flag: non-full margins require margin-aware wrapping.
        self.has_horizontal_margins = !self.cursor_state.horizontal_margins().is_full(self.cols);
    }

    /// Reset horizontal margins to full width.
    #[inline]
    pub fn reset_horizontal_margins(&mut self) {
        self.cursor_state.reset_horizontal_margins(self.cols);
        self.has_horizontal_margins = false;
    }
}

#[cfg(test)]
mod tests {
    use crate::grid::Grid;
    use crate::{Cursor, LineSize};

    fn make_storage(rows: u16, cols: u16) -> Grid {
        Grid::new(rows, cols)
    }

    #[test]
    fn row_access_out_of_bounds_returns_none() {
        let g = make_storage(5, 10);
        assert!(g.storage.row(5).is_none());
        assert!(g.storage.row(100).is_none());
    }

    #[test]
    fn row_mut_out_of_bounds_returns_none() {
        let mut g = make_storage(5, 10);
        assert!(g.storage.row_mut(5).is_none());
    }

    #[test]
    fn row_mut_with_effective_cols_out_of_bounds() {
        let mut g = make_storage(5, 80);
        assert!(g.storage.row_mut_with_effective_cols(5).is_none());
    }

    #[test]
    fn cursor_take_pending_wrap_returns_true_and_clears() {
        let mut g = make_storage(24, 80);
        g.storage.mark_pending_wrap();
        assert!(g.storage.take_pending_wrap());
        assert!(!g.storage.pending_wrap());
    }

    #[test]
    fn cursor_take_pending_wrap_returns_false_when_not_set() {
        let mut g = make_storage(24, 80);
        assert!(!g.storage.take_pending_wrap());
    }

    #[test]
    fn cursor_save_and_restore() {
        let mut g = make_storage(24, 80);
        g.storage.set_cursor_position(10, 20);
        g.storage.mark_pending_wrap();
        g.storage.save_cursor();

        let saved = g.storage.saved_cursor();
        assert!(saved.valid);
        assert_eq!(saved.cursor, Cursor::new(10, 20));
        assert!(saved.pending_wrap);
    }

    #[test]
    fn scroll_region_invalid_range_resets_to_full() {
        let mut g = make_storage(24, 80);
        // top >= bottom should reset to full
        g.storage.set_scroll_region(10, 10);
        assert!(g.storage.scroll_region().is_full(24));
    }

    #[test]
    fn horizontal_margins_set_custom() {
        let mut g = make_storage(24, 80);
        g.storage.set_horizontal_margins(10, 70);
        let margins = g.storage.horizontal_margins();
        assert_eq!(margins.left, 10);
        assert_eq!(margins.right, 70);
        assert!(g.storage.has_horizontal_margins);
    }

    #[test]
    fn horizontal_margins_reset_clears_flag() {
        let mut g = make_storage(24, 80);
        g.storage.set_horizontal_margins(10, 70);
        g.storage.reset_horizontal_margins();
        let margins = g.storage.horizontal_margins();
        assert!(margins.is_full(80));
        assert!(!g.storage.has_horizontal_margins);
    }

    #[test]
    fn horizontal_margins_invalid_resets_to_full() {
        let mut g = make_storage(24, 80);
        // left >= right should reset to full
        g.storage.set_horizontal_margins(50, 50);
        assert!(g.storage.horizontal_margins().is_full(80));
    }

    #[test]
    fn clamp_col_for_row_within_bounds() {
        let g = make_storage(5, 80);
        assert_eq!(g.storage.clamp_col_for_row(0, 50), 50);
    }

    #[test]
    fn clamp_col_for_row_at_max() {
        let g = make_storage(5, 80);
        assert_eq!(g.storage.clamp_col_for_row(0, 79), 79);
    }

    #[test]
    fn clamp_col_for_row_exceeds_max() {
        let g = make_storage(5, 80);
        assert_eq!(g.storage.clamp_col_for_row(0, 200), 79);
    }

    #[test]
    fn effective_cols_double_width_row() {
        let mut g = make_storage(5, 80);
        g.storage.any_double_width = true;
        if let Some(row) = g.storage.row_mut(2) {
            row.set_line_size(LineSize::DoubleWidth);
        }
        assert_eq!(g.storage.effective_cols_for_row(2), 40);
        assert_eq!(g.storage.effective_cols_for_row(0), 80);
    }

    #[test]
    fn effective_cols_double_height_top() {
        let mut g = make_storage(5, 80);
        g.storage.any_double_width = true;
        if let Some(row) = g.storage.row_mut(1) {
            row.set_line_size(LineSize::DoubleHeightTop);
        }
        assert_eq!(g.storage.effective_cols_for_row(1), 40);
    }

    #[test]
    fn effective_cols_double_height_bottom() {
        let mut g = make_storage(5, 80);
        g.storage.any_double_width = true;
        if let Some(row) = g.storage.row_mut(1) {
            row.set_line_size(LineSize::DoubleHeightBottom);
        }
        assert_eq!(g.storage.effective_cols_for_row(1), 40);
    }

    #[test]
    fn max_col_double_width() {
        let mut g = make_storage(5, 80);
        g.storage.any_double_width = true;
        if let Some(row) = g.storage.row_mut(0) {
            row.set_line_size(LineSize::DoubleWidth);
        }
        assert_eq!(g.storage.max_col_for_row(0), 39);
    }

    #[test]
    fn shift_rows_up_n_zero_is_noop() {
        let mut g = make_storage(5, 10);
        g.write_char('A');
        let before = g.storage.row(0).map(|r| r.get(0).map(|c| c.char()));
        g.storage.shift_visible_rows_up(0, 4, 0);
        let after = g.storage.row(0).map(|r| r.get(0).map(|c| c.char()));
        assert_eq!(before, after);
    }

    #[test]
    fn shift_rows_down_n_zero_is_noop() {
        let mut g = make_storage(5, 10);
        g.write_char('B');
        let before = g.storage.row(0).map(|r| r.get(0).map(|c| c.char()));
        g.storage.shift_visible_rows_down(0, 4, 0);
        let after = g.storage.row(0).map(|r| r.get(0).map(|c| c.char()));
        assert_eq!(before, after);
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

    #[test]
    fn shift_rows_up_n_exceeds_range_is_noop() {
        let mut g = make_storage(5, 10);
        label_rows(&mut g);
        let before = first_cells(&g);
        // top + n > bottom: nothing moves.
        g.storage.shift_visible_rows_up(0, 2, 5);
        assert_eq!(first_cells(&g), before);
    }

    #[test]
    fn shift_rows_down_n_exceeds_range_is_noop() {
        let mut g = make_storage(5, 10);
        label_rows(&mut g);
        let before = first_cells(&g);
        g.storage.shift_visible_rows_down(0, 2, 5);
        assert_eq!(first_cells(&g), before);
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
    /// from the page store. The swap chain permutes `Row`s: no slice is lost,
    /// aliased or freshly allocated.
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

    /// Negative control: replacing one ring row with a fresh allocation (what a
    /// copy-based shift would do) changes both readings the A3 test compares.
    #[test]
    fn a_row_replaced_by_a_fresh_allocation_is_seen_by_both_readings() {
        let mut g = Grid::with_scrollback(6, 8, 4);
        for _ in 0..10 {
            g.write_char('x');
            g.carriage_return();
            g.line_feed();
        }
        let identities = ring_slice_identities(&g);
        let allocations = g.storage.pages.stats().allocations;
        // SAFETY: the new row's slice comes from this grid's own page store,
        // which outlives every row (`rows` drops before `pages`).
        let fresh = unsafe { crate::Row::new(8, &mut g.storage.pages) };
        g.storage.rows[0] = fresh;
        assert_ne!(ring_slice_identities(&g), identities);
        assert_ne!(g.storage.pages.stats().allocations, allocations);
    }

    #[test]
    fn push_ring_extras_empty() {
        let mut g = make_storage(5, 10);
        let extras = super::ScrolledRowExtras::default();
        assert!(extras.is_empty());
        g.storage.push_ring_extras(extras);
        assert_eq!(g.storage.ring_extras.len(), 1);
        // Empty extras are stored as `None`.
        assert!(g.storage.ring_extras.get(0).is_none());
        g.storage.ring_extras.assert_running_sums();
    }

    #[test]
    fn resize_viewport_clamps_cursor() {
        let mut g = make_storage(24, 80);
        g.storage.set_cursor_position(20, 70);
        g.storage.resize_viewport_state(10, 40);
        let cursor = g.storage.cursor();
        assert!(
            cursor.row < 10,
            "cursor row should be clamped to new row count"
        );
        assert!(
            cursor.col < 40,
            "cursor col should be clamped to new col count"
        );
    }

    #[test]
    fn resize_viewport_resets_scroll_region() {
        let mut g = make_storage(24, 80);
        g.storage.set_scroll_region(5, 20);
        g.storage.resize_viewport_state(30, 100);
        let region = g.storage.scroll_region();
        assert!(region.is_full(30));
    }

    #[test]
    fn resize_viewport_resets_horizontal_margins() {
        let mut g = make_storage(24, 80);
        g.storage.set_horizontal_margins(10, 70);
        g.storage.resize_viewport_state(24, 100);
        let margins = g.storage.horizontal_margins();
        assert!(margins.is_full(100));
        assert!(!g.storage.has_horizontal_margins);
    }

    #[test]
    fn resize_viewport_clears_pending_wrap() {
        let mut g = make_storage(24, 80);
        g.storage.mark_pending_wrap();
        g.storage.resize_viewport_state(24, 100);
        assert!(!g.storage.pending_wrap());
    }

    #[test]
    fn resize_viewport_grows_tab_stops() {
        let mut g = make_storage(24, 80);
        g.storage.resize_viewport_state(24, 120);
        assert!(g.storage.tab_stops.len() >= 120);
        // New columns get the every-8 defaults.
        assert!(g.storage.is_tab_stop(80));
        assert!(g.storage.is_tab_stop(88));
        assert!(g.storage.is_tab_stop(96));
        assert!(!g.storage.is_tab_stop(81));
    }

    #[test]
    fn resize_viewport_shrink_preserves_tab_stops() {
        let mut g = make_storage(24, 80);
        let len_before = g.storage.tab_stops.len();
        g.storage.resize_viewport_state(24, 40);
        // Tab stops never shrink (as in xterm).
        assert_eq!(g.storage.tab_stops.len(), len_before);
    }

    /// The saved cursor stays where it was saved; DECRC clamps what it reads
    /// (`Grid::restore_cursor`).
    #[test]
    fn resize_viewport_leaves_the_saved_cursor_to_its_reader() {
        let mut g = make_storage(24, 80);
        g.storage.set_cursor_position(20, 70);
        g.storage.save_cursor();
        g.storage.resize_viewport_state(10, 40);
        let saved = g.storage.saved_cursor();
        assert_eq!((saved.cursor.row, saved.cursor.col), (20, 70));
        g.restore_cursor();
        assert_eq!((g.cursor_row(), g.cursor_col()), (9, 39));
    }

    #[test]
    fn attach_scrollback_sets_some() {
        let mut g = make_storage(24, 80);
        assert!(g.storage.scrollback().is_none());
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
        g.storage.attach_scrollback(sb);
        assert!(g.storage.scrollback().is_some());
    }

    #[test]
    fn scrollback_line_limit() {
        let mut g = make_storage(24, 80);
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
        g.storage.attach_scrollback(sb);
        // The raw writer takes the store share; the getter reports the total
        // (share + ring cap), the arithmetic `Grid::set_scrollback_line_limit`
        // applies.
        g.storage.set_store_line_limit(Some(500));
        assert_eq!(
            g.storage.scrollback_line_limit(),
            Some(500 + g.storage.max_scrollback)
        );
    }

    #[test]
    fn tab_stops_clear_all() {
        let mut g = make_storage(24, 80);
        g.storage.clear_all_tab_stops();
        for col in 0..80 {
            assert!(
                !g.storage.is_tab_stop(col),
                "col {col} should not be a tab stop"
            );
        }
    }

    #[test]
    fn tab_stops_reset() {
        let mut g = make_storage(24, 80);
        g.storage.clear_all_tab_stops();
        g.storage.reset_tab_stops(80);
        assert!(g.storage.is_tab_stop(8));
        assert!(g.storage.is_tab_stop(16));
    }
}
