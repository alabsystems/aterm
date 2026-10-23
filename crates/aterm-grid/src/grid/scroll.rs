// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid display, content, and DECSTBM/DECLRMM region scrolling operations.

use crate::damage::compute_display_offset_damage;
use crate::row::LineSize;

use super::Grid;
use super::row_u16;
use crate::Row;

impl Grid {
    /// Reset `display_offset` to `0`, damaging only newly exposed bottom rows.
    #[inline]
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
    #[inline]
    fn mark_scroll_damage(&mut self, n: usize) {
        let visible_rows = self.storage.visible_rows;
        self.storage.mark_content_scroll(visible_rows, n);
    }

    /// Record that the reader just brought the viewport down to the live
    /// bottom: the one bump site for `reader_live_bottom_gen`, advancing when
    /// `before` (the offset just before a reader-facing viewport primitive ran)
    /// was non-zero and the offset is now zero.
    #[inline]
    const fn note_reader_descent_to_live_bottom(&mut self, before: usize) {
        if before != 0 && self.storage.display_offset == 0 {
            self.storage.reader_live_bottom_gen += 1;
        }
    }

    /// Pin the viewport to `0` during an output batch (paired with [`Self::repin_display_offset`]).
    #[inline]
    pub fn pin_viewport_to_live_for_output_batch(&mut self) {
        self.reset_display_offset_with_damage();
        debug_assert_eq!(self.storage.display_offset, 0);
    }

    /// Scroll the viewport by `delta` lines (positive = older, negative = newer).
    pub fn scroll_display(&mut self, delta: i32) {
        let max_offset = self.storage.scrollback_lines();
        let old_offset = self.storage.display_offset;
        let current: i32 = self.storage.display_offset.try_into().unwrap_or(i32::MAX);
        let clamped = current.saturating_add(delta).max(0);
        let new_offset = usize::try_from(clamped).unwrap_or(0);
        self.storage.display_offset = new_offset.min(max_offset);

        let dmg =
            compute_display_offset_damage(old_offset, self.storage.display_offset, self.rows());
        self.storage.damage.apply_display_offset_damage(dmg);
        self.note_reader_descent_to_live_bottom(old_offset);
        self.note_detached_reader_motion(
            super::scrollback_offload::ReaderMotion::By(delta),
            old_offset,
        );
        debug_assert!(self.storage.display_offset <= self.storage.scrollback_lines());
    }

    /// Restore and advance `display_offset` after an output batch.
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

    /// Scroll the viewport to the oldest scrollback line.
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

    /// Scroll the viewport to the live bottom (`display_offset = 0`).
    #[inline]
    pub fn scroll_to_bottom(&mut self) {
        let before = self.storage.display_offset;
        self.reset_display_offset_with_damage();
        self.note_reader_descent_to_live_bottom(before);
        self.note_detached_reader_motion(super::scrollback_offload::ReaderMotion::Live, before);
        debug_assert_eq!(self.storage.display_offset, 0);
    }

