// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grid erase operations: EL, ED, ED 3 (scrollback), selective erase (DECSEL/DECSED), DECALN, and
//! VT420 rectangular/attribute operations (DECERA, DECFRA, DECSERA, DECCARA, DECRARA, DECCRA).

use super::{Cursor, Grid, ScrollRegion};
use crate::PageStore;
use crate::Row;

/// One captured source cell for a DECCRA (`copy_rect`) ring restore:
/// `(row_offset, col_offset, fg_rgb, bg_rgb, complex_char)`, all relative to the
/// destination rect.
type RingCapture = (u16, u16, Option<[u8; 3]>, Option<[u8; 3]>, Option<char>);

impl Grid {
    /// Write BCE truecolor background into `RgbColorRing` for a column range.
    #[inline]
    pub fn fill_bce_rgb_range(&mut self, row: u16, col_start: u16, col_end: u16) {
        if let Some(bg_rgb) = self.storage.cursor_template_bg_rgb {
            let vis = self.storage.visible_rows;
            let cols = self.storage.cols;
            self.storage.extras.set_rgb_ring_range(
                row,
                col_start,
                col_end,
                None,
                Some(bg_rgb),
                vis,
                cols,
            );
        }
    }

    /// Give the wide halves a row op blanked with the BCE fill
    /// ([`BlankedHalves`](crate::row::BlankedHalves)) the fill's ring bytes, as
    /// [`Self::fill_bce_rgb_range`] gives the span it vacated.
    #[inline]
    pub(in crate::grid) fn fill_bce_rgb_cols(&mut self, row: u16, cols: crate::row::BlankedHalves) {
        for col in cols.into_iter().flatten() {
            self.fill_bce_rgb_range(row, col, col.saturating_add(1));
        }
    }

    /// Write BCE truecolor background for a range of full rows.
    #[inline]
    pub(in crate::grid) fn fill_bce_rgb_rows(&mut self, range: core::ops::Range<u16>) {
        self.fill_bce_rgb_rect(range, 0..self.storage.cols);
    }

    /// Write BCE truecolor background for a rectangular area.
    #[inline]
    pub(in crate::grid) fn fill_bce_rgb_rect(
        &mut self,
        rows: core::ops::Range<u16>,
        cols: core::ops::Range<u16>,
    ) {
        if self.storage.cursor_template_bg_rgb.is_some() {
            for row in rows {
                self.fill_bce_rgb_range(row, cols.start, cols.end);
            }
        }
    }

    /// Write BCE truecolor background for unprotected cells of `cols` on `row`.
    fn fill_bce_rgb_unprotected(&mut self, row: u16, cols: core::ops::Range<u16>) {
        if self.storage.cursor_template_bg_rgb.is_none() {
            return;
        }
        let mut next = cols.start;
        while next < cols.end {
            let Some(cells) = self.row(row) else {
                return;
            };
            let Some(start) = (next..cols.end).find(|&col| !cells.is_cell_protected(col)) else {
                break;
            };
            let end = (start.saturating_add(1)..cols.end)
                .find(|&col| cells.is_cell_protected(col))
                .unwrap_or(cols.end);
            // RGB fills touch only extras, so the row's protection mask stays
            // fixed while its unprotected runs are emitted.
            self.fill_bce_rgb_range(row, start, end);
            next = end.saturating_add(1);
        }
    }

    /// Erase from cursor to end of line.
    fn erase_to_end_of_line_impl(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.damage_selection_visible_rows(cursor_row, cursor_row);
        self.erase_to_end_of_line_core(selective);
    }

