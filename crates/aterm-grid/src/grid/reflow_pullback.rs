// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Width-reflow bottom-anchoring: the belt lift and the deficit fill.
//!
//! A width change rewraps the viewport and the off-screen history separately
//! (`reflow` / `scrollback_reflow`), which leaves two seams:
//!
//! * a logical line whose soft-wrapped tail sits at the top of the viewport
//!   (head in history) would re-split there on every resize;
//!   [`Grid::take_boundary_continuation_lines`] lifts that tail into the
//!   history rewrap so the line rewraps as one unit;
//! * a width grow unwraps content into fewer rows, which would pad the bottom
//!   of the viewport with blanks; [`Grid::fill_viewport_deficit_from_history`]
//!   pulls the newest history back in until the trailing-blank band returns to
//!   its pre-resize count.
//!
//! Both run inside the synchronous width-reflow resize; the offloaded path runs
//! the fill at re-attach against the `pending_fill_target` captured at detach.

use aterm_scrollback::Line;

use super::row_u16;
use super::{Grid, LineSize};
use crate::Damage;

impl Grid {
    /// Count the trailing blank viewport rows (empty, not a wrap continuation)
    /// strictly below the cursor, from the bottom up to the first non-blank row.
    ///
    /// Reads content rows via `row_at_screen`, not the `display_offset`-mapped
    /// `row()`: the offloaded detach captures this before `resize()` zeroes the
    /// offset.
    pub(super) fn trailing_blank_rows_below_cursor(&self) -> usize {
        let visible = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cursor_row = usize::from(self.storage.cursor.row);
        let mut blanks = 0usize;
        for r in (0..visible).rev() {
            if r <= cursor_row {
                break;
            }
            match self.storage.row_at_screen(row_u16(r)) {
                Some(row) if row.is_empty() && !row.is_wrapped() => blanks += 1,
                _ => break,
            }
        }
        blanks
    }

    /// Lift the viewport's leading soft-wrap continuation rows (the tail of a
    /// logical line whose head is the newest history line) out of the ring as
    /// [`Line`]s, so the history rewrap processes the whole line in one pass.
    ///
    /// The belt is the maximal prefix of visible rows that are wrapped
    /// single-width continuations, strictly above the cursor row. Extras ride in
    /// the produced `Line`s; cursor, saved cursor and live extras shift up by
    /// the lifted count. Callers append the result to the ring scrollback they
    /// just took (`take_scrollback_lines` / `take_ring_scrollback_lines`).
    // COST: O(belt rows × cols) — bounded by the viewport height.
    pub(super) fn take_boundary_continuation_lines(&mut self) -> Vec<Line> {
        let visible = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cursor_row = usize::from(self.storage.cursor.row);
        let mut belt = 0usize;
        // Content coordinates, as in the extraction below (see
        // `trailing_blank_rows_below_cursor`).
        while belt < visible.min(cursor_row) {
            match self.storage.row_at_screen(row_u16(belt)) {
                Some(r) if r.is_wrapped() && r.line_size() == LineSize::SingleWidth => belt += 1,
                _ => break,
            }
        }
        if belt == 0 {
            return Vec::new();
        }

        // Linearize so Vec order == logical order, then address the belt rows
        // at their physical positions just after any (residual) ring history.
        self.storage.linearize_ring();
        let hist = self
            .storage
            .total_lines
            .saturating_sub(usize::from(self.storage.visible_rows));

        let mut lines = Vec::with_capacity(belt);
        for i in 0..belt {
            let extracted = Self::extract_row_extras(
                &self.storage.rows[hist + i],
                &self.storage.extras,
                row_u16(i),
            );
            let row = &self.storage.rows[hist + i];
            // Read as a history row is: a belt row its successor continues
            // keeps every cell but a recorded early-wrap hole.
            let len =
                super::scrollback_reflow::history_row_len(row, self.storage.rows.get(hist + i + 1));
            lines.push(Self::row_to_line_with_stored_extras_at_len(
                row, &extracted, len,
            ));
        }
        self.storage.recycle_rows(hist..hist + belt);
        self.storage.total_lines -= belt;
        let belt_u16 = row_u16(belt);
        let old_bottom = self.storage.visible_rows.saturating_sub(1);
        self.storage.visible_rows -= belt_u16;
        // Live extras follow their rows up; the belt rows' entries drop (their
        // content now rides the extracted `Line`s).
        self.storage
            .extras
            .shift_region_up_by(0, old_bottom, belt_u16);
        self.storage.cursor.row -= belt_u16;
        if self.storage.saved_cursor.valid {
            self.storage.saved_cursor.cursor.row = self
                .storage
                .saved_cursor
                .cursor
                .row
                .saturating_sub(belt_u16);
        }
        lines
    }

