// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid scroll operations: display scrolling (viewing history), content
//! scrolling (scroll_up, scroll_down) and region scrolling (within DECSTBM
//! margins).
//!
//! Content-modifying scrolls preserve the `pending_wrap` flag, as in xterm:
//! `xtermScroll` saves and restores `screen->do_wrap` and `RevScroll` never
//! touches it (util.c), so a scrolling LF/RI/SU/SD leaves a deferred wrap
//! pending. Only cursor-motion ops (CursorUp/Down/Back/Forward/Set, CR) and
//! the editing ops with an explicit xterm `ResetWrap` (ICH/DCH/ECH/IL/DL and
//! the ED/EL-right family) cancel it.
//!
//! Row-to-line conversion helpers are in [`super::scroll_convert`].

//!
//! ## Ring Buffer Design
//!
//! The grid uses a ring buffer for O(1) display scrolling. When scrolling up:
//! - If not at capacity, new rows are appended
//! - If at capacity, oldest row is reused (optionally pushed to scrollback first)
//!
//! The `ring_head` index tracks the oldest row in the buffer.

use crate::damage::compute_display_offset_damage;
use crate::row::LineSize;

use super::Grid;
use super::row_u16;
use crate::Row;

impl Grid {
    /// Reset display_offset to 0 with targeted damage.
    ///
    /// Marks only the newly exposed bottom rows (as `scroll_display` does for a
    /// down-scroll) instead of `mark_full()`. Used by `scroll_to_bottom` and by
    /// operations whose row arithmetic needs `display_offset == 0`.
    ///
    /// Machine motion, so it does not bump `reader_live_bottom_gen`; most
    /// callers (IL/DL, ED, the unscroll, the `scroll_up` family) are forcing
    /// their own precondition. The reader's End press is `scroll_to_bottom`,
    /// which bumps for itself.
    pub(crate) fn reset_display_offset_with_damage(&mut self) {
        let old_offset = self.storage.display_offset;
        if old_offset == 0 {
            return;
        }
        self.storage.display_offset = 0;
        let dmg = compute_display_offset_damage(old_offset, 0, self.storage.visible_rows);
        self.storage.damage.apply_display_offset_damage(dmg);
    }

    /// Mark scroll damage: targeted rows for small scrolls, full for large.
    ///
    /// This is the content-bearing `scroll_up` path (rows move into scrollback,
    /// `absolute_row_counter += rows`), so it bumps `content_gen` via
    /// `mark_content_scroll`. Pure viewport scrolls (`scroll_display` /
    /// `apply_display_offset_damage`) do not come here and leave `content_gen`
    /// unchanged.
    fn mark_scroll_damage(&mut self, n: usize) {
        let visible_rows = self.storage.visible_rows;
        self.storage.mark_content_scroll(visible_rows, n);
    }

    /// Record that the reader just brought the viewport down to the live bottom.
    ///
    /// The one bump site for `reader_live_bottom_gen` (see its field doc).
    /// Called with the offset read just before a reader-facing viewport
    /// primitive ran; advances on a non-zero to zero transition, as used by
    /// [`crate::grid::scrollback_offload`]'s re-attach.
    ///
    /// `before != 0`: an End at a viewport already at the live bottom moved
    /// nothing and is not evidence of a choice. `after == 0`: a reader who
    /// scrolled up did not choose the live bottom, so if the machine later puts
    /// them there it must not be mistaken for their choice.
    fn note_reader_descent_to_live_bottom(&mut self, before: usize) {
        if before != 0 && self.storage.display_offset == 0 {
            self.storage.reader_live_bottom_gen += 1;
        }
    }

    /// Force the viewport to the live bottom for the duration of an output
    /// batch, undone by [`Self::repin_display_offset`].
    ///
    /// Same motion as [`Self::scroll_to_bottom`] but a separate entry point:
    /// this is the machine satisfying the `display_offset == 0` precondition of
    /// the VT row arithmetic downstream of `row_index`, and it re-pins the
    /// reader afterwards, so it must not touch `reader_live_bottom_gen`.
    /// Otherwise every output batch arriving while someone reads history
    /// (`tail -f`) would look like the reader pressing End.
    pub fn pin_viewport_to_live_for_output_batch(&mut self) {
        self.reset_display_offset_with_damage();
        debug_assert_eq!(self.storage.display_offset, 0);
    }