    /// Core implementation for erase-to-end-of-line.
    fn erase_to_end_of_line_core(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let right_bound = self.storage.effective_cols_for_row(cursor_row);
        let fill = self.storage.cursor_template;
        if cursor_col < right_bound {
            let mut bce = cursor_col..right_bound;
            if let Some(row) = self.row_mut(cursor_row) {
                if selective {
                    bce = row.selective_clear_range(cursor_col, right_bound, fill);
                } else {
                    // Fill any orphaned wide half with the BCE blank rather than a default-colored blank.
                    bce = row.clear_range_with_orphan(cursor_col, right_bound, fill, fill);
                }
            }
            let erased_from = bce.start;
            if selective {
                self.drop_unclaimed_extras(
                    cursor_row,
                    cursor_col.saturating_sub(1)..right_bound.saturating_add(1),
                );
                self.fill_bce_rgb_unprotected(cursor_row, bce);
            } else {
                self.storage
                    .extras
                    .clear_range(cursor_row, bce.start, bce.end);
                self.fill_bce_rgb_range(cursor_row, bce.start, bce.end);
            }
            self.keep_len_over_marks(cursor_row, erased_from);
            self.storage.mark_content_row(cursor_row);
        }
        self.clear_wrap_into_next_row(cursor_row);
    }

    /// Keep `row`'s length over the last cell left of `before` that still
    /// carries a combining mark.
    fn keep_len_over_marks(&mut self, row: u16, before: u16) {
        let len = self.row(row).map_or(0, crate::Row::len);
        if len >= before || self.storage.extras.is_empty() {
            return;
        }
        let marked = (len..before).rev().find(|&col| {
            self.cell_extra(row, col)
                .is_some_and(|extra| !extra.combining().is_empty())
        });
        if let (Some(col), Some(r)) = (marked, self.row_mut(row)) {
            r.update_len(col.saturating_add(1));
        }
    }

    /// Break the soft wrap from `row` into the row below after a right-hand erase.
    pub(in crate::grid) fn clear_wrap_into_next_row(&mut self, row: u16) {
        let next = row.saturating_add(1);
        if next >= self.storage.visible_rows {
            return;
        }
        if let Some(below) = self.row_mut(next)
            && below.is_wrapped()
        {
            below.set_wrapped(false);
            self.storage.mark_content_row(next);
        }
    }

    /// Erase from start of line to cursor.
    fn erase_from_start_of_line_impl(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.damage_selection_visible_rows(cursor_row, cursor_row);
        self.erase_from_start_of_line_core(selective);
    }

