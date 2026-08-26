// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Width-reflow bottom-anchoring (fixwave5): the belt lift and the deficit fill.
//!
//! A width change rewraps the viewport and the off-screen history SEPARATELY
//! (`reflow` / `scrollback_reflow`), which leaves two seams the feel audit
//! caught as permanent wrap-topology corruption after a rapid width sweep:
//!
//! * a logical line whose soft-wrapped tail sat at the TOP of the viewport
//!   (head in history) re-split at the seam on every resize and never
//!   rejoined — [`Grid::take_boundary_continuation_lines`] lifts that tail
//!   into the history rewrap so the line rewraps as one unit;
//! * a width-GROW unwraps content into fewer rows and used to pad the bottom
//!   of the viewport with blanks, stranding the prompt mid-window while the
//!   content above it stayed in history — [`Grid::fill_viewport_deficit_from_history`]
//!   pulls the newest history back in until the trailing-blank band returns
//!   to its pre-resize count, bottom-anchoring the viewport.
//!
//! Both run inside the synchronous width-reflow resize; the offloaded path
//! (history detached for an off-thread rewrap) runs the fill at re-attach
//! instead, against the `pending_fill_target` captured at detach.

use aterm_scrollback::Line;

use super::row_u16;
use super::{Grid, LineSize};
use crate::Damage;

impl Grid {
    /// Count the trailing viewport rows STRICTLY below the cursor that are
    /// blank (empty, not a wrap continuation) — the band a width-grow reflow
    /// pads. Counted from the bottom up to the first non-blank row.
    ///
    /// Reads CONTENT rows via `row_at_screen`, never the `display_offset`-mapped
    /// `row()`: the offloaded detach captures this target BEFORE `resize()`
    /// zeroes the offset, and the cursor lives in the content frame — a
    /// scrolled-back reader must not shift which rows the count sees.
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