    /// Scroll the display by delta lines.
    ///
    /// Positive delta = scroll up (show older content).
    /// Negative delta = scroll down (show newer content).
    ///
    /// Reader motion: bumps `reader_live_bottom_gen` when it lands the viewport on
    /// the live bottom from above (see `note_reader_descent_to_live_bottom`).
    ///
    /// ENSURES: self.storage.display_offset <= self.storage.scrollback_lines()
    pub fn scroll_display(&mut self, delta: i32) {
        let max_offset = self.storage.scrollback_lines();
        let old_offset = self.storage.display_offset;
        // display_offset <= MAX_SCROLLBACK_LINES (1M), which fits in i32.
        let current: i32 = self.storage.display_offset.try_into().unwrap_or(i32::MAX);
        let clamped = current.saturating_add(delta).max(0);
        let new_offset = usize::try_from(clamped).unwrap_or(0);
        self.storage.display_offset = new_offset.min(max_offset);

        let dmg =
            compute_display_offset_damage(old_offset, self.storage.display_offset, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
        self.note_reader_descent_to_live_bottom(old_offset);
        // Ruling 238: while the history is away, the reader's ask is kept.
        self.note_detached_reader_motion(
            super::scrollback_offload::ReaderMotion::By(delta),
            old_offset,
        );
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Re-pin the viewport after a batch of output.
    ///
    /// `prev_offset` is the user's display_offset before processing reset it to
    /// 0; `lines_added` is the number of lines that entered scrollback during
    /// processing (the rise in `absolute_row_counter`). To keep the same content
    /// in view, the new offset is `prev_offset + lines_added`, clamped to
    /// `scrollback_lines()` so `display_offset <= scrollback_lines()` holds even
    /// when eviction discarded some of those lines.
    ///
    /// Machine motion (the epilogue of
    /// [`Self::pin_viewport_to_live_for_output_batch`]), so it does not bump
    /// `reader_live_bottom_gen`.
    ///
    /// ENSURES: self.storage.display_offset <= self.storage.scrollback_lines()
    pub fn repin_display_offset(&mut self, prev_offset: usize, lines_added: u64) {
        let max_offset = self.storage.scrollback_lines();
        let target = prev_offset
            .saturating_add(usize::try_from(lines_added).unwrap_or(usize::MAX))
            .min(max_offset);
        let old_offset = self.storage.display_offset;
        if target == old_offset {
            return;
        }
        self.storage.display_offset = target;
        let dmg = compute_display_offset_damage(old_offset, target, self.storage.visible_rows);
        self.storage.damage.apply_display_offset_damage(dmg);
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Scroll to the top of scrollback.
    ///
    /// Uses targeted row-level damage when the scroll delta is smaller than
    /// visible rows: only the top N rows are marked dirty. Falls back to
    /// `mark_full()` for large scrolls.
    ///
    /// Reader motion: bumps `reader_live_bottom_gen` when it lands the viewport on
    /// the live bottom from above (see `note_reader_descent_to_live_bottom`).
    ///
    /// ENSURES: self.storage.display_offset == self.storage.scrollback_lines()
    pub fn scroll_to_top(&mut self) {
        let target = self.storage.scrollback_lines();
        let old_offset = self.storage.display_offset;
        self.storage.display_offset = target;

        let dmg = compute_display_offset_damage(old_offset, target, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
        self.note_reader_descent_to_live_bottom(old_offset);
        self.note_detached_reader_motion(super::scrollback_offload::ReaderMotion::Top, old_offset);
        debug_assert_eq!(self.storage.display_offset, self.storage.scrollback_lines());
    }

    /// Scroll to live position (bottom).
    ///
    /// Uses targeted row-level damage instead of `mark_full()`:
    /// only the newly-exposed bottom rows are marked dirty.
    ///
    /// Reader motion (the End press): bumps `reader_live_bottom_gen` only when
    /// the viewport was not already at the live bottom. The machine's identical
    /// motion uses [`Self::pin_viewport_to_live_for_output_batch`] or
    /// [`Self::reset_display_offset_with_damage`].
    ///
    /// ENSURES: self.storage.display_offset == 0
    #[inline]
    pub fn scroll_to_bottom(&mut self) {
        let before = self.storage.display_offset;
        self.reset_display_offset_with_damage();
        self.note_reader_descent_to_live_bottom(before);
        self.note_detached_reader_motion(super::scrollback_offload::ReaderMotion::Live, before);
        debug_assert_eq!(self.storage.display_offset, 0);
    }

    /// Scroll the viewport so `target_abs_row` (an absolute row number, e.g. a
    /// command mark's `prompt_start_row`) sits at the top visible line, clamped
    /// to the valid history range: prompt-to-prompt navigation. A target at or
    /// below the live top clamps to the live bottom (offset 0); one older than
    /// the oldest retained line clamps to the top of scrollback. Same targeted
    /// damage as [`scroll_display`](Self::scroll_display); no damage when the
    /// offset is unchanged.
    ///
    /// Reader motion: bumps `reader_live_bottom_gen` when it lands the viewport on
    /// the live bottom from above (see `note_reader_descent_to_live_bottom`).
    ///
    /// Its one machine caller, a rows-only resize re-anchoring the viewport
    /// (`reflow.rs`), cannot descend to the live bottom: `Grid::resize` zeroes
    /// `display_offset` first, so the re-anchor can only move the viewport up.
    ///
    /// ENSURES: self.storage.display_offset <= self.storage.scrollback_lines()
    pub fn scroll_to_absolute_row(&mut self, target_abs_row: u64) {
        // The absolute row shown at the top of the LIVE (offset 0) viewport.
        let live_top = self
            .storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows));
        // How far ABOVE the live top `target_abs_row` sits = the offset that
        // lifts it to the top. A target at/below the live top saturates to 0.
        let want = live_top.saturating_sub(target_abs_row);
        let max_offset = self.storage.scrollback_lines();
        let new_offset = usize::try_from(want).unwrap_or(usize::MAX).min(max_offset);
        let old_offset = self.storage.display_offset;
        if new_offset == old_offset {
            return;
        }
        self.storage.display_offset = new_offset;
        let dmg = compute_display_offset_damage(old_offset, new_offset, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
        self.note_reader_descent_to_live_bottom(old_offset);
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Clamp display_offset to valid bounds.
    ///
    /// Call this after operations that may reduce scrollback size
    /// (e.g., truncation) to maintain the DisplayOffsetValid invariant.
    ///
    /// Machine motion (history went away under the viewport), so it does not
    /// bump `reader_live_bottom_gen`.
    ///
    /// Uses targeted row-level damage when the clamping delta is smaller than
    /// visible rows: only the bottom N rows are marked dirty.
    ///
    /// ENSURES: self.storage.display_offset <= self.storage.scrollback_lines()
    pub fn clamp_display_offset(&mut self) {
        let max_offset = self.storage.scrollback_lines();
        if self.storage.display_offset > max_offset {
            let old_offset = self.storage.display_offset;
            self.storage.display_offset = max_offset;
            let dmg = compute_display_offset_damage(old_offset, max_offset, self.rows());
            self.storage.damage.apply_display_offset_damage(dmg);
        }
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Scroll content up by n lines (new empty lines at bottom).
    ///
    /// When a scrollback is attached and the ring buffer is at capacity,
    /// the oldest row is converted to a [`Line`] and pushed to the scrollback
    /// before being overwritten.
    ///
    /// O(n × cols): each scrolled line is converted to a scrollback line
    /// (`row_to_line_with_stored_extras`) and the reused row is cleared and
    /// resized (`scroll_up_linear_time`, `scroll_up_handles_many_rows`).
    /// Batched: rows to add vs reuse are computed up front, growth capacity is
    /// reserved once, and counters update in bulk.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.rows.len() <= (self.storage.visible_rows as usize) + self.storage.max_scrollback
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    pub fn scroll_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }

        // SU reveals genuine blank rows at the bottom — same fill-target rule
        // as DL and the ED family.
        self.invalidate_pending_fill_target();
        self.scroll_up_storage(n);
        self.finish_scroll_up(n);
    }

    /// Perform the ring/tiered-history mutation for an upward scroll without
    /// recording presentation damage. Callers must finish with exactly one
    /// `mark_content_*` operation for the content shape they actually expose.
    fn scroll_up_storage(&mut self, n: usize) {
        debug_assert!(n > 0);
        debug_assert!(
            !self.storage.rows.is_empty(),
            "scroll_up_storage: ring buffer has zero rows"
        );

        let capacity = (self.storage.visible_rows as usize) + self.storage.max_scrollback;
        let cols = self.storage.cols;

        // Pre-calculate: how many rows can we add before hitting capacity?
        let rows_until_capacity = capacity.saturating_sub(self.storage.total_lines);
        let rows_to_add = n.min(rows_until_capacity);
        let rows_to_reuse = n.saturating_sub(rows_to_add);

        if rows_to_add > 0 {
            self.grow_scrollback_ring(rows_to_add, cols);
        }

        if rows_to_reuse > 0 {
            self.reuse_scrolled_rows(rows_to_reuse, cols);
        }
        // The rows just scrolled off took their links into history: hold the
        // uncompressed history's link payload to its byte budget (O(1) under it).
        self.enforce_history_link_budget();

        debug_assert!(
            self.storage.rows.len()
                <= (self.storage.visible_rows as usize) + self.storage.max_scrollback
        );
    }

    fn grow_scrollback_ring(&mut self, rows_to_add: usize, cols: u16) {
        debug_assert!(
            !self.storage.rows.is_empty(),
            "grow_scrollback_ring: ring buffer has zero rows"
        );
        let ring_sb = self.storage.ring_buffer_scrollback();
        let row_count = self.storage.rows.len();
        for i in 0..rows_to_add {
            let row_idx = row_u16(i);
            let phys = (self.storage.ring_head + ring_sb + i) % row_count;
            let extracted = Self::extract_row_extras(
                &self.storage.rows[phys],
                &self.storage.extras,
                row_idx,
                self.styles(),
            );
            self.storage.push_ring_extras(extracted);
        }

        self.storage.rows.reserve(rows_to_add);
        let fill = self.storage.cursor_template;
        {
            let storage = &mut self.storage;
            let rows = &mut storage.rows;
            let pages = &mut storage.pages;
            // `Row::new` already yields an all-EMPTY row with len 0 and DIRTY
            // flags — exactly the state `erase_with(Cell::EMPTY)` produces —
            // so the BCE fill pass is needed only for a non-default template.
            let needs_fill = fill != crate::Cell::EMPTY;
            for _ in 0..rows_to_add {
                // SAFETY: New rows are stored in the same `GridStorage` that owns
                // `pages`, and rows drop before the backing pages.
                let mut row = unsafe { Row::new(cols, pages) };
                // Apply BCE fill so new bottom rows inherit the current SGR
                // background color per VT420/xterm spec.
                if needs_fill {
                    row.erase_with(fill);
                }
                rows.push(row);
            }
        }
        self.storage.total_lines += rows_to_add;
        self.storage.absolute_row_counter += rows_to_add as u64;
        self.storage
            .extras
            .shift_rows_up_by(0, row_u16(rows_to_add));
        // Fill BCE RGB in vacated bottom rows after shift.
        let vis = self.storage.visible_rows;
        self.fill_bce_rgb_rows(vis.saturating_sub(row_u16(rows_to_add))..vis);
    }

    fn reuse_scrolled_rows(&mut self, rows_to_reuse: usize, cols: u16) {
        debug_assert!(
            !self.storage.rows.is_empty(),
            "reuse_scrolled_rows: ring buffer has zero rows"
        );
        let row_count = self.storage.rows.len();
        let ring_sb = self.storage.ring_buffer_scrollback();
        let has_scrollback = self.storage.stages_evicted_rows();

        if rows_to_reuse == 1 && !has_scrollback {
            self.reuse_one_scrolled_row_no_scrollback(cols, row_count, ring_sb);
        } else {
            self.reuse_scrolled_rows_general(
                rows_to_reuse,
                cols,
                row_count,
                ring_sb,
                has_scrollback,
            );
        }

        // Drain lazy buffer to tiered scrollback when threshold is exceeded.
        // This amortizes the materialization cost over many scroll operations.
        // Callers that need all lines in tiered storage (unscroll, reflow)
        // drain explicitly via drain_lazy_buffer().
        if self.storage.lazy_buffer.should_drain() {
            // During an off-thread reflow the store is detached and drain is
            // suppressed (staged lines must survive), so cap the buffer here to
            // keep heavy streaming through a long window bounded.
            if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
                self.bound_detached_lazy_buffer();
            } else if self.storage.compress_offload_active {
                // An off-thread compression worker owns the drain: leave the
                // backlog staged (readable and accounted in the lazy buffer) for
                // it to promote in bounded batches, off the PTY reader's path.
                //
                // Backpressure: past the staging cap the worker has fallen behind
                // (under a sustained flood it only trickles one batch a second).
                // The reader must never pay LZ4/zstd inline, which would collapse
                // flood throughput, so it drops the oldest staged lines instead
                // (O(1)). The cap is `flood_overflow`; how the loss is accounted
                // is `drop_flood_overflow`.
                let over = self.flood_overflow();
                if over > 0 {
                    self.drop_flood_overflow(over);
                }
            } else {
                self.drain_lazy_buffer();
            }
        }

        self.storage
            .extras
            .shift_rows_up_by(0, row_u16(rows_to_reuse));
        // Fill BCE RGB in vacated bottom rows after shift.
        let vis = self.storage.visible_rows;
        self.fill_bce_rgb_rows(vis.saturating_sub(row_u16(rows_to_reuse))..vis);
        self.storage.absolute_row_counter += rows_to_reuse as u64;
    }

    /// Steady-state line-feed fast path: single-row scroll with no tiered
    /// scrollback attached. Same semantics as the general path, but skips the
    /// intermediate extraction `Vec` and reuses the popped `ring_extras`
    /// allocation for the new row's extraction.
    fn reuse_one_scrolled_row_no_scrollback(
        &mut self,
        cols: u16,
        row_count: usize,
        ring_sb: usize,
    ) {
        let fill = self.storage.cursor_template;
        let oldest = self.storage.ring_head;
        let phys = (oldest + ring_sb) % row_count;

        if self.storage.ring_extras.is_empty() {
            // Net no-op in the general path: the freshly extracted extras are
            // pushed and immediately popped, then dropped (no tiered
            // scrollback consumes them). Skip the extraction entirely.
        } else if let Some(mut bx) = self.storage.ring_extras.pop_front().flatten() {
            // Recycle the popped box (and its Vec capacities) as scratch.
            Self::extract_row_extras_into(
                &mut bx,
                &self.storage.rows[phys],
                &self.storage.extras,
                0,
                self.styles(),
            );
            // Preserve the `None ⟺ empty` ring_extras encoding.
            self.storage
                .ring_extras
                .push_back(if bx.is_empty() { None } else { Some(bx) });
        } else {
            // Popped entry was None (plain row) — nothing to recycle.
            let extracted = Self::extract_row_extras(
                &self.storage.rows[phys],
                &self.storage.extras,
                0,
                self.styles(),
            );
            self.storage.push_ring_extras(extracted);
        }

        let evicted_page = self.storage.rows[oldest].page_id();
        self.storage.generations.evict_page(evicted_page);
        {
            let storage = &mut self.storage;
            let pages = &mut storage.pages;
            // SAFETY: The reused row remains stored in `storage.rows`, and
            // `storage.pages` continues to outlive that owner.
            unsafe { storage.rows[oldest].resize(cols, pages) };
            // One fused BCE fill: reused bottom rows inherit the current SGR
            // background (VT420/xterm).
            storage.rows[oldest].reset_with(fill);
        }
        self.storage.ring_head = (self.storage.ring_head + 1) % row_count;
    }

    /// General multi-row (or tiered-scrollback) reuse path.
    fn reuse_scrolled_rows_general(
        &mut self,
        rows_to_reuse: usize,
        cols: u16,
        row_count: usize,
        ring_sb: usize,
        has_scrollback: bool,
    ) {
        // The reused bottom rows inherit the current SGR background (BCE fill);
        // nothing below mutates it before use.
        let fill = self.storage.cursor_template;

        // Single-row fast path (every newline once the ring is full): extract
        // the entering row's extras directly. With `ring_head` unadvanced there
        // is no cross-iteration ordering to preserve, so the general path's
        // 1-element Vec is skipped. Same behaviour as the loop below with
        // `rows_to_reuse == 1`.
        if rows_to_reuse == 1 {
            let phys = (self.storage.ring_head + ring_sb) % row_count;
            let new_extras = Self::extract_row_extras(
                &self.storage.rows[phys],
                &self.storage.extras,
                row_u16(0),
                self.styles(),
            );
            self.reuse_one_scrolled_row(new_extras, cols, row_count, has_scrollback, fill);
            return;
        }

        // Multi-row: each row's `phys` is computed against the original
        // `ring_head`, which `reuse_one_scrolled_row` advances, so extraction
        // must finish before any reuse. Extras are extracted even without tiered
        // scrollback, so a later attach converts rows correctly (cheap when
        // CellExtras is empty).
        let new_scrollback_extras: Vec<_> = (0..rows_to_reuse)
            .map(|i| {
                let row_idx = row_u16(i);
                let phys = (self.storage.ring_head + ring_sb + i) % row_count;
                Self::extract_row_extras(
                    &self.storage.rows[phys],
                    &self.storage.extras,
                    row_idx,
                    self.styles(),
                )
            })
            .collect();

        for new_extras in new_scrollback_extras {
            self.reuse_one_scrolled_row(new_extras, cols, row_count, has_scrollback, fill);
        }
    }

    /// Recycle one oldest ring row after its entering-row extras were
    /// extracted: stage the evicted row to scrollback (lazy `DeferredLine`),
    /// evict its page, resize and BCE-reset it as the fresh bottom row, and
    /// advance `ring_head`. Shared by the general loop and the single-row fast
    /// path.
    fn reuse_one_scrolled_row(
        &mut self,
        new_extras: super::scroll_convert::ScrolledRowExtras,
        cols: u16,
        row_count: usize,
        has_scrollback: bool,
        fill: crate::Cell,
    ) {
        let oldest = self.storage.ring_head;
        self.storage.push_ring_extras(new_extras);

        // Keep the push-then-pop order: with a zero-sized ring (`ring_sb == 0`)
        // it hands this row's own freshly extracted extras straight to
        // `push_row_boxed`, and it keeps `ring_extras.len() ==
        // ring_buffer_scrollback()`. The popped `Option<Box<..>>` is passed
        // through whole, avoiding a re-box per scrolled line. Without scrollback
        // the box is dropped below.
        let extras = self.storage.ring_extras.pop_front().flatten();

        // Lazy scrollback promotion: snapshot the row as a DeferredLine (O(cells)
        // memcpy) instead of converting it; the line materializes on first read.
        //
        // `push_row` fills a recycled cell body from the lazy buffer's pool
        // rather than allocating: this runs once per newline inside the PTY
        // reader's `term_lock` hold, which the UI's keystroke echo waits on.
        if has_scrollback {
            self.storage
                .lazy_buffer
                .push_row_boxed(&self.storage.rows[oldest], extras);
        }

        let evicted_page = self.storage.rows[oldest].page_id();
        self.storage.generations.evict_page(evicted_page);
        {
            let storage = &mut self.storage;
            let pages = &mut storage.pages;
            // SAFETY: The reused row remains stored in `storage.rows`, and
            // `storage.pages` continues to outlive that owner.
            unsafe { storage.rows[oldest].resize(cols, pages) };
            // One fused BCE fill: reused bottom rows inherit the current SGR
            // background (VT420/xterm).
            storage.rows[oldest].reset_with(fill);
        }
        self.storage.ring_head = (self.storage.ring_head + 1) % row_count;
    }

    /// The staging cap of the detached-reflow window: past it the oldest
    /// staged window output is dropped. Generous: normal windows finish in
    /// well under a second; only heavy streaming through a long window hits it.
    const DETACHED_LAZY_CAP: usize = 50_000;

    /// Bound the lazy buffer while the tiered store is detached for an
    /// off-thread reflow. Normally [`drain_lazy_buffer`](Self::drain_lazy_buffer)
    /// moves staged lines into the store, but during the reflow window the store
    /// is gone and draining is suppressed, so heavy streaming would grow the
    /// buffer without bound. Drop the oldest staged window output beyond the
    /// cap. With the store in the job, it cannot be known which of those lines
    /// the scrollback limit would have kept, so every one is counted and marked
    /// as a cut.
    ///
    /// Logged per WINDOW, not per drop: once at the cap, a stream drops one
    /// line for every line it prints, on the PTY reader under the terminal
    /// lock, so a line per drop let one resize during a flood roll
    /// `aterm.log` over in under a second, audit lines and all. The window's
    /// first drop is written here and its total when it closes
    /// ([`close_reflow_window_cap_run`](Self::close_reflow_window_cap_run)).
    fn bound_detached_lazy_buffer(&mut self) {
        let len = self.storage.lazy_buffer.len();
        if len > Self::DETACHED_LAZY_CAP {
            let drop_n = len - Self::DETACHED_LAZY_CAP;
            // Real retention loss: counted, and marked in place by the buffer's
            // flood cut; the re-attach flush drains the marker ahead of the
            // survivors, so the history shows the hole where it is.
            self.storage.flood_truncated_lines += self.storage.lazy_buffer.drop_oldest(drop_n);
            let before = self.storage.reflow_window_cap_dropped;
            self.storage.reflow_window_cap_dropped =
                before.saturating_add(u64::try_from(drop_n).unwrap_or(u64::MAX));
            if before == 0 {
                aterm_log::warn!(
                    "reflow window: lazy buffer exceeded {} lines; dropping the oldest \
                     staged line(s) to bound memory until the window closes",
                    Self::DETACHED_LAZY_CAP
                );
            }
        }
    }

    /// Close the current detach window's cap run: write how many staged lines
    /// the cap dropped in it, if any, and zero the count for the next window.
    /// Called wherever a detach window ends (re-attach, abort) and opens.
    pub(crate) fn close_reflow_window_cap_run(&mut self) {
        let dropped = std::mem::take(&mut self.storage.reflow_window_cap_dropped);
        if dropped > 0 {
            aterm_log::warn!(
                "reflow window: closed after dropping {dropped} staged line(s) past its \
                 {}-line cap",
                Self::DETACHED_LAZY_CAP
            );
        }
    }

    /// Backpressure floor: the lazy backlog may always reach this many lines
    /// before the flood path drops, however little memory headroom the store
    /// reports. Well above the 1000-line drain threshold, so a keeping-up
    /// worker never trips it, and below the 50k detached-reflow cap. See
    /// [`flood_overflow`](Self::flood_overflow) for the cap itself.
    pub(crate) const ASYNC_COMPRESS_BACKPRESSURE: usize = 20_000;

    /// How many of the oldest staged rows the flood path must drop now, so the
    /// backlog fits both of its limits, never going below the floor
    /// ([`ASYNC_COMPRESS_BACKPRESSURE`](Self::ASYNC_COMPRESS_BACKPRESSURE)):
    /// * the store's line share (`line_limit`): rows past it are the configured
    ///   limit's to evict, which
    ///   [`drop_flood_overflow`](Self::drop_flood_overflow) does silently;
    /// * the store's unused memory budget: the staged rows, weighed as held
    ///   (the lazy buffer's running byte sum of occupied cells, not the window
    ///   width), may use no more than the budget has left.
    ///
    /// A fixed line cap bounds memory in the wrong unit and, under a flood the
    /// worker cannot keep up with, drops lines from the middle of a history the
    /// user configured to be much deeper. The memory headroom is the one
    /// statement of how much this scrollback may use (the engine's
    /// `memory_budget`), and the staged rows are headed for that same store.
    /// At the defaults (100k lines, 100 MB) it covers the whole configured
    /// limit for rows of up to ~110 occupied cells; wider floods are cut and
    /// marked. The cost is a peak: the worker's post-burst drain returns the
    /// bodies (the pool keeps at most 512 KiB) and releases the deque spine
    /// ([`LazyBuffer::release_spine_slack`](super::scroll_convert::LazyBuffer::release_spine_slack)).
    /// Materialized text of staged rows (built only on reads) is not weighed.
    ///
    /// Cost on the reader's per-newline path: O(1) within both limits; past
    /// the memory one, one step per row dropped.
    pub(crate) fn flood_overflow(&self) -> usize {
        let buffer = &self.storage.lazy_buffer;
        let floor = Self::ASYNC_COMPRESS_BACKPRESSURE;
        let staged = buffer.len();
        if staged <= floor {
            return 0;
        }
        let Some(store) = self.storage.scrollback.as_ref() else {
            return staged - floor;
        };
        let by_lines = store
            .line_limit()
            .map_or(0, |share| staged.saturating_sub(share.max(floor)));
        let headroom = store
            .memory_budget()
            .saturating_sub(store.budgeted_memory_used());
        by_lines.max(buffer.front_rows_over(headroom, floor))
    }

    /// Drop `over` of the oldest staged rows because the backlog is over its
    /// cap ([`flood_overflow`](Self::flood_overflow)), and account for them.
    ///
    /// Two kinds of drop:
    /// * Ordinary eviction: rows the configured line limit would not hold
    ///   anyway. Retention takes the oldest first, store before staged, so this
    ///   applies it now in that order (store rows via `truncate_oldest`, whole
    ///   blocks, no decompression; then staged rows). No hole opens; the top of
    ///   history moves silently. A flood into a configured limit ends holding
    ///   exactly the newest `limit` lines with no marker.
    /// * A flood cut: rows the limit would have kept, because the memory
    ///   headroom ran out first. Counted in `flood_truncated_lines` and marked
    ///   in place by the lazy buffer's cut, which readers materialize as one
    ///   dim `— aterm dropped N lines here …` row (the counter is fed from the
    ///   marker's own count).
    ///
    /// The split is decided against the history as it would stand had nothing
    /// been dropped, oldest first: stored rows, rows the open cut names, rows
    /// dropped now, remaining staged rows. The limit's share holds the newest
    /// `share` of that; everything older is ordinary. So the marker names
    /// exactly the lines inside the configured limit that are missing, and
    /// stops growing (or shrinks) as the window slides past the hole. The
    /// counter, which only grows, adds each cut's peak.
    ///
    /// While a cut is open every drop moves or grows the marker under a key a
    /// content row held (and a cut below stored rows renumbers them, since a
    /// retained row's key is `oldest + i`), so it bumps the renumber epoch like
    /// other total-moving mutations (see `history_renumber_epoch`). Ordinary
    /// eviction with no cut keeps every survivor's key and bumps nothing.
    fn drop_flood_overflow(&mut self, over: usize) {
        let staged = self.storage.lazy_buffer.staged_rows();
        let over = over.min(staged);
        if over == 0 {
            return;
        }
        let cut = usize::try_from(self.storage.lazy_buffer.cut_dropped()).unwrap_or(usize::MAX);
        let (stored, share) = self
            .storage
            .scrollback
            .as_ref()
            .map_or((0, None), |store| (store.line_count(), store.line_limit()));
        // Rows of that would-be history older than the window (none without a
        // line limit: then every drop is a loss).
        let beyond = share.map_or(0, |share| {
            stored
                .saturating_add(cut)
                .saturating_add(staged)
                .saturating_sub(share)
        });
        let mut from_store = beyond.min(stored);
        if from_store > 0 {
            if let Some(store) = self.storage.scrollback.as_mut()
                && let Err(error) = store.truncate_oldest(from_store)
            {
                // Nothing was evicted, so the stored rows still sit above the
                // staged ones: every drop below is then a hole, and is marked.
                aterm_log::warn!(
                    "flood path: evicting {from_store} stored line(s) failed: {error}"
                );
                from_store = 0;
            }
            self.clamp_display_offset();
        }
        // The cut's rows plus this drop's that are still inside the window.
        // The window's top can reach them only once no stored row survives.
        let in_window = if from_store == stored {
            cut.saturating_add(over).saturating_sub(beyond - from_store)
        } else {
            cut.saturating_add(over)
        };
        // 0: the rows that remain fill the window by themselves, so the whole
        // drop is the top of history moving and an open marker goes with it
        // (it is the oldest row, and there is no hole below the top to name).
        let hole = u64::try_from(in_window).unwrap_or(u64::MAX);
        self.storage.flood_truncated_lines += self.storage.lazy_buffer.shed_oldest(over, hole);
        if hole > 0 {
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
        }
    }

    /// Lines currently staged in the lazy buffer awaiting promotion into the
    /// tiered store (the off-thread compression worker's backlog).
    #[must_use]
    #[inline]
    pub fn lazy_backlog_len(&self) -> usize {
        self.storage.lazy_buffer.len()
    }

    /// One bounded drain opportunity for a host polling while output may still
    /// flood in (the GUI compression worker's once-a-second trickle, the wasm
    /// hosts' per-frame drain). It is
    /// [`drain_lazy_bounded`](Self::drain_lazy_bounded), except that it skips
    /// the batch (returning the unchanged backlog) while a flood cut is open
    /// and at least `max_lines` rows were staged since the previous
    /// opportunity.
    ///
    /// Skip then: a batch promotes the marker and the rows after it, and a
    /// stream delivering a batch or more per opportunity refills the backlog
    /// past its cap, so the next drop would open a new hole (one marker per
    /// batch instead of one per flood). Only then: a slower stream is one a
    /// batch outruns, so promoting it ends the loss, while skipping would keep
    /// dropping a slow tail. Arrivals, not drops, are the measure, so a cut
    /// opened just before the opportunity is judged by the stream's rate.
    pub fn trickle_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let arrived = self.storage.lazy_buffer.take_arrivals();
        if self.storage.lazy_buffer.cut_open()
            && arrived >= u64::try_from(max_lines).unwrap_or(u64::MAX)
        {
            return self.storage.lazy_buffer.len();
        }
        self.drain_lazy_bounded(max_lines)
    }

    /// Attach/detach the off-thread compression worker for this grid. While
    /// attached, the reader-thread ingest path defers lazy-buffer draining to the
    /// worker (which calls [`drain_lazy_bounded`](Self::drain_lazy_bounded) in
    /// bounded batches), keeping the LZ4/zstd promotion spike off the PTY-drain
    /// critical path. Idempotent; set once at session setup.
    pub fn set_compress_offload_active(&mut self, active: bool) {
        self.storage.compress_offload_active = active;
    }

    /// Drain up to `max_lines` of the oldest staged lines into the tiered
    /// store (LZ4/zstd promotion for just that batch), then enforce the budget.
    /// Returns the number of lines still staged, so the worker can loop. A
    /// no-op (returns 0) with no backlog or no store; while the store is
    /// detached for a reflow the staged lines are kept for the re-attach flush
    /// (like [`drain_lazy_buffer`](Self::drain_lazy_buffer)).
    ///
    /// The caller must hold the term lock, like every `&mut Grid` mutation;
    /// batches keep each hold short.
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        // A drain is an opportunity too: the next trickle measures the stream
        // from here (see `trickle_lazy_bounded`).
        self.storage.lazy_buffer.take_arrivals();
        if max_lines == 0 || self.storage.lazy_buffer.is_empty() {
            return self.storage.lazy_buffer.len();
        }
        // Store detached for an off-thread reflow: keep staged lines (flushed on
        // re-attach between reflowed history and the live ring).
        if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
            return self.storage.lazy_buffer.len();
        }
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
            // No scrollback attached — discard deferred lines (as drain does).
            self.storage.lazy_buffer.clear();
            return 0;
        };

        for line in self.storage.lazy_buffer.drain_front(max_lines) {
            if let Err(error) = scrollback.push_line(line) {
                aterm_log::warn!("scrollback push_line failed (bounded drain): {error}");
            }
        }
        self.storage.lazy_buffer.release_spine_slack();
        self.enforce_scrollback_budget_and_clamp();
        self.storage.lazy_buffer.len()
    }

    pub(crate) fn drain_lazy_buffer(&mut self) {
        if self.storage.lazy_buffer.is_empty() {
            return;
        }
        // The store is out for an off-thread reflow: keep the staged lines
        // (flushed on re-attach, between the reflowed history and the live
        // ring), or output produced during the window would be lost.
        if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
            return;
        }
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
            // No scrollback attached — discard deferred lines.
            self.storage.lazy_buffer.clear();
            return;
        };

        for line in self.storage.lazy_buffer.drain_all() {
            if let Err(error) = scrollback.push_line(line) {
                aterm_log::warn!("scrollback push_line failed: {error}");
            }
        }
        self.storage.lazy_buffer.release_spine_slack();

        self.enforce_scrollback_budget_and_clamp();
    }

    /// Epilogue for any bulk `push_line` sequence into tiered scrollback
    /// (lazy-buffer drain, scrollback-reflow restore): enforce the memory
    /// budget, then re-clamp the display offset.
    pub(crate) fn enforce_scrollback_budget_and_clamp(&mut self) {
        // push_line can evict (line limit or memory pressure), so a scrolled-back
        // display_offset may now exceed scrollback_lines(); clamp to keep the
        // DisplayOffsetValid invariant.
        self.clamp_display_offset();
    }

    fn finish_scroll_up(&mut self, n: usize) {
        let delta = i32::try_from(n).unwrap_or(i32::MAX);
        self.storage.content_scroll_delta = self.storage.content_scroll_delta.saturating_add(delta);
        self.mark_scroll_damage(n);
    }

    /// Scroll content down by n lines (new empty lines at top).
    ///
    /// Shifts all visible rows down by `n` — test convenience wrapper
    /// over [`scroll_region_down`]. Production code uses `scroll_region_down` directly.
    #[cfg(test)]
    pub(crate) fn scroll_down(&mut self, n: usize) {
        self.scroll_region_down(n);
    }

    /// Scroll within scroll region: move content up (blank line at bottom of region).
    ///
    /// This is used when cursor is at bottom of scroll region and line feed is issued.
    /// Only lines within the scroll region are affected.
    ///
    /// REQUIRES: self.storage.scroll_region.top <= self.storage.scroll_region.bottom
    /// REQUIRES: self.storage.scroll_region.bottom < self.storage.visible_rows
    pub fn scroll_region_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        // Full-screen scrolls, including 1-row terminals, enter scrollback.
        // Only non-full degenerate regions are no-ops.
        //
        // The fresh-session blank gate below does not apply to this full-screen
        // path: archiving every scrolled row is xterm parity, and the Tier-1
        // bindings (`conformance_retention.rs`, `conformance_offload.rs`) model
        // each full-screen scroll as one retained line
        // (`ring_extras_len_equals_ring_buffer_scrollback` pins the ring growth).
        // So a fresh session padding a blank screen with LFs mints blank
        // scrollback here, as xterm does; the top-anchored region path gets
        // the gate.
        //
        // The count is clamped to the screen, as xterm's `xtermScroll` and
        // Alacritty's `scroll_up_relative` clamp it to the region: a scroll of
        // a screenful or more leaves the screen blank and archives exactly that
        // screenful. Unclamped, `CSI 65535 S` (8 bytes, the parser's ceiling)
        // archived 65,535 rows, ~4 ms under the terminal lock each, so 16 KiB of
        // program output held the lock — and the window's redraw — for seconds
        // (hunt 7, 2026-10-09).
        if self
            .storage
            .scroll_region
            .is_full(self.storage.visible_rows)
        {
            self.reset_display_offset_with_damage();
            self.scroll_up(n.min(usize::from(self.storage.visible_rows)));
            return;
        }
        // Degenerate single-row region: nowhere to scroll to — no-op.
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        // Row index arithmetic requires display_offset == 0. line_feed() resets
        // it, but other callers (advance_autowrap_line, CSI S) may not.
        self.reset_display_offset_with_damage();

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);

        // Clamp before selecting the archival path: unlike a full-screen scroll,
        // a partial region cannot consume more rows than it contains.
        let region_size = bottom - top + 1;
        let n = n.min(region_size);

        // A full-width region rooted at row zero is a viewport with fixed rows
        // below it (e.g. Codex's inline-history renderer). Rows displaced through
        // the physical top still belong in primary-screen history.
        //
        // `max_scrollback == 0` with no tiered store is the alternate-screen
        // shape, where output stays ephemeral. A tiered grid may use a
        // zero-sized ring, so store presence also enables archival.
        //
        // The one exception is the fresh-session blank band (see
        // `fresh_session_blank_prefix`): while the grid holds no history, a
        // leading run of blank displaced rows carries no transcript, and
        // archiving it would create blank history a later rows-grow reveals as
        // a dead band. Blank rows after the first written row, or once any
        // history exists, are transcript and archive like any other row.
        let history_enabled = self.storage.max_scrollback > 0
            || self.storage.scrollback.is_some()
            || self.storage.scrollback_detached_for_reflow;
        if top == 0 && history_enabled {
            let blank_prefix = self.fresh_session_blank_prefix(n);
            if blank_prefix < n {
                // Drop the leading fresh-session blanks with a history-free
                // scroll, then archive from the first written row. Two
                // sequential scrolls compose to the same visible result as one
                // of `n`, while history gets only the transcript-bearing rows.
                if blank_prefix > 0 {
                    self.scroll_region_up_history_free(top, bottom, blank_prefix);
                }
                self.scroll_top_anchored_region_up_with_history(n - blank_prefix, bottom);
                return;
            }
            // blank_prefix == n: the whole displaced set is fresh-session
            // blank — fall through to the history-free scroll below.
        }

        // Scroll within an interior or history-free region only (no scrollback).
        self.scroll_region_up_history_free(top, bottom, n);
    }

    /// The non-archival region scroll: shift rows `[top..=bottom]` up by `n`
    /// within the viewport, BCE-fill the vacated bottom rows, and shift the
    /// region's `CellExtras`; history and `absolute_row_counter` are untouched.
    /// The interior-region / history-free / fresh-session-blank path; the
    /// archival top-anchored path is
    /// [`Self::scroll_top_anchored_region_up_with_history`].
    ///
    /// REQUIRES: 0 < n <= bottom - top + 1, bottom < visible_rows,
    /// display_offset == 0 (callers reset via
    /// `reset_display_offset_with_damage`).
    fn scroll_region_up_history_free(&mut self, top: usize, bottom: usize, n: usize) {
        debug_assert!(n > 0 && n <= bottom - top + 1);
        debug_assert!(bottom < usize::from(self.storage.visible_rows));

        // Shift rows up within the region using pre-computed physical indices.
        self.storage.shift_visible_rows_up(top, bottom, n);

        // Clear the bottom n rows of the region with BCE fill, resetting line
        // size to SingleWidth so DECDWL/DECDHL does not leak from recycled rows.
        let fill = self.storage.cursor_template;
        for row in (bottom + 1 - n)..=bottom {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }

        // Batch shift CellExtras within the region: O(E) regardless of n
        let top_u16 = row_u16(top);
        let bottom_u16 = row_u16(bottom);
        let shift_n = row_u16(n);
        self.storage
            .extras
            .shift_region_up_by(top_u16, bottom_u16, shift_n);
        // Fill BCE RGB in vacated bottom rows after shift.
        self.fill_bce_rgb_rows(row_u16(bottom + 1 - n)..bottom_u16.saturating_add(1));

        // Only the region's rows moved: record that band, and a selection is
        // cleared iff it overlaps it (one anchored in scrollback survives).
        // History and `absolute_row_counter` are untouched.
        //
        // The band record also carries the numbers: the region's rows went up
        // by `n` and its top `n` rows left the band (see `RowBandMove`). An `n`
        // past `i16` poisons the record rather than recording a wrong
        // displacement.
        self.record_row_band_move_or_poison(top_u16, bottom_u16, n, false);
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        // Mark only the scroll region rows as dirty, not the full screen.
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Record rows `top..=bottom` moving by `n` rows — DOWN the screen when `down`,
    /// UP otherwise — on this batch's band record, or poison the record when `n`
    /// does not fit an `i16` displacement (see
    /// `GridPresentationState::record_row_band_move`).
    fn record_row_band_move_or_poison(&mut self, top: u16, bottom: u16, n: usize, down: bool) {
        match i16::try_from(n) {
            Ok(rows) => {
                let delta = if down { rows } else { -rows };
                self.storage
                    .presentation
                    .record_row_band_move(top, bottom, delta);
            }
            Err(_) => self.storage.presentation.poison_row_band_moves(),
        }
    }

    /// Length of the leading run of blank rows among the `n` viewport rows a
    /// scroll would displace through the physical top (rows `0..n`), but only
    /// while the grid holds no history at all (`scrollback_lines() == 0`,
    /// covering ring, lazy staging and tiered store); otherwise 0.
    ///
    /// `Row::len == 0` means blank, not never-written: a row erased with the
    /// default background, or a separator a TUI prints as nothing, is also
    /// len 0. Blank lines between written lines are transcript, so blankness
    /// must never drop a row once anything has been archived. The safe case
    /// is a brand-new grid scrolling before anything was archived: its
    /// leading blanks separate nothing, and archiving them would create blank
    /// history that a later rows-grow reveals above the content.
    fn fresh_session_blank_prefix(&self, n: usize) -> usize {
        if self.storage.scrollback_lines() > 0 {
            return 0;
        }
        (0..n)
            .take_while(|&row| {
                u16::try_from(row)
                    .ok()
                    .and_then(|r| self.storage.row(r))
                    .is_some_and(Row::is_empty)
            })
            .count()
    }

    /// Archive a full-width, top-anchored partial region while preserving the
    /// fixed rows below its bottom margin.
    ///
    /// First perform the normal whole-grid archival scroll, which moves the
    /// displaced rows into the ring/tiered history with all of their metadata.
    /// That temporarily shifts the fixed footer too, so shift that suffix back
    /// down and clear the vacated rows at the bottom of the scrolling region.
    fn scroll_top_anchored_region_up_with_history(&mut self, n: usize, bottom: usize) {
        debug_assert!(n > 0);
        debug_assert!(bottom + 1 < usize::from(self.storage.visible_rows));

        // This scroll is a logical insertion immediately before the protected
        // footer: region rows retain their old absolute identities, while every
        // fixed footer row moves forward by `n` in the monotonic row space.
        let old_live_top = self
            .storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows));
        let footer_start = u64::try_from(bottom + 1).unwrap_or(u64::MAX);
        let insertion_at = old_live_top.saturating_add(footer_start);

        // Use the storage half of a whole-screen scroll. Its normal damage
        // epilogue would incorrectly mark the protected footer and increment
        // content_gen a second time after the partial-region mark below.
        self.scroll_up_storage(n);

        let visible_bottom = usize::from(self.storage.visible_rows) - 1;
        let vacated_top = bottom + 1 - n;
        self.shift_rows_down(vacated_top, visible_bottom, n);

        let fill = self.storage.cursor_template;
        for row in vacated_top..(vacated_top + n) {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }

        let shift_n = row_u16(n);
        self.storage.extras.shift_region_down_by(
            row_u16(vacated_top),
            row_u16(visible_bottom),
            shift_n,
        );
        self.fill_bce_rgb_rows(row_u16(vacated_top)..row_u16(vacated_top + n));

        self.storage
            .presentation
            .record_absolute_row_splice(insertion_at, u64::try_from(n).unwrap_or(u64::MAX));
        // On screen this is one band move: rows `0..=bottom` went up by `n` (the
        // top `n` entered history), the footer below `bottom` did not move. The
        // splice above carries the history insertion point for absolute-row
        // metadata; this carries the screen-row transform cursor-effects hosts
        // need.
        self.record_row_band_move_or_poison(0, row_u16(bottom), n, false);

        // The whole-grid archival step cannot be presented as a hardware scroll:
        // the footer was restored in place. Force ordinary content invalidation.
        // Selection remapping is piecewise for this path: content before the
        // footer moves toward history while footer content stays at its screen
        // row. `record_absolute_row_splice` retained an independent update for
        // Terminal post-processing, so do not install the generic region-scroll
        // clear sentinel here.
        self.storage
            .mark_content_rows(0, row_u16(bottom).saturating_add(1));
    }

    /// Shift rows down within a region (backwards to avoid overwriting).
    ///
    /// Copies `n` rows downward within `[top..=bottom]`. Does NOT clear vacated
    /// rows or shift extras — callers handle those steps.
    /// Uses pre-computed physical indices for sequential access.
    /// REQUIRES: display_offset == 0 (callers guarantee via reset_display_offset_with_damage).
    pub(super) fn shift_rows_down(&mut self, top: usize, bottom: usize, n: usize) {
        self.storage.shift_visible_rows_down(top, bottom, n);
    }

    /// Scroll within scroll region: move content down (blank line at top of region).
    ///
    /// This is used when cursor is at top of scroll region and reverse line feed is issued.
    /// Only lines within the scroll region are affected.
    ///
    /// REQUIRES: self.storage.scroll_region.top <= self.storage.scroll_region.bottom
    /// REQUIRES: self.storage.scroll_region.bottom < self.storage.visible_rows
    pub fn scroll_region_down(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        // Degenerate single-row region: nowhere to scroll to — no-op.
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        // Row index arithmetic requires display_offset == 0. Reset
        // defensively for callers that may not have done so.
        self.reset_display_offset_with_damage();

        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        let top = usize::from(top_u16);
        let bottom = usize::from(bottom_u16);
        let region_size = bottom - top + 1;
        let n = n.min(region_size);

        self.shift_rows_down(top, bottom, n);

        // Clear the top n rows of the region with BCE fill, resetting line size
        // to SingleWidth so DECDWL/DECDHL does not leak from recycled rows.
        let fill = self.storage.cursor_template;
        for row in top..(top + n) {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }

        // Batch shift CellExtras within the region: O(E) regardless of n
        let shift_n = row_u16(n);
        self.storage
            .extras
            .shift_region_down_by(top_u16, bottom_u16, shift_n);
        // Fill BCE RGB in vacated top rows after shift.
        self.fill_bce_rgb_rows(top_u16..row_u16(top + n));

        // The region's rows are the selection damage; a reverse region scroll
        // adds nothing to history, so the counter is untouched.
        //
        // The band record: the region's rows went down by `n`, its bottom `n`
        // rows left the band. Consecutive reverse indices at a region top
        // compose into one entry.
        self.record_row_band_move_or_poison(top_u16, bottom_u16, n, true);
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        // Mark only the scroll region rows as dirty, not the full screen.
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Rectangular scroll up within horizontal margins (DECLRMM + SU).
    ///
    /// When DECLRMM is active, SU only scrolls the cells within the horizontal
    /// margin region on each row, leaving cells outside the margins untouched.
    /// Blank cells fill the vacated positions at the bottom of the margin region.
    pub fn scroll_region_up_margined(&mut self, n: usize, left: u16, right: u16) {
        if n == 0 {
            return;
        }
        // Degenerate single-row region: nowhere to scroll to — no-op.
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        self.reset_display_offset_with_damage();

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);
        let region_size = bottom - top + 1;
        let n = n.min(region_size);
        let left_usize = usize::from(left);
        let right_usize = usize::from(right);
        let width = right_usize + 1 - left_usize;

        // Copy cells from row (src_row) to row (dst_row) within [left, right].
        // Process top-to-bottom so we don't overwrite source data.
        // Hoist buffer outside loop to avoid per-row heap allocation.
        let cols = self.storage.cols as usize;
        let mut buf = vec![super::Cell::EMPTY; width];
        for dst_offset in 0..(region_size - n) {
            let dst_row = row_u16(top + dst_offset);
            let src_row = row_u16(top + dst_offset + n);
            buf.fill(super::Cell::EMPTY);
            if let Some(src) = self.row(src_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = src.get(row_u16(col)) {
                        buf[i] = *c;
                    }
                }
            }
            if let Some(dst) = self.row_mut(dst_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = dst.get_mut(row_u16(col)) {
                        *c = buf[i];
                    }
                }
                // Wide char fixup at rectangle boundaries. Whole-row rect copy:
                // no per-cell authoritative signal, so keep the char==' ' spacer
                // heuristic (true).
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The get_mut writes bypass len maintenance: blank source cells
                // copied over the dst tail leave len stale-high (phantom trailing
                // spaces). Recompute per modified row (never over-shrinks).
                dst.recompute_len();
            }
        }
        // Clear the bottom n rows within margins with BCE fill.
        let fill = self.storage.cursor_template;
        for clear_offset in (region_size - n)..region_size {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                // Wide char fixup at rectangle boundaries.
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The BCE fill via get_mut bypasses len maintenance: an empty
                // cursor_template can orphan the tail, a colored one extends it.
                // Recompute the true extent.
                r.recompute_len();
            }
        }

        // Shift extras within the margin columns: rows [top+n..bottom] shift up
        // by n, rows [top..top+n) are dropped, keeping hyperlinks, RGB colors
        // and combining marks on shifted rows.
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .extras
            .shift_rect_up_by(top_u16, bottom_u16, left, right, row_u16(n));
        // Fill BCE RGB in vacated bottom-right rect after shift.
        self.fill_bce_rgb_rect(
            row_u16(top + region_size - n)..bottom_u16.saturating_add(1),
            left..right.saturating_add(1),
        );

        // A margined region scroll moves a rectangle, but the selection lattice
        // is row-granular, so the band is the region's rows (wider than the
        // rectangle, which is the safe direction).
        //
        // A rectangle is not a row translate (cells outside the margins
        // stayed), so the band record is poisoned and hosts see a plain
        // invalidation.
        self.storage.presentation.poison_row_band_moves();
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Rectangular scroll down within horizontal margins (DECLRMM + SD).
    ///
    /// When DECLRMM is active, SD only scrolls the cells within the horizontal
    /// margin region on each row, leaving cells outside the margins untouched.
    /// Blank cells fill the vacated positions at the top of the margin region.
    pub fn scroll_region_down_margined(&mut self, n: usize, left: u16, right: u16) {
        if n == 0 {
            return;
        }
        // Degenerate single-row region: nowhere to scroll to — no-op.
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        self.reset_display_offset_with_damage();

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);
        let region_size = bottom - top + 1;
        let n = n.min(region_size);
        let left_usize = usize::from(left);
        let right_usize = usize::from(right);
        let width = right_usize + 1 - left_usize;

        // Copy cells from row (src_row) to row (dst_row) within [left, right].
        // Process bottom-to-top so we don't overwrite source data.
        // Hoist buffer outside loop to avoid per-row heap allocation.
        let cols = self.storage.cols as usize;
        let mut buf = vec![super::Cell::EMPTY; width];
        for dst_offset in (n..region_size).rev() {
            let dst_row = row_u16(top + dst_offset);
            let src_row = row_u16(top + dst_offset - n);
            buf.fill(super::Cell::EMPTY);
            if let Some(src) = self.row(src_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = src.get(row_u16(col)) {
                        buf[i] = *c;
                    }
                }
            }
            if let Some(dst) = self.row_mut(dst_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = dst.get_mut(row_u16(col)) {
                        *c = buf[i];
                    }
                }
                // Wide char fixup at rectangle boundaries. Whole-row rect copy:
                // no per-cell authoritative signal, so keep the char==' ' spacer
                // heuristic (true).
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The get_mut writes bypass len maintenance: blank source cells
                // copied over the dst tail leave len stale-high (phantom trailing
                // spaces). Recompute per modified row (never over-shrinks).
                dst.recompute_len();
            }
        }
        // Clear the top n rows within margins with BCE fill.
        let fill = self.storage.cursor_template;
        for clear_offset in 0..n {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                // Wide char fixup at rectangle boundaries.
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The BCE fill via get_mut bypasses len maintenance: an empty
                // cursor_template can orphan the tail, a colored one extends it.
                // Recompute the true extent.
                r.recompute_len();
            }
        }

        // Shift extras within the margin columns: rows [top..bottom-n] shift
        // down by n, rows [bottom-n+1..bottom] are dropped.
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .extras
            .shift_rect_down_by(top_u16, bottom_u16, left, right, row_u16(n));
        // Fill BCE RGB in vacated top-left rect after shift.
        self.fill_bce_rgb_rect(top_u16..row_u16(top + n), left..right.saturating_add(1));

        // A margined region scroll moves a rectangle, but the selection lattice
        // is row-granular, so the band is the region's rows (wider than the
        // rectangle, which is the safe direction).
        //
        // A rectangle is not a row translate (cells outside the margins
        // stayed), so the band record is poisoned and hosts see a plain
        // invalidation.
        self.storage.presentation.poison_row_band_moves();
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }
}

