// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Line feeds coalesced at the bottom of a scroll region that keeps no history: the grid side of
//! a run of text lines a parser sink takes whole (`aterm_parser::ActionSink::print_lines`). A run
//! of `k` line feeds there is one shift of the region by `k`, so a line that would scroll out of
//! the region before the run ends need never be written at all.

use super::Grid;
use super::row_u16;
use crate::Cell;
use crate::row::LineSize;

/// A run of lines at the bottom of a full screen that [`Grid::begin_screen_text_lines`] began:
/// what its line feeds share, so each [`Grid::screen_text_line`] need not work it out again, and
/// the line feeds whose per-row bookkeeping waits for the run's next flush. A run ends with
/// [`Grid::settle_screen_text_lines`], which flushes it.
#[derive(Debug)]
#[must_use = "a run's deferred line feeds are flushed only by settle_screen_text_lines"]
pub struct ScreenTextLines {
    /// Whether the row each line feed takes from the ring is staged for the store rather than
    /// dropped.
    stages: bool,
    /// Line feeds since the last flush: their staged rows' flood shed, the absolute row count
    /// and the history's carried count wait for it.
    deferred: usize,
    /// The deferred count at which the run flushes: 1, every line feed, where the shed cannot
    /// wait (`Grid::screen_text_shed_batch`).
    flush_at: usize,
    /// The staged bytes past which the run flushes before the next row could bring the memory
    /// bound into play.
    byte_limit: usize,
}

impl Grid {
    /// Whether a scroll of a region anchored at row 0 archives what leaves it: the grid keeps
    /// history (a ring, a tiered store, or one detached for reflow).
    #[inline]
    pub(super) const fn top_anchored_scroll_archives(&self) -> bool {
        self.storage.max_scrollback > 0
            || self.storage.scrollback.is_some()
            || self.storage.scrollback_detached_for_reflow
    }

    /// The scroll region `(top, bottom)` when the cursor is on its bottom row and a line feed
    /// there shifts the region without archiving anything: a region of two or more rows, not the
    /// whole screen, and not anchored at row 0 of a grid that keeps history. `None` everywhere
    /// else, and wherever double-width rows or DECLRMM margins could change what a line feed or
    /// a write does.
    #[must_use]
    #[inline]
    pub fn line_run_region(&self) -> Option<(u16, u16)> {
        let region = self.storage.scroll_region;
        if self.storage.cursor.row != region.bottom
            || region.top >= region.bottom
            || region.is_full(self.storage.visible_rows)
            || self.storage.any_double_width
            || self.storage.has_horizontal_margins
            || (region.top == 0 && self.top_anchored_scroll_archives())
        {
            return None;
        }
        Some((region.top, region.bottom))
    }