    /// Lift the viewport's LEADING soft-wrap continuation rows — the tail of a
    /// logical line whose head is the newest history line — out of the ring as
    /// materialized [`Line`]s, so a width-change rewrap can process the whole
    /// boundary-straddling logical line in ONE pass (the history rewrap).
    ///
    /// The belt is the maximal prefix of visible rows that are wrapped
    /// single-width continuations, capped strictly below the cursor row (the
    /// cursor must stay in the viewport). Extras ride along inside the
    /// produced `Line`s exactly as ring-scrollback extraction does. The
    /// cursor, saved cursor and live extras are shifted up by the lifted count.
    ///
    /// Callers run this immediately after the ring scrollback has been taken
    /// (`take_scrollback_lines` / `take_ring_scrollback_lines`), appending the
    /// result to the taken history so the belt joins its head for the rewrap.
    // COST: O(belt rows × cols) — bounded by the viewport height.
    pub(super) fn take_boundary_continuation_lines(&mut self) -> Vec<Line> {
        let visible = usize::from(self.storage.visible_rows).min(self.storage.rows.len());
        let cursor_row = usize::from(self.storage.cursor.row);
        let mut belt = 0usize;
        // Detect the belt in CONTENT coordinates (`row_at_screen`), matching the
        // physical extraction below. The offloaded detach runs this BEFORE
        // `resize()` zeroes `display_offset`; the offset-mapped `row()` would
        // probe the SCROLLED view here and then extract different rows at
        // `hist + i` — shredding a scrolled-back reader's viewport.
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
        if self.storage.ring_head != 0 {
            let ring_head = self.storage.ring_head;
            self.storage.rows.rotate_left(ring_head);
            self.storage.ring_head = 0;
        }
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
                self.styles(),
            );
            let row = &self.storage.rows[hist + i];
            // A belt row whose successor (the next belt row, or the first
            // remaining viewport row for the last) is a wrap continuation was
            // filled to its last column by autowrap: materialize its trailing
            // blank cells too, or the sweep erodes a mid-line space at the
            // boundary (fixwave5). EXCEPT when the continuation opens with a
            // WIDE cell: a wide char that cannot start at the last column
            // EARLY-WRAPS, leaving that cell unwritten — materializing it
            // would inject a phantom space before the wide char.
            let successor = self.storage.rows.get(hist + i + 1);
            let len = if row.line_size() == LineSize::SingleWidth
                && successor.is_some_and(super::Row::is_wrapped)
            {
                // Autowrap filled this row — its trailing blanks are content.
                // A successor OPENING wide means exactly ONE cell (the early-
                // wrap hole) was never written; real trimmed spaces before it
                // still materialize.
                let hole = successor
                    .and_then(|r| r.as_slice().first())
                    .is_some_and(super::Cell::is_wide);
                row.cols() - u16::from(hole)
            } else {
                row.len()
            };
            lines.push(Self::row_to_line_with_stored_extras_at_len(
                row, &extracted, len,
            ));
        }
        drop(self.storage.rows.drain(hist..hist + belt));
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

    /// Pull the newest history lines back into the TOP of the viewport until
    /// the trailing-blank band below the cursor shrinks to `target_blanks`
    /// (its pre-resize count) — the bottom-anchor step of a width-grow reflow,
    /// and the step that rejoins the boundary line the belt lift handed to the
    /// history rewrap. Content (the cursor's row included) moves DOWN by the
    /// pulled count; the pulled physical lines are already wrapped at the
    /// current width, so this is a pure re-seating, never a re-split.
    ///
    /// No-ops (returns 0) when the reader is scrolled back, when a TUI has a
    /// partial scroll region, or when there is nothing to pull. Genuine blank
    /// rows (post-`clear` screens) are protected by `target_blanks`: only the
    /// blanks a reflow CREATED are filled.
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
        // Pull only from tiers whose newest lines we can also REMOVE without
        // reordering: the ring (newest), then the tiered store — but the store
        // only when no lazy-staged lines sit between them in age order.
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

        // Read every line BEFORE any destructive step (the #4521 unscroll
        // pattern): a decode failure aborts the fill instead of half-moving.
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
            // Lines remain duplicated in history rather than lost (#4638 shape).
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
        if self.storage.saved_cursor.valid {
            self.storage.saved_cursor.cursor.row = self
                .storage
                .saved_cursor
                .cursor
                .row
                .saturating_add(row_u16(pull))
                .min(max_row);
        }

        // History lost its newest suffix: every older retained line keeps its
        // content but its absolute key shifts — the renumbering epoch exists
        // for exactly this (see `unscroll_from_scrollback`).
        self.storage.history_renumber_epoch = self.storage.history_renumber_epoch.saturating_add(1);
        self.force_selection_invalidation();
        self.storage.damage = Damage::Full;
        self.storage.content_gen += 1;
        pull
    }

    /// Invalidate the offloaded reflow's deficit-fill debt: a screen-clearing
    /// erase (ED 0/2, DECSED 0/2) during the detach window makes the blank
    /// band below the cursor GENUINE — the pre-erase `pending_fill_target`
    /// no longer describes reflow-created blanks, and honoring it at re-attach
    /// would resurrect pre-clear history onto the freshly cleared screen.
    /// No-op outside a detach window (the field is `None` there).
    #[inline]
    pub(super) fn invalidate_pending_fill_target(&mut self) {
        self.storage.pending_fill_target = None;
    }

    /// Drop the `n` NEWEST ring-scrollback rows (the rows directly above the
    /// viewport) after their content has been re-seated into the viewport by
    /// the deficit fill. Their `ring_extras` entries drop with them.
    fn drop_newest_ring_scrollback(&mut self, n: usize) {
        let hist = self.storage.ring_buffer_scrollback();
        let n = n.min(hist);
        if n == 0 {
            return;
        }
        if self.storage.ring_head != 0 {
            let ring_head = self.storage.ring_head;
            self.storage.rows.rotate_left(ring_head);
            self.storage.ring_head = 0;
        }
        let keep = hist - n;
        drop(self.storage.rows.drain(keep..hist));
        while self.storage.ring_extras.len() > keep {
            self.storage.ring_extras.pop_back();
        }
        self.storage.total_lines -= n;
    }
}