    /// Core implementation for erase-from-start-of-line.
    fn erase_from_start_of_line_core(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        let end = cursor_col.saturating_add(1).min(effective_cols);
        let fill = self.storage.cursor_template;
        if end > 0 {
            let mut bce = 0..end;
            if let Some(row) = self.row_mut(cursor_row) {
                if selective {
                    bce = row.selective_clear_range(0, end, fill);
                } else {
                    bce = row.clear_range_with_orphan(0, end, fill, fill);
                }
            }
            if selective {
                self.drop_unclaimed_extras(cursor_row, 0..end.saturating_add(1));
                self.fill_bce_rgb_unprotected(cursor_row, bce);
            } else {
                self.storage
                    .extras
                    .clear_range(cursor_row, bce.start, bce.end);
                self.fill_bce_rgb_range(cursor_row, bce.start, bce.end);
            }
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Erase entire line at cursor.
    fn erase_line_impl(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.damage_selection_visible_rows(cursor_row, cursor_row);
        let fill = self.storage.cursor_template;
        if let Some(row) = self.row_mut(cursor_row) {
            if selective {
                row.selective_clear(fill);
            } else {
                // `erase_with` preserves DECDWL/DECDHL line attributes on EL.
                row.erase_with(fill);
            }
        }
        if selective {
            self.drop_unclaimed_extras(cursor_row, 0..self.storage.cols);
            self.fill_bce_rgb_unprotected(cursor_row, 0..self.storage.cols);
        } else {
            self.storage.extras.clear_row(cursor_row);
            self.fill_bce_rgb_range(cursor_row, 0, self.storage.cols);
        }
        self.storage.mark_content_row(cursor_row);
    }

    /// Clear a range of full rows for a screen-level erase, resetting line attributes when non-selective.
    fn clear_rows(&mut self, range: core::ops::Range<u16>, selective: bool) {
        let fill = self.storage.cursor_template;
        for row in range.clone() {
            if let Some(r) = self.row_mut(row) {
                if selective {
                    r.selective_clear(fill);
                } else {
                    r.reset_with(fill);
                }
            }
        }
        if selective {
            for row in range.clone() {
                self.drop_unclaimed_extras(row, 0..self.storage.cols);
                self.fill_bce_rgb_unprotected(row, 0..self.storage.cols);
            }
        } else {
            self.storage.extras.clear_rows(range.clone());
            self.fill_bce_rgb_rows(range);
            self.resync_any_double_width();
        }
    }

    /// Recompute the `any_double_width` fast path from the visible rows.
    pub(super) fn resync_any_double_width(&mut self) {
        if !self.storage.any_double_width {
            return;
        }
        self.storage.any_double_width =
            (0..self.storage.visible_rows).any(|row| self.storage.row_is_double_width(row));
    }

    /// Erase from cursor to end of line.
    #[inline]
    pub fn erase_to_end_of_line(&mut self) {
        self.erase_to_end_of_line_impl(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase from start of line to cursor.
    #[inline]
    pub fn erase_from_start_of_line(&mut self) {
        self.erase_from_start_of_line_impl(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase entire line.
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn erase_line(&mut self) {
        self.erase_line_impl(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase from cursor to end of screen.
    #[doc(hidden)] // pub for crate benchmarks; not part of stable API
    #[inline]
    pub fn erase_to_end_of_screen(&mut self) {
        self.erase_below_cursor(false);
        self.storage.clear_pending_wrap();
    }

    /// Erase from start of screen to cursor.
    #[inline]
    pub fn erase_from_start_of_screen(&mut self) {
        self.erase_above_cursor(false);
        self.storage.clear_pending_wrap();
    }

    /// ED 0 / DECSED 0 without the wrap reset: the cursor row from the cursor
    /// on, then every row below it.
    fn erase_below_cursor(&mut self, selective: bool) {
        self.erase_to_end_of_line_core(selective);
        let cursor_row = self.storage.cursor.row;
        let visible_rows = self.storage.visible_rows;
        self.clear_rows(cursor_row.saturating_add(1)..visible_rows, selective);
        self.forget_pushed_rows_erased();
        self.damage_selection_visible_rows_ext(cursor_row, visible_rows.saturating_sub(1), false);
        self.storage
            .mark_content_rows(cursor_row.saturating_add(1), visible_rows);
    }

    /// ED 1 / DECSED 1 without the wrap reset: every row above the cursor, then
    /// the cursor row up to and including the cursor.
    fn erase_above_cursor(&mut self, selective: bool) {
        let cursor_row = self.storage.cursor.row;
        self.clear_rows(0..cursor_row, selective);
        self.erase_from_start_of_line_core(selective);
        self.damage_selection_visible_rows_ext(0, cursor_row, false);
        self.storage.mark_content_rows(0, cursor_row);
    }

    /// Erase entire screen (ED 2) with the BCE cursor template.
    pub fn erase_screen(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.display_offset = 0;
        self.forget_revealed_rows();
        self.forget_pushed_rows_erased();
        let fill = self.storage.cursor_template;
        for row in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row) {
                r.reset_with(fill);
            }
        }
        self.storage.extras.clear();
        self.fill_bce_rgb_rows(0..self.storage.visible_rows);
        self.storage.any_double_width = false;
        let last = self.storage.visible_rows.saturating_sub(1);
        self.damage_selection_visible_rows_ext(0, last, false);
        self.drop_fill_debt();
        self.storage.mark_content_full();
    }

    /// Clear all DEC line attributes (decdwl/decdhl) on every visible row.
    pub fn clear_line_attributes(&mut self) {
        let mut changed = false;
        for row_idx in 0..self.storage.visible_rows {
            if let Some(r) = self.storage.row_at_screen_mut(row_idx)
                && r.line_size() != super::LineSize::SingleWidth
            {
                r.set_line_size(super::LineSize::SingleWidth);
                changed = true;
            }
        }
        self.storage.any_double_width = false;
        if changed {
            self.storage.mark_content_full();
        }
    }

    /// Erase scrollback.
    pub fn erase_scrollback(&mut self) {
        self.storage.scrollback_clear_gen = self.storage.scrollback_clear_gen.wrapping_add(1);
        self.storage.note_history_cleared();
        let scrollback = self
            .storage
            .total_lines
            .saturating_sub(self.storage.visible_rows as usize);
        if scrollback == 0 {
            if let Some(scrollback) = self.storage.scrollback.as_mut()
                && let Err(e) = scrollback.clear()
            {
                aterm_log::warn!("scrollback clear failed: {e}");
            }
            self.storage.lazy_buffer.clear();
            self.storage.ring_extras.clear();
            self.storage.display_offset = 0;
            self.storage.mark_content_full();
            self.discard_history_selection();
            self.anchors_clamp_to_oldest();
            debug_assert_eq!(self.storage.display_offset, 0);
            debug_assert_eq!(self.storage.total_lines, self.storage.visible_rows as usize);
            return;
        }

        debug_assert!(
            !self.storage.rows.is_empty(),
            "clear_scrollback: ring buffer has zero rows"
        );
        let live_top = (self.storage.ring_head + scrollback) % self.storage.rows.len();
        let mut new_pages = PageStore::new();
        let mut new_rows = Vec::with_capacity(self.storage.visible_rows as usize);
        for i in 0..self.storage.visible_rows {
            let idx = (live_top + i as usize) % self.storage.rows.len();
            // SAFETY: `new_rows` and `new_pages` are installed into `self`
            // together before the rebuilt rows can outlive this method.
            let mut row = unsafe { Row::new(self.storage.cols, &mut new_pages) };
            row.copy_from(&self.storage.rows[idx]);
            new_rows.push(row);
        }

        self.storage.rows = new_rows;
        self.storage.pages = new_pages;
        self.storage.total_lines = self.storage.visible_rows as usize;
        self.storage.ring_head = 0;
        self.storage.display_offset = 0;
        if let Some(scrollback) = self.storage.scrollback.as_mut()
            && let Err(e) = scrollback.clear()
        {
            aterm_log::warn!("scrollback clear failed: {e}");
        }
        self.storage.lazy_buffer.clear();
        self.storage.ring_extras.clear();
        self.storage.mark_content_full();
        self.discard_history_selection();
        self.anchors_clamp_to_oldest();
        debug_assert_eq!(self.storage.display_offset, 0);
        debug_assert_eq!(self.storage.total_lines, self.storage.visible_rows as usize);
    }

    /// Selectively erase from cursor to end of line (DECSEL 0).
    pub fn selective_erase_to_end_of_line(&mut self) {
        self.erase_to_end_of_line_impl(true);
        self.storage.clear_pending_wrap();
    }

    /// Selectively erase from start of line to cursor (DECSEL 1).
    pub fn selective_erase_from_start_of_line(&mut self) {
        self.erase_from_start_of_line_impl(true);
        let cursor_row = self.storage.cursor.row;
        let span_end = self.storage.cursor.col.saturating_add(1);
        self.reset_wrap_unless_span_protected(0..0, Some((cursor_row, span_end)));
    }

    /// Selectively erase entire line (DECSEL 2).
    pub fn selective_erase_line(&mut self) {
        self.erase_line_impl(true);
        let cursor_row = self.storage.cursor.row;
        self.reset_wrap_unless_span_protected(cursor_row..cursor_row.saturating_add(1), None);
    }

    /// Selectively erase from cursor to end of screen (DECSED 0).
    pub fn selective_erase_to_end_of_screen(&mut self) {
        let at_origin = self.storage.cursor.row == 0 && self.storage.cursor.col == 0;
        self.erase_below_cursor(true);
        if at_origin {
            self.reset_wrap_unless_span_protected(0..self.storage.visible_rows, None);
        } else {
            self.storage.clear_pending_wrap();
        }
    }

    /// Selectively erase from start of screen to cursor (DECSED 1).
    pub fn selective_erase_from_start_of_screen(&mut self) {
        let cursor_row = self.storage.cursor.row;
        let span_end = self.storage.cursor.col.saturating_add(1);
        self.erase_above_cursor(true);
        self.reset_wrap_unless_span_protected(0..cursor_row, Some((cursor_row, span_end)));
    }

    /// Selectively erase entire screen (DECSED 2).
    pub fn selective_erase_screen(&mut self) {
        let visible_rows = self.storage.visible_rows;
        self.clear_rows(0..self.storage.visible_rows, true);
        self.drop_fill_debt();
        let last = self.storage.visible_rows.saturating_sub(1);
        self.damage_selection_visible_rows_ext(0, last, false);
        self.storage.mark_content_full();
        self.reset_wrap_unless_span_protected(0..visible_rows, None);
    }

    /// Reset the deferred wrap the way xterm's protected erase (`ClearInLine2`)
    /// does: only when some cell of the erase's span is unprotected. A glyph the cursor follows
    /// goes the same way as the wrap ([`Self::cursor_follows_glyph`]).
    fn reset_wrap_unless_span_protected(
        &mut self,
        full_rows: core::ops::Range<u16>,
        partial: Option<(u16, u16)>,
    ) {
        if !self.storage.pending_wrap() && !self.storage.follows_glyph {
            return;
        }
        let cols = self.storage.cols;
        let span_has_unprotected = |grid: &Self, row: u16, end: u16| {
            let end = end.min(cols);
            grid.row(row)
                .map_or(end > 0, |r| (0..end).any(|col| !r.is_cell_protected(col)))
        };
        let any_unprotected = full_rows
            .into_iter()
            .any(|row| span_has_unprotected(self, row, cols))
            || partial.is_some_and(|(row, end)| span_has_unprotected(self, row, end));
        if any_unprotected {
            self.storage.clear_pending_wrap();
        }
    }

    /// DECALN (`ESC # 8`): fill the screen with 'E' and reset all margins. Each row is filled
    /// whole, as ED 2 fills it, and the screen is new from its top, so the resize hand-back
    /// forgets what it owed as it does after ED 2.
    pub fn screen_alignment_pattern(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.scroll_region = ScrollRegion::full(self.storage.visible_rows);
        self.storage.reset_horizontal_margins();
        self.forget_revealed_rows();
        self.forget_pushed_rows_erased();
        self.drop_fill_debt();

        let glyph = super::Cell::from_ascii_fast(b'E');
        for row in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row) {
                r.fill_written(glyph);
            }
        }

        self.storage.extras.clear();

        self.storage.any_double_width = false;

        self.storage.cursor = Cursor::default();

        let last = self.storage.visible_rows.saturating_sub(1);
        self.damage_selection_visible_rows_ext(0, last, false);
        self.storage.mark_content_full();
        debug_assert_eq!(self.storage.cursor.row, 0);
        debug_assert_eq!(self.storage.cursor.col, 0);
        debug_assert!(
            self.storage
                .scroll_region
                .is_full(self.storage.visible_rows)
        );
    }

    /// Erase rectangular area (DECERA - VT420+).
    pub fn erase_rect(&mut self, top: u16, left: u16, bottom: u16, right: u16) {
        self.fill_rect(
            self.storage.cursor_template,
            top,
            left,
            bottom,
            right,
            None,
            self.storage.cursor_template_bg_rgb,
        );
    }

    /// Fill rectangular area with a cell template (DECFRA - VT420+).
    #[allow(
        clippy::too_many_arguments,
        reason = "DECFRA fill needs the rect bounds + fill cell + the resolved \
                  fg/bg truecolor bytes to repopulate the out-of-line RGB ring"
    )]
    pub fn fill_rect(
        &mut self,
        fill: super::Cell,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        fg_rgb: Option<[u8; 3]>,
        bg_rgb: Option<[u8; 3]>,
    ) {
        let top = top.min(self.storage.visible_rows.saturating_sub(1));
        let bottom = bottom.min(self.storage.visible_rows.saturating_sub(1));
        let left = left.min(self.storage.cols.saturating_sub(1));
        let right = right.min(self.storage.cols.saturating_sub(1));

        if top > bottom || left > right {
            return;
        }

        self.damage_selection_visible_rows(top, bottom);

        let end_col = right.saturating_add(1);

        for row_idx in top..=bottom {
            if let Some(row) = self.row_mut(row_idx) {
                row.clear_range_with(left, end_col, fill);
            }
            self.storage.mark_content_row(row_idx);
        }
        self.storage
            .extras
            .clear_rect(top..bottom.saturating_add(1), left..end_col);
        for row_idx in top..=bottom {
            self.drop_unclaimed_extras(row_idx, left.saturating_sub(1)..end_col.saturating_add(1));
        }

        if fg_rgb.is_some() || bg_rgb.is_some() {
            let visible_rows = self.storage.visible_rows;
            let cols = self.storage.cols;
            for row_idx in top..=bottom {
                self.storage.extras.set_rgb_ring_range(
                    row_idx,
                    left,
                    end_col,
                    fg_rgb,
                    bg_rgb,
                    visible_rows,
                    cols,
                );
            }
        }
    }

    /// Selectively erase rectangular area (DECSERA - VT420+), 0-indexed.
    pub fn selective_erase_rect(&mut self, top: u16, left: u16, bottom: u16, right: u16) {
        let top = top.min(self.storage.visible_rows.saturating_sub(1));
        let bottom = bottom.min(self.storage.visible_rows.saturating_sub(1));
        let left = left.min(self.storage.cols.saturating_sub(1));
        let right = right.min(self.storage.cols.saturating_sub(1));

        if top > bottom || left > right {
            return;
        }

        self.damage_selection_visible_rows(top, bottom);

        let end_col = right.saturating_add(1);

        let scrub_extras = !self.storage.extras.is_empty();
        let scrub_complex = self.storage.extras.has_complex_ring();
        let mut wiped_cols: Vec<u16> = Vec::new();
        for row_idx in top..=bottom {
            wiped_cols.clear();
            if let Some(row) = self.row_mut(row_idx) {
                row.selective_wipe_range(left, end_col, &mut wiped_cols);
            }
            if scrub_complex {
                for &col in &wiped_cols {
                    self.storage.extras.clear_complex_ring_cell(row_idx, col, 1);
                }
            }
            let mut removed_any = false;
            if scrub_extras {
                for &col in &wiped_cols {
                    if self.storage.extras.scrub_char_marks(row_idx, col) {
                        removed_any |= self.remove_cell_extra(row_idx, col);
                    }
                }
            }
            if removed_any
                && let Some(row) = self.row_mut(row_idx)
                && let Some(last) = row.len().checked_sub(1)
                && row.get(last).is_some_and(super::Cell::is_empty)
            {
                row.recompute_len();
            }
            self.storage.mark_content_row(row_idx);
        }
    }

    /// Rewrite the attribute flags of every cell in a rectangle or DECSACE character stream via `edit`.
    fn edit_attrs(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        stream: bool,
        edit: impl Fn(super::CellFlags) -> super::CellFlags,
    ) {
        let top = top.min(self.storage.visible_rows.saturating_sub(1));
        let bottom = bottom.min(self.storage.visible_rows.saturating_sub(1));
        let left = left.min(self.storage.cols.saturating_sub(1));
        let right = right.min(self.storage.cols.saturating_sub(1));

        if top > bottom || (!stream && left > right) {
            return;
        }

        let max_col = self.storage.cols.saturating_sub(1);

        for row_idx in top..=bottom {
            let start = if stream && row_idx != top { 0 } else { left };
            let end = if stream && row_idx != bottom {
                max_col
            } else {
                right
            };

            if start > end {
                continue;
            }

            if let Some(row) = self.row_mut(row_idx) {
                for col in start..=end {
                    if let Some(cell) = row.get_mut(col) {
                        let flags = edit(cell.flags());
                        cell.set_flags(flags);
                    }
                }
                row.recompute_len();
            }
            self.storage.mark_content_row(row_idx);
        }
    }

    /// Reverse one cell's attribute flags for DECRARA.
    #[inline]
    const fn reverse_cell_attrs(
        flags: super::CellFlags,
        toggle: super::CellFlags,
        toggle_underline: bool,
    ) -> super::CellFlags {
        let mut flags = super::CellFlags::from_bits(flags.bits() ^ toggle.bits());
        if toggle_underline {
            if flags.intersects(super::CellFlags::ALL_UNDERLINES) {
                flags = flags.difference(super::CellFlags::ALL_UNDERLINES);
            } else {
                flags = flags.union(super::CellFlags::UNDERLINE);
            }
        }
        flags
    }

    /// Change attributes in rectangular area (DECCARA - VT420+).
    pub fn change_attrs_rect(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        flags_to_set: super::CellFlags,
        flags_to_clear: super::CellFlags,
    ) {
        self.edit_attrs(top, left, bottom, right, false, |flags| {
            flags.difference(flags_to_clear).union(flags_to_set)
        });
    }

    /// Change attributes in a character stream (decsace stream mode).
    pub fn change_attrs_stream(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        flags_to_set: super::CellFlags,
        flags_to_clear: super::CellFlags,
    ) {
        self.edit_attrs(top, left, bottom, right, true, |flags| {
            flags.difference(flags_to_clear).union(flags_to_set)
        });
    }

    /// Reverse attributes in rectangular area (DECRARA - VT400+).
    pub fn reverse_attrs_rect(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        toggle: super::CellFlags,
        toggle_underline: bool,
    ) {
        self.edit_attrs(top, left, bottom, right, false, |flags| {
            Self::reverse_cell_attrs(flags, toggle, toggle_underline)
        });
    }

    /// Reverse attributes in a character stream (decsace stream mode).
    pub fn reverse_attrs_stream(
        &mut self,
        top: u16,
        left: u16,
        bottom: u16,
        right: u16,
        toggle: super::CellFlags,
        toggle_underline: bool,
    ) {
        self.edit_attrs(top, left, bottom, right, true, |flags| {
            Self::reverse_cell_attrs(flags, toggle, toggle_underline)
        });
    }

    /// Copy rectangular area (DECCRA - VT420+).
    pub fn copy_rect(
        &mut self,
        src_top: u16,
        src_left: u16,
        src_bottom: u16,
        src_right: u16,
        dst_top: u16,
        dst_left: u16,
    ) {
        let max_row = self.storage.visible_rows.saturating_sub(1);
        let max_col = self.storage.cols.saturating_sub(1);
        let src_top = src_top.min(max_row);
        let src_bottom = src_bottom.min(max_row);
        let src_left = src_left.min(max_col);
        let src_right = src_right.min(max_col);

        if src_top > src_bottom || src_left > src_right {
            return;
        }

        let dst_top = dst_top.min(max_row);
        let dst_left = dst_left.min(max_col);
        let rect_height = (src_bottom - src_top + 1).min(max_row - dst_top + 1);
        let rect_width = (src_right - src_left + 1).min(max_col - dst_left + 1);

        let mut buf: Vec<Vec<super::Cell>> = Vec::with_capacity(rect_height as usize);
        for row_offset in 0..rect_height {
            let src_row = src_top + row_offset;
            let mut row_cells = Vec::with_capacity(rect_width as usize);
            for col_offset in 0..rect_width {
                let src_col = src_left + col_offset;
                let cell = self
                    .cell(src_row, src_col)
                    .copied()
                    .unwrap_or(super::Cell::EMPTY);
                row_cells.push(cell);
            }
            if self
                .row(src_row)
                .is_some_and(|r| r.is_cell_wide_continuation(src_left))
            {
                row_cells[0] = super::Cell::EMPTY;
            }
            if let Some(last) = row_cells.last()
                && last.flags().contains(super::CellFlags::WIDE)
            {
                let last_idx = row_cells.len() - 1;
                row_cells[last_idx] = super::Cell::EMPTY;
            }
            buf.push(row_cells);
        }

        let mut extras_buf: Vec<(u16, u16, super::CellExtra)> = Vec::new();
        let mut ring_buf: Vec<RingCapture> = Vec::new();
        for (row_cells, row_offset) in buf.iter().zip(0u16..) {
            let src_row = src_top + row_offset;
            for (cell, col_offset) in row_cells.iter().zip(0u16..) {
                let src_col = src_left + col_offset;
                let coord = crate::CellCoord {
                    row: src_row,
                    col: src_col,
                };
                if (cell.has_extras()
                    || cell.is_complex()
                    || cell.fg_needs_overflow()
                    || cell.bg_needs_overflow())
                    && let Some(extra) = self.storage.extras.get(coord)
                {
                    extras_buf.push((row_offset, col_offset, extra.clone()));
                }
                let fg = if cell.fg_needs_overflow() {
                    self.storage.extras.fg_rgb_for(src_row, src_col)
                } else {
                    None
                };
                let bg = if cell.bg_needs_overflow() {
                    self.storage.extras.bg_rgb_for(src_row, src_col)
                } else {
                    None
                };
                let cx = if cell.is_complex()
                    && self
                        .storage
                        .extras
                        .complex_char_arc_for(src_row, src_col)
                        .is_none()
                {
                    self.storage.extras.complex_codepoint_for(src_row, src_col)
                } else {
                    None
                };
                if fg.is_some() || bg.is_some() || cx.is_some() {
                    ring_buf.push((row_offset, col_offset, fg, bg, cx));
                }
            }
        }

        for (row_cells, row_offset) in buf.iter().zip(0u16..) {
            let dst_row = dst_top.saturating_add(row_offset);
            if dst_row > max_row {
                break;
            }
            let actual_width = row_cells.len().min((max_col - dst_left + 1) as usize);
            if let Some(row) = self.row_mut(dst_row) {
                if actual_width > 0 {
                    let _ = row.fixup_wide_chars_in_range(
                        dst_left,
                        actual_width as u16,
                        crate::Cell::EMPTY,
                    );
                }
                for (&cell, col_offset) in row_cells.iter().zip(0u16..) {
                    let dst_col = dst_left.saturating_add(col_offset);
                    if dst_col > max_col {
                        break;
                    }
                    row.set(dst_col, cell);
                }
                row.recompute_len();
            }
            self.storage.mark_content_row(dst_row);
        }

        let dst_bottom_eff = dst_top
            .saturating_add(rect_height.saturating_sub(1))
            .min(max_row);
        self.damage_selection_visible_rows(dst_top, dst_bottom_eff);
        if dst_top != src_top {
            self.lose_anchors();
        }
        let dst_right_eff = dst_left
            .saturating_add(rect_width.saturating_sub(1))
            .min(max_col);
        self.storage.extras.clear_rect(
            dst_top..dst_bottom_eff.saturating_add(1),
            dst_left..dst_right_eff.saturating_add(1),
        );
        for (row_offset, col_offset, extra) in extras_buf {
            let dst_row = dst_top.saturating_add(row_offset);
            let dst_col = dst_left.saturating_add(col_offset);
            if dst_row <= max_row && dst_col <= max_col {
                let coord = crate::CellCoord {
                    row: dst_row,
                    col: dst_col,
                };
                self.storage.extras.set(coord, extra);
            }
        }

        if !ring_buf.is_empty() {
            let visible_rows = self.storage.visible_rows;
            let cols = self.storage.cols;
            for (row_offset, col_offset, fg, bg, cx) in ring_buf {
                let dst_row = dst_top.saturating_add(row_offset);
                let dst_col = dst_left.saturating_add(col_offset);
                if dst_row > max_row || dst_col > max_col {
                    continue;
                }
                if fg.is_some() || bg.is_some() {
                    self.storage.extras.set_rgb_ring_range(
                        dst_row,
                        dst_col,
                        dst_col.saturating_add(1),
                        fg,
                        bg,
                        visible_rows,
                        cols,
                    );
                }
                if let Some(ch) = cx {
                    self.storage.extras.set_complex_char_ring(
                        dst_row,
                        dst_col,
                        ch,
                        visible_rows,
                        cols,
                    );
                }
            }
        }

        for dst_row in dst_top..=dst_bottom_eff {
            self.drop_unclaimed_extras(
                dst_row,
                dst_left.saturating_sub(1)..dst_right_eff.saturating_add(2),
            );
        }
    }
}

#[cfg(test)]
#[path = "erase_tests.rs"]
mod tests;