    /// Scroll the viewport so `target_abs_row` sits at the top visible row.
    pub fn scroll_to_absolute_row(&mut self, target_abs_row: u64) {
        let live_top = self
            .storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows));
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

    /// Clamp `display_offset` to `self.storage.scrollback_lines()`.
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

    /// Scroll full-screen content up by `n` lines, pushing evicted top rows into scrollback.
    #[doc(hidden)]
    pub fn scroll_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }

        self.drop_fill_debt();
        self.scroll_up_storage(n);
        self.finish_scroll_up(n);
    }

    /// Perform the ring/tiered-history mutation for an upward scroll without
    /// recording presentation damage.
    fn scroll_up_storage(&mut self, n: usize) {
        debug_assert!(n > 0);
        debug_assert!(
            !self.storage.rows.is_empty(),
            "scroll_up_storage: ring buffer has zero rows"
        );

        let capacity = (self.storage.visible_rows as usize) + self.storage.max_scrollback;
        let cols = self.storage.cols;

        let rows_until_capacity = capacity.saturating_sub(self.storage.total_lines);
        let rows_to_add = n.min(rows_until_capacity);
        let rows_to_reuse = n.saturating_sub(rows_to_add);

        if rows_to_add > 0 {
            self.grow_scrollback_ring(rows_to_add, cols);
        }

        if rows_to_reuse > 0 {
            self.reuse_scrolled_rows(rows_to_reuse, cols);
        }
        self.enforce_history_link_budget();

        debug_assert!(
            self.storage.rows.len()
                <= (self.storage.visible_rows as usize) + self.storage.max_scrollback
        );
    }

    /// The append arm of `scroll_up`: the ring is below its cap, so the rows
    /// scrolling off are kept and `rows_to_add` fresh rows are pushed as the new
    /// bottom.
    fn grow_scrollback_ring(&mut self, rows_to_add: usize, cols: u16) {
        debug_assert!(
            !self.storage.rows.is_empty(),
            "grow_scrollback_ring: ring buffer has zero rows"
        );
        self.storage.linearize_ring();
        let ring_sb = self.storage.ring_buffer_scrollback();
        let row_count = self.storage.rows.len();
        for i in 0..rows_to_add {
            let row_idx = row_u16(i);
            let phys = (self.storage.ring_head + ring_sb + i) % row_count;
            let extracted =
                Self::extract_row_extras(&self.storage.rows[phys], &self.storage.extras, row_idx);
            self.storage.push_ring_extras(extracted);
        }

        self.storage.rows.reserve(rows_to_add);
        let fill = self.storage.cursor_template;
        {
            let storage = &mut self.storage;
            let rows = &mut storage.rows;
            let pages = &mut storage.pages;
            let needs_fill = fill != crate::Cell::EMPTY;
            for _ in 0..rows_to_add {
                // SAFETY: New rows are stored in the same `GridStorage` that owns `pages`, and rows drop before the backing pages.
                let mut row = unsafe { Row::new(cols, pages) };
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

        if self.storage.lazy_buffer.should_drain() {
            // During an off-thread reflow the store is detached and drain is
            if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
                self.bound_detached_lazy_buffer();
            } else if self.storage.compress_offload_active {
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
        let vis = self.storage.visible_rows;
        self.fill_bce_rgb_rows(vis.saturating_sub(row_u16(rows_to_reuse))..vis);
        self.storage.absolute_row_counter += rows_to_reuse as u64;
    }

    /// Steady-state line-feed fast path: single-row scroll with no tiered
    /// scrollback attached.
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
            // Skip extraction when no tiered scrollback consumes popped ring_extras.
        } else if let Some(mut bx) = self.storage.ring_extras.pop_front().flatten() {
            Self::extract_row_extras_into(
                &mut bx,
                &self.storage.rows[phys],
                &self.storage.extras,
                0,
            );
            self.storage
                .ring_extras
                .push_back(if bx.is_empty() { None } else { Some(bx) });
        } else {
            let extracted =
                Self::extract_row_extras(&self.storage.rows[phys], &self.storage.extras, 0);
            self.storage.push_ring_extras(extracted);
        }

        {
            let storage = &mut self.storage;
            let pages = &mut storage.pages;
            // SAFETY: The reused row remains stored in `storage.rows`, and `storage.pages` continues to outlive that owner.
            unsafe { storage.rows[oldest].resize(cols, pages) };
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
        let fill = self.storage.cursor_template;

        if rows_to_reuse == 1 {
            let phys = (self.storage.ring_head + ring_sb) % row_count;
            let new_extras = Self::extract_row_extras(
                &self.storage.rows[phys],
                &self.storage.extras,
                row_u16(0),
            );
            self.reuse_one_scrolled_row(new_extras, cols, row_count, has_scrollback, fill);
            return;
        }

        let new_scrollback_extras: Vec<_> = (0..rows_to_reuse)
            .map(|i| {
                let row_idx = row_u16(i);
                let phys = (self.storage.ring_head + ring_sb + i) % row_count;
                Self::extract_row_extras(&self.storage.rows[phys], &self.storage.extras, row_idx)
            })
            .collect();

        for new_extras in new_scrollback_extras {
            self.reuse_one_scrolled_row(new_extras, cols, row_count, has_scrollback, fill);
        }
    }

    /// Recycle one oldest ring row after its entering-row extras were
    /// extracted: stage the evicted row to scrollback (lazy `DeferredLine`),
    /// evict its page, resize and BCE-reset it as the fresh bottom row, and
    /// advance `ring_head`.
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

        let extras = self.storage.ring_extras.pop_front().flatten();

        if has_scrollback {
            self.storage
                .lazy_buffer
                .push_row_boxed(&self.storage.rows[oldest], extras);
        }

        {
            let storage = &mut self.storage;
            let pages = &mut storage.pages;
            // SAFETY: The reused row remains stored in `storage.rows`, and `storage.pages` continues to outlive that owner.
            unsafe { storage.rows[oldest].resize(cols, pages) };
            storage.rows[oldest].reset_with(fill);
        }
        self.storage.ring_head = (self.storage.ring_head + 1) % row_count;
    }

    /// The staging cap of the detached-reflow window: past it the oldest
    /// staged window output is dropped.
    const DETACHED_LAZY_CAP: usize = 50_000;

    /// Bound the lazy buffer while the tiered store is detached for an
    /// off-thread reflow: draining is suppressed then, so heavy streaming would
    /// grow it without bound.
    fn bound_detached_lazy_buffer(&mut self) {
        let len = self.storage.lazy_buffer.len();
        if len > Self::DETACHED_LAZY_CAP {
            let drop_n = len - Self::DETACHED_LAZY_CAP;
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

    /// Record and reset the count of staged lines dropped during a reflow detach window.
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

    /// Minimum lazy-backlog line capacity before flood overflow drops oldest staged rows.
    pub(crate) const ASYNC_COMPRESS_BACKPRESSURE: usize = 20_000;

    /// Number of oldest staged rows that exceed the lazy-backlog line or byte limits.
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

    /// Drop `over` of the oldest staged rows (the backlog is over its cap,
    /// [`flood_overflow`](Self::flood_overflow)) and account for them:
    /// * ordinary eviction: rows the configured line limit would not hold
    ///   anyway, oldest first, store before staged (`truncate_oldest`).
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
                aterm_log::warn!(
                    "flood path: evicting {from_store} stored line(s) failed: {error}"
                );
                from_store = 0;
            }
            self.clamp_display_offset();
        }
        let in_window = if from_store == stored {
            cut.saturating_add(over).saturating_sub(beyond - from_store)
        } else {
            cut.saturating_add(over)
        };
        let hole = u64::try_from(in_window).unwrap_or(u64::MAX);
        self.storage.flood_truncated_lines += self.storage.lazy_buffer.shed_oldest(over, hole);
        if hole > 0 {
            self.storage.history_renumber_epoch =
                self.storage.history_renumber_epoch.saturating_add(1);
            self.lose_anchors_renumbered();
        }
    }

    /// Number of lines staged in the lazy buffer awaiting tiered-store promotion.
    #[must_use]
    #[inline]
    pub fn lazy_backlog_len(&self) -> usize {
        self.storage.lazy_buffer.len()
    }

    const FLOOD_WEIGHT_SAMPLE: usize = 1024;

    /// Number of staged rows to promote before a memory-bound flood reaches its byte cap.
    #[must_use]
    pub fn flood_promotion_due(&self) -> usize {
        let buffer = &self.storage.lazy_buffer;
        if buffer.cut_open() {
            return 0;
        }
        let Some(store) = self.storage.scrollback.as_ref() else {
            return 0;
        };
        let floor = Self::ASYNC_COMPRESS_BACKPRESSURE;
        let headroom = store
            .memory_budget()
            .saturating_sub(store.budgeted_memory_used());
        let mark = headroom / 2;
        if !buffer.outweighs(mark) {
            return 0;
        }
        if let Some(share) = store.line_limit()
            && (share <= floor
                || buffer.projected_peak(share, Self::FLOOD_WEIGHT_SAMPLE) <= headroom)
        {
            return 0;
        }
        buffer.front_rows_over(mark, floor / 2)
    }

    /// Drain up to `max_lines` under memory pressure, dropping overflow without inline LZ4.
    pub fn drain_lazy_pressure_bounded(&mut self, max_lines: usize) -> usize {
        let remaining = self.storage.lazy_buffer.len();
        if self.storage.lazy_buffer.cut_open() || self.storage.scrollback.is_none() {
            return remaining;
        }
        let max_lines =
            max_lines.min(remaining.saturating_sub(Self::ASYNC_COMPRESS_BACKPRESSURE / 2));
        if max_lines == 0 {
            return remaining;
        }
        self.drain_lazy_bounded(max_lines)
    }

    /// Drain up to `max_lines` during active output without inline LZ4 promotion.
    pub fn trickle_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let arrived = self.storage.lazy_buffer.take_arrivals();
        if self.storage.lazy_buffer.cut_open()
            && arrived >= u64::try_from(max_lines).unwrap_or(u64::MAX)
        {
            return self.storage.lazy_buffer.len();
        }
        self.drain_lazy_bounded(max_lines)
    }

    /// Enable or disable off-thread scrollback compression for this grid.
    #[inline]
    pub const fn set_compress_offload_active(&mut self, active: bool) {
        self.storage.compress_offload_active = active;
    }

    /// Drain up to `max_lines` of staged scrollback into the tiered store.
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        self.storage.lazy_buffer.take_arrivals();
        if max_lines == 0 || self.storage.lazy_buffer.is_empty() {
            return self.storage.lazy_buffer.len();
        }
        if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
            return self.storage.lazy_buffer.len();
        }
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
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
        if self.storage.scrollback_detached_for_reflow && self.storage.scrollback.is_none() {
            return;
        }
        let Some(scrollback) = self.storage.scrollback.as_mut() else {
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

    /// Enforce the scrollback memory budget and clamp `display_offset`.
    pub(crate) fn enforce_scrollback_budget_and_clamp(&mut self) {
        self.clamp_display_offset();
    }

    fn finish_scroll_up(&mut self, n: usize) {
        let delta = i32::try_from(n).unwrap_or(i32::MAX);
        self.storage.content_scroll_delta = self.storage.content_scroll_delta.saturating_add(delta);
        self.mark_scroll_damage(n);
    }

    /// Scroll full-screen content down by `n` lines.
    #[cfg(test)]
    pub(crate) fn scroll_down(&mut self, n: usize) {
        self.scroll_region_down(n);
    }

    /// Scroll the active vertical scroll region up by `n` lines.
    pub fn scroll_region_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if self
            .storage
            .scroll_region
            .is_full(self.storage.visible_rows)
        {
            self.reset_display_offset_with_damage();
            self.scroll_up(n.min(usize::from(self.storage.visible_rows)));
            return;
        }
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom {
            return;
        }
        self.reset_display_offset_with_damage();

        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);

        let region_size = bottom - top + 1;
        let n = n.min(region_size);

        let history_enabled = self.storage.max_scrollback > 0
            || self.storage.scrollback.is_some()
            || self.storage.scrollback_detached_for_reflow;
        if top == 0 && history_enabled {
            let blank_prefix = self.fresh_session_blank_prefix(n);
            if blank_prefix < n {
                // Drop the leading fresh-session blanks with a history-free
                if blank_prefix > 0 {
                    self.scroll_region_up_history_free(top, bottom, blank_prefix);
                }
                self.scroll_top_anchored_region_up_with_history(n - blank_prefix, bottom);
                return;
            }
        }

        self.scroll_region_up_history_free(top, bottom, n);
    }

    /// The non-archival region scroll: shift rows `[top..=bottom]` up by `n`
    /// within the viewport, BCE-fill the vacated bottom rows, and shift the
    /// region's `CellExtras`; history and `absolute_row_counter` are untouched.
    fn scroll_region_up_history_free(&mut self, top: usize, bottom: usize, n: usize) {
        debug_assert!(n > 0 && n <= bottom - top + 1);
        debug_assert!(bottom < usize::from(self.storage.visible_rows));

        self.storage.shift_visible_rows_up(top, bottom, n);

        let fill = self.storage.cursor_template;
        for row in (bottom + 1 - n)..=bottom {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }

        let top_u16 = row_u16(top);
        let bottom_u16 = row_u16(bottom);
        let shift_n = row_u16(n);
        self.storage
            .extras
            .shift_region_up_by(top_u16, bottom_u16, shift_n);
        self.fill_bce_rgb_rows(row_u16(bottom + 1 - n)..bottom_u16.saturating_add(1));

        self.record_row_band_move_or_poison(top_u16, bottom_u16, n, false);
        self.anchors_band_move(top_u16, bottom_u16, -i64::try_from(n).unwrap_or(i64::MAX));
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Record rows `top..=bottom` moving by `n` rows (down the screen when
    /// `down`) on this batch's band record, or poison the record when `n` does
    /// not fit an `i16` displacement (see
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
    /// while the grid holds no history at all; otherwise 0.
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
    fn scroll_top_anchored_region_up_with_history(&mut self, n: usize, bottom: usize) {
        debug_assert!(n > 0);
        debug_assert!(bottom + 1 < usize::from(self.storage.visible_rows));

        let old_live_top = self
            .storage
            .absolute_row_counter
            .saturating_sub(u64::from(self.storage.visible_rows));
        let footer_start = u64::try_from(bottom + 1).unwrap_or(u64::MAX);
        let insertion_at = old_live_top.saturating_add(footer_start);

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
        self.anchors_splice(insertion_at, u64::try_from(n).unwrap_or(u64::MAX));
        self.record_row_band_move_or_poison(0, row_u16(bottom), n, false);

        self.storage
            .mark_content_rows(0, row_u16(bottom).saturating_add(1));
    }

    /// Shift rows `[top..=bottom]` down by `n` slots.
    pub(super) fn shift_rows_down(&mut self, top: usize, bottom: usize, n: usize) {
        self.storage.shift_visible_rows_down(top, bottom, n);
    }

    /// Scroll the active vertical scroll region down by `n` lines.
    pub fn scroll_region_down(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom
            && !self
                .storage
                .scroll_region
                .is_full(self.storage.visible_rows)
        {
            return;
        }
        self.reset_display_offset_with_damage();

        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        let top = usize::from(top_u16);
        let bottom = usize::from(bottom_u16);
        let region_size = bottom - top + 1;
        let n = n.min(region_size);

        self.shift_rows_down(top, bottom, n);

        let fill = self.storage.cursor_template;
        for row in top..(top + n) {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }

        let shift_n = row_u16(n);
        self.storage
            .extras
            .shift_region_down_by(top_u16, bottom_u16, shift_n);
        self.fill_bce_rgb_rows(top_u16..row_u16(top + n));

        self.record_row_band_move_or_poison(top_u16, bottom_u16, n, true);
        self.anchors_band_move(top_u16, bottom_u16, i64::try_from(n).unwrap_or(i64::MAX));
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Scroll up by `n` lines within vertical and horizontal (`left..=right`) margins.
    pub fn scroll_region_up_margined(&mut self, n: usize, left: u16, right: u16) {
        if n == 0 {
            return;
        }
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom
            && !self
                .storage
                .scroll_region
                .is_full(self.storage.visible_rows)
        {
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

        let cols = self.storage.cols as usize;
        let mut buf = vec![super::Cell::EMPTY; width];
        for dst_offset in 0..(region_size - n) {
            let dst_row = row_u16(top + dst_offset);
            let src_row = row_u16(top + dst_offset + n);
            buf.fill(super::Cell::EMPTY);
            let src_marks = if let Some(src) = self.row(src_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = src.get(row_u16(col)) {
                        buf[i] = *c;
                    }
                }
                src.end_marks()
            } else {
                crate::RowFlags::empty()
            };
            if let Some(dst) = self.row_mut(dst_row) {
                dst.overwrite_span(left_usize, &buf);
                if right_usize + 1 >= cols {
                    dst.set_end_marks(src_marks);
                }
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                dst.recompute_len();
            }
        }
        let fill = self.storage.cursor_template;
        for clear_offset in (region_size - n)..region_size {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                if right_usize + 1 >= cols {
                    r.set_end_marks(crate::RowFlags::empty());
                }
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                r.recompute_len();
            }
        }

        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .extras
            .shift_rect_up_by(top_u16, bottom_u16, left, right, row_u16(n));
        self.fill_bce_rgb_rect(
            row_u16(top + region_size - n)..bottom_u16.saturating_add(1),
            left..right.saturating_add(1),
        );

        for row in top_u16..=bottom_u16 {
            self.drop_unclaimed_extras(row, left.saturating_sub(1)..right.saturating_add(2));
        }

        self.storage.presentation.poison_row_band_moves();
        self.lose_anchors();
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }

    /// Scroll down by `n` lines within vertical and horizontal (`left..=right`) margins.
    pub fn scroll_region_down_margined(&mut self, n: usize, left: u16, right: u16) {
        if n == 0 {
            return;
        }
        if self.storage.scroll_region.top == self.storage.scroll_region.bottom
            && !self
                .storage
                .scroll_region
                .is_full(self.storage.visible_rows)
        {
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

        let cols = self.storage.cols as usize;
        let mut buf = vec![super::Cell::EMPTY; width];
        for dst_offset in (n..region_size).rev() {
            let dst_row = row_u16(top + dst_offset);
            let src_row = row_u16(top + dst_offset - n);
            buf.fill(super::Cell::EMPTY);
            let src_marks = if let Some(src) = self.row(src_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = src.get(row_u16(col)) {
                        buf[i] = *c;
                    }
                }
                src.end_marks()
            } else {
                crate::RowFlags::empty()
            };
            if let Some(dst) = self.row_mut(dst_row) {
                dst.overwrite_span(left_usize, &buf);
                if right_usize + 1 >= cols {
                    dst.set_end_marks(src_marks);
                }
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                dst.recompute_len();
            }
        }
        let fill = self.storage.cursor_template;
        for clear_offset in 0..n {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                if right_usize + 1 >= cols {
                    r.set_end_marks(crate::RowFlags::empty());
                }
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                r.recompute_len();
            }
        }

        let top_u16 = self.storage.scroll_region.top;
        let bottom_u16 = self.storage.scroll_region.bottom;
        self.storage
            .extras
            .shift_rect_down_by(top_u16, bottom_u16, left, right, row_u16(n));
        self.fill_bce_rgb_rect(top_u16..row_u16(top + n), left..right.saturating_add(1));

        for row in top_u16..=bottom_u16 {
            self.drop_unclaimed_extras(row, left.saturating_sub(1)..right.saturating_add(2));
        }

        self.storage.presentation.poison_row_band_moves();
        self.lose_anchors();
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
    }
}