    /// `k` line feeds at the bottom of the scroll region [`Self::line_run_region`] names, as one
    /// shift of the region. The grid is left as `k` calls of [`Self::line_feed`] leave it: the
    /// region's rows move up `k` (all of them leave once `k` reaches its height) and the rows
    /// that enter are blank in the cursor's template, with the extras, the row-band record, the
    /// anchors, the selection damage and the render damage as after `k` one-line shifts, and the
    /// pending wrap cleared. Only `content_gen` and the damage mark clock advance once rather
    /// than `k` times; each still says only that the grid changed.
    pub fn line_feeds_at_region_bottom(&mut self, k: usize) {
        debug_assert!(
            self.line_run_region().is_some(),
            "line_feeds_at_region_bottom: the cursor is not at the bottom of a history-free region"
        );
        if k == 0 {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        let k = k - usize::from(crate::test_counters::short_line_run_shift());
        self.reset_display_offset_with_damage();
        let top = usize::from(self.storage.scroll_region.top);
        let bottom = usize::from(self.storage.scroll_region.bottom);
        let n = k.min(bottom - top + 1);
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_coalesced_shift(k, bottom - top + 1);

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
        self.storage
            .extras
            .shift_region_up_by(top_u16, bottom_u16, row_u16(n));
        self.fill_bce_rgb_rows(row_u16(bottom + 1 - n)..bottom_u16.saturating_add(1));

        self.storage
            .presentation
            .record_row_band_moves(top_u16, bottom_u16, -1, k);
        self.anchors_band_move(top_u16, bottom_u16, -i64::try_from(k).unwrap_or(i64::MAX));
        self.damage_selection_visible_rows_ext(top_u16, bottom_u16, true);
        self.storage
            .mark_content_rows(top_u16, bottom_u16.saturating_add(1));
        self.storage.clear_pending_wrap();
    }

    /// Whether the cursor is on the bottom row of a scroll region that is the whole screen, where a
    /// line feed scrolls the screen up (into history when the grid keeps one): the line feeds
    /// [`Self::screen_line_feed`] takes. `false` wherever double-width rows or DECLRMM margins
    /// could change what a line feed does.
    #[must_use]
    #[inline]
    pub fn at_screen_line_run(&self) -> bool {
        let region = self.storage.scroll_region;
        self.storage.cursor.row == region.bottom
            && region.is_full(self.storage.visible_rows)
            && !self.storage.any_double_width
            && !self.storage.has_horizontal_margins
    }

    /// One line feed where [`Self::at_screen_line_run`] holds, as [`Self::line_feed`] takes it,
    /// except for what [`Self::settle_screen_line_feeds`] records once for a run of them: the
    /// screen scrolls up one row (the row that leaves it goes to history as `scroll_up` sends it),
    /// the new bottom row is blank in the cursor's template, and the pending wrap is cleared.
    #[inline]
    pub fn screen_line_feed(&mut self) {
        debug_assert!(self.at_screen_line_run());
        self.reset_display_offset_with_damage();
        self.drop_fill_debt();
        self.scroll_up_storage(1);
        self.storage.clear_pending_wrap();
    }

    /// Record `k` [`Self::screen_line_feed`]s as `k` line feeds record them: the content scroll
    /// delta grows by `k` and the bottom row is damaged (each line feed damages only the row that
    /// enters). The content generation and the damage mark clock advance once.
    pub fn settle_screen_line_feeds(&mut self, k: usize) {
        if k == 0 {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_screen_settle(k);
        #[cfg(any(test, debug_assertions))]
        let k = k - usize::from(crate::test_counters::short_screen_settle());
        let delta = i32::try_from(k).unwrap_or(i32::MAX);
        self.storage.content_scroll_delta = self.storage.content_scroll_delta.saturating_add(delta);
        self.mark_scroll_damage(1);
    }

    /// Begin a run of [`Self::screen_text_line`]s where [`Self::at_screen_line_run`] holds and
    /// every line feed of the run would take the same steps:
    ///
    /// - the ring is full, so each line feed reuses its oldest row;
    /// - the extras hold nothing at all (no map entry, live or stale, and no dense ring), so a row
    ///   that scrolls off carries none, the extras' shift moves nothing and a plain write leaves
    ///   none behind;
    /// - the cursor's template is the empty cell, so the row that enters is blank and empty;
    /// - the history's links are within their budget. No line feed strips any, since staging
    ///   and shedding rows never adds link bytes.
    ///
    /// Then this brings the view to the live bottom and drops the reflow's fill debt, which only
    /// the run's first line feed would do, and returns the run's [`ScreenTextLines`]. Elsewhere
    /// it returns `None` and changes nothing.
    #[must_use]
    pub fn begin_screen_text_lines(&mut self) -> Option<ScreenTextLines> {
        let storage = &self.storage;
        let capacity = usize::from(storage.visible_rows).saturating_add(storage.max_scrollback);
        let ready = self.at_screen_line_run()
            && storage.total_lines >= capacity
            && storage.rows.len() == storage.total_lines
            && storage.extras.holds_nothing()
            && self.scrolled_rows_hold_no_extras()
            && storage.cursor_template == Cell::EMPTY
            && storage.cursor_template_bg_rgb.is_none()
            && self.history_link_bytes() <= self.history_link_budget();
        if !ready {
            return None;
        }
        self.reset_display_offset_with_damage();
        self.drop_fill_debt();
        let stages = self.storage.stages_evicted_rows();
        let (flush_at, byte_limit) = self.screen_text_shed_batch(stages);
        Some(ScreenTextLines {
            stages,
            deferred: 0,
            flush_at,
            byte_limit,
        })
    }

    /// One line feed at the bottom of a full screen, in a run [`Self::begin_screen_text_lines`]
    /// began, then `text` (printable ASCII that fits the row from the cursor's column) written
    /// in the default pen where it leaves the cursor. The grid is left as
    /// [`Self::screen_line_feed`] and the blast writer leave it, except for the damage and the
    /// content generation: [`Self::settle_screen_text_lines`] records those once for the run.
    /// The line feed takes the same per-row steps a reused ring row takes there: the oldest row
    /// staged or dropped, then the bound on what staging holds, the absolute row count and the
    /// history's carried count, which a flood's run defers to its next flush (see
    /// [`Self::screen_text_shed_batch`]). The steps the run's preconditions make no-ops are left
    /// out: the extras' shift, the BCE fill and the link budget.
    #[inline]
    pub fn screen_text_line(&mut self, run: &mut ScreenTextLines, text: &[u8]) {
        let cols = self.storage.cols;
        let oldest = self.storage.ring_head;
        self.reuse_quiet_scrolled_row(run.stages, cols);
        if run.flush_at == 1 {
            self.bound_staged_rows();
            self.storage.absolute_row_counter += 1;
            self.storage.note_history_pushed(1);
        } else {
            run.deferred += 1;
            if run.deferred >= run.flush_at || self.storage.lazy_buffer.outweighs(run.byte_limit) {
                self.flush_screen_text_lines(run);
            }
        }
        self.storage.clear_pending_wrap();

        if text.is_empty() {
            return;
        }
        let col = self.storage.cursor.col;
        let end = usize::from(col) + text.len();
        debug_assert!(
            end <= usize::from(cols),
            "screen_text_line: the text overflows the row"
        );
        // The reused row is the new bottom row: the ring's head moved past it.
        #[cfg(any(test, debug_assertions))]
        let oldest = if crate::test_counters::screen_text_slot_one_up() {
            let row_count = self.storage.rows.len();
            super::state::wrap_ring_index(oldest + row_count - 1, row_count)
        } else {
            oldest
        };
        self.storage.rows[oldest].write_plain_ascii(col, text);
        if end >= usize::from(cols) {
            self.storage.cursor.col = cols - 1;
            self.storage.mark_pending_wrap();
        } else {
            self.storage.cursor.col = row_u16(end);
        }
    }

    /// The per-row steps of the line feeds `run` deferred, as those line feeds one at a time
    /// would have taken them: the bound on what staging holds, then the absolute row count and
    /// the history's carried count for them all; then the next batch's limits.
    #[inline(never)]
    fn flush_screen_text_lines(&mut self, run: &mut ScreenTextLines) {
        let feeds = std::mem::take(&mut run.deferred);
        if feeds == 0 {
            return;
        }
        #[cfg(test)]
        let stored = self.stored_line_count();
        self.bound_staged_rows();
        #[cfg(test)]
        crate::test_counters::count_screen_text_flush(feeds, stored, self.stored_line_count());
        self.storage.absolute_row_counter += feeds as u64;
        self.storage.note_history_pushed(feeds);
        (run.flush_at, run.byte_limit) = self.screen_text_shed_batch(run.stages);
    }

    /// The lines the tiered store holds: none without one.
    #[cfg(test)]
    fn stored_line_count(&self) -> usize {
        self.storage
            .scrollback
            .as_ref()
            .map_or(0, |store| store.line_count())
    }

    /// Record a run of `feeds` [`Self::screen_text_line`]s as their line feeds and writes record
    /// it: the line feeds `run` still defers flushed, [`Self::settle_screen_line_feeds`], and the
    /// bottom row's content damage when a line wrote text (each write damages the row it writes,
    /// the bottom one).
    pub fn settle_screen_text_lines(
        &mut self,
        mut run: ScreenTextLines,
        feeds: usize,
        wrote: bool,
    ) {
        self.flush_screen_text_lines(&mut run);
        #[cfg(any(test, debug_assertions))]
        if feeds > 0 {
            crate::test_counters::count_screen_text_run(feeds);
        }
        self.settle_screen_line_feeds(feeds);
        if wrote {
            let bottom = self.storage.scroll_region.bottom;
            self.storage.mark_content_row(bottom);
        }
    }

    /// Put the cursor at `row`, `col` with pending wrap `wrap`, and nothing else: for a run of
    /// text lines that writes a line on the row [`Self::line_feeds_at_region_bottom`] moved it
    /// to, then puts the cursor back where the run's line feeds leave it.
    #[inline]
    pub fn place_line_run_cursor(&mut self, row: u16, col: u16, wrap: bool) {
        debug_assert!(row < self.storage.visible_rows && col < self.storage.cols);
        self.storage.cursor.row = row;
        self.storage.cursor.col = col;
        if wrap {
            self.storage.mark_pending_wrap();
        } else {
            self.storage.clear_pending_wrap();
        }
    }
}