/// The flood cut: a burst the compression worker cannot keep up with must
/// never leave a silent hole in history. Each test models the worst case (an
/// attached worker that never runs, or only trickles a batch now and then) and
/// reads the history back as `line`/`search` do, without draining first.
#[cfg(test)]
mod flood_cut_tests {
    use super::super::Grid;
    use super::super::scroll_convert::FLOOD_MARKER_PREFIX;
    use super::super::test_support::{feed_numbered_lines, history_trimmed};
    use crate::CellFlags;
    use aterm_scrollback::Scrollback;

    /// The engine's default scrollback memory budget (`TerminalConfig`,
    /// `Scrollback::with_defaults`): what the GUI's sessions run with.
    const DEFAULT_BUDGET: usize = 100 * 1024 * 1024;

    /// A budget whose headroom holds fewer staged `L<n>` rows than the 20k
    /// floor, so the staging cap IS the floor and a test-sized flood reaches
    /// the drop path — the memory-bound case, where a cut is a loss (each test
    /// checks it with [`assert_floor_is_the_cap`]).
    const FLOOR_BUDGET: usize = 2 * 1024 * 1024;

    /// The worker's batch (`COMPRESS_BUDGET` in the GUI's compress worker).
    const BATCH: usize = 256;

    /// A grid whose compression worker is attached but never runs on its own
    /// (the starved-worker flood), over a store with this memory budget and
    /// the store's own default 100k line limit.
    fn flooded_grid(cols: u16, ring: usize, budget: usize) -> Grid {
        let mut grid =
            Grid::with_tiered_scrollback(3, cols, ring, Scrollback::new(100, 1000, budget));
        grid.set_compress_offload_active(true);
        grid
    }