/// The flood cut: a burst the compression worker cannot keep up with must
/// never leave a silent hole in history.
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
    /// floor, so the staging cap is the floor and a test-sized flood reaches
    /// the drop path: the memory-bound case, where a cut is a loss (each test
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
    /// and each marker's count equals the numbering gap across it.
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

    /// A lower bound on what one staged `L<n>` row weighs (`feed`'s rows run
    /// 2..=7 cells; a recycled body can be larger, never smaller): what a test
    /// budget is sized against, erring toward more rows fitting.
    fn staged_row_bytes() -> usize {
        std::mem::size_of::<super::super::scroll_convert::DeferredLine>()
            + 7 * std::mem::size_of::<crate::Cell>()
    }

    /// Precondition for the memory-bound tests: the store's headroom holds
    /// fewer staged `L<n>` rows than the floor, so the floor is the cap and a
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

        assert!(grid.scrollback_mut().is_some());
        assert_eq!(history_trimmed(&grid), newest(120_000, 100_000));
        assert_eq!(grid.truncated_lines(), 0);
        grid.assert_invariants();
    }

    #[test]
    fn a_short_line_flood_is_weighed_by_its_cells_not_the_window_width() {
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

    /// Write `n` numbered full-width rows `L{start}----…` (every cell occupied),
    /// CR + text + LF each.
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
        let marker = grid.get_history_line(BATCH).expect("marker row");
        assert!(
            marker.get_attr(0).flags & CellFlags::DIM.bits() != 0,
            "the marker row is dim"
        );
        assert!(
            grid.history_renumber_epoch() > epoch,
            "renumber epoch advanced"
        );
        assert!(grid.lazy_backlog_len() <= Grid::ASYNC_COMPRESS_BACKPRESSURE);
        grid.assert_invariants();
    }

    #[test]
    fn a_cut_deeper_than_the_limit_names_only_what_the_limit_would_keep() {
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

        assert!(grid.scrollback_mut().is_some());
        assert_eq!(
            history_trimmed(&grid),
            newest(21_000, 5_000),
            "ring share + store share"
        );
        assert_eq!(grid.truncated_lines(), 0);
        grid.assert_invariants();
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
        let fragmented = trickled_flood_markers(40_000, false);
        assert!(
            fragmented.len() > 1,
            "the unguarded trickle fragments the cut: {fragmented:?}"
        );
    }

    /// A memory-bound flood (40k lines, a trickle opportunity every 1000), then
    /// a slow tail: 20 opportunities with `per_tick` lines before each, with no
    /// quiet window for the worker.
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
        let (lost, markers) = slow_tail(50, false);
        assert_eq!(lost, 0, "the tail loses nothing once a batch outruns it");
        assert_eq!(markers.len(), 1, "one flood, one cut: {markers:?}");
        let (lost, _) = slow_tail(50, true);
        assert_eq!(lost, 19 * 50, "skipping while cut drops every tail line");
    }

    #[test]
    fn a_window_that_slides_past_the_whole_hole_takes_the_marker() {
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
        let line = flood_marker_line(9_999_999);
        let text = line.to_string();
        let width = text.chars().count();
        assert_eq!(marker_count(&text), Some(9_999_999), "{text:?}");
        assert!(width <= 80, "{text:?} is {width} columns");
        assert!(
            (0..width).all(|i| line.get_attr(i).flags & CellFlags::DIM.bits() != 0),
            "the whole row is dim"
        );
    }

    #[test]
    fn with_no_line_limit_every_drop_is_a_loss() {
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

    /// A late worker must not take the open marker even if pressure remains.
    #[test]
    fn pressure_relief_after_a_late_worker_leaves_one_marker_for_continued_overload() {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        feed_numbered_lines(&mut grid, 0, 22_000);
        assert!(grid.truncated_lines() > 0, "the worker was already late");
        for start in (22_000..42_000).step_by(1_000) {
            feed_numbered_lines(&mut grid, start, 1_000);
            let before = grid.lazy_backlog_len();
            assert_eq!(grid.flood_promotion_due(), 0, "the cut stays open");
            assert_eq!(grid.drain_lazy_pressure_bounded(BATCH), before);
            assert_eq!(
                grid.trickle_lazy_bounded(BATCH),
                before,
                "the skipped pressure batch did not erase the arrival count"
            );
        }
        while grid.drain_lazy_bounded(BATCH) > 0 {}
        let markers = assert_no_silent_hole(&history_trimmed(&grid), 0);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].1, grid.truncated_lines());
        grid.assert_invariants();
    }

    /// An owed count survives a worker's lock release, but permission to drain
    /// does not: a reader can open a cut before the next bounded batch.
    #[test]
    fn a_cut_opened_between_pressure_batches_overrides_the_owed_count() {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        feed_numbered_lines(&mut grid, 0, 15_000);
        let owed = grid.flood_promotion_due();
        assert!(owed > BATCH, "several batches were counted before the cut");
        let before = grid.lazy_backlog_len();
        assert_eq!(grid.drain_lazy_pressure_bounded(BATCH), before - BATCH);
        let owed = owed - BATCH;
        assert!(owed > 0);

        feed_numbered_lines(&mut grid, 15_000, 10_000);
        assert!(grid.truncated_lines() > 0, "the reader opened a cut");
        let before = grid.lazy_backlog_len();
        assert_eq!(
            grid.drain_lazy_pressure_bounded(BATCH),
            before,
            "a counted batch must recheck before taking the marker"
        );
        assert_eq!(grid.trickle_lazy_bounded(BATCH), before);
        assert_eq!(grid.flood_promotion_due(), 0);

        while grid.drain_lazy_bounded(BATCH) > 0 {}
        let markers = assert_no_silent_hole(&history_trimmed(&grid), 0);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].0, BATCH, "the cut follows the first batch");
        assert_eq!(markers[0].1, grid.truncated_lines());
        grid.assert_invariants();
    }

    fn feed_long(grid: &mut Grid, start: usize, n: usize, width: usize) {
        let mut row = Vec::with_capacity(width);
        for i in start..start + n {
            row.clear();
            row.extend_from_slice(format!("L{i} ").as_bytes());
            row.resize(width, b'p');
            grid.carriage_return();
            grid.write_ascii_blast(&row);
            grid.line_feed();
        }
    }

    fn relieve(grid: &mut Grid) {
        loop {
            let due = grid.flood_promotion_due();
            if due == 0 {
                return;
            }
            grid.drain_lazy_pressure_bounded(due.min(BATCH));
        }
    }

    fn maximized_flood(relieving: bool) -> (Grid, usize, u64) {
        let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(100_000));
        let mut peak = 0;
        let mut fed = 0;
        let mut reads = 0;
        while fed < 120_000 {
            feed_long(&mut grid, fed, 300, 230);
            fed += 300;
            reads += 1;
            peak = peak.max(grid.storage.lazy_buffer.staged_bytes());
            if relieving {
                relieve(&mut grid);
            }
            if reads % 57 == 0 {
                grid.trickle_lazy_bounded(BATCH);
            }
        }
        let lost = grid.truncated_lines();
        (grid, peak, lost)
    }

    #[test]
    fn a_maximized_flood_of_long_lines_keeps_the_whole_limit_when_promoted_under_pressure() {
        let (mut grid, peak, lost) = maximized_flood(true);
        assert_eq!(lost, 0, "nothing was cut");
        let hist = history_trimmed(&grid);
        assert!(assert_no_silent_hole(&hist, 0).is_empty(), "no marker");
        assert!(hist.len() >= 100_000);
        assert_eq!(line_number(&hist[hist.len() - 1]), 120_000 - 3);

        while grid.drain_lazy_bounded(BATCH) > 0 {}
        let hist = history_trimmed(&grid);
        assert!(
            assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
            "no marker"
        );
        assert_eq!(hist.len(), 100_000, "the whole configured limit");
        assert_eq!(line_number(&hist[0]), window_top(120_000, 100_000));
        assert_eq!(
            line_number(&hist[hist.len() - 1]),
            120_000 - 3,
            "the newest line"
        );
        assert_eq!(grid.truncated_lines(), 0);
        let per_row = std::mem::size_of::<super::super::scroll_convert::DeferredLine>()
            + 253 * std::mem::size_of::<crate::Cell>();
        assert!(
            peak <= DEFAULT_BUDGET / 2 + 300 * per_row,
            "the backlog peaked at {peak} bytes"
        );
        let store = grid.scrollback().expect("a tiered store");
        assert!(store.budgeted_memory_used() <= store.memory_budget());
        grid.assert_invariants();

        let (starved, _, lost) = maximized_flood(false);
        assert!(
            lost > 40_000 && starved.scrollback_lines() < 60_000,
            "the trickle alone keeps {} lines and loses {lost}",
            starved.scrollback_lines()
        );
    }

    #[test]
    fn promotion_is_due_only_past_half_the_headroom_and_names_the_excess() {
        let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
        feed_long(&mut grid, 0, 20_000, 253);
        assert_eq!(grid.flood_promotion_due(), 0, "under the mark");
        feed_long(&mut grid, 20_000, 20_000, 253);
        let due = grid.flood_promotion_due();
        assert!(due > 0, "past the mark");
        let store = grid.scrollback().expect("a tiered store");
        let mark = (store.memory_budget() - store.budgeted_memory_used()) / 2;
        let bytes = grid.storage.lazy_buffer.staged_bytes();
        let row = bytes / grid.storage.lazy_buffer.staged_rows();
        assert_eq!(
            due,
            (bytes - mark).div_ceil(row),
            "exactly the rows that take {bytes} bytes back to {mark}"
        );
        grid.drain_lazy_pressure_bounded(due);
        assert!(grid.flood_promotion_due() < BATCH);
        relieve(&mut grid);
        assert_eq!(grid.flood_promotion_due(), 0, "the loop clears it");
        assert_eq!(grid.truncated_lines(), 0);

        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        let half_floor = Grid::ASYNC_COMPRESS_BACKPRESSURE / 2;
        feed_numbered_lines(&mut grid, 0, half_floor);
        assert!(grid.lazy_backlog_len() <= half_floor);
        assert_eq!(grid.flood_promotion_due(), 0, "within half the floor");
        feed_numbered_lines(&mut grid, half_floor, 5_000);
        assert_eq!(
            grid.flood_promotion_due(),
            grid.lazy_backlog_len() - half_floor,
            "every row past half the floor"
        );
        assert_eq!(grid.truncated_lines(), 0, "and the cut has not started");

        let mut ring_only = Grid::new(3, 253);
        feed_long(&mut ring_only, 0, 1_000, 253);
        assert_eq!(ring_only.flood_promotion_due(), 0);
    }

    #[test]
    fn no_promotion_is_due_while_the_store_is_away_for_a_reflow() {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        let pending = grid
            .resize_offloading_scrollback(3, 100)
            .expect("a width change detaches the store");
        feed_numbered_lines(&mut grid, 0, Grid::ASYNC_COMPRESS_BACKPRESSURE);
        assert!(
            grid.lazy_backlog_len() > Grid::ASYNC_COMPRESS_BACKPRESSURE / 2,
            "precondition: past the mark had the store been here"
        );
        assert_eq!(grid.flood_promotion_due(), 0);
        let before = grid.lazy_backlog_len();
        assert_eq!(grid.drain_lazy_pressure_bounded(BATCH), before);
        grid.reattach_reflowed_scrollback(pending.reflow());
        grid.assert_invariants();
    }

    #[test]
    fn a_promoted_flood_under_the_floor_budget_leaves_no_cut() {
        let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
        assert_floor_is_the_cap(&grid);
        let mut fed = 0;
        while fed < 40_000 {
            feed_numbered_lines(&mut grid, fed, 1_000);
            fed += 1_000;
            relieve(&mut grid);
            grid.trickle_lazy_bounded(BATCH);
        }
        assert_eq!(grid.truncated_lines(), 0, "nothing lost");
        let hist = history_trimmed(&grid);
        assert!(assert_no_silent_hole(&hist, 0).is_empty(), "no marker");
        assert_eq!(hist, newest(40_000, 40_000 - 2), "every line, contiguous");
        grid.assert_invariants();
    }

    fn row_bytes(width: usize) -> usize {
        std::mem::size_of::<super::super::scroll_convert::DeferredLine>()
            + width * std::mem::size_of::<crate::Cell>()
    }

    #[test]
    fn a_flood_the_line_limit_holds_never_makes_a_promotion_due() {
        for (cols, width) in [(80u16, 80usize), (120, 100)] {
            assert!(
                100_000 * row_bytes(width) < DEFAULT_BUDGET,
                "precondition: 100k raw rows of {width} cells fit the budget"
            );
            let mut grid = flooded_grid(cols, 8, DEFAULT_BUDGET);
            grid.set_scrollback_line_limit(Some(100_000));
            let mut past_half = false;
            let mut fed = 0;
            while fed < 120_000 {
                feed_long(&mut grid, fed, 300, width);
                fed += 300;
                assert_eq!(
                    grid.flood_promotion_due(),
                    0,
                    "{width} cells at {cols} columns, {fed} lines in"
                );
                let store = grid.scrollback().expect("a tiered store");
                let headroom = store.memory_budget() - store.budgeted_memory_used();
                past_half |= grid.storage.lazy_buffer.outweighs(headroom / 2);
            }
            assert!(
                past_half,
                "the backlog of {width}-cell rows passed half the headroom"
            );
            assert_eq!(grid.truncated_lines(), 0, "nothing was cut");
            let hist = history_trimmed(&grid);
            let top = window_top(120_000, 100_000);
            assert!(assert_no_silent_hole(&hist, top).is_empty(), "no marker");
            assert_eq!(hist.len(), 100_000, "the whole configured limit");
            assert_eq!(line_number(&hist[0]), top);
            grid.assert_invariants();
        }
    }

    fn widening_flood(relieving: bool) -> (Grid, Option<bool>) {
        let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(100_000));
        let mut fed = 0;
        while fed < 100_000 {
            feed_numbered_lines(&mut grid, fed, 1_000);
            fed += 1_000;
            assert_eq!(grid.flood_promotion_due(), 0, "short rows: none due");
        }
        let mut average_lagged = None;
        while fed < 160_000 {
            feed_long(&mut grid, fed, 300, 230);
            fed += 300;
            if !relieving {
                continue;
            }
            if average_lagged.is_none() && grid.flood_promotion_due() > 0 {
                let buffer = &grid.storage.lazy_buffer;
                let store = grid.scrollback().expect("a tiered store");
                let headroom = store.memory_budget() - store.budgeted_memory_used();
                let share = store.line_limit().expect("a line limit");
                let average = buffer.staged_bytes() / buffer.staged_rows();
                average_lagged = Some(average * share <= headroom);
            }
            relieve(&mut grid);
        }
        (grid, average_lagged)
    }

    #[test]
    fn a_flood_that_turns_wide_after_its_backlog_filled_is_promoted_before_it_cuts() {
        let (mut grid, average_lagged) = widening_flood(true);
        assert_eq!(grid.truncated_lines(), 0, "nothing was cut");
        assert_eq!(
            average_lagged,
            Some(true),
            "relief began while the whole backlog's average projected no cut"
        );
        while grid.drain_lazy_bounded(BATCH) > 0 {}
        let hist = history_trimmed(&grid);
        let top = window_top(160_000, 100_000);
        assert!(assert_no_silent_hole(&hist, top).is_empty(), "no marker");
        assert_eq!(hist.len(), 100_000, "the whole configured limit");
        assert_eq!(line_number(&hist[0]), top);
        assert_eq!(line_number(&hist[hist.len() - 1]), 160_000 - 3);
        grid.assert_invariants();

        let (starved, _) = widening_flood(false);
        assert!(
            starved.truncated_lines() > 0,
            "the unrelieved flood kept {} lines and lost none",
            starved.scrollback_lines()
        );
    }

    #[test]
    fn the_projection_weighs_the_flood_by_its_newest_rows() {
        let cap = 100_000;
        let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
        feed_numbered_lines(&mut grid, 0, 5_000);
        feed_long(&mut grid, 5_000, 2_000, 253);
        let buffer = &grid.storage.lazy_buffer;
        let average = buffer.staged_bytes() / buffer.staged_rows();
        let peak = buffer.projected_peak(cap, Grid::FLOOD_WEIGHT_SAMPLE);
        assert_eq!(peak, cap * row_bytes(253), "cap rows of the newest weight");
        assert!(peak > 2 * average * cap, "far above the whole average");

        let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
        feed_long(&mut grid, 0, 5_000, 253);
        feed_numbered_lines(&mut grid, 5_000, 2_000);
        let buffer = &grid.storage.lazy_buffer;
        let staged = buffer.staged_bytes();
        let peak = buffer.projected_peak(cap, Grid::FLOOD_WEIGHT_SAMPLE);
        assert!(peak >= staged, "the staged rows are part of the peak");
        assert!(
            peak < staged + cap * row_bytes(16),
            "the rows to come weigh what the newest (short) rows do"
        );
    }

    #[test]
    fn a_line_share_within_the_floor_never_makes_a_promotion_due() {
        let mut grid = flooded_grid(253, 8, FLOOR_BUDGET);
        grid.set_scrollback_line_limit(Some(15_000));
        feed_long(&mut grid, 0, 30_000, 253);
        assert!(
            grid.storage.lazy_buffer.outweighs(FLOOR_BUDGET),
            "precondition: far past the mark"
        );
        assert_eq!(grid.flood_promotion_due(), 0);
        assert_eq!(grid.truncated_lines(), 0, "nothing was cut");
    }
}
