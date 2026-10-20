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
//! Both run inside the synchronous width-reflow resize; inside an offloaded
//! reflow's detach window the history is with the job, so the fill comes up
//! short and the re-attach pays what is owed ([`FillDebt`]).

use aterm_scrollback::Line;

use super::row_u16;
use super::state::FillDebt;
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
        let belt = self.boundary_continuation_rows();
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

    /// How many rows [`Self::take_boundary_continuation_lines`] lifts: the
    /// maximal prefix of visible rows that are wrapped single-width
    /// continuations, strictly above the cursor row.
    pub(super) fn boundary_continuation_rows(&self) -> usize {
        let visible = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cursor_row = usize::from(self.storage.cursor.row);
        let mut belt = 0usize;
        // Content coordinates, as the extraction reads them (see
        // `trailing_blank_rows_below_cursor`).
        while belt < visible.min(cursor_row) {
            match self.storage.row_at_screen(row_u16(belt)) {
                Some(r) if r.is_wrapped() && r.line_size() == LineSize::SingleWidth => belt += 1,
                _ => break,
            }
        }
        belt
    }

    /// The rows at `new_cols` of the lines a rows-only shrink just demoted:
    /// the `demoted` history rows under the `pushed` newest (the bottom-push
    /// corner rotates its rows in after them). Each logical line that opens
    /// among them counts whole, its tail still on the screen (the rows
    /// [`Self::take_boundary_continuation_lines`] lifts) with it, rewrapped as
    /// the width change rewraps it. A demoted row that continues an older
    /// history line is that line's, which one pass lifted off the screen too,
    /// so it counts nothing. A row that cannot be read counts nothing.
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
        self.paint_wrap_holes(0..row_u16(pull));

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

    /// Bring up to `want` of the newest history lines held past the ring (the
    /// lazy buffer's staged rows, then the tiered store's) into the front of
    /// the ring, oldest first, so a rows-only grow reveals them as it reveals
    /// ring rows: the reveal draws on history wherever the grid keeps it, as
    /// the deficit fill does. Without it a grid whose history had gone to its
    /// store (a rewrap, a restore from a checkpoint) or whose demoted rows the
    /// ring could not hold grew by appending blanks where the same grid with a
    /// larger ring revealed history. Returns how many lines came.
    ///
    /// The store is reached only through an empty lazy buffer (its lines sit
    /// behind the staged ones and an open flood cut's marker, which stays). The
    /// lines move between tiers only, so every retained line keeps its absolute
    /// row and the counter does not move. Nothing moves during a detach window
    /// (the store is with the worker) or when a line cannot be read.
    // COST: O(want × cols) for the lines, plus one splice of the ring's rows;
    // `want` is bounded by the grow.
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
        // Read every line before any destructive step, oldest first: a decode
        // failure moves nothing.
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

    /// The rows a detach window owes its re-attach's fill as a resize pass
    /// begins ([`FillDebt`]). Of the trimmed rows owed back, only those the
    /// rows hand-back still owes can stand for owed rows: output that scrolled
    /// or a rewrap that used trimmed rows up took them first.
    pub(super) fn fill_debt_at_pass_start(&mut self) -> usize {
        if !self.storage.scrollback_detached_for_reflow {
            return 0;
        }
        let trimmed = self.trimmed_rows();
        let debt = &mut self.storage.fill_debt;
        debt.trimmed = debt.trimmed.min(trimmed);
        debt.rows
    }

    /// Settle a detach window's fill debt after the rows move of a rows-only
    /// pass inside it (`grid::resize_handback`'s edge moves). A shrink trims
    /// the blank rows the synchronous resize trims, then those that stand for
    /// owed rows, which that resize demotes instead: no longer owed, but the
    /// trimmed rows a later grow appends back are owed again, where that
    /// resize reveals the demoted rows. A grow owes the `owed` rows it
    /// appended for history the job holds (`pull_history_into_ring` moves
    /// nothing inside a window), which the synchronous grow reveals.
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

    /// A width pass's bottom-anchoring fill: pull the newest history back
    /// until the trailing band holds the `pre` blank rows it measured before
    /// the rows moved and the `refilled` blanks it put back. Inside a detach
    /// window the synchronous pass measured `owed` fewer (the rows owed then
    /// stand for its history rows), and of the blanks put back, those that
    /// stood for owed rows (`refill_owed`, `Refill`) are demoted history rows
    /// it pulled back; what the window cannot fill from the rows it holds is
    /// owed to the re-attach.
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

    /// After a width pass's bottom-anchoring fill toward `target` blank rows
    /// under the cursor: inside a detach window, owe the re-attach the blank
    /// rows past `target` the fill could not pull from the rows the grid holds
    /// ([`FillDebt`]). Whether a detach window is open.
    pub(super) fn owe_fill_after_width_pass(&mut self, target: usize) -> bool {
        if !self.storage.scrollback_detached_for_reflow {
            return false;
        }
        let trailing = self.trailing_blank_rows_below_cursor();
        self.storage.fill_debt.rows = trailing.saturating_sub(target);
        true
    }

    /// Pay a detach window's fill debt at the re-attach that brought the
    /// history back: pull as many history rows down over the blank rows that
    /// stood for them, as the synchronous resizes revealed and filled them.
    /// Returns the rows pulled.
    pub(super) fn pay_fill_debt(&mut self) -> usize {
        let owed = std::mem::take(&mut self.storage.fill_debt).rows;
        if owed == 0 {
            return 0;
        }
        let trailing = self.trailing_blank_rows_below_cursor();
        self.fill_viewport_deficit_from_history(trailing - owed.min(trailing))
    }

    /// Drop the offloaded reflow's fill debt. An op that erases the rows it
    /// stands for, moves the screen past them or addresses the screen around
    /// them during the detach window (ED and DECSED 2, SU, absolute cursor
    /// positioning to another row through [`Grid::set_cursor`], a width change
    /// that pushes rows off the bottom) calls this:
    /// filling them would resurrect pre-clear history or move what the program
    /// drew. A move that follows the content
    /// ([`Grid::set_cursor_following_content`]: DECRC, the 1049 restore, VT52
    /// cursor up and down, the reverse wrap) keeps it. No-op outside a detach.
    #[inline]
    pub(super) fn drop_fill_debt(&mut self) {
        self.storage.fill_debt = FillDebt::default();
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
        // The counter does not move, so the rows hand-back must not go on
        // owing the pushed rows as the newest lines (`grid::resize_handback`).
        self.forget_pushed_rows();
    }
}
