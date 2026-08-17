// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid scroll operations.
//!
//! This module handles all scrolling operations for the terminal grid:
//! - Display scrolling (viewing history)
//! - Content scrolling (scroll_up, scroll_down)
//! - Region scrolling (within DECSTBM scroll margins)
//!
//! Content-modifying scroll operations PRESERVE the `pending_wrap` flag.
//! This matches xterm: `xtermScroll` explicitly saves and restores
//! `screen->do_wrap` around the scroll and `RevScroll` never touches it
//! (util.c), so a scrolling LF/RI/SU/SD leaves a deferred wrap pending.
//! Only cursor-motion ops (CursorUp/Down/Back/Forward/Set, CR) and the
//! editing ops with an explicit xterm `ResetWrap` (ICH/DCH/ECH/IL/DL and
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
    /// Marks only the newly-exposed bottom rows instead of `mark_full()`.
    /// Follows `scroll_display`'s down-scroll pattern: bottom N rows are new
    /// content, upper rows shift up via GPU vertex-shift.
    ///
    /// Used by `scroll_to_bottom` and defensive offset resets in operations
    /// that require `display_offset == 0` for row arithmetic.
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
    /// This is the CONTENT-bearing `scroll_up` path (rows move into scrollback,
    /// `absolute_row_counter += rows`), so it bumps `content_gen` via the fused
    /// `mark_content_scroll` wrapper. Pure VIEWPORT scrolls (`scroll_display` /
    /// `apply_display_offset_damage`) deliberately do NOT route here and leave
    /// `content_gen` unchanged.
    fn mark_scroll_damage(&mut self, n: usize) {
        let visible_rows = self.storage.visible_rows;
        self.storage.mark_content_scroll(visible_rows, n);
    }
    /// Scroll the display by delta lines.
    ///
    /// Positive delta = scroll up (show older content).
    /// Negative delta = scroll down (show newer content).
    ///
    /// ENSURES: self.storage.display_offset <= self.storage.scrollback_lines()
    pub fn scroll_display(&mut self, delta: i32) {
        let max_offset = self.storage.scrollback_lines();
        let old_offset = self.storage.display_offset;
        // display_offset is bounded by max scrollback (MAX_SCROLLBACK_LINES = 1M)
        // which fits in i32. Use saturating conversion for safety.
        let current: i32 = self.storage.display_offset.try_into().unwrap_or(i32::MAX);
        let clamped = current.saturating_add(delta).max(0);
        // max(0) ensures non-negative; try_from is lossless for non-negative i32→usize
        let new_offset = usize::try_from(clamped).unwrap_or(0);
        self.storage.display_offset = new_offset.min(max_offset);

        let dmg =
            compute_display_offset_damage(old_offset, self.storage.display_offset, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Re-pin the viewport after a batch of output (SCR-1).
    ///
    /// `prev_offset` is the user's display_offset before processing reset it to
    /// 0; `lines_added` is the number of lines that entered scrollback during
    /// processing (the rise in `absolute_row_counter`). To keep the same content
    /// in view, the new offset is `prev_offset + lines_added`, clamped to
    /// `scrollback_lines()` so `display_offset <= scrollback_lines()` holds even
    /// when eviction discarded some of those lines.
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
    /// ENSURES: self.storage.display_offset == self.storage.scrollback_lines()
    pub fn scroll_to_top(&mut self) {
        let target = self.storage.scrollback_lines();
        let old_offset = self.storage.display_offset;
        self.storage.display_offset = target;

        let dmg = compute_display_offset_damage(old_offset, target, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
        debug_assert_eq!(self.storage.display_offset, self.storage.scrollback_lines());
    }

    /// Scroll to live position (bottom).
    ///
    /// Uses targeted row-level damage instead of `mark_full()`:
    /// only the newly-exposed bottom rows are marked dirty.
    ///
    /// ENSURES: self.storage.display_offset == 0
    #[inline]
    pub fn scroll_to_bottom(&mut self) {
        self.reset_display_offset_with_damage();
        debug_assert_eq!(self.storage.display_offset, 0);
    }

    /// Scroll the viewport so `target_abs_row` (an ABSOLUTE row number — e.g. a
    /// command mark's `prompt_start_row`) sits at the TOP visible line, clamped
    /// to the valid history range. This is the primitive behind prompt-to-prompt
    /// navigation. A target at or below the live top clamps to the live bottom
    /// (offset 0); a target older than the oldest retained line clamps to the top
    /// of scrollback. Same targeted display-offset damage as
    /// [`scroll_display`](Self::scroll_display); a no-op (no damage) when the
    /// resolved offset is unchanged.
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
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Clamp display_offset to valid bounds.
    ///
    /// Call this after operations that may reduce scrollback size
    /// (e.g., truncation) to maintain the DisplayOffsetValid invariant.
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
    /// ## Complexity
    ///
    /// O(n × cols) where n is the number of lines scrolled and cols is the
    /// grid column count. Each scrolled line requires:
    /// - O(cols) to convert row to scrollback line via `row_to_line_with_stored_extras`
    /// - O(cols) to clear and resize the reused row
    ///
    /// Verified by performance tests: `scroll_up_linear_time`, `scroll_up_handles_many_rows`
    ///
    /// ## Optimization
    ///
    /// This function is optimized for batch operations:
    /// - Pre-calculates how many rows to add vs reuse
    /// - Batch reserves Vec capacity for growth phase
    /// - Updates counters in bulk to reduce loop overhead
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    /// ENSURES: self.storage.rows.len() <= (self.storage.visible_rows as usize) + self.storage.max_scrollback
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    pub fn scroll_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }

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
                // background color per VT420/xterm spec (#7522).
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
        // Fill BCE RGB in vacated bottom rows after shift (#7685).
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
            self.reuse_scrolled_rows_general(rows_to_reuse, cols, row_count, ring_sb);
        }

        // Drain lazy buffer to tiered scrollback when threshold is exceeded.
        // This amortizes the materialization cost over many scroll operations.
        // Callers that need all lines in tiered storage (unscroll, reflow)
        // drain explicitly via drain_lazy_buffer().
        if self.storage.lazy_buffer.should_drain() {
            // During an off-thread reflow the store is detached and drain is
            // suppressed (audit bug B keeps staged lines alive), so cap the buffer
            // here instead — otherwise heavy streaming through a long reflow window
            // grows it without bound (audit #4).
            if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
                self.bound_detached_lazy_buffer();
            } else if self.storage.compress_offload_active {
                if self.storage.lazy_buffer.len() <= Self::ASYNC_COMPRESS_BACKPRESSURE {
                    // THRU-5: an off-thread compression worker owns the drain.
                    // Leave the backlog staged (it stays readable + accounted in
                    // the lazy buffer) for the worker to promote in bounded batches
                    // off this PTY-reader critical path — so the reader no longer
                    // pays the ~1000-line LZ4/zstd promotion spike inline.
                } else {
                    // Backpressure: the backlog passed the cap, so the worker has
                    // fallen behind (a sustained flood: the reader holds the term
                    // lock ~continuously, starving the worker). The reader must
                    // NEVER pay LZ4/zstd on its PTY-drain critical path — that
                    // inline promotion is what collapsed cat-flood throughput
                    // (SCROLL-1 regression: 193 -> 59 MB/s). Instead DROP the oldest
                    // staged lines (O(1), no compression) to hold the backlog at the
                    // cap. Deep history beyond the ring + cap is truncated under an
                    // extreme sustained flood (ghostty bounds scrollback the same
                    // way, at a far smaller ~10k) — a deliberate throughput-over-
                    // depth trade that only triggers when the worker cannot keep up.
                    let over = self
                        .storage
                        .lazy_buffer
                        .len()
                        .saturating_sub(Self::ASYNC_COMPRESS_BACKPRESSURE);
                    self.storage.lazy_buffer.drop_oldest(over);
                    // Real retention loss under flood — surfaced OUT-OF-BAND
                    // via the truncation counter, never a sentinel (E10a).
                    self.storage.flood_truncated_lines += over as u64;
                }
            } else {
                self.drain_lazy_buffer();
            }
        }

        self.storage
            .extras
            .shift_rows_up_by(0, row_u16(rows_to_reuse));
        // Fill BCE RGB in vacated bottom rows after shift (#7685).
        let vis = self.storage.visible_rows;
        self.fill_bce_rgb_rows(vis.saturating_sub(row_u16(rows_to_reuse))..vis);
        self.storage.absolute_row_counter += rows_to_reuse as u64;
    }

    /// Steady-state line-feed fast path: single-row scroll with no tiered
    /// scrollback attached. Semantically identical to the general path but
    /// avoids the intermediate extraction `Vec` and recycles the popped
    /// `ring_extras` allocation as scratch for the new row's extraction,
    /// eliminating per-scroll heap churn (one `Box` + two `Vec`s per styled
    /// row) on the dominant one-line scroll.
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
            // Single fused fill (replaces clear + erase_with): applies BCE
            // fill so reused bottom rows inherit the current SGR background
            // color per VT420/xterm spec (#7522).
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
    ) {
        let has_scrollback = self.storage.stages_evicted_rows();

        // The reused bottom rows inherit the current SGR background (BCE fill). Read
        // before the extraction below — nothing between mutates it (byte-identical to
        // reading it after the old up-front `collect`).
        let fill = self.storage.cursor_template;

        // STEADY-STATE FAST PATH: a single-row scroll — the dominant case once the
        // ring is full (every newline of a cat/tail/build-log stream) — extracts the
        // one entering row's extras DIRECTLY. With `ring_head` still unadvanced
        // (`i == 0`) there is no cross-iteration ordering to preserve, so the throwaway
        // 1-element `Vec<ScrolledRowExtras>` the general path allocates per newline is
        // elided. Behaviorally identical to `rows_to_reuse == 1` through the loop below.
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

        // MULTI-ROW: the Vec is REQUIRED — each row's `phys` is computed against the
        // ORIGINAL `ring_head`, which `reuse_one_scrolled_row` advances per iteration,
        // so extraction must complete before any reuse. Extract extras for rows
        // entering ring-buffer scrollback (kept even with no tiered scrollback attached,
        // so a later attach converts rows correctly; the extraction is cheap when the
        // CellExtras is empty — common for plain text).
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

    /// Recycle ONE oldest ring row after its entering-row extras have been extracted:
    /// stage the evicted row to scrollback (lazy `DeferredLine`), evict its page,
    /// resize + BCE-reset it as the fresh bottom row, and advance `ring_head`. The
    /// per-call sequencing (push/pop `ring_extras`, deferred push, `ring_head` advance)
    /// is exactly the body the general [`Self::reuse_scrolled_rows_general`] loop ran
    /// inline; sharing it lets the `rows_to_reuse == 1` fast path skip the throwaway
    /// per-row `Vec` without duplicating the logic.
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

        // Keep the push-then-pop ORDER: with a zero-sized ring (`ring_sb == 0`)
        // it is an identity that hands this row's own freshly extracted extras
        // straight to `push_row_boxed`, and it is what keeps
        // `ring_extras.len() == ring_buffer_scrollback()`. Only the BOX is now
        // carried through whole — the popped `Option<Box<..>>` is exactly the
        // value `DeferredLine` wants to store, so unboxing it here just to have
        // `DeferredLine::new` re-box it cost one malloc + one free per
        // extras-carrying scrolled line inside the reader's `term_lock` hold.
        // When `!has_scrollback` the box is simply dropped below, as before.
        let extras = self.storage.ring_extras.pop_front().flatten();

        // Lazy scrollback promotion: snapshot the row as a DeferredLine
        // (O(cells) memcpy) instead of the O(cols) row_to_line conversion.
        // The line is materialized lazily on first read access.
        //
        // `push_row` fills a RECYCLED cell body from the lazy buffer's pool
        // rather than `to_vec`-ing a fresh one. This runs once per newline
        // inside the PTY reader's single `term_lock` hold for a whole read
        // batch (~800 newlines for a 64 KiB batch at 80 columns), so the malloc
        // it removes was ~800 malloc/free pairs of pure lock-hold — time the UI
        // thread's keystroke-echo present spends blocked, i.e. time the user
        // feels as typing lag under flood.
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
            // Single fused fill (replaces clear + erase_with): applies BCE
            // fill so reused bottom rows inherit the current SGR background
            // color per VT420/xterm spec (#7522).
            storage.rows[oldest].reset_with(fill);
        }
        self.storage.ring_head = (self.storage.ring_head + 1) % row_count;
    }

    /// Drain all pending deferred lines from the lazy buffer into tiered scrollback.
    ///
    /// Materializes each `DeferredLine` into a `Line` and pushes it to the
    /// tiered scrollback storage. Called when the lazy buffer exceeds its
    /// threshold, when scrollback is accessed, or at checkpoint time.
    ///
    /// After draining, enforces the memory budget (if configured) by evicting
    /// oldest cold-tier lines to a disk spill file.
    /// Bound the lazy buffer while the tiered store is detached for an off-thread
    /// reflow. Normally [`drain_lazy_buffer`](Self::drain_lazy_buffer) offloads /
    /// compresses staged lines into the tiered store, but the store is gone during
    /// the reflow window (drain is suppressed), so under heavy streaming the buffer
    /// would grow without bound (audit #4). Cap it, dropping the OLDEST staged window
    /// output beyond the cap — those oldest lines would be trimmed by the scrollback
    /// limit eventually anyway.
    fn bound_detached_lazy_buffer(&mut self) {
        // Generous cap for the transient reflow window: normal windows finish in well
        // under a second and never approach it; only pathological streaming through a
        // long (deep-history) window hits it.
        const DETACHED_LAZY_CAP: usize = 50_000;
        let len = self.storage.lazy_buffer.len();
        if len > DETACHED_LAZY_CAP {
            let drop_n = len - DETACHED_LAZY_CAP;
            self.storage.lazy_buffer.drop_oldest(drop_n);
            // Real retention loss (reflow-window cap) — counted out-of-band (E10a).
            self.storage.flood_truncated_lines += drop_n as u64;
            aterm_log::warn!(
                "reflow window: lazy buffer exceeded {DETACHED_LAZY_CAP} lines; \
                 dropped {drop_n} oldest staged line(s) to bound memory"
            );
        }
    }

    /// THRU-5 backpressure cap: once the lazy backlog passes this many lines the
    /// off-thread compression worker has fallen behind, so the reader drains
    /// inline (a bounded spike) to keep memory bounded. Sized well above the
    /// 1000-line drain threshold so a keeping-up worker never trips it, yet far
    /// below the 50k detached-reflow cap so a sustained overload can't balloon
    /// uncompressed history.
    pub(crate) const ASYNC_COMPRESS_BACKPRESSURE: usize = 20_000;

    /// Lines currently staged in the lazy buffer awaiting promotion into the
    /// tiered store (the off-thread compression worker's backlog).
    #[must_use]
    #[inline]
    pub fn lazy_backlog_len(&self) -> usize {
        self.storage.lazy_buffer.len()
    }

    /// Attach/detach the off-thread compression worker for this grid. While
    /// attached, the reader-thread ingest path defers lazy-buffer draining to the
    /// worker (which calls [`drain_lazy_bounded`](Self::drain_lazy_bounded) in
    /// bounded batches), keeping the LZ4/zstd promotion spike off the PTY-drain
    /// critical path. Idempotent; set once at session setup.
    pub fn set_compress_offload_active(&mut self, active: bool) {
        self.storage.compress_offload_active = active;
    }

    /// THRU-5: drain up to `max_lines` of the OLDEST staged lines into the tiered
    /// store, running the LZ4/zstd promotion for just that bounded batch, then
    /// enforce the budget. Returns the number of lines STILL staged afterward, so
    /// the worker can loop until the backlog is drained. A no-op (returns 0) when
    /// there is no backlog or the store is unavailable; while the store is
    /// detached for a reflow the staged lines are kept for that window's re-attach
    /// flush (mirrors [`drain_lazy_buffer`](Self::drain_lazy_buffer)).
    ///
    /// The caller must hold the term lock (same single-writer-under-mutex
    /// discipline as every other `&mut Grid` mutation); this splits the reader's
    /// former one-shot ~1000-line drain into short worker-driven holds.
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        if max_lines == 0 || self.storage.lazy_buffer.is_empty() {
            return self.storage.lazy_buffer.len();
        }
        // Store detached for an off-thread reflow: keep staged lines (flushed on
        // re-attach between reflowed history and the live ring — audit bug B).
        if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
            return self.storage.lazy_buffer.len();
        }
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
            // No scrollback attached — discard deferred lines (as drain does).
            self.storage.lazy_buffer.clear();
            return 0;
        };

        // Collect the front batch first (borrow: lazy_buffer and scrollback are
        // both behind &mut self.storage).
        let lines: Vec<_> = self.storage.lazy_buffer.drain_front(max_lines).collect();
        for line in lines {
            if let Err(error) = scrollback.push_line(line) {
                aterm_log::warn!("scrollback push_line failed (bounded drain): {error}");
            }
        }
        self.enforce_scrollback_budget_and_clamp();
        self.storage.lazy_buffer.len()
    }

    pub(crate) fn drain_lazy_buffer(&mut self) {
        if self.storage.lazy_buffer.is_empty() {
            return;
        }
        // The store is out for an off-thread reflow: keep the staged lines in the
        // lazy buffer (they are flushed on re-attach, between the reflowed history
        // and the live ring). Discarding them here would drop output produced
        // during the reflow window (audit bug B).
        if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
            return;
        }
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
            // No scrollback attached — discard deferred lines.
            self.storage.lazy_buffer.clear();
            return;
        };

        // Collect lines first to avoid borrow conflict (lazy_buffer and scrollback
        // are both behind &mut self.storage).
        let lines: Vec<_> = self.storage.lazy_buffer.drain_all().collect();
        for line in lines {
            if let Err(error) = scrollback.push_line(line) {
                aterm_log::warn!("scrollback push_line failed: {error}");
            }
        }

        self.enforce_scrollback_budget_and_clamp();
    }

    /// Epilogue for any bulk `push_line` sequence into tiered scrollback
    /// (lazy-buffer drain, scrollback-reflow restore): enforce the memory
    /// budget, then re-clamp the display offset.
    pub(crate) fn enforce_scrollback_budget_and_clamp(&mut self) {
        // Enforce memory budget: evict oldest cold-tier lines to disk spill
        // if the scrollback exceeds the configured budget. Disk cold-tier only;
        // on wasm (feature off) there is no disk spill — hot/warm RAM tiers only.
        #[cfg(feature = "disk-tier")]
        if let Some(enforcer) = self.storage.budget_enforcer.as_mut()
            && let Some(scrollback) = self.storage.scrollback.as_mut()
            && let Err(error) = enforcer.enforce(scrollback)
        {
            aterm_log::warn!("scrollback budget enforcement failed: {error}");
        }

        // push_line can trigger line-limit enforcement or memory-pressure
        // eviction, reducing total scrollback lines.  If the user was scrolled
        // back, display_offset may now exceed scrollback_lines(), violating the
        // DisplayOffsetValid invariant.  Clamp to restore it (#7240).
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
        // Only non-full degenerate regions are no-ops (#7751).
        //
        // The fresh-session blank gate below deliberately does NOT extend to
        // this full-screen path: unconditional archival of every scrolled row
        // is xterm parity, and it is load-bearing pinned semantics — the
        // Tier-1 spec bindings (`conformance_retention.rs`,
        // `conformance_offload.rs`) model every quiescent full-screen scroll
        // as exactly one retained line, and
        // `ring_extras_len_equals_ring_buffer_scrollback` pins the ring
        // growth per scroll. So a fresh session that pads a blank screen with
        // LFs still mints blank scrollback here (as xterm does), while the
        // Codex-shaped top-anchored region path gets the gate.
        if self
            .storage
            .scroll_region
            .is_full(self.storage.visible_rows)
        {
            self.reset_display_offset_with_damage();
            self.scroll_up(n);
            return;
        }
        // Degenerate single-row region: nowhere to scroll to — no-op (#7751).
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        // Row index arithmetic requires display_offset == 0. Callers like
        // line_feed() reset it, but others (advance_autowrap_line, CSI S) may
        // not. Reset here defensively so every path is safe. (#5019)
        self.reset_display_offset_with_damage();

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);

        // Clamp before selecting the archival path: unlike a full-screen scroll,
        // a partial region cannot consume more rows than it contains.
        let region_size = bottom - top + 1;
        let n = n.min(region_size);

        // A full-width region rooted at row zero is a viewport with fixed rows
        // below it (the shape used by Codex's inline-history renderer).  Rows
        // displaced through the physical top still belong in primary-screen
        // history; treating every partial DECSTBM region as history-free loses
        // the transcript permanently and leaves the scrollbar at zero.
        //
        // `max_scrollback == 0` with no tiered store is the alternate-screen
        // shape, where output must remain ephemeral.  A tiered grid may use a
        // zero-sized ring, so store presence also enables archival.
        //
        // The ONE exception to "displaced rows archive" is the fresh-session
        // blank band (see `fresh_session_blank_prefix`): while the grid holds
        // no history at all, a leading run of blank displaced rows carries no
        // transcript, and archiving it mints blank history lines that a later
        // rows-grow reveal paints as a dead band above the content. Blank
        // rows AFTER the first written row — and every blank row once ANY
        // history exists — are transcript (a paragraph break a TUI displays
        // by printing nothing, or a written row ED/EL-erased back to blank)
        // and archive like any written row.
        let history_enabled = self.storage.max_scrollback > 0
            || self.storage.scrollback.is_some()
            || self.storage.scrollback_detached_for_reflow;
        if top == 0 && history_enabled {
            let blank_prefix = self.fresh_session_blank_prefix(n);
            if blank_prefix < n {
                // Drop the leading fresh-session blanks (if any) with a
                // history-free scroll, then archive from the first written
                // row onward. Two sequential scrolls of the same region
                // compose to the same visible result as one scroll of `n`,
                // while history receives exactly the transcript-bearing
                // suffix of the displaced set.
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
    /// region's `CellExtras` — history and `absolute_row_counter` are
    /// untouched. This is the interior-region / history-free /
    /// fresh-session-blank path; the archival top-anchored path is
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

        // Clear the bottom n rows of the region with BCE fill (#7522).
        // Reset line size to SingleWidth so DECDWL/DECDHL flags don't leak
        // from recycled rows that previously had double-width attributes.
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
        // Fill BCE RGB in vacated bottom rows after shift (#7685).
        self.fill_bce_rgb_rows(row_u16(bottom + 1 - n)..bottom_u16.saturating_add(1));

        // Partial-region scroll invalidates selection coordinates in complex ways.
        // Use saturating_add with large value to force selection clear via adjust_for_scroll.
        self.storage.content_scroll_delta = i32::MAX;
        // Mark only the scroll region rows as dirty, not the full screen.
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Length of the LEADING run of blank rows among the `n` viewport rows a
    /// scroll would displace through the physical top (rows `0..n`) — but
    /// ONLY while the grid carries no history at all (`scrollback_lines() ==
    /// 0`, which covers the ring window, the lazy staging buffer, AND the
    /// tiered store). Once any history exists this returns 0 unconditionally.
    ///
    /// `Row::len == 0` means BLANK, not never-written: a row erased with the
    /// default background also has len 0 (`erase_with` keeps len at 0 for a
    /// default-color fill — the ED/EL path), and so does a separator line a
    /// TUI "prints" by printing nothing. Blank lines BETWEEN written lines
    /// are transcript (Codex's paragraph breaks vanished when blankness alone
    /// dropped them), so blankness must never drop a row once anything has
    /// been archived. The one shape where dropping is safe is the
    /// fresh-session band this gate exists for: a brand-new grid that scrolls
    /// before anything was archived displaces leading blanks that cannot
    /// separate transcript (nothing precedes them, nothing is retained), and
    /// archiving them mints blank history that a later rows-grow reveal
    /// paints as a dead band above the content.
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
        // Degenerate single-row region: nowhere to scroll to — no-op (#7751).
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        // Row index arithmetic requires display_offset == 0. Reset
        // defensively for callers that may not have done so. (#5019)
        self.reset_display_offset_with_damage();

        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        let top = usize::from(top_u16);
        let bottom = usize::from(bottom_u16);
        let region_size = bottom - top + 1;
        let n = n.min(region_size);

        self.shift_rows_down(top, bottom, n);

        // Clear the top n rows of the region with BCE fill (#7522).
        // Reset line size to SingleWidth so DECDWL/DECDHL flags don't leak
        // from recycled rows that previously had double-width attributes.
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
        // Fill BCE RGB in vacated top rows after shift (#7685).
        self.fill_bce_rgb_rows(top_u16..row_u16(top + n));

        // Force selection clear: partial-region coordinate mapping is non-trivial,
        // and full-screen scroll_down() also delegates here.
        self.storage.content_scroll_delta = i32::MAX;
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
        // Degenerate single-row region: nowhere to scroll to — no-op (#7751).
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
                // Wide char fixup at rectangle boundaries (#7500).
                // Whole-row rect copy: no per-cell authoritative signal, keep the
                // char==' ' spacer heuristic (true).
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The get_mut writes above bypass len maintenance: copying blank
                // source cells over the dst row's tail content leaves len stale-high
                // (#7522 phantom trailing spaces via row_text/search/scrollback).
                // Recompute the true content extent per modified row (never
                // over-shrinks; margined SU/SD is off the hot path).
                dst.recompute_len();
            }
        }
        // Clear the bottom n rows within margins with BCE fill (#7522).
        let fill = self.storage.cursor_template;
        for clear_offset in (region_size - n)..region_size {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                // Wide char fixup at rectangle boundaries (#7500).
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The BCE fill via get_mut bypasses len maintenance: an empty
                // cursor_template can orphan the tail (stale-high), a colored one
                // extends it (stale-low). Recompute the true extent (#7522).
                r.recompute_len();
            }
        }

        // Shift extras within the margin columns: rows [top+n..bottom] shift
        // up by n, rows [top..top+n) are dropped. Preserves hyperlinks, RGB
        // colors, and combining marks on shifted rows. (#7415)
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .extras
            .shift_rect_up_by(top_u16, bottom_u16, left, right, row_u16(n));
        // Fill BCE RGB in vacated bottom-right rect after shift (#7685).
        self.fill_bce_rgb_rect(
            row_u16(top + region_size - n)..bottom_u16.saturating_add(1),
            left..right.saturating_add(1),
        );

        self.storage.content_scroll_delta = i32::MAX;
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
        // Degenerate single-row region: nowhere to scroll to — no-op (#7751).
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
                // Wide char fixup at rectangle boundaries (#7500).
                // Whole-row rect copy: no per-cell authoritative signal, keep the
                // char==' ' spacer heuristic (true).
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The get_mut writes above bypass len maintenance: copying blank
                // source cells over the dst row's tail content leaves len stale-high
                // (#7522 phantom trailing spaces via row_text/search/scrollback).
                // Recompute the true content extent per modified row (never
                // over-shrinks; margined SU/SD is off the hot path).
                dst.recompute_len();
            }
        }
        // Clear the top n rows within margins with BCE fill (#7522).
        let fill = self.storage.cursor_template;
        for clear_offset in 0..n {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                // Wide char fixup at rectangle boundaries (#7500).
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The BCE fill via get_mut bypasses len maintenance: an empty
                // cursor_template can orphan the tail (stale-high), a colored one
                // extends it (stale-low). Recompute the true extent (#7522).
                r.recompute_len();
            }
        }

        // Shift extras within the margin columns: rows [top..bottom-n] shift
        // down by n, rows [bottom-n+1..bottom] are dropped. (#7415)
        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .extras
            .shift_rect_down_by(top_u16, bottom_u16, left, right, row_u16(n));
        // Fill BCE RGB in vacated top-left rect after shift (#7685).
        self.fill_bce_rgb_rect(top_u16..row_u16(top + n), left..right.saturating_add(1));

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }
}

// Kitty CSI + T unscroll implementation extracted to scroll_unscroll.rs.