    fn marker_count(row: &str) -> Option<u64> {
        row.strip_prefix(FLOOD_MARKER_PREFIX)?
            .split(' ')
            .next()?
            .parse()
            .ok()
    }

    /// The number of a content row `L<n>` (a padded wide row `L<n>-----`
    /// included).
    fn line_number(row: &str) -> usize {
        row.strip_prefix('L')
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("expected a content row `L<n>`, got {row:?}"))
    }

    /// The contract: history is contiguous except where a marker row stands,
    /// and each marker's count equals the numbering gap across it. `origin` is
    /// where history would start had nothing been lost (the top of the
    /// limit's window over everything fed, `0` while fewer lines than the
    /// limit were fed), so a marker at the top names only lines the limit
    /// would have kept. Returns every marker as `(row index, count)`.
    fn assert_no_silent_hole(hist: &[String], origin: usize) -> Vec<(usize, u64)> {
        let mut markers = Vec::new();
        let mut expect_next: Option<usize> = None;
        for (idx, row) in hist.iter().enumerate() {
            if let Some(count) = marker_count(row) {
                let after =
                    expect_next.unwrap_or(origin) + usize::try_from(count).expect("count fits");
                markers.push((idx, count));
                expect_next = Some(after);
                continue;
            }
            let n = line_number(row);
            if let Some(expected) = expect_next {
                assert_eq!(
                    n,
                    expected,
                    "silent hole: row {idx} is L{n} but L{expected} should follow \
                     (history {hist_len} rows, markers so far {markers:?})",
                    hist_len = hist.len()
                );
            }
            expect_next = Some(n + 1);
        }
        markers
    }

    /// The newest `limit` lines of `total` fed, as history rows: what a
    /// scrollback with that limit holds once `total` lines have scrolled
    /// through a 3-row screen whose last row is the blank after the final LF.
    fn newest(total: usize, limit: usize) -> Vec<String> {
        let first_on_screen = total - 2;
        (first_on_screen - limit..first_on_screen)
            .map(|n| format!("L{n}"))
            .collect()
    }

    /// The line a history of `limit` rows starts at once `total` lines have
    /// been fed — the `origin` of [`assert_no_silent_hole`].
    fn window_top(total: usize, limit: usize) -> usize {
        (total - 2).saturating_sub(limit)
    }

    /// A LOWER bound on what one staged `L<n>` row weighs (`feed`'s rows run
    /// 2..=7 cells; a recycled body can be larger, never smaller): what a
    /// test budget is sized against, erring toward MORE rows fitting.
    fn staged_row_bytes() -> usize {
        std::mem::size_of::<super::super::scroll_convert::DeferredLine>()
            + 7 * std::mem::size_of::<crate::Cell>()
    }

    /// Precondition for the memory-bound tests: the store's headroom holds
    /// fewer staged `L<n>` rows than the floor, so the FLOOR is the cap and a
    /// test-sized flood reaches the drop path well inside the 100k limit.
    fn assert_floor_is_the_cap(grid: &Grid) {
        let store = grid.scrollback().expect("a tiered store");
        let headroom = store.memory_budget() - store.budgeted_memory_used();
        assert!(
            headroom / staged_row_bytes() < Grid::ASYNC_COMPRESS_BACKPRESSURE,
            "the budget must hold fewer staged rows than the floor"
        );
    }

    #[test]
    fn a_120k_burst_into_a_100k_scrollback_keeps_the_newest_100k_with_no_hole() {
        // Default budget: 80 columns, a 100k limit, the worker never running.
        // The headroom holds the whole share raw, so every drop is the limit's
        // own eviction: exactly the newest 100k, nothing marked or counted.
        let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(100_000));
        feed_numbered_lines(&mut grid, 0, 120_000);
        assert_eq!(
            grid.lazy_backlog_len(),
            100_000 - 8,
            "the backlog is held at the store's share"
        );

        let hist = history_trimmed(&grid);
        assert!(
            assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
            "no marker"
        );
        assert_eq!(
            hist,
            newest(120_000, 100_000),
            "the newest 100k, contiguous"
        );
        assert_eq!(
            grid.truncated_lines(),
            0,
            "the limit's eviction is not a loss"
        );

        // The worker catches up: the drained history is the same 100k.
        assert!(grid.scrollback_mut().is_some());
        assert_eq!(history_trimmed(&grid), newest(120_000, 100_000));
        assert_eq!(grid.truncated_lines(), 0);
        grid.assert_invariants();
    }

    #[test]
    fn a_short_line_flood_is_weighed_by_its_cells_not_the_window_width() {
        // The same burst at 200 columns: rows are weighed by what they hold,
        // not the window width, so the backlog still fits the whole share.
        let mut grid = flooded_grid(200, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(100_000));
        feed_numbered_lines(&mut grid, 0, 120_000);

        let hist = history_trimmed(&grid);
        assert!(
            assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
            "no marker"
        );
        assert_eq!(
            hist,
            newest(120_000, 100_000),
            "the newest 100k, contiguous"
        );
        assert_eq!(grid.truncated_lines(), 0, "nothing lost");
        let store = grid.scrollback().expect("a tiered store");
        assert!(
            grid.storage.lazy_buffer.staged_bytes()
                <= store.memory_budget() - store.budgeted_memory_used(),
            "and the staged rows still fit the memory headroom"
        );
        grid.assert_invariants();
    }

    /// Write `n` numbered FULL-WIDTH rows `L{start}----…` (every cell
    /// occupied), CR + text + LF each.
    fn feed_wide(grid: &mut Grid, start: usize, n: usize, cols: u16) {
        for i in start..start + n {
            grid.carriage_return();
            let text = format!("L{i}");
            for c in text.chars() {
                grid.write_char(c);
            }
            for _ in text.len()..usize::from(cols) {
                grid.write_char('-');
            }
            grid.line_feed();
        }
    }

    #[test]
    fn a_full_width_flood_is_cut_at_the_memory_headroom_and_marked_exactly() {
        // Full-width rows weigh what they hold: a budget holding ~25k of them
        // (between the 20k floor and the 100k share) caps the backlog by bytes,
        // and what does not fit is a marked, counted cut. History already in
        // the store spends part of the budget first.
        let per_row = std::mem::size_of::<super::super::scroll_convert::DeferredLine>()
            + 80 * std::mem::size_of::<crate::Cell>();
        let mut grid = flooded_grid(80, 8, 25_000 * per_row);
        feed_wide(&mut grid, 0, 5_000, 80);
        assert!(grid.scrollback_mut().is_some(), "drain it into the store");
        let stored = grid.tiered_scrollback_lines();
        assert!(
            grid.scrollback()
                .is_some_and(|store| store.budgeted_memory_used() > 0),
            "precondition: the store holds part of the budget"
        );
        feed_wide(&mut grid, 5_000, 40_000, 80);

        let store = grid.scrollback().expect("a tiered store");
        let headroom = store.memory_budget() - store.budgeted_memory_used();
        assert!(
            grid.storage.lazy_buffer.staged_bytes() <= headroom,
            "the staged rows fit the headroom"
        );
        let staged = grid.lazy_backlog_len();
        assert!(
            staged > Grid::ASYNC_COMPRESS_BACKPRESSURE && staged < 30_000,
            "the headroom, not the floor or the share, set the cap ({staged} staged)"
        );
        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, 0);
        let [(idx, count)] = markers[..] else {
            panic!("exactly one cut expected: {markers:?}");
        };
        assert_eq!(idx, stored, "the cut sits right below the stored history");
        assert_eq!(count, grid.truncated_lines(), "marker and counter agree");
        grid.assert_invariants();
    }

    #[test]
    fn a_trickling_worker_is_evicted_ahead_of_the_flood_not_cut_around() {
        // The worker promotes one 256-line batch now and then. Those promoted
        // rows are the oldest, so the limit evicts them first, ahead of the
        // staged rows, never leaving them stranded above a hole.
        let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(100_000));
        let epoch = grid.history_renumber_epoch();
        let mut fed = 0;
        while fed < 120_000 {
            feed_numbered_lines(&mut grid, fed, 10_000);
            fed += 10_000;
            grid.drain_lazy_bounded(BATCH);
        }
        assert!(
            grid.tiered_scrollback_lines() > grid.lazy_backlog_len(),
            "precondition: the trickle left rows in the store"
        );

        let hist = history_trimmed(&grid);
        assert!(
            assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
            "no marker"
        );
        assert_eq!(
            hist,
            newest(120_000, 100_000),
            "the newest 100k, contiguous"
        );
        assert_eq!(
            grid.truncated_lines(),
            0,
            "the limit's eviction is not a loss"
        );
        assert_eq!(
            grid.history_renumber_epoch(),
            epoch,
            "evicting from the top keeps every survivor's absolute key"
        );
        grid.assert_invariants();
    }

    #[test]
    fn a_cut_beneath_promoted_lines_sits_exactly_where_the_hole_is() {
        // The memory-bound case: the floor is the cap, far below the store's
        // 100k share, so the dropped rows are ones the limit would have kept.
        // One worker batch promotes L0..L255 before the flood outruns it.
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        feed_numbered_lines(&mut grid, 0, 1_500);
        assert!(
            grid.drain_lazy_bounded(BATCH) > 0,
            "a backlog remains after one batch"
        );
        assert_eq!(
            grid.tiered_scrollback_lines() - grid.lazy_backlog_len(),
            BATCH,
            "the store holds exactly the one promoted batch"
        );
        let epoch = grid.history_renumber_epoch();
        feed_numbered_lines(&mut grid, 1_500, 23_500);

        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, 0);
        let [(idx, count)] = markers[..] else {
            panic!("exactly one cut expected, got {markers:?}");
        };
        assert_eq!(
            idx, BATCH,
            "the marker stands right after the promoted prefix"
        );
        assert_eq!(hist[BATCH - 1], format!("L{}", BATCH - 1));
        assert_eq!(count, grid.truncated_lines(), "marker and counter agree");
        assert_eq!(
            line_number(&hist[BATCH + 1]),
            BATCH + usize::try_from(count).unwrap()
        );
        assert!(count > 0, "the flood must have cut something");
        assert_eq!(
            hist.last().map(String::as_str),
            newest(25_000, 1).last().map(String::as_str),
            "the history still ends at the newest line"
        );
        // Rendered as system chrome, not program output: DIM.
        let marker = grid.get_history_line(BATCH).expect("marker row");
        assert!(
            marker.get_attr(0).flags & CellFlags::DIM.bits() != 0,
            "the marker row is dim"
        );
        // Dropping beneath stored lines moved their absolute keys.
        assert!(
            grid.history_renumber_epoch() > epoch,
            "renumber epoch advanced"
        );
        assert!(grid.lazy_backlog_len() <= Grid::ASYNC_COMPRESS_BACKPRESSURE);
        grid.assert_invariants();
    }

    #[test]
    fn a_cut_deeper_than_the_limit_names_only_what_the_limit_would_keep() {
        // Memory-bound (the floor is the cap), flooding well past a 30k limit,
        // with one batch promoted before the worker falls behind. The limit
        // keeps the newest 30k lines, so the promoted batch and the oldest
        // dropped lines are ordinary eviction: the batch leaves the store, and
        // the marker (now the top) names only the missing lines inside the
        // limit.
        const LIMIT: usize = 30_000;
        const TOTAL: usize = 60_000;
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        grid.set_scrollback_line_limit(Some(LIMIT));
        feed_numbered_lines(&mut grid, 0, 1_500);
        grid.drain_lazy_bounded(BATCH);
        feed_numbered_lines(&mut grid, 1_500, TOTAL - 1_500);

        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, window_top(TOTAL, LIMIT));
        let [(0, count)] = markers[..] else {
            panic!("exactly one cut, at the top, expected: {markers:?}");
        };
        assert_eq!(
            usize::try_from(count).unwrap() + hist.len() - 1,
            LIMIT,
            "the marker plus the survivors are exactly the limit's window"
        );
        assert_eq!(
            hist.last().map(String::as_str),
            newest(TOTAL, 1).last().map(String::as_str),
            "the history still ends at the newest line"
        );
        assert_eq!(
            grid.tiered_scrollback_lines(),
            grid.lazy_backlog_len(),
            "the promoted batch was older than the window, and was evicted"
        );
        assert_eq!(count, grid.truncated_lines(), "marker and counter agree");
        grid.assert_invariants();
    }

    #[test]
    fn a_marker_shrinks_as_the_window_slides_past_its_hole() {
        // A cut's oldest lines stop being ones the limit would keep once the
        // rows below the marker fill that much more of the window. Opened at
        // the top (10 lines lost) under a limit the backlog then grows to,
        // the marker must not keep naming lines the limit has moved past.
        let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(21_000));
        feed_numbered_lines(&mut grid, 0, 1_500);
        let counted = grid.storage.lazy_buffer.drop_oldest(10);
        assert_eq!(counted, 10);
        grid.storage.flood_truncated_lines += counted;
        feed_numbered_lines(&mut grid, 1_500, 25_000);

        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, window_top(26_500, 21_000));
        assert_eq!(
            markers,
            [(0, 1)],
            "one line of the window is missing: the one the marker stands in"
        );
        assert_eq!(hist.len(), 21_000, "the limit's window, marker included");
        assert_eq!(
            grid.truncated_lines(),
            10,
            "the counter keeps the loss the cut named at its deepest"
        );
        grid.assert_invariants();
    }

    #[test]
    fn a_drain_promotes_the_marker_first_and_a_later_drop_opens_a_new_cut() {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        feed_numbered_lines(&mut grid, 0, 22_000);
        let first_cut = grid.truncated_lines();
        assert!(first_cut > 0, "precondition: the flood cut");

        // The worker catches up on a batch: the marker leads it into the store.
        grid.drain_lazy_bounded(300);
        assert_eq!(
            marker_count(&history_trimmed(&grid)[0]),
            Some(first_cut),
            "the store's oldest row is the marker for the first cut"
        );
        assert_eq!(
            grid.tiered_scrollback_lines() - grid.lazy_backlog_len(),
            300
        );

        // The flood resumes: a NEW cut opens behind the promoted lines, and the
        // two markers together are the whole loss.
        feed_numbered_lines(&mut grid, 22_000, 2_000);
        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, 0);
        assert_eq!(markers.len(), 2, "two cuts: {markers:?}");
        assert_eq!(markers[0], (0, first_cut));
        assert_eq!(
            markers[1].0, 300,
            "the second cut follows the promoted batch"
        );
        assert_eq!(
            markers.iter().map(|(_, c)| c).sum::<u64>(),
            grid.truncated_lines(),
            "the markers sum to the counter"
        );
        grid.assert_invariants();
    }

    #[test]
    fn ordinary_top_eviction_is_neither_counted_nor_marked() {
        // A flood into a SMALL configured limit, the floor (20k) far above the
        // store's share: every drop is a row the limit was always going to
        // trim — including the batch the worker promoted mid-flood, which is
        // evicted from the store's front rather than left above a hole.
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        grid.set_scrollback_line_limit(Some(5_000));
        feed_numbered_lines(&mut grid, 0, 3_000);
        grid.drain_lazy_bounded(BATCH);
        feed_numbered_lines(&mut grid, 3_000, 18_000);
        assert_eq!(
            grid.lazy_backlog_len(),
            Grid::ASYNC_COMPRESS_BACKPRESSURE,
            "the floor beats a tiny share: the backlog is held at the floor"
        );
        let hist = history_trimmed(&grid);
        assert!(
            assert_no_silent_hole(&hist, window_top(21_000, 5_000)).is_empty(),
            "no marker for ordinary eviction"
        );
        assert_eq!(grid.truncated_lines(), 0, "ordinary eviction is not a loss");

        // Nor after the drain trims to the configured total.
        assert!(grid.scrollback_mut().is_some());
        assert_eq!(
            history_trimmed(&grid),
            newest(21_000, 5_000),
            "ring share + store share"
        );
        assert_eq!(grid.truncated_lines(), 0);
        grid.assert_invariants();
    }

    #[test]
    fn no_marker_when_nothing_was_dropped() {
        let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
        feed_numbered_lines(&mut grid, 0, 30_000);
        let hist = history_trimmed(&grid);
        assert!(assert_no_silent_hole(&hist, 0).is_empty());
        assert_eq!(hist, newest(30_000, 30_000 - 2), "every line from L0 on");
        assert_eq!(grid.truncated_lines(), 0);
    }

    /// Flood `total` lines in 1000-line chunks with one worker opportunity per
    /// chunk — `guarded`: the GUI worker's `trickle_lazy_bounded`; otherwise
    /// an unconditional batch — then the post-flood drain; returns every
    /// marker left.
    fn trickled_flood_markers(total: usize, guarded: bool) -> Vec<(usize, u64)> {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        let mut fed = 0;
        while fed < total {
            feed_numbered_lines(&mut grid, fed, 1_000);
            fed += 1_000;
            if guarded {
                grid.trickle_lazy_bounded(BATCH);
            } else {
                grid.drain_lazy_bounded(BATCH);
            }
        }
        while grid.drain_lazy_bounded(BATCH) > 0 {}
        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, 0);
        assert_eq!(
            markers.iter().map(|(_, count)| count).sum::<u64>(),
            grid.truncated_lines(),
            "the markers sum to the counter"
        );
        grid.assert_invariants();
        markers
    }

    #[test]
    fn a_trickle_that_skips_an_open_cut_leaves_one_marker_per_flood() {
        let markers = trickled_flood_markers(40_000, true);
        assert_eq!(markers.len(), 1, "one flood, one cut: {markers:?}");
        // Negative control: the unguarded trickle promotes the marker and the
        // rows after it, and every later drop opens a new cut behind them.
        let fragmented = trickled_flood_markers(40_000, false);
        assert!(
            fragmented.len() > 1,
            "the unguarded trickle fragments the cut: {fragmented:?}"
        );
    }

    /// A memory-bound flood (40k lines, a trickle opportunity every 1000), then
    /// a slow tail: 20 opportunities with `per_tick` lines before each, with no
    /// quiet window for the worker. `skip_while_cut` uses the naive rule (skip
    /// whenever a cut is open). Returns the lines lost during the tail (after
    /// its first opportunity) and the markers left.
    fn slow_tail(per_tick: usize, skip_while_cut: bool) -> (u64, Vec<(usize, u64)>) {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        let opportunity = |grid: &mut Grid| {
            if skip_while_cut {
                if !grid.storage.lazy_buffer.cut_open() {
                    grid.drain_lazy_bounded(BATCH);
                }
            } else {
                grid.trickle_lazy_bounded(BATCH);
            }
        };
        let mut fed = 0;
        while fed < 40_000 {
            feed_numbered_lines(&mut grid, fed, 1_000);
            fed += 1_000;
            opportunity(&mut grid);
        }
        assert!(
            grid.storage.lazy_buffer.cut_open(),
            "precondition: the flood is cutting"
        );
        feed_numbered_lines(&mut grid, fed, per_tick);
        fed += per_tick;
        opportunity(&mut grid);
        let before = grid.truncated_lines();
        for _ in 1..20 {
            feed_numbered_lines(&mut grid, fed, per_tick);
            fed += per_tick;
            opportunity(&mut grid);
        }
        let lost = grid.truncated_lines() - before;
        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, 0);
        assert_eq!(
            hist.last().map(String::as_str),
            newest(fed, 1).last().map(String::as_str),
            "the history ends at the newest line"
        );
        grid.assert_invariants();
        (lost, markers)
    }

    #[test]
    fn a_slow_tail_after_a_flood_is_promoted_not_dropped() {
        // A skip-while-cut rule would lose a 50 line/s tail for as long as it
        // lasts (the backlog sits at its cap). Judged by the stream's rate, the
        // first slow opportunity promotes the marker and a batch, the backlog
        // falls under the cap, and nothing more is lost.
        let (lost, markers) = slow_tail(50, false);
        assert_eq!(lost, 0, "the tail loses nothing once a batch outruns it");
        assert_eq!(markers.len(), 1, "one flood, one cut: {markers:?}");
        // Negative control: the rule it replaces keeps dropping the tail.
        let (lost, _) = slow_tail(50, true);
        assert_eq!(lost, 19 * 50, "skipping while cut drops every tail line");
    }

    #[test]
    fn a_window_that_slides_past_the_whole_hole_takes_the_marker() {
        // A tiny share under the 20k floor: the staged rows alone fill the
        // window, so a cut opened at the top (10 lines lost) has nothing left
        // inside the limit to name — the marker goes with the top of history,
        // as the store's own trim evicts a promoted one, and the counter keeps
        // the loss it named.
        let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(15_000));
        feed_numbered_lines(&mut grid, 0, 1_500);
        let counted = grid.storage.lazy_buffer.drop_oldest(10);
        grid.storage.flood_truncated_lines += counted;
        assert_eq!(marker_count(&history_trimmed(&grid)[0]), Some(10));
        assert_eq!(history_trimmed(&grid)[1], "L10");
        feed_numbered_lines(&mut grid, 1_500, 25_000);
        let hist = history_trimmed(&grid);
        assert!(
            assert_no_silent_hole(&hist, window_top(26_500, 15_000)).is_empty(),
            "the marker went with the top"
        );
        assert_eq!(grid.truncated_lines(), 10);
        grid.assert_invariants();
    }

    #[test]
    fn the_marker_states_the_count_first_and_fits_the_default_width() {
        use super::super::scroll_convert::flood_marker_line;
        // Seven digits is ~10M lines lost at one cut, far past any one flood.
        let line = flood_marker_line(9_999_999);
        let text = line.to_string();
        let width = text.chars().count();
        assert_eq!(marker_count(&text), Some(9_999_999), "{text:?}");
        assert!(width <= 80, "{text:?} is {width} columns");
        assert!(
            (0..width).all(|i| line.get_attr(i).flags & CellFlags::DIM.bits() != 0),
            "the whole row is dim"
        );
        assert!(
            flood_marker_line(1)
                .to_string()
                .starts_with("— aterm dropped 1 line here"),
            "a single line is not `1 lines`"
        );
    }

    #[test]
    fn with_no_line_limit_every_drop_is_a_loss() {
        // No configured limit means no window to evict ahead of: memory is the
        // only bound, and every row it cannot hold is counted and marked.
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        grid.set_scrollback_line_limit(None);
        feed_numbered_lines(&mut grid, 0, 25_000);
        assert_eq!(grid.lazy_backlog_len(), Grid::ASYNC_COMPRESS_BACKPRESSURE);
        let hist = history_trimmed(&grid);
        let markers = assert_no_silent_hole(&hist, 0);
        let [(0, count)] = markers[..] else {
            panic!("exactly one cut, at the top, expected: {markers:?}");
        };
        assert_eq!(
            usize::try_from(count).unwrap(),
            25_000 - 2 - (hist.len() - 1),
            "every line not retained is named"
        );
        assert_eq!(count, grid.truncated_lines());
        grid.assert_invariants();
    }

    #[test]
    fn a_drained_flood_backlog_gives_its_spine_back() {
        // A deep backlog grows the deque; the worker's post-burst drain must
        // hand that spine back, not keep the flood's peak for the session.
        let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
        feed_numbered_lines(&mut grid, 0, 40_000);
        assert!(grid.storage.lazy_buffer.spine_capacity() >= 32_768);
        while grid.drain_lazy_bounded(BATCH) > BATCH {}
        assert!(
            grid.storage.lazy_buffer.spine_capacity() <= 4_000,
            "spine kept {} rows of capacity for a {}-row backlog",
            grid.storage.lazy_buffer.spine_capacity(),
            grid.lazy_backlog_len()
        );
        grid.assert_invariants();
    }
}
