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

/// Effective tiered-scrollback settings captured while its store is detached
/// for off-thread reflow.
///
/// The values keep getters truthful during the detach window. The `changed`
/// bits distinguish an untouched snapshot from a host mutation that must be
/// replayed when the store returns (or, for a line limit, onto the ring after
/// an aborted reflow).
///
/// A deferred raise changes the surviving store when it re-attaches; it cannot
/// resurrect history the worker already evicted under the detach-time limit or
/// budget, matching an ordinary raise after an eviction. The worker therefore
/// remains bounded by the settings it detached with.
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
    /// way): where the reader came back to inside the attached rows after
    /// aiming past them (`rows == 0` for End, a keystroke's snap, or scrolling
    /// all the way down). The restore rule does not apply to them.
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
    /// Display offset (for O(1) scrolling).
    /// 0 = showing live content, >0 = scrolled back into history.
    pub display_offset: usize,
    /// Ring buffer head index (oldest row).
    pub ring_head: usize,
    /// Optional tiered scrollback for long-term history.
    /// Supports both memory-only and disk-backed storage via [`ScrollbackStorage`].
    pub scrollback: Option<ScrollbackStorage>,
    /// Lazy buffer: deferred lines awaiting materialization.
    ///
    /// Lines pushed here during `scroll_up` are O(1) memcpy snapshots.
    /// They are drained to tiered scrollback on first read access,
    /// when the buffer exceeds 1000 lines, or at checkpoint time.
    /// Logically sits between the ring buffer and tiered scrollback:
    /// `tiered scrollback | lazy_buffer | ring buffer`
    pub(crate) lazy_buffer: LazyBuffer,
    /// True while the tiered `scrollback` store is temporarily detached for an
    /// off-thread reflow (see [`crate::grid::scrollback_offload`]). Keeps
    /// scroll-off capture alive (evicted rows still staged to `lazy_buffer`) even
    /// though `scrollback` is `None`, so output produced during the reflow window
    /// is not silently dropped from history.
    pub(crate) scrollback_detached_for_reflow: bool,
    /// Settings snapshot for the detached tiered store, plus mutations that
    /// arrived while the worker owned it. `None` outside a detach window.
    pub(crate) pending_scrollback_settings: Option<PendingScrollbackSettings>,
    /// The viewport's trailing-blank-row count captured when a width-reflow
    /// resize detached the tiered store (see `resize_offloading_scrollback`).
    /// The synchronous resize cannot bottom-anchor while the history is with
    /// the worker, so the deficit fill (`fill_viewport_deficit_from_history`)
    /// runs at re-attach against this target. `None` outside a detach window;
    /// consumed by the first width-matched re-attach, cleared by
    /// erase/replacement/abort.
    pub(crate) pending_fill_target: Option<usize>,
    /// The reader's aim while the history is away. `None` while every gesture
    /// in a detach window fits the attached rows (the re-attach's restore rule
    /// stands); `Some` once the reader asks for more than is attached, so the
    /// re-attach can land them there. Recorded only by the reader-facing
    /// primitives (`scroll_display`, `scroll_to_top`, `scroll_to_bottom`);
    /// cleared at every detach, re-attach and abort.
    pub(crate) detached_reader_aim: Option<DetachedReaderAim>,
    /// True when an off-thread compression worker is attached (set once at
    /// session setup). The reader-thread ingest path then does not drain the
    /// lazy buffer inline on `should_drain`; the worker drains it in bounded
    /// batches via [`Grid::drain_lazy_bounded`], keeping LZ4/zstd promotion off
    /// the PTY reader. Past the flood staging cap (`Grid::flood_overflow`) the
    /// reader drops its oldest staged lines instead of promoting, so memory
    /// stays bounded: rows the limit would evict go silently, any other loss
    /// is counted and marked (`flood_truncated_lines`, the flood cut).
    pub(crate) compress_offload_active: bool,
    /// Monotonic count of staged history lines dropped by flood backpressure
    /// (the staging cap while a compress worker is behind, and the
    /// detached-reflow-window cap) that the configured limit would have kept.
    /// Surfaced out of band here (for `metrics`/`lines`, adding each cut's
    /// peak) and in band as one dim marker row per cut
    /// (`scroll_convert::FloodCut`). Ordinary retention eviction and
    /// user-requested limit shrinks are not counted.
    pub(crate) flood_truncated_lines: u64,
    /// Staged lines the detached-reflow-window cap has dropped in the CURRENT
    /// detach window (`Grid::bound_detached_lazy_buffer`), zeroed when a window
    /// opens. The cap is a fact about the window, so its log line is written at
    /// the window's first drop and its total when the window closes, not once
    /// per streamed line (`Grid::close_reflow_window_cap_run`).
    pub(crate) reflow_window_cap_dropped: u64,
    /// Ring-byte watermark budget: an approximate byte budget the host sets so
    /// ring-only "unlimited" retention still reports memory pressure
    /// ([`Grid::ring_watermark_level`]). `None` (default) = no watermark,
    /// level reads Green.
    pub(crate) ring_byte_watermark: Option<usize>,
    /// Monotonic generation bumped whenever off-screen scrollback is erased
    /// (ED3 / full reset). Captured when the store is detached for reflow and
    /// re-checked on re-attach: if it advanced, the reflowed (pre-erase) store is
    /// dropped rather than resurrecting history the user explicitly cleared.
    pub(crate) scrollback_clear_gen: u64,
    /// Monotonic generation bumped whenever rows are renumbered in the
    /// absolute-row space with no [`AbsoluteRowUpdate`](crate::AbsoluteRowUpdate)
    /// emitted, i.e. an absolute row number starts naming different text
    /// without `base_y()` arithmetic or the protected-footer revision moving:
    ///
    /// * any width reflow (`Grid::resize_with_reflow_mode`): the rewrap
    ///   re-splits the same lines across a different number of rows, live
    ///   screen included, so the bump is unconditional on a width change;
    /// * the Kitty CSI +T unscroll (`unscroll_from_scrollback`), removing the
    ///   newest scrollback lines;
    /// * the width rewrap's scrollback restore (`scrollback_reflow.rs`) and the
    ///   deficit pullback (`reflow_pullback.rs`), which move the retained-line
    ///   total under a still-standing `absolute_row_counter`. Subsumed by the
    ///   width bump above during a resize, and kept because the OFFLOADED
    ///   re-attach reaches both outside one;
    /// * the resize undo handing a PUSHED row back to the bottom of a grid that
    ///   keeps no history (`Grid::note_bottom_end_renumbered`), the exact inverse
    ///   of the shrink whose eviction took it. A rows-only grow's blank append
    ///   and shrink's trailing-blank trim move the counter with their rows under
    ///   both resize policies and renumber nothing; rows moved ACROSS the
    ///   live/history boundary (reveal, top-demote) leave the total unchanged
    ///   and are pure relabels. Neither bumps this.
    /// * a width rewrap that leaves the counter below the retained rows
    ///   (`Grid::cover_retained_rows` raises it), which renumbers the write map
    ///   onto the read map.
    ///
    /// The last three move the retained-line total under a fixed
    /// `absolute_row_counter`, so `oldest_absolute_row()` slides and carries
    /// every retained row's `oldest + i` key with it.
    ///
    /// Ordinary retention eviction drops the oldest lines and keeps every
    /// survivor's key, so it does not bump this. Consumers maintaining
    /// absolute-row-keyed caches over history (the terminal's search index)
    /// rebuild on any advance; nothing in the `(content_gen,
    /// absolute_row_revision, geometry)` key set shows this change.
    pub(crate) history_renumber_epoch: u64,
    /// Monotonic count of rows-grow reveals that handed history lines back to
    /// the live screen (`Grid::resize`, via `adjust_row_count`).
    ///
    /// A reveal is a pure relabel (keys unchanged, so
    /// `history_renumber_epoch` does not move), but a revealed line is live
    /// again: it can be rewritten and pushed back into history under the same
    /// key with other text. So history read by absolute row before a reveal
    /// cannot be trusted after one; the self-update's history export fences on
    /// this (`HistoryFence::reveal_gen`).
    pub(crate) history_reveal_gen: u64,
    /// Preserved extras for ring buffer scrollback rows.
    ///
    /// When rows scroll from the visible area into ring buffer scrollback,
    /// their extras (hyperlinks, complex chars, combining marks, RGB colors)
    /// are extracted from `CellExtras` before the extras are discarded.
    /// Indexed by ring buffer scrollback position, oldest first.
    ///
    /// `Option<Box<>>` avoids 120 bytes of empty Vec headers per plain-text
    /// row: `None` = no overflow data (common, 8 bytes). The container keeps
    /// running sums of what the boxes retain, for `Grid::memory_used` and the
    /// history link budget (`grid::ring_extras`).
    pub(crate) ring_extras: RingExtras,
    /// The alt-screen resize undo: what the last rows-only shrink(s) of a grid
    /// that keeps no history took off the screen, for a grow back with nothing
    /// drawn in between to hand back exactly (`grid::resize_undo`). `None`
    /// outside such a flap. Session-only: never checkpointed.
    pub(crate) resize_undo: Option<Box<super::super::resize_undo::ResizeUndo>>,
    /// Generation tracker for pin invalidation.
    /// Tracks page evictions to detect stale pins.
    pub generations: GenerationTracker,
    /// Absolute row counter (monotonically increasing).
    /// Used for creating absolute pins that survive scrollback eviction.
    pub absolute_row_counter: u64,
    /// Monotonic content-generation counter.
    ///
    /// Bumped once per content mutation: every cell/line/scrollback change that
    /// marks damage bumps it, because the bump is fused into the
    /// `mark_content_*` wrappers below. Not bumped on a pure viewport change
    /// (`scroll_display` / `apply_display_offset_damage`), so a cached search
    /// index or a peer session detects content change in O(1) without
    /// invalidating on a scroll.
    ///
    /// Starts at `1`, so `0` is a "never observed" sentinel. Read only by
    /// [`Grid::content_gen`]; no render or parse path reads it.
    pub content_gen: u64,
    /// Monotonic generation bumped once each time the reader brings the
    /// viewport down to the live bottom: `display_offset` going from non-zero
    /// to zero through a reader-facing scroll primitive, and nothing else.
    ///
    /// [`crate::grid::scrollback_offload`]'s re-attach must tell a reader's End
    /// during a reflow window apart from the detach's own clamp, a retention
    /// clamp, a rows-only resize re-anchoring the viewport, and the per-batch
    /// pin/repin around output. All of those write the same `0`, so no reading
    /// of the value can separate them; a generation sampled at detach and
    /// compared at re-attach records provenance instead.
    ///
    /// The bump is `before != 0 && after == 0` inside the four reader-facing
    /// primitives in [`crate::grid::scroll`] (`scroll_display`,
    /// `scroll_to_top`, `scroll_to_bottom`, `scroll_to_absolute_row`). So an
    /// End at a viewport already at the bottom records nothing, and a reader
    /// who scrolls up and stops records nothing.
    ///
    /// Machine motion never bumps it, via its own entry points:
    /// `reset_display_offset_with_damage` (the row-arithmetic precondition of
    /// IL/DL/ED/unscroll/`scroll_up`),
    /// `pin_viewport_to_live_for_output_batch` + `repin_display_offset`,
    /// `scroll_to_absolute_row` from the rows-only resize re-anchor (it
    /// cannot reach the live bottom there, since `Grid::resize` zeroes
    /// `display_offset` first; see its doc),
    /// `clamp_display_offset`, the reflow clamp and the erase resets.
    ///
    /// Read only by that consumer; no render or parse path reads it.
    pub(crate) reader_live_bottom_gen: u64,
    /// Monotonic autowrap serial: the emulator's wrap fact.
    ///
    /// Bumped once each time `advance_autowrap_line` runs, the one funnel
    /// every autowrap line advance goes through (deferred wraps via
    /// `resolve_pending_wrap`, plus the bulk/split write paths that advance
    /// mid-run). A host diffing it against a last-seen value learns that the
    /// terminal wrapped, which distinguishes a scrolled bottom-row wrap from
    /// `Home` pressed at the last column.
    ///
    /// Transient, not persisted: checkpoint/restore does not capture it
    /// (`GridCursorRepr` round-trips `pending_wrap` only), and main/alt swaps
    /// do not migrate it (each grid keeps its own serial). A restored session
    /// or buffer switch may give the host at most one spurious read, which a
    /// per-window `(session, last-serial)` baseline tolerates. No render or
    /// parse path reads it.
    pub wrap_serial: u64,
    /// Fast-path flag: set when any row uses DECDWL/DECDHL (double-width/height).
    /// When false, `effective_cols_for_row` skips the ring-buffer lookup
    /// (which checks row line_size) and returns `self.cols` directly.
    /// Sticky: once set, only cleared on grid reset or resize.
    pub any_double_width: bool,
    /// Fast-path flag: set when horizontal margins are non-full-width (DECLRMM active).
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
    /// their cell allocations to the SAME arena for future row construction.
    pub(crate) fn recycle_rows(&mut self, range: std::ops::Range<usize>) {
        for row in self.rows.drain(range) {
            // SAFETY: every row in GridStorage is allocated by its own pages;
            // drain transfers ownership and removes all access through rows.
            unsafe { row.recycle(&mut self.pages) };
        }
    }

    // -------------------------------------------------------------------------
    // Content-mutation damage wrappers
    //
    // Each does the `damage.mark_*` call and bumps `content_gen`, so any content
    // mutation that marks damage also advances `content_gen`. Viewport-only
    // marks (`apply_display_offset_damage`) do not route here: a viewport
    // scroll changes what is shown, not the content.
    //
    // Plain `+= 1` (not `wrapping_add`): a debug build panics rather than
    // silently wrapping a generation token.
    // -------------------------------------------------------------------------

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

    /// Convert a visible row index to the internal ring buffer index.
    ///
    /// Returns `None` if the row is in tiered scrollback (display_offset exceeds
    /// ring buffer bounds). This prevents underflow when scrolled beyond ring buffer.
    ///
    /// Fast path: when `display_offset == 0` (live terminal, not scrolled back),
    /// skips the offset subtraction check. This is the common case during writes.
    #[inline]
    pub(crate) fn row_index(&self, visible_row: u16) -> Option<usize> {
        debug_assert!(
            !self.rows.is_empty(),
            "row_index: ring buffer has zero rows"
        );

        let ring_scrollback = self.total_lines.saturating_sub(self.visible_rows as usize);
        let base = ring_scrollback + usize::from(visible_row);

        // Fast path: display_offset == 0 (live terminal, 99% of write calls)
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

    /// Shift rows UP by `n` within visible range `[top, bottom]`.
    ///
    /// Swaps Row structs (~24 bytes each) instead of copying cell data (~640 bytes
    /// at 80 cols). For n=1 in a 16-row region this moves ~360 bytes vs 9,600.
    /// After the swap chain, positions `[bottom-n+1..=bottom]` hold the old top
    /// rows (which the caller clears).
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

    /// Shift rows DOWN by `n` within visible range `[top, bottom]`.
    ///
    /// Swaps Row structs instead of copying cell data. Iterates backwards to
    /// propagate correctly. After the swap chain, positions `[top..top+n-1]`
    /// hold the old bottom rows (which the caller clears).
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

    // -------------------------------------------------------------------------
    // Row access
    // -------------------------------------------------------------------------

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
        // `get_unchecked` below is sound: `row_index` returns
        // `(ring_head + base) % self.rows.len()` on both paths, so
        // `idx < self.rows.len()` (proved in
        // crates/aterm-spec-models/proofs/ay/a1_row_index/row_index_in_bounds.smt2).
        // `len != 0` holds because modulo by zero panics inside `row_index`, and
        // `self.rows` is not resized between here and the two uses below. This is
        // the most-executed lookup (every bulk write), so the redundant bounds
        // check is removed.
        //
        // The debug_asserts bracket that span as a tripwire: an edit that
        // resizes `self.rows` in between would make `idx` stale and fail the
        // tests that drive this path.
        debug_assert!(
            idx < self.rows.len(),
            "A1 violated: row_index returned out-of-bounds idx {idx} (len {})",
            self.rows.len()
        );
        let cols = self.cols.max(1);
        // Fast path: skip the line_size check when no double-width rows exist
        // (DECDWL/DECDHL are rare).
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
        // Second tripwire (closes the full lexical span to the &mut use): see above.
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

    /// Get or create extras for a cell whose HAS_EXTRAS flag is already set.
    ///
    /// Skips the `set_cell_has_extras_flag` ring-buffer lookup. The caller
    /// MUST have already set the HAS_EXTRAS bit in the cell's PackedColors
    /// (e.g., via `colors.with_extras_flag()` during the write step).
    #[inline]
    pub(crate) fn cell_extra_mut_preflagged(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.extras_mut().get_or_create(CellCoord::new(row, col))
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
        // Phase 1: clear all HAS_EXTRAS flags.
        for row in &mut self.rows {
            for cell in row.iter_mut() {
                cell.set_has_extras(false);
            }
        }
        // Phase 2: set flags only for cells with actual extras entries.
        // Split borrows: extras lives in cursor_state.presentation, rows is separate.
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

    // -------------------------------------------------------------------------
    // Double-width / column geometry
    // -------------------------------------------------------------------------

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
        // Fast path: skip ring-buffer lookup when no double-width rows exist.
        // DECDWL/DECDHL are extremely rare; this avoids pointer chasing in
        // cursor_up, cursor_down, line_feed, set_cursor, cursor_forward, etc.
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
                        self.tab_stops[col] = col > 0 && col % 8 == 0;
                    }
                }
            }
        }

        self.visible_rows = new_rows;
        self.cols = new_cols;

        let row = self.cursor.row.min(new_rows.saturating_sub(1));
        let col = self.clamp_col_for_row(row, self.cursor.col);
        self.set_cursor_position(row, col);

        // The saved cursor is not clamped here. It names a cell by absolute
        // coordinates: the resize moves it only with its text (the reveal,
        // the demote, the rewrap, the lift and the fill), and DECRC and the
        // 1049 exit clamp what they read, as xterm's `CursorRestore` does. A
        // slot on a row a shrink trimmed stays past the grid, so a grow back
        // restores it onto that row (`Grid::lend_saved_cursor`).

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

    // Helper: build a Grid and return mutable access to its storage.
    fn make_storage(rows: u16, cols: u16) -> Grid {
        Grid::new(rows, cols)
    }

    // =========================================================================
    // Row access — row(), row_mut()
    // =========================================================================

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

    // =========================================================================
    // row_mut_with_effective_cols
    // =========================================================================

    #[test]
    fn row_mut_with_effective_cols_out_of_bounds() {
        let mut g = make_storage(5, 80);
        assert!(g.storage.row_mut_with_effective_cols(5).is_none());
    }

    // =========================================================================
    // Cursor state — via Deref to GridCursorState
    // =========================================================================

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

    // =========================================================================
    // Scroll region
    // =========================================================================

    #[test]
    fn scroll_region_invalid_range_resets_to_full() {
        let mut g = make_storage(24, 80);
        // top >= bottom should reset to full
        g.storage.set_scroll_region(10, 10);
        assert!(g.storage.scroll_region().is_full(24));
    }

    // =========================================================================
    // Horizontal margins
    // =========================================================================

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

    // =========================================================================
    // Effective column count / double-width
    // =========================================================================

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

    // =========================================================================
    // Double-width row — effective cols halved
    // =========================================================================

    #[test]
    fn effective_cols_double_width_row() {
        let mut g = make_storage(5, 80);
        g.storage.any_double_width = true;
        // Set row 2 to double-width
        if let Some(row) = g.storage.row_mut(2) {
            row.set_line_size(LineSize::DoubleWidth);
        }
        assert_eq!(g.storage.effective_cols_for_row(2), 40);
        // Row 0 remains normal
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

    // =========================================================================
    // shift_visible_rows_up / shift_visible_rows_down
    // =========================================================================

    #[test]
    fn shift_rows_up_n_zero_is_noop() {
        let mut g = make_storage(5, 10);
        // Write content to row 0 so we can verify no change
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
    /// moved ANY row would show up in [`first_cells`].
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

    /// A3, BOUNDED AND EXHAUSTIVE: at steady state — the ring full
    /// (`total_lines == rows.len()`) with its head rotated through every
    /// position — every `shift_visible_rows_up/down(top, bottom, n)` a 6-row
    /// screen admits, applied in sequence, leaves the multiset of live row
    /// slices exactly as it was and allocates nothing from the page store. The
    /// swap chain is a PERMUTATION of `Row` structs: no slice is lost,
    /// duplicated (two rows aliasing one arena slice) or freshly allocated.
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

    /// The negative control: the multiset check is not vacuous. Overwriting one
    /// ring row with a fresh allocation — what a copy-based shift that
    /// allocated a new row would do — changes both readings the lemma test
    /// compares.
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

    // =========================================================================
    // push_ring_extras
    // =========================================================================

    #[test]
    fn push_ring_extras_empty() {
        let mut g = make_storage(5, 10);
        let extras = super::ScrolledRowExtras::default();
        assert!(extras.is_empty());
        g.storage.push_ring_extras(extras);
        assert_eq!(g.storage.ring_extras.len(), 1);
        // Empty extras stored as None (optimization)
        assert!(g.storage.ring_extras.get(0).is_none());
        g.storage.ring_extras.assert_running_sums();
    }

    // =========================================================================
    // resize_viewport_state
    // =========================================================================

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
        // Tab stops should have been extended to 120 entries
        assert!(g.storage.tab_stops.len() >= 120);
        // Check that new tab stops follow 8-column pattern
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
        // Tab stops should NOT shrink (xterm behavior)
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

    // =========================================================================
    // Scrollback attachment
    // =========================================================================

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
        // The raw writer takes the store SHARE; the getter reports the ONE
        // unified total = share + ring cap, so the pair composes
        // to the same arithmetic `Grid::set_scrollback_line_limit` applies.
        g.storage.set_store_line_limit(Some(500));
        assert_eq!(
            g.storage.scrollback_line_limit(),
            Some(500 + g.storage.max_scrollback)
        );
    }

    // =========================================================================
    // Tab stops via Deref
    // =========================================================================

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
