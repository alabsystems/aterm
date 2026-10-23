// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Width-reflow bottom-anchoring.

use aterm_scrollback::Line;

use super::row_u16;
use super::state::FillDebt;
use super::{Grid, LineSize};
use crate::Damage;

impl Grid {
    /// Count the trailing blank viewport rows (empty, not a wrap continuation) strictly below the cursor, from the bottom up to the first non-blank row.
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

    /// Lift leading soft-wrap continuation rows out of the viewport ring as [`Line`]s so history rewrap processes the whole logical line.
    // COST: O(belt rows × cols) — bounded by the viewport height.
    pub(super) fn take_boundary_continuation_lines(&mut self) -> Vec<Line> {
        let belt = self.boundary_continuation_rows();
        if belt == 0 {
            return Vec::new();
        }

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

    /// How many rows [`Self::take_boundary_continuation_lines`] lifts.
    pub(super) fn boundary_continuation_rows(&self) -> usize {
        let visible = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cursor_row = usize::from(self.storage.cursor.row);
        let mut belt = 0usize;
        while belt < visible.min(cursor_row) {
            match self.storage.row_at_screen(row_u16(belt)) {
                Some(r) if r.is_wrapped() && r.line_size() == LineSize::SingleWidth => belt += 1,
                _ => break,
            }
        }
        belt
    }

    /// The rows at `new_cols` of the lines a rows-only shrink just demoted.
    // COST: O((demoted + belt rows) × cols) — bounded by the viewport height.
    pub(super) fn demoted_rows_at(&self, pushed: u16, demoted: u16, new_cols: u16) -> u16 {
        let mut lines: Vec<Line> = (0..usize::from(demoted))
            .rev()
            .filter_map(|i| {
                self.try_history_line_rev(usize::from(pushed) + i)
                    .ok()
                    .flatten()
                    .map(std::borrow::Cow::into_owned)
            })
            .collect();
        let Some(head) = lines.iter().position(|line| !line.is_wrapped()) else {
            return 0;
        };
        lines.drain(..head);
        for r in 0..self.boundary_continuation_rows() {
            let screen_row = row_u16(r);
            let Some(row) = self.storage.row_at_screen(screen_row) else {
                break;
            };
            let extras = Self::extract_row_extras(row, &self.storage.extras, screen_row);
            let len = super::scrollback_reflow::history_row_len(
                row,
                self.storage.row_at_screen(screen_row + 1),
            );
            lines.push(Self::row_to_line_with_stored_extras_at_len(
                row, &extras, len,
            ));
        }
        row_u16(super::scrollback_reflow::reflow_scrollback_lines(&lines, new_cols).len())
    }

    /// Pull the newest history lines into the top of the viewport until trailing blank rows shrink to `target_blanks`.
    // COST: O(pull × cols) — bounded by the viewport height.
    pub(crate) fn fill_viewport_deficit_from_history(&mut self, target_blanks: usize) -> usize {
        if self.storage.display_offset != 0 {
            return 0;
        }
        let visible = usize::from(self.storage.visible_rows);
        if visible == 0 || self.storage.rows.len() < visible {
            return 0;
        }
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

        let mut lines: Vec<Line> = Vec::with_capacity(pull);
        for i in (0..pull).rev() {
            match self.try_history_line_rev(i) {
                Ok(Some(line)) => lines.push(line.into_owned()),
                Ok(None) | Err(_) => return 0,
            }
        }

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
        self.paint_wrap_holes(0..row_u16(pull));

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

        let max_row = row_u16(visible - 1);
        self.storage.cursor.row = self
            .storage
            .cursor
            .row
            .saturating_add(row_u16(pull))
            .min(max_row);
        if self.storage.saved_cursor.valid {
            self.storage.saved_cursor.cursor.row = self
                .storage
                .saved_cursor
                .cursor
                .row
                .saturating_add(row_u16(pull));
        }

        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        self.force_selection_invalidation();
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
        pull
    }

    /// Stage up to `want` of the newest history lines from lazy/tiered scrollback into the front of the ring.
    // COST: O(want × cols) for the lines, plus one splice of the ring's rows;
    pub(super) fn pull_history_into_ring(&mut self, want: usize) -> usize {
        if want == 0 || self.storage.scrollback_detached_for_reflow {
            return 0;
        }
        let staged = self.storage.lazy_buffer.staged_rows();
        let from_lazy = want.min(staged);
        let from_store = if from_lazy == self.storage.lazy_buffer.len() {
            let held = self
                .storage
                .scrollback
                .as_ref()
                .map_or(0, aterm_scrollback::ScrollbackStorage::line_count);
            (want - from_lazy).min(held)
        } else {
            0
        };
        let pull = from_lazy + from_store;
        if pull == 0 {
            return 0;
        }
        let ring = self.storage.ring_buffer_scrollback();
        let mut lines: Vec<Line> = Vec::with_capacity(pull);
        for i in (0..pull).rev() {
            match self.try_history_line_rev(ring + i) {
                Ok(Some(line)) => lines.push(line.into_owned()),
                Ok(None) | Err(_) => return 0,
            }
        }
        self.storage
            .lazy_buffer
            .drain_back(from_lazy)
            .for_each(drop);
        if from_store > 0
            && let Some(store) = self.storage.scrollback.as_mut()
            && let Err(error) = store.remove_newest(from_store)
        {
            // Lines remain duplicated in history rather than lost.
            aterm_log::warn!("rows grow: failed to remove {from_store} history lines: {error}");
        }
        let cols = self.storage.cols;
        self.seat_lines_at_ring_front(&lines, cols)
    }

    /// The rows a detach window owes its re-attach's fill as a resize pass begins ([`FillDebt`]).
    pub(super) fn fill_debt_at_pass_start(&mut self) -> usize {
        if !self.storage.scrollback_detached_for_reflow {
            return 0;
        }
        let trimmed = self.trimmed_rows();
        let debt = &mut self.storage.fill_debt;
        debt.trimmed = debt.trimmed.min(trimmed);
        debt.rows
    }

    /// Settle a detach window's fill debt after the rows move of a rows-only pass inside it (`grid::resize_handback`'s edge moves).
    pub(super) fn settle_fill_debt_rows(&mut self, owed: usize) {
        if !self.storage.scrollback_detached_for_reflow {
            return;
        }
        let handed_back = usize::from(self.storage.last_resize_shape.appended).saturating_sub(owed);
        let trailing = self.trailing_blank_rows_below_cursor();
        let debt = &mut self.storage.fill_debt;
        let kept = debt.rows.min(trailing);
        debt.trimmed = debt.trimmed.saturating_add(row_u16(debt.rows - kept));
        let again = handed_back.min(usize::from(debt.trimmed));
        debt.trimmed -= row_u16(again);
        debt.rows = kept + owed + again;
    }

    /// A width pass's bottom-anchoring fill.
    pub(super) fn fill_after_width_pass(
        &mut self,
        owed: usize,
        pre: usize,
        refilled: usize,
        refill_owed: u16,
    ) {
        let back = refilled.min(usize::from(refill_owed));
        let target = pre - owed.min(pre) + refilled - back;
        self.fill_viewport_deficit_from_history(target);
        if self.owe_fill_after_width_pass(target) {
            let debt = &mut self.storage.fill_debt;
            debt.trimmed = debt.trimmed.saturating_sub(row_u16(back));
        }
    }

    /// After a width pass's bottom-anchoring fill toward `target` blank rows under the cursor.
    pub(super) fn owe_fill_after_width_pass(&mut self, target: usize) -> bool {
        if !self.storage.scrollback_detached_for_reflow {
            return false;
        }
        let trailing = self.trailing_blank_rows_below_cursor();
        self.storage.fill_debt.rows = trailing.saturating_sub(target);
        true
    }

    /// Pay a detach window's fill debt at the re-attach that brought the history back.
    pub(super) fn pay_fill_debt(&mut self) -> usize {
        let owed = std::mem::take(&mut self.storage.fill_debt).rows;
        if owed == 0 {
            return 0;
        }
        let trailing = self.trailing_blank_rows_below_cursor();
        self.fill_viewport_deficit_from_history(trailing - owed.min(trailing))
    }

    /// Drop the offloaded reflow's fill debt.
    #[inline]
    pub(super) fn drop_fill_debt(&mut self) {
        self.storage.fill_debt = FillDebt::default();
    }

    /// Drop the `n` newest ring-scrollback rows and their `ring_extras` after re-seating them into the viewport.
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
        self.forget_pushed_rows();
    }
}