    /// Pull the newest history lines back into the top of the viewport until the
    /// trailing-blank band below the cursor shrinks to `target_blanks` (its
    /// pre-resize count). This bottom-anchors a width grow and rejoins the line
    /// the belt lift handed to the history rewrap. Content moves down by the
    /// pulled count; pulled lines are already wrapped at the current width.
    ///
    /// Returns 0 without effect when scrolled back, under a partial scroll
    /// region, or with nothing to pull. Only blanks the reflow created are
    /// filled; `target_blanks` protects genuine blank rows.
    // COST: O(pull × cols) — bounded by the viewport height.
    pub(crate) fn fill_viewport_deficit_from_history(&mut self, target_blanks: usize) -> usize {
        if self.storage.display_offset != 0 {
            return 0;
        }
        let visible = usize::from(self.storage.visible_rows);
        if visible == 0 || self.storage.rows.len() < visible {
            return 0;
        }
        // A partial scroll region is TUI-owned layout: never re-seat rows under it.
        if usize::from(self.storage.scroll_region.top) != 0
            || usize::from(self.storage.scroll_region.bottom) != visible - 1
        {
            return 0;
        }
        let deficit = self
            .trailing_blank_rows_below_cursor()
            .saturating_sub(target_blanks);
        if deficit == 0 {
            return 0;
        }
        // Pull only from tiers whose newest lines can be removed without
        // reordering: the ring, then the tiered store when no lazy-staged lines
        // sit between them.
        let ring = self.storage.ring_buffer_scrollback();
        let avail = if self.storage.lazy_buffer_lines() > 0 {
            ring
        } else {
            ring + self.storage.tiered_scrollback_lines()
        };
        let pull = deficit.min(avail);
        if pull == 0 {
            return 0;
        }

        // Read every line before any destructive step (as unscroll
        // does): a decode failure aborts the fill instead of half-moving.
        let mut lines: Vec<Line> = Vec::with_capacity(pull);
        for i in (0..pull).rev() {
            match self.try_history_line_rev(i) {
                Ok(Some(line)) => lines.push(line.into_owned()),
                Ok(None) | Err(_) => return 0,
            }
        }

        // Make room: content slides down by `pull`; the vacated top rows are
        // the (blank) former bottom rows, cleared by the fill below.
        let bottom = visible - 1;
        self.shift_rows_down(0, bottom, pull);
        self.storage
            .extras
            .shift_region_down_by(0, row_u16(bottom), row_u16(pull));
        let cols = self.storage.cols;
        for (i, line) in lines.iter().enumerate() {
            if let Some(r) = self.row_mut(row_u16(i)) {
                // Recycled blank rows must not leak a stale DECDWL flag.
                r.set_line_size(LineSize::SingleWidth);
            }
            self.fill_row_from_line(row_u16(i), line, cols);
        }

        // Remove the pulled lines from history — ring first (they are the
        // newest), then the tiered store for any remainder.
        let from_ring = pull.min(ring);
        if from_ring > 0 {
            self.drop_newest_ring_scrollback(from_ring);
        }
        let from_tiered = pull - from_ring;
        if from_tiered > 0
            && let Some(scrollback) = self.storage.scrollback.as_mut()
            && let Err(error) = scrollback.remove_newest(from_tiered)
        {
            // Lines remain duplicated in history rather than lost.
            aterm_log::warn!("deficit fill: failed to remove {from_tiered} history lines: {error}");
        }

        // The cursor (and saved cursor) follow their content down.
        let max_row = row_u16(visible - 1);
        self.storage.cursor.row = self
            .storage
            .cursor
            .row
            .saturating_add(row_u16(pull))
            .min(max_row);
        // The saved cursor is not clamped (DECRC clamps what it reads): one on
        // a blank row the fill pushed off the bottom stays past the grid, as a
        // trimmed row's slot does.
        if self.storage.saved_cursor.valid {
            self.storage.saved_cursor.cursor.row = self
                .storage
                .saved_cursor
                .cursor
                .row
                .saturating_add(row_u16(pull));
        }

        // History lost its newest suffix, so older lines' absolute keys shift.
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        self.force_selection_invalidation();
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
        pull
    }

    /// Drop the offloaded reflow's deficit-fill debt. An op that mints genuine
    /// blank rows during the detach window (ED/DECSED 0 and 2, DL, SU) calls this:
    /// filling them would resurrect pre-clear history. No-op outside a detach.
    #[inline]
    pub(super) fn invalidate_pending_fill_target(&mut self) {
        self.storage.pending_fill_target = None;
    }

    /// Drop the `n` newest ring-scrollback rows (directly above the viewport)
    /// and their `ring_extras` after the deficit fill or a Kitty CSI + T
    /// unscroll re-seated them into the viewport.
    pub(super) fn drop_newest_ring_scrollback(&mut self, n: usize) {
        let hist = self.storage.ring_buffer_scrollback();
        let n = n.min(hist);
        if n == 0 {
            return;
        }
        self.storage.linearize_ring();
        let keep = hist - n;
        self.storage.recycle_rows(keep..hist);
        while self.storage.ring_extras.len() > keep {
            self.storage.ring_extras.pop_back();
        }
        self.storage.total_lines -= n;
    }
}
