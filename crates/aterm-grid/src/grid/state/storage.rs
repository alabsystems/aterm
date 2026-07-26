// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Storage- and history-oriented `Grid` state.

use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};

use aterm_scrollback::ScrollbackStorage;

use super::super::GenerationTracker;
use super::super::LineSize;
use super::super::ScrolledRowExtras;
use super::super::scroll_convert::LazyBuffer;
#[cfg(feature = "disk-tier")]
use super::super::scrollback_budget::BudgetEnforcer;
use super::super::{CellCoord, CellExtra};
use super::super::{HorizontalMargins, PageStore, Row, ScrollRegion};
use super::GridCursorState;

/// Ring cap meaning "unlimited retention" (`set_scrollback_line_limit(None)`
/// on a ring-only grid). Deliberately NOT `usize::MAX`: `scroll_up` computes
/// ring capacity as `visible_rows + max_scrollback`, and `visible_rows` is
/// clamped to `MAX_GRID_ROWS`, so this headroom keeps that sum overflow-free.
pub(crate) const UNLIMITED_RING_SCROLLBACK: usize = usize::MAX - crate::MAX_GRID_ROWS as usize;

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
    /// THRU-5: true when an off-thread compression worker is attached to this
    /// session (set once at session setup). While true, the reader-thread ingest
    /// path does NOT drain the lazy buffer inline on `should_drain` — it lets the
    /// worker drain it in bounded batches via [`Grid::drain_lazy_bounded`], so the
    /// LZ4/zstd promotion spike stays off the PTY-drain critical path. The reader
    /// still drains inline as a bounded fallback once the backlog exceeds
    /// `Grid::ASYNC_COMPRESS_BACKPRESSURE` (the worker fell behind), so memory
    /// stays bounded and history is never lost.
    pub(crate) compress_offload_active: bool,
    /// Monotonic count of STAGED history lines dropped by flood backpressure
    /// (the lazy-buffer THRU-5 cap while a compress worker is behind, and the
    /// detached-reflow-window cap) — real retention loss, surfaced OUT-OF-BAND
    /// (audit E10a: never a sentinel line in content). User-requested limit
    /// shrinks are NOT counted.
    pub(crate) flood_truncated_lines: u64,
    /// Ring-byte watermark budget (audit E10a): an approximate byte budget the
    /// host sets so ring-only "unlimited" retention still reports memory
    /// pressure ([`Grid::ring_watermark_level`]). `None` (default) = no
    /// watermark, level reads Green.
    pub(crate) ring_byte_watermark: Option<usize>,
    /// Monotonic generation bumped whenever off-screen scrollback is erased
    /// (ED3 / full reset). Captured when the store is detached for reflow and
    /// re-checked on re-attach: if it advanced, the reflowed (pre-erase) store is
    /// dropped rather than resurrecting history the user explicitly cleared.
    pub(crate) scrollback_clear_gen: u64,
    /// Preserved extras for ring buffer scrollback rows.
    ///
    /// When rows scroll from the visible area into ring buffer scrollback,
    /// their extras (hyperlinks, complex chars, combining marks, RGB colors)
    /// are extracted from `CellExtras` before the extras are discarded.
    /// Indexed by ring buffer scrollback position:
    /// front = oldest (same order as ring buffer).
    ///
    /// Uses `Option<Box<>>` to avoid 120 bytes of empty Vec headers per
    /// plain-text row. `None` = no overflow data (common case, 8 bytes).
    /// `Some(Box<..>)` = has overflow data (rare, heap-allocated).
    pub ring_extras: VecDeque<Option<Box<ScrolledRowExtras>>>,
    /// Generation tracker for pin invalidation.
    /// Tracks page evictions to detect stale pins.
    pub generations: GenerationTracker,
    /// Absolute row counter (monotonically increasing).
    /// Used for creating absolute pins that survive scrollback eviction.
    pub absolute_row_counter: u64,
    /// Monotonic content-generation counter (P1.0).
    ///
    /// Bumped exactly once per CONTENT mutation — every cell/line/scrollback
    /// change that marks damage also bumps this, because the bump is FUSED into
    /// the `mark_content_*` wrappers below (the same total-coverage trick
    /// `damage_epoch` gets from the damage-mark funnel). It does NOT bump on a
    /// pure VIEWPORT change (`scroll_display` / `apply_display_offset_damage`),
    /// so a cached search index or a peer session can detect content change in
    /// O(1) without invalidating on a mere scroll.
    ///
    /// Initialized NONZERO (`1`) so a reader can use `0` as a "never observed"
    /// sentinel. WRITE-ONLY except for [`Grid::content_gen`]: no render or parse
    /// path reads it, so it cannot alter any rendered output.
    pub content_gen: u64,
    /// Fast-path flag: set when any row uses DECDWL/DECDHL (double-width/height).
    /// When false, `effective_cols_for_row` skips the ring-buffer lookup
    /// (which checks row line_size) and returns `self.cols` directly.
    /// Sticky: once set, only cleared on grid reset or resize.
    pub any_double_width: bool,
    /// Fast-path flag: set when horizontal margins are non-full-width (DECLRMM active).
    pub has_horizontal_margins: bool,
    /// Memory budget enforcer for scrollback disk spill.
    /// Tracks in-memory scrollback usage and evicts to a temp mmap file
    /// when the budget is exceeded. `None` until explicitly enabled.
    /// Disk cold-tier only; absent on wasm (no libc/zstd-sys).
    #[cfg(feature = "disk-tier")]
    pub(crate) budget_enforcer: Option<BudgetEnforcer>,
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
    #[cfg(kani)]
    pub(crate) fn kani_stub(
        pages: PageStore,
        rows: Vec<Row>,
        visible_rows: u16,
        cols: u16,
        max_scrollback: usize,
    ) -> Self {
        Self {
            pages,
            rows,
            visible_rows,
            cols,
            max_scrollback,
            total_lines: visible_rows as usize,
            display_offset: 0,
            ring_head: 0,
            scrollback: None,
            lazy_buffer: LazyBuffer::new(),
            scrollback_detached_for_reflow: false,
            compress_offload_active: false,
            flood_truncated_lines: 0,
            ring_byte_watermark: None,
            scrollback_clear_gen: 0,
            ring_extras: VecDeque::new(),
            generations: GenerationTracker::new(),
            absolute_row_counter: u64::from(visible_rows),
            // Init NONZERO so `0` is a usable "never observed" sentinel (P1.0).
            content_gen: 1,
            any_double_width: false,
            has_horizontal_margins: false,
            #[cfg(feature = "disk-tier")]
            budget_enforcer: None,
            cursor_state: GridCursorState::kani_stub(visible_rows, cols),
        }
    }

    // -------------------------------------------------------------------------
    // Content-mutation damage wrappers (P1.0)
    //
    // Each does the EXISTING `damage.mark_*` call AND bumps `content_gen`. The
    // bump is FUSED here so any CONTENT mutation that correctly marks damage also
    // advances `content_gen` — it cannot be forgotten on a future mutation path,
    // inheriting the same total coverage the damage funnel already has. VIEWPORT
    // -only marks (`apply_display_offset_damage`) deliberately do NOT route here:
    // a viewport scroll changes what is shown, not the content, so a cached
    // search index / peer session must NOT invalidate.
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
        self.ring_extras.push_back(if extracted.is_empty() {
            None
        } else {
            Some(Box::new(extracted))
        });
    }

    #[must_use]
    #[inline]
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
    pub(crate) fn visible_rows(&self) -> u16 {
        self.visible_rows
    }

    #[must_use]
    #[inline]
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
    pub(crate) fn cols(&self) -> u16 {
        self.cols
    }

    #[must_use]
    #[inline]
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
    pub(crate) fn total_lines(&self) -> usize {
        self.total_lines
    }

    #[must_use]
    #[inline]
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
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

    /// The live on-screen row at `screen_row`, IGNORING `display_offset` (as if
    /// the viewport were at the bottom). Absolute-frame readers (block text
    /// extraction) use this so the current scroll position cannot shift which
    /// physical row they read. Equivalent to `row_index`'s `display_offset == 0`
    /// fast path.
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
        // EARNED get_unchecked (PCP A1): `row_index` returns `(ring_head + base) %
        // self.rows.len()` (both its display_offset fast and slow paths end in `% len`),
        // so `idx < self.rows.len()` — the A1 bounds theorem `(ring_head + base) % len <
        // len` for all `len != 0`, DISCHARGED on `ay`
        // (crates/aterm-spec-models/proofs/ay/a1_row_index/row_index_in_bounds.smt2 →
        // unsat; both Some-returning paths share this `% len` postcondition). The `len !=
        // 0` side condition is enforced by Rust's DETERMINISTIC modulo-by-zero PANIC (a
        // checked trap, never UB) inside `row_index` — a `Some` is unreachable when `len
        // == 0`, in release too — NOT by the elidable `is_empty` debug_assert there. And
        // `self.rows` is neither pushed/popped nor resized anywhere in this `&mut self`
        // method between this computation and the two uses below (the single-borrow
        // lexical lemma — verified by inspection; the only `self.rows` mutators are in
        // OTHER methods). This is the engine's single most-executed lookup (every bulk
        // write); the conversion removes the redundant slice bounds check the modulo
        // already makes impossible to fail.
        //
        // The arithmetic bound `x % len < len` is TOTAL — never violable post-modulo — so
        // the `debug_assert`s are really a TRIPWIRE for the LEXICAL lemma: if a future
        // edit inserts a `self.rows` length-mutation into this span, the now-stale `idx`
        // reddens the suite (the conformance + bulk-write tests drive this path across
        // many ring-head / offset states). One assert at the index site and one
        // immediately before the `&mut` use bracket the full lexical span.
        debug_assert!(
            idx < self.rows.len(),
            "A1 violated: row_index returned out-of-bounds idx {idx} (len {})",
            self.rows.len()
        );
        let cols = self.cols.max(1);
        // Fast path: skip line_size check when no double-width rows exist.
        // This is the common case — DECDWL/DECDHL are rare (VT100 era).
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
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
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
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
    pub(crate) fn cell_extra_mut_preflagged(&mut self, row: u16, col: u16) -> &mut CellExtra {
        self.extras_mut().get_or_create(CellCoord::new(row, col))
    }

    #[inline]
    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
    pub(crate) fn remove_cell_extra(&mut self, row: u16, col: u16) -> bool {
        let removed = self.extras_mut().remove(CellCoord::new(row, col));
        if removed {
            self.set_cell_has_extras_flag(row, col, false);
        }
        removed
    }

    #[allow(
        dead_code,
        reason = "called via Grid wrapper; lint cannot see through Deref delegation"
    )]
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
    /// Replaces the previous O(rows * cols) hash-probe approach (#5859).
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
    #[allow(
        dead_code,
        reason = "called by cell_extra_mut and remove_cell_extra above"
    )]
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
            // Only grow tab_stops, never shrink. This preserves custom tab stops
            // set beyond the current width during a shrink+grow cycle, matching
            // xterm behavior. Tab operations already bounds-check via max_col
            // (clamped to cols-1), so a larger array is safe. (#7479)
            if usize::from(new_cols) > self.tab_stops.len() {
                let old_len = self.tab_stops.len();
                self.tab_stops.resize(usize::from(new_cols), false);
                for col in old_len..self.tab_stops.len() {
                    self.tab_stops[col] = col > 0 && col % 8 == 0;
                }
            }
        }

        self.visible_rows = new_rows;
        self.cols = new_cols;

        let row = self.cursor.row.min(new_rows.saturating_sub(1));
        let col = self.clamp_col_for_row(row, self.cursor.col);
        self.set_cursor_position(row, col);

        if self.saved_cursor.valid {
            self.saved_cursor.cursor.row =
                self.saved_cursor.cursor.row.min(new_rows.saturating_sub(1));
            let saved_row = self.saved_cursor.cursor.row;
            self.saved_cursor.cursor.col =
                self.clamp_col_for_row(saved_row, self.saved_cursor.cursor.col);
            // Clear stale pending_wrap — the saved cursor may no longer be at the
            // right margin after resize, making the deferred wrap invalid.
            let max_col = self.max_col_for_row(saved_row);
            if self.saved_cursor.cursor.col < max_col {
                self.saved_cursor.pending_wrap = false;
            }
        }

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

    fn make_storage_with_scrollback(rows: u16, cols: u16, max_sb: usize) -> Grid {
        Grid::with_scrollback(rows, cols, max_sb)
    }

    // =========================================================================
    // Construction — dimensions, initial state
    // =========================================================================

    #[test]
    fn construction_visible_rows() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.visible_rows, 24);
    }

    #[test]
    fn construction_cols() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.cols, 80);
    }

    #[test]
    fn construction_total_lines_equals_visible() {
        let g = make_storage(10, 40);
        assert_eq!(g.storage.total_lines, 10);
    }

    #[test]
    fn construction_display_offset_zero() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.display_offset, 0);
    }

    #[test]
    fn construction_ring_head_zero() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.ring_head, 0);
    }

    #[test]
    fn construction_no_scrollback() {
        let g = make_storage(24, 80);
        assert!(g.storage.scrollback.is_none());
    }

    #[test]
    fn construction_ring_extras_empty() {
        let g = make_storage(24, 80);
        assert!(g.storage.ring_extras.is_empty());
    }

    #[test]
    fn construction_flags_default_false() {
        let g = make_storage(24, 80);
        assert!(!g.storage.any_double_width);
        assert!(!g.storage.has_horizontal_margins);
    }

    #[test]
    fn construction_absolute_row_counter() {
        let g = make_storage(10, 40);
        assert_eq!(g.storage.absolute_row_counter, 10);
    }

    #[test]
    fn construction_row_vec_len_equals_visible() {
        let g = make_storage(5, 20);
        assert_eq!(g.storage.rows.len(), 5);
    }

    // =========================================================================
    // 1x1 edge case
    // =========================================================================

    #[test]
    fn construction_1x1_dimensions() {
        let g = make_storage(1, 1);
        assert_eq!(g.storage.visible_rows, 1);
        assert_eq!(g.storage.cols, 1);
        assert_eq!(g.storage.rows.len(), 1);
    }

    #[test]
    fn construction_zero_clamped_to_1x1() {
        let g = make_storage(0, 0);
        assert_eq!(g.storage.visible_rows, 1);
        assert_eq!(g.storage.cols, 1);
    }

    // =========================================================================
    // row_index — ring buffer mapping
    // =========================================================================

    #[test]
    fn row_index_first_visible_row() {
        let g = make_storage(5, 10);
        let idx = g.storage.row_index(0);
        assert_eq!(idx, Some(0));
    }

    #[test]
    fn row_index_last_visible_row() {
        let g = make_storage(5, 10);
        let idx = g.storage.row_index(4);
        assert_eq!(idx, Some(4));
    }

    #[test]
    fn row_index_middle_row() {
        let g = make_storage(8, 10);
        let idx = g.storage.row_index(3);
        assert_eq!(idx, Some(3));
    }

    // =========================================================================
    // row_index_base — fast path base
    // =========================================================================

    #[test]
    fn row_index_base_no_scrollback() {
        let g = make_storage(5, 10);
        // With no scrollback, ring_scrollback = 0, base = ring_head + 0 = 0
        assert_eq!(g.storage.row_index_base(), 0);
    }

    // =========================================================================
    // Row access — row(), row_mut()
    // =========================================================================

    #[test]
    fn row_access_valid_row() {
        let g = make_storage(5, 10);
        assert!(g.storage.row(0).is_some());
        assert!(g.storage.row(4).is_some());
    }

    #[test]
    fn row_access_out_of_bounds_returns_none() {
        let g = make_storage(5, 10);
        assert!(g.storage.row(5).is_none());
        assert!(g.storage.row(100).is_none());
    }

    #[test]
    fn row_mut_valid_row() {
        let mut g = make_storage(5, 10);
        assert!(g.storage.row_mut(0).is_some());
        assert!(g.storage.row_mut(4).is_some());
    }

    #[test]
    fn row_mut_out_of_bounds_returns_none() {
        let mut g = make_storage(5, 10);
        assert!(g.storage.row_mut(5).is_none());
    }

    #[test]
    fn row_has_correct_column_count() {
        let g = make_storage(3, 20);
        let r = g.storage.row(0).expect("row 0 should exist");
        assert_eq!(r.cols(), 20);
    }

    #[test]
    fn row_initially_empty() {
        let g = make_storage(3, 10);
        let r = g.storage.row(0).expect("row 0 should exist");
        assert!(r.is_empty());
    }

    // =========================================================================
    // row_mut_with_effective_cols
    // =========================================================================

    #[test]
    fn row_mut_with_effective_cols_normal_row() {
        let mut g = make_storage(5, 80);
        let result = g.storage.row_mut_with_effective_cols(0);
        assert!(result.is_some());
        let (row, eff_cols) = result.unwrap();
        assert_eq!(eff_cols, 80);
        assert_eq!(row.cols(), 80);
    }

    #[test]
    fn row_mut_with_effective_cols_out_of_bounds() {
        let mut g = make_storage(5, 80);
        assert!(g.storage.row_mut_with_effective_cols(5).is_none());
    }

    // =========================================================================
    // Cursor state — via Deref to GridCursorState
    // =========================================================================

    #[test]
    fn cursor_initial_position() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.cursor(), Cursor::new(0, 0));
    }

    #[test]
    fn cursor_set_position() {
        let mut g = make_storage(24, 80);
        g.storage.set_cursor_position(5, 10);
        assert_eq!(g.storage.cursor(), Cursor::new(5, 10));
    }

    #[test]
    fn cursor_pending_wrap_initially_false() {
        let g = make_storage(24, 80);
        assert!(!g.storage.pending_wrap());
    }

    #[test]
    fn cursor_mark_pending_wrap() {
        let mut g = make_storage(24, 80);
        g.storage.mark_pending_wrap();
        assert!(g.storage.pending_wrap());
    }

    #[test]
    fn cursor_clear_pending_wrap() {
        let mut g = make_storage(24, 80);
        g.storage.mark_pending_wrap();
        g.storage.clear_pending_wrap();
        assert!(!g.storage.pending_wrap());
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

    // =========================================================================
    // Scroll region
    // =========================================================================

    #[test]
    fn scroll_region_initial_full() {
        let g = make_storage(24, 80);
        let region = g.storage.scroll_region();
        assert_eq!(region.top, 0);
        assert_eq!(region.bottom, 23);
        assert!(region.is_full(24));
    }

    #[test]
    fn scroll_region_set_custom() {
        let mut g = make_storage(24, 80);
        g.storage.set_scroll_region(5, 20);
        let region = g.storage.scroll_region();
        assert_eq!(region.top, 5);
        assert_eq!(region.bottom, 20);
        assert!(!region.is_full(24));
    }

    #[test]
    fn scroll_region_reset() {
        let mut g = make_storage(24, 80);
        g.storage.set_scroll_region(5, 20);
        g.storage.reset_scroll_region();
        let region = g.storage.scroll_region();
        assert!(region.is_full(24));
    }

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
    fn horizontal_margins_initial_full() {
        let g = make_storage(24, 80);
        let margins = g.storage.horizontal_margins();
        assert_eq!(margins.left, 0);
        assert_eq!(margins.right, 79);
        assert!(margins.is_full(80));
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

    // =========================================================================
    // Scrollback — ring buffer scrollback count
    // =========================================================================

    #[test]
    fn scrollback_initially_zero() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.ring_buffer_scrollback(), 0);
    }

    #[test]
    fn scrollback_lines_initially_zero() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.scrollback_lines(), 0);
    }

    // =========================================================================
    // Effective column count / double-width
    // =========================================================================

    #[test]
    fn effective_cols_normal_row() {
        let g = make_storage(5, 80);
        assert_eq!(g.storage.effective_cols_for_row(0), 80);
    }

    #[test]
    fn effective_cols_all_rows_normal() {
        let g = make_storage(5, 40);
        for row in 0..5 {
            assert_eq!(g.storage.effective_cols_for_row(row), 40);
        }
    }

    #[test]
    fn effective_cols_1_col_normal() {
        let g = make_storage(1, 1);
        assert_eq!(g.storage.effective_cols_for_row(0), 1);
    }

    #[test]
    fn max_col_for_row_normal() {
        let g = make_storage(5, 80);
        assert_eq!(g.storage.max_col_for_row(0), 79);
    }

    #[test]
    fn max_col_for_row_1_col() {
        let g = make_storage(1, 1);
        assert_eq!(g.storage.max_col_for_row(0), 0);
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
    fn row_is_double_width_false_by_default() {
        let g = make_storage(5, 80);
        assert!(!g.storage.row_is_double_width(0));
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

    #[test]
    fn shift_rows_up_n_exceeds_range_is_noop() {
        let mut g = make_storage(5, 10);
        // top + n > bottom should be a no-op
        g.storage.shift_visible_rows_up(0, 2, 5);
        // Just verify no panic
    }

    #[test]
    fn shift_rows_down_n_exceeds_range_is_noop() {
        let mut g = make_storage(5, 10);
        g.storage.shift_visible_rows_down(0, 2, 5);
        // Just verify no panic
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
        assert!(g.storage.ring_extras[0].is_none());
    }

    // =========================================================================
    // resize_viewport_state
    // =========================================================================

    #[test]
    fn resize_viewport_updates_dimensions() {
        let mut g = make_storage(24, 80);
        g.storage.resize_viewport_state(30, 100);
        assert_eq!(g.storage.visible_rows, 30);
        assert_eq!(g.storage.cols, 100);
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
        // Tab stops should NOT shrink (xterm behavior, #7479)
        assert_eq!(g.storage.tab_stops.len(), len_before);
    }

    #[test]
    fn resize_viewport_clamps_saved_cursor() {
        let mut g = make_storage(24, 80);
        g.storage.set_cursor_position(20, 70);
        g.storage.save_cursor();
        g.storage.resize_viewport_state(10, 40);
        let saved = g.storage.saved_cursor();
        assert!(saved.cursor.row < 10);
        assert!(saved.cursor.col < 40);
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
        // unified total = share + ring cap (audit E1), so the pair composes
        // to the same arithmetic `Grid::set_scrollback_line_limit` applies.
        g.storage.set_store_line_limit(Some(500));
        assert_eq!(
            g.storage.scrollback_line_limit(),
            Some(500 + g.storage.max_scrollback)
        );
    }

    // =========================================================================
    // Lazy buffer
    // =========================================================================

    #[test]
    fn lazy_buffer_initially_empty() {
        let g = make_storage(24, 80);
        assert_eq!(g.storage.lazy_buffer_lines(), 0);
    }

    // =========================================================================
    // visible_rows / cols / total_lines / display_offset accessors
    // =========================================================================

    #[test]
    fn accessor_visible_rows() {
        let g = make_storage(15, 50);
        assert_eq!(g.storage.visible_rows(), 15);
    }

    #[test]
    fn accessor_cols() {
        let g = make_storage(15, 50);
        assert_eq!(g.storage.cols(), 50);
    }

    #[test]
    fn accessor_total_lines() {
        let g = make_storage(15, 50);
        assert_eq!(g.storage.total_lines(), 15);
    }

    #[test]
    fn accessor_display_offset() {
        let g = make_storage(15, 50);
        assert_eq!(g.storage.display_offset(), 0);
    }

    // =========================================================================
    // Tab stops via Deref
    // =========================================================================

    #[test]
    fn tab_stops_default_every_8() {
        let g = make_storage(24, 80);
        assert!(!g.storage.is_tab_stop(0));
        assert!(g.storage.is_tab_stop(8));
        assert!(g.storage.is_tab_stop(16));
        assert!(g.storage.is_tab_stop(24));
        assert!(!g.storage.is_tab_stop(7));
        assert!(!g.storage.is_tab_stop(9));
    }

    #[test]
    fn tab_stop_set_and_clear() {
        let mut g = make_storage(24, 80);
        g.storage.set_tab_stop_at(5);
        assert!(g.storage.is_tab_stop(5));
        g.storage.clear_tab_stop_at(5);
        assert!(!g.storage.is_tab_stop(5));
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

    // =========================================================================
    // Large grid — stress test
    // =========================================================================

    #[test]
    fn large_grid_row_access() {
        let g = make_storage(200, 300);
        assert_eq!(g.storage.rows.len(), 200);
        assert!(g.storage.row(0).is_some());
        assert!(g.storage.row(199).is_some());
        assert!(g.storage.row(200).is_none());
    }

    // =========================================================================
    // with_scrollback constructor
    // =========================================================================

    #[test]
    fn with_scrollback_zero_scrollback() {
        let g = make_storage_with_scrollback(5, 10, 0);
        assert_eq!(g.storage.max_scrollback, 0);
        assert_eq!(g.storage.rows.len(), 5);
    }

    #[test]
    fn with_scrollback_large() {
        let g = make_storage_with_scrollback(24, 80, 50_000);
        assert_eq!(g.storage.max_scrollback, 50_000);
    }
}
