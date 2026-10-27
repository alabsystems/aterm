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
    pub(in crate::grid) fn fill_bce_rgb_range(&mut self, row: u16, col_start: u16, col_end: u16) {
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
        let cleared: Vec<u16> = match self.row(row) {
            Some(r) => cols.filter(|&c| !r.is_cell_protected(c)).collect(),
            None => return,
        };
        for col in cleared {
            self.fill_bce_rgb_range(row, col, col.saturating_add(1));
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
    fn resync_any_double_width(&mut self) {
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
        for row_idx in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row_idx) {
                r.set_line_size(super::LineSize::SingleWidth);
            }
        }
        self.storage.any_double_width = false;
    }

    /// Erase scrollback.
    pub fn erase_scrollback(&mut self) {
        self.storage.scrollback_clear_gen = self.storage.scrollback_clear_gen.wrapping_add(1);
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
    /// does: only when some cell of the erase's span is unprotected.
    fn reset_wrap_unless_span_protected(
        &mut self,
        full_rows: core::ops::Range<u16>,
        partial: Option<(u16, u16)>,
    ) {
        if !self.storage.pending_wrap() {
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

    /// DECALN (`ESC # 8`): fill the screen with 'E' and reset all margins.
    pub fn screen_alignment_pattern(&mut self) {
        self.storage.clear_pending_wrap();
        self.storage.scroll_region = ScrollRegion::full(self.storage.visible_rows);
        self.storage.reset_horizontal_margins();

        let cols = self.storage.cols;

        for row in 0..self.storage.visible_rows {
            if let Some(r) = self.row_mut(row) {
                r.clear();
                for col in 0..cols {
                    r.write_char(col, 'E');
                }
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
        let mut wiped_cols: Vec<u16> = Vec::new();
        for row_idx in top..=bottom {
            wiped_cols.clear();
            if let Some(row) = self.row_mut(row_idx) {
                row.selective_wipe_range(left, end_col, &mut wiped_cols);
            }
            if scrub_extras {
                for &col in &wiped_cols {
                    if self.storage.extras.scrub_char_marks(row_idx, col) {
                        self.remove_cell_extra(row_idx, col);
                    }
                }
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

        let rect_height = src_bottom - src_top + 1;
        let rect_width = src_right - src_left + 1;

        let dst_top = dst_top.min(max_row);
        let dst_left = dst_left.min(max_col);

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
                if let Some(extra) = self.storage.extras.get(coord) {
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
mod tests {
    use super::Grid;
    use crate::cell_flags::CellFlags;
    use crate::grid::test_support::wrapped_pair;

    /// Write ASCII text at the current cursor position.
    fn write_text(grid: &mut Grid, text: &str) {
        for c in text.chars() {
            grid.write_char(c);
        }
    }

    /// Read the character at a visible grid position.
    fn char_at(grid: &Grid, row: u16, col: u16) -> char {
        grid.cell(row, col).map_or(' ', |c| c.char())
    }

    /// Check whether the cell at `(row, col)` is empty.
    fn is_empty_at(grid: &Grid, row: u16, col: u16) -> bool {
        grid.cell(row, col).is_none_or(|c| c.is_empty())
    }

    /// Set the PROTECTED flag on a cell.
    fn protect_cell(grid: &mut Grid, row: u16, col: u16) {
        if let Some(cell) = grid.cell_mut(row, col) {
            let flags = cell.flags().union(CellFlags::PROTECTED);
            cell.set_flags(flags);
        }
    }

    /// Fill `row` with `'A' + col_offset`.
    fn fill_row(grid: &mut Grid, row: u16, cols: u16) {
        grid.move_cursor_to(row, 0);
        for col in 0..cols {
            let ch = (b'A' + (col % 26) as u8) as char;
            grid.write_char(ch);
        }
    }

    /// Fill the entire grid with distinct characters.
    fn fill_grid(grid: &mut Grid, rows: u16, cols: u16) {
        for row in 0..rows {
            fill_row(grid, row, cols);
        }
    }

    /// Arm a deferred wrap by printing `row` to its last column.
    fn arm_wrap_on_row(grid: &mut Grid, row: u16, cols: u16) {
        grid.move_cursor_to(row, 0);
        for col in 0..cols {
            grid.write_char_wrap((b'A' + (col % 26) as u8) as char);
        }
        assert!(
            grid.pending_wrap(),
            "precondition: a filled row arms the wrap"
        );
        assert_eq!(
            grid.cursor_row(),
            row,
            "precondition: cursor stays on the row"
        );
        assert_eq!(
            grid.cursor_col(),
            cols - 1,
            "precondition: cursor parked on the last column"
        );
    }

    /// Verify that after an erase resets the pending wrap, the next glyph overwrites the parked last column of `row`.
    fn assert_next_glyph_overwrites_last_col(grid: &mut Grid, row: u16, cols: u16) {
        grid.write_char_wrap('?');
        assert_eq!(
            char_at(grid, row, cols - 1),
            '?',
            "the glyph after the erase lands on the parked column (xterm)"
        );
        assert_eq!(grid.cursor_row(), row, "nothing wrapped");
        assert!(grid.pending_wrap(), "the new glyph re-arms the wrap");
    }

    #[derive(Clone, Copy, Debug)]
    enum Erase {
        El0,
        El1,
        El2,
        Ed0,
        Ed1,
        Ed2,
        Ed3,
        Sel0,
        Sel1,
        Sel2,
        Sed0,
        Sed1,
        Sed2,
    }

    impl Erase {
        const DISPLAY_AND_LINE: [Self; 12] = [
            Self::El0,
            Self::El1,
            Self::El2,
            Self::Ed0,
            Self::Ed1,
            Self::Ed2,
            Self::Sel0,
            Self::Sel1,
            Self::Sel2,
            Self::Sed0,
            Self::Sed1,
            Self::Sed2,
        ];

        fn apply(self, grid: &mut Grid) {
            match self {
                Self::El0 => grid.erase_to_end_of_line(),
                Self::El1 => grid.erase_from_start_of_line(),
                Self::El2 => grid.erase_line(),
                Self::Ed0 => grid.erase_to_end_of_screen(),
                Self::Ed1 => grid.erase_from_start_of_screen(),
                Self::Ed2 => grid.erase_screen(),
                Self::Ed3 => grid.erase_scrollback(),
                Self::Sel0 => grid.selective_erase_to_end_of_line(),
                Self::Sel1 => grid.selective_erase_from_start_of_line(),
                Self::Sel2 => grid.selective_erase_line(),
                Self::Sed0 => grid.selective_erase_to_end_of_screen(),
                Self::Sed1 => grid.selective_erase_from_start_of_screen(),
                Self::Sed2 => grid.selective_erase_screen(),
            }
        }

        /// Whether the erase reaches `(row, col)` with the cursor at
        /// `(cr, cc)`: the cursor cell is always inclusive.
        fn covers(self, (cr, cc): (u16, u16), (row, col): (u16, u16)) -> bool {
            match self {
                Self::El0 | Self::Sel0 => row == cr && col >= cc,
                Self::El1 | Self::Sel1 => row == cr && col <= cc,
                Self::El2 | Self::Sel2 => row == cr,
                Self::Ed0 | Self::Sed0 => row > cr || (row == cr && col >= cc),
                Self::Ed1 | Self::Sed1 => row < cr || (row == cr && col <= cc),
                Self::Ed2 | Self::Sed2 => true,
                Self::Ed3 => false,
            }
        }

        fn selective(self) -> bool {
            matches!(
                self,
                Self::Sel0 | Self::Sel1 | Self::Sel2 | Self::Sed0 | Self::Sed1 | Self::Sed2
            )
        }
    }

    /// Run `op` on a filled `size` grid with the cursor at `cursor` (parked
    /// there by a real deferred wrap when `armed`) and `protected` cells, then
    /// check every cell: erased exactly where the op reaches, minus protected
    /// cells for the selective ops.
    fn erase_and_check(
        op: Erase,
        size: (u16, u16),
        cursor: (u16, u16),
        armed: bool,
        protected: &[(u16, u16)],
    ) -> Grid {
        let (rows, cols) = size;
        let mut grid = Grid::new(rows, cols);
        fill_grid(&mut grid, rows, cols);
        if armed {
            assert_eq!(cursor.1, cols - 1, "a wrap parks on the last column");
            arm_wrap_on_row(&mut grid, cursor.0, cols);
        } else {
            grid.move_cursor_to(cursor.0, cursor.1);
        }
        for &(row, col) in protected {
            protect_cell(&mut grid, row, col);
        }
        op.apply(&mut grid);
        grid.assert_invariants();
        for row in 0..rows {
            for col in 0..cols {
                let erased = op.covers(cursor, (row, col))
                    && !(op.selective() && protected.contains(&(row, col)));
                let want = if erased {
                    ' '
                } else {
                    (b'A' + (col % 26) as u8) as char
                };
                assert_eq!(
                    char_at(&grid, row, col),
                    want,
                    "{op:?} at {cursor:?} (armed {armed}, protected {protected:?}): ({row}, {col})"
                );
            }
        }
        grid
    }

    /// Every EL, ED, DECSEL, and DECSED erases its exact span while selective forms preserve DECSCA-protected cells.
    #[test]
    fn every_erase_clears_exactly_its_span() {
        const PROTECTED: &[(u16, u16)] = &[(0, 1), (1, 3), (1, 6), (2, 0), (2, 8), (3, 4), (3, 9)];
        for op in Erase::DISPLAY_AND_LINE {
            let protected = if op.selective() { PROTECTED } else { &[] };
            for cursor in [(0, 0), (1, 5), (2, 4), (3, 9)] {
                erase_and_check(op, (4, 10), cursor, false, protected);
            }
            erase_and_check(op, (1, 1), (0, 0), false, &[]);
            erase_and_check(op, (1, 10), (0, 5), false, &[]);
        }
    }

    /// Pending-wrap resets across EL, ED, DECSEL, and DECSED match xterm's `ClearRight` and `ClearInLine2` rules.
    #[test]
    fn erases_reset_the_pending_wrap_as_xterm_does() {
        const ROW_0: &[(u16, u16)] = &[(0, 0), (0, 1), (0, 2), (0, 3), (0, 4)];
        const ROWS_0_1: &[(u16, u16)] = &[
            (0, 0),
            (0, 1),
            (0, 2),
            (0, 3),
            (0, 4),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (1, 4),
        ];
        let screen = |rows: u16, cols: u16| -> Vec<(u16, u16)> {
            (0..rows)
                .flat_map(|r| (0..cols).map(move |c| (r, c)))
                .collect()
        };
        use Erase::*;
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let cases: Vec<(Erase, (u16, u16), u16, Vec<(u16, u16)>, bool)> = vec![
            (El0, (4, 5), 0, vec![], false),
            (El1, (4, 5), 0, vec![], false),
            (El2, (4, 5), 0, vec![], false),
            (Ed0, (4, 5), 0, vec![], false),
            (Ed0, (3, 1), 0, vec![], false),
            (Ed1, (4, 5), 1, vec![], false),
            (Ed1, (3, 5), 2, vec![], false),
            (Ed2, (4, 5), 1, vec![], false),
            (Ed3, (4, 5), 1, vec![], true),
            (Sel0, (4, 5), 0, vec![], false),
            (Sel0, (4, 5), 0, vec![(0, 4)], false),
            (Sel1, (4, 5), 0, vec![], false),
            (Sel1, (4, 5), 0, ROW_0.to_vec(), true),
            (Sel1, (4, 5), 0, vec![(0, 0), (0, 1), (0, 3), (0, 4)], false),
            (Sel2, (4, 5), 0, vec![(0, 4)], false),
            (Sel2, (4, 5), 0, ROW_0.to_vec(), true),
            (Sed0, (4, 5), 1, vec![], false),
            (Sed0, (4, 5), 1, vec![(1, 4)], false),
            (Sed0, (3, 1), 0, screen(3, 1), true),
            (Sed0, (3, 1), 0, vec![(0, 0), (1, 0)], false),
            (Sed1, (4, 5), 1, vec![], false),
            (Sed1, (4, 5), 1, ROWS_0_1.to_vec(), true),
            (Sed1, (2, 3), 1, screen(2, 3), true),
            (Sed2, (4, 5), 1, vec![(1, 4)], false),
            (Sed2, (3, 4), 1, screen(3, 4), true),
        ];
        for (op, size, row, protected, keeps_wrap) in cases {
            let cursor = (row, size.1 - 1);
            let mut grid = erase_and_check(op, size, cursor, true, &protected);
            assert_eq!(
                grid.pending_wrap(),
                keeps_wrap,
                "{op:?} on {size:?} row {row}, protected {protected:?}"
            );
            if !keeps_wrap {
                assert_next_glyph_overwrites_last_col(&mut grid, row, size.1);
            }
        }
    }

    /// DECALN fills the whole screen with default-attribute 'E's over whatever
    /// was there, homes the cursor, resets the scroll region, clears the
    /// pending wrap and returns every row to single width.
    #[test]
    fn decaln_fills_the_screen_and_resets_the_cursor_state() {
        for (rows, cols) in [(4, 10), (1, 1)] {
            let mut grid = Grid::new(rows, cols);
            fill_grid(&mut grid, rows, cols);
            if rows > 2 {
                grid.set_scroll_region(1, rows - 2);
            }
            if let Some(r) = grid.row_mut(0) {
                r.set_line_size(crate::LineSize::DoubleWidth);
            }
            grid.mark_has_double_width();
            grid.move_cursor_to(rows - 1, cols - 1);
            grid.set_pending_wrap(true);

            grid.screen_alignment_pattern();

            for row in 0..rows {
                for col in 0..cols {
                    let cell = grid.cell(row, col).expect("cell");
                    assert_eq!(cell.char(), 'E', "({row}, {col})");
                    assert!(cell.flags().is_empty(), "({row}, {col})");
                }
            }
            assert_eq!((grid.cursor_row(), grid.cursor_col()), (0, 0));
            let region = grid.scroll_region();
            assert_eq!((region.top, region.bottom), (0, rows - 1));
            assert!(!grid.pending_wrap());
            assert!(!grid.storage.any_double_width);
        }
    }

    /// DECERA, DECSERA, DECFRA, and DECCARA modify only their clamped rectangle without consuming pending wrap.
    #[test]
    fn rect_ops_touch_exactly_their_rectangle() {
        type Rect = (u16, u16, u16, u16);
        const PROTECTED: &[(u16, u16)] = &[(1, 3), (2, 4)];
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let ops: [(&str, fn(&mut Grid, Rect)); 5] = [
            ("DECERA", |g, (t, l, b, r)| g.erase_rect(t, l, b, r)),
            ("DECSERA", |g, (t, l, b, r)| {
                g.selective_erase_rect(t, l, b, r)
            }),
            ("DECFRA", |g, (t, l, b, r)| {
                g.fill_rect(crate::Cell::from_ascii_fast(b'X'), t, l, b, r, None, None);
            }),
            ("DECCARA set", |g, (t, l, b, r)| {
                g.change_attrs_rect(t, l, b, r, CellFlags::BOLD, CellFlags::empty());
            }),
            ("DECCARA clear", |g, (t, l, b, r)| {
                g.change_attrs_rect(0, 0, 3, 9, CellFlags::BOLD, CellFlags::empty());
                g.change_attrs_rect(t, l, b, r, CellFlags::empty(), CellFlags::BOLD);
            }),
        ];
        for rect in [
            (1, 2, 2, 5),
            (2, 3, 2, 3),
            (2, 8, 100, 100),
            (0, 0, 3, 9),
            (3, 5, 1, 2),
        ] {
            let (top, left, bottom, right) = rect;
            let inside =
                |r: u16, c: u16| (top..=bottom).contains(&r) && (left..=right).contains(&c);
            for (name, op) in ops {
                let mut grid = Grid::new(4, 10);
                fill_grid(&mut grid, 4, 10);
                for &(row, col) in PROTECTED {
                    protect_cell(&mut grid, row, col);
                }
                grid.set_pending_wrap(true);
                op(&mut grid, rect);
                assert!(grid.pending_wrap(), "{name} {rect:?} consumed the wrap");
                for row in 0..4 {
                    for col in 0..10 {
                        let original = (b'A' + col as u8) as char;
                        let hit = inside(row, col);
                        let at = format!("{name} {rect:?} at ({row}, {col})");
                        let cell = grid.cell(row, col).expect("cell");
                        match name {
                            "DECERA" => {
                                assert_eq!(cell.char(), if hit { ' ' } else { original }, "{at}");
                            }
                            "DECSERA" => {
                                let erased = hit && !PROTECTED.contains(&(row, col));
                                assert_eq!(
                                    cell.char(),
                                    if erased { ' ' } else { original },
                                    "{at}"
                                );
                            }
                            "DECFRA" => {
                                assert_eq!(cell.char(), if hit { 'X' } else { original }, "{at}");
                            }
                            "DECCARA set" => {
                                assert_eq!(cell.flags().contains(CellFlags::BOLD), hit, "{at}");
                            }
                            _ => {
                                assert_eq!(cell.flags().contains(CellFlags::BOLD), !hit, "{at}");
                            }
                        }
                    }
                }
            }
        }

        let mut grid = Grid::new(4, 10);
        fill_grid(&mut grid, 4, 10);
        grid.set_pending_wrap(true);
        grid.copy_rect(3, 5, 1, 2, 2, 0);
        assert_eq!(
            char_at(&grid, 2, 0),
            'A',
            "an inverted DECCRA source copies nothing"
        );
        grid.copy_rect(0, 0, 0, 4, 1, 0);
        assert!(grid.pending_wrap(), "DECCRA consumed the wrap");
    }

    #[test]
    fn test_el0_wide_glyph_parked_on_continuation_clears_both_halves_and_wrap() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "ABC");
        assert!(grid.write_wide_autowrap_fast(
            '中',
            crate::PackedColors::DEFAULT,
            CellFlags::empty()
        ));
        assert!(
            grid.pending_wrap(),
            "precondition: wide glyph arms the wrap"
        );
        assert_eq!(
            grid.cursor_col(),
            4,
            "precondition: parked on the continuation"
        );
        assert!(grid.cell(0, 3).unwrap().flags().contains(CellFlags::WIDE));

        grid.erase_to_end_of_line();

        for col in 3..5 {
            let cell = grid.cell(0, col).unwrap();
            assert_eq!(cell.char(), ' ', "col {col} of the wide glyph is blank");
            assert!(
                !cell.flags().contains(CellFlags::WIDE),
                "col {col} keeps no WIDE half"
            );
        }
        assert_eq!(char_at(&grid, 0, 2), 'C', "cells left of the glyph survive");
        assert!(!grid.pending_wrap(), "EL 0 resets the deferred wrap");
    }

    #[test]
    fn test_el0_breaks_the_wrap_into_the_row_below_as_xterm_clear_right_does() {
        for col in [0u16, 2, 4] {
            let mut grid = wrapped_pair();
            grid.move_cursor_to(0, col);
            grid.erase_to_end_of_line();
            assert!(
                !grid.row(1).unwrap().is_wrapped(),
                "EL 0 at col {col} breaks the link"
            );
            assert_eq!(char_at(&grid, 1, 0), 'F', "row 1's cells survive");
            assert_eq!(char_at(&grid, 1, 1), 'G');
        }
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, 3);
        grid.selective_erase_to_end_of_line();
        assert!(!grid.row(1).unwrap().is_wrapped(), "DECSEL 0 too");
    }

    #[test]
    fn test_el1_el2_and_el0_on_the_continuation_keep_the_link() {
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, 2);
        grid.erase_from_start_of_line();
        assert!(grid.row(1).unwrap().is_wrapped(), "EL 1 keeps the link");
        let mut grid = wrapped_pair();
        grid.move_cursor_to(0, 2);
        grid.erase_line();
        assert!(grid.row(1).unwrap().is_wrapped(), "EL 2 keeps the link");
        let mut grid = wrapped_pair();
        grid.move_cursor_to(1, 0);
        grid.erase_to_end_of_line();
        assert!(
            grid.row(1).unwrap().is_wrapped(),
            "EL 0 on row 1 leaves row 1's own flag"
        );
        grid.move_cursor_to(3, 0);
        grid.erase_to_end_of_line();
    }

    #[test]
    fn test_ed2_resets_display_offset() {
        let mut grid = Grid::new(4, 10);
        grid.scroll_up(3);
        grid.scroll_display(2);
        assert_eq!(
            grid.display_offset(),
            2,
            "precondition: viewport in history"
        );
        grid.erase_screen();
        assert_eq!(grid.display_offset(), 0);
    }

    /// DECSEL and DECSED fill unprotected cells with the BCE background while leaving protected cells untouched.
    #[test]
    fn selective_erases_fill_with_the_bce_background() {
        use crate::{Cell, PackedColors};
        let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
        type Case = (
            &'static str,
            (u16, u16),
            fn(&mut Grid),
            &'static [(u16, u16)],
        );
        let cases: [Case; 5] = [
            (
                "DECSEL 0",
                (1, 2),
                Grid::selective_erase_to_end_of_line,
                &[(1, 2), (1, 4)],
            ),
            (
                "DECSEL 1",
                (1, 4),
                Grid::selective_erase_from_start_of_line,
                &[(1, 0), (1, 2), (1, 4)],
            ),
            (
                "DECSEL 2",
                (1, 0),
                Grid::selective_erase_line,
                &[(1, 0), (1, 4)],
            ),
            (
                "DECSED 0",
                (1, 2),
                Grid::selective_erase_to_end_of_screen,
                &[(1, 2), (1, 4), (2, 0)],
            ),
            (
                "DECSED 2",
                (0, 0),
                Grid::selective_erase_screen,
                &[(0, 0), (1, 4), (2, 4)],
            ),
        ];
        for (what, (row, col), erase, erased) in cases {
            let mut grid = Grid::new(3, 5);
            fill_grid(&mut grid, 3, 5);
            protect_cell(&mut grid, 1, 3);
            grid.set_cursor_template(fill, None);
            grid.move_cursor_to(row, col);
            erase(&mut grid);
            for &(r, c) in erased {
                let cell = grid.cell(r, c).unwrap();
                assert_eq!(cell.char(), ' ', "{what}: ({r},{c}) erased");
                assert_eq!(
                    cell.colors(),
                    fill.colors(),
                    "{what}: ({r},{c}) takes the BCE blank"
                );
            }
            let kept = grid.cell(1, 3).unwrap();
            assert_ne!(
                kept.char(),
                ' ',
                "{what}: the protected cell keeps its glyph"
            );
            assert_eq!(
                kept.colors(),
                PackedColors::DEFAULT,
                "{what}: and its colours"
            );
        }
    }

    /// A selective erase with a BCE background extends row length to the last colored cell.
    #[test]
    fn a_selective_bce_erase_counts_as_row_content() {
        use crate::{Cell, PackedColors};
        let mut grid = Grid::new(2, 8);
        grid.move_cursor_to(0, 0);
        grid.write_char('a');
        grid.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(2)), None);
        grid.move_cursor_to(0, 3);
        grid.selective_erase_to_end_of_line();
        assert_eq!(
            grid.row(0).unwrap().len(),
            8,
            "coloured through the last column"
        );
        grid.set_cursor_template(Cell::EMPTY, None);
        grid.move_cursor_to(0, 1);
        grid.selective_erase_to_end_of_line();
        assert_eq!(
            grid.row(0).unwrap().len(),
            1,
            "a default erase shrinks it back to 'a'"
        );
    }

    /// The truecolor half: the cleared cells carry only the RGB marker, so the
    /// ring must hold their bytes; a protected cell's stay untouched.
    #[test]
    fn a_selective_erase_writes_the_truecolor_ring_for_the_cleared_cells_only() {
        use crate::{Cell, PackedColor};
        let mut grid = Grid::new(2, 5);
        fill_grid(&mut grid, 2, 5);
        protect_cell(&mut grid, 0, 2);
        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(10, 20, 30));
        grid.set_cursor_template(fill, Some([10, 20, 30]));
        grid.move_cursor_to(0, 0);
        grid.selective_erase_line();
        for col in [0, 1, 3, 4] {
            assert_eq!(grid.bg_rgb_at(0, col), Some([10, 20, 30]), "col {col}");
        }
        assert_eq!(
            grid.bg_rgb_at(0, 2),
            None,
            "the protected cell has no truecolor"
        );
        assert_ne!(char_at(&grid, 0, 2), ' ');
    }

    #[test]
    fn test_selective_erase_rect_scrubs_combining_marks() {
        let mut grid = Grid::new(2, 10);

        grid.move_cursor_to(0, 4);
        grid.write_char('e');
        if let Some(r) = grid.row_mut(0) {
            r.get_mut(4).unwrap().set_has_extras(true);
        }
        grid.extras_mut()
            .get_or_create(crate::CellCoord::new(0, 4))
            .add_combining('\u{0301}');

        assert!(
            grid.row_text(0).unwrap().contains("e\u{0301}"),
            "precondition: combining mark present before DECSERA"
        );

        grid.selective_erase_rect(0, 4, 0, 4);

        assert_eq!(char_at(&grid, 0, 4), ' ', "cell wiped to a space");
        let text = grid.row_text(0).unwrap();
        assert!(
            !text.contains('\u{0301}'),
            "combining acute must not survive DECSERA, got: {text:?}"
        );
        assert!(
            !grid.cell(0, 4).unwrap().has_extras(),
            "HAS_EXTRAS must be cleared once the extra is empty"
        );
        assert!(
            grid.extras().get(crate::CellCoord::new(0, 4)).is_none(),
            "empty extra entry must be dropped"
        );
    }

    #[test]
    fn test_selective_erase_rect_preserves_truecolor_extra() {
        let mut grid = Grid::new(2, 10);

        grid.move_cursor_to(0, 4);
        grid.write_char('e');
        if let Some(r) = grid.row_mut(0) {
            r.get_mut(4).unwrap().set_has_extras(true);
        }
        {
            let extra = grid.extras_mut().get_or_create(crate::CellCoord::new(0, 4));
            extra.add_combining('\u{0301}');
            extra.set_bg_rgb(Some([10, 20, 30]));
        }

        grid.selective_erase_rect(0, 4, 0, 4);

        assert_eq!(char_at(&grid, 0, 4), ' ', "cell wiped to a space");
        assert!(
            !grid.row_text(0).unwrap().contains('\u{0301}'),
            "combining acute must be scrubbed"
        );
        assert!(
            grid.cell(0, 4).unwrap().has_extras(),
            "HAS_EXTRAS must remain set while color-bearing data survives"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 4),
            Some([10, 20, 30]),
            "bg truecolor must survive DECSERA"
        );
    }

    #[test]
    fn test_selective_erase_rect_wide_cell_leaves_no_orphan_continuation() {
        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(
            grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: col 0 is the wide lead"
        );
        assert!(
            grid.cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: col 1 is the continuation"
        );

        grid.selective_erase_rect(0, 1, 0, 1);

        assert_eq!(char_at(&grid, 0, 0), ' ', "lead wiped to a space");
        assert_eq!(
            char_at(&grid, 0, 1),
            ' ',
            "continuation wiped, not orphaned"
        );
        assert!(
            !grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "lead WIDE flag cleared (no stale wide space)"
        );
        assert!(
            !grid
                .cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "continuation flag cleared (no orphaned spacer)"
        );
    }

    #[test]
    fn copy_rect_preserves_protected_cell_at_source_left_edge() {
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(0, 0);
        for ch in ['A', 'B', 'C'] {
            grid.write_char(ch);
        }
        protect_cell(&mut grid, 0, 0); // 'A' at the source-rect left edge is protected
        assert!(
            grid.cell(0, 0)
                .unwrap()
                .flags()
                .contains(CellFlags::PROTECTED),
            "precondition: (0,0) carries the PROTECTED (bit-10) flag"
        );
        assert!(
            !grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: it is a NON-wide protected cell (no real continuation)"
        );

        grid.copy_rect(0, 0, 0, 2, 2, 0);

        assert_eq!(
            char_at(&grid, 2, 0),
            'A',
            "protected left-edge cell must be copied verbatim, not blanked"
        );
        assert_eq!(char_at(&grid, 2, 1), 'B');
        assert_eq!(char_at(&grid, 2, 2), 'C');
    }

    #[test]
    fn copy_rect_shrinks_dst_len_when_copied_blanks_empty_the_tail() {
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(0, 0);
        for ch in ['h', 'e', 'l', 'l', 'o'] {
            grid.write_char(ch);
        }
        assert_eq!(
            grid.row_len(0),
            Some(5),
            "precondition: row 0 is \"hello\", len 5"
        );

        grid.copy_rect(2, 0, 2, 1, 0, 3);

        assert_eq!(char_at(&grid, 0, 0), 'h');
        assert_eq!(char_at(&grid, 0, 2), 'l');
        assert!(
            grid.cell(0, 3).unwrap().is_empty(),
            "blanked tail cell must be empty"
        );
        assert_eq!(
            grid.row_len(0),
            Some(3),
            "len must shrink to 3, not stay stale-high at 5"
        );
        assert_eq!(
            grid.row_text(0).as_deref(),
            Some("hel"),
            "no phantom trailing spaces"
        );
    }

    #[test]
    fn change_attrs_rect_maintains_len_both_directions() {
        let mut grid = Grid::new(3, 10);
        assert_eq!(grid.row_len(0), Some(0), "precondition: row 0 blank");

        grid.change_attrs_rect(0, 5, 0, 9, CellFlags::BOLD, CellFlags::empty());
        assert_eq!(
            grid.row_len(0),
            Some(10),
            "styled (non-empty) tail cells must be counted in len (stale-low fixed)"
        );

        grid.change_attrs_rect(0, 5, 0, 9, CellFlags::empty(), CellFlags::BOLD);
        assert_eq!(
            grid.row_len(0),
            Some(0),
            "len must shrink when the tail becomes empty again (stale-high fixed)"
        );
    }

    #[test]
    fn reverse_attrs_rect_maintains_len_both_directions() {
        let mut grid = Grid::new(3, 10);
        assert_eq!(grid.row_len(0), Some(0), "precondition: row 0 blank");

        grid.reverse_attrs_rect(0, 5, 0, 9, CellFlags::BOLD, false);
        assert_eq!(
            grid.row_len(0),
            Some(10),
            "reversing BOLD onto blank cells makes them non-empty"
        );

        grid.reverse_attrs_rect(0, 5, 0, 9, CellFlags::BOLD, false);
        assert_eq!(
            grid.row_len(0),
            Some(0),
            "reversing it back empties the tail again"
        );
    }

    #[test]
    fn reverse_attrs_of_underline_family_never_yields_an_unnamed_style() {
        for start in [
            CellFlags::UNDERLINE,
            CellFlags::DOUBLE_UNDERLINE,
            CellFlags::CURLY_UNDERLINE,
            CellFlags::DOTTED_UNDERLINE,
            CellFlags::DASHED_UNDERLINE,
        ] {
            let got = Grid::reverse_cell_attrs(start, CellFlags::empty(), true);
            assert_eq!(
                got.bits(),
                0,
                "every underline style reverses to no underline, got {got:?} from {start:?}"
            );
        }
        let got = Grid::reverse_cell_attrs(CellFlags::empty(), CellFlags::empty(), true);
        assert_eq!(
            got,
            CellFlags::UNDERLINE,
            "no underline reverses to the single underline SGR 4 itself sets"
        );
        let got = Grid::reverse_cell_attrs(
            CellFlags::BOLD.union(CellFlags::CURLY_UNDERLINE),
            CellFlags::empty(),
            true,
        );
        assert_eq!(
            got,
            CellFlags::BOLD,
            "reversing the underline family leaves unrelated attributes alone"
        );
    }

    #[test]
    fn copy_rect_still_blanks_genuine_orphaned_wide_continuation_at_left_edge() {
        let mut grid = Grid::new(3, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.move_cursor_to(0, 2);
        grid.write_char('X');
        assert!(
            grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: (0,0) is the WIDE base"
        );
        assert!(
            grid.cell(0, 1)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: (0,1) is the continuation"
        );

        grid.copy_rect(0, 1, 0, 2, 2, 0);

        assert!(
            is_empty_at(&grid, 2, 0),
            "genuine orphaned continuation at src_left must still be blanked"
        );
        assert_eq!(
            char_at(&grid, 2, 1),
            'X',
            "the in-range cell copies normally"
        );
    }

    #[test]
    fn erase_rect_preserves_protected_cell_left_of_the_region() {
        let mut grid = Grid::new(3, 10);
        fill_row(&mut grid, 0, 10); // 'A','B','C',… — all narrow, no wide pairs
        protect_cell(&mut grid, 0, 1); // protect the region's left-edge cell (col 1)

        grid.erase_rect(0, 1, 0, 5);

        assert_eq!(
            char_at(&grid, 0, 0),
            'A',
            "the cell LEFT of the region must not be corrupted by the boundary fixup"
        );
        for col in 1..=5 {
            assert_eq!(
                char_at(&grid, 0, col),
                ' ',
                "col {col} inside the region is erased"
            );
        }
        assert_eq!(
            char_at(&grid, 0, 6),
            'G',
            "the cell right of the region survives"
        );
    }

    /// EL 0 preserves the row length over a preceding blank cell that carries a combining mark.
    #[test]
    fn erase_to_end_of_line_keeps_a_marked_blank_in_the_row() {
        for marked in [true, false] {
            let mut grid = Grid::new(2, 10);
            grid.write_char('$');
            grid.write_char(' ');
            if marked {
                grid.cell_extra_mut(0, 1).add_combining('\u{301}');
                if let Some(r) = grid.row_mut(0) {
                    r.update_len(2);
                }
            }
            grid.write_char('n');
            grid.move_cursor_to(0, 2);
            grid.erase_to_end_of_line();
            let want = if marked { Some(2) } else { Some(1) };
            assert_eq!(grid.row_len(0), want, "marked {marked}");
            if marked {
                assert!(
                    grid.row_text(0).is_some_and(|t| t.contains(" \u{301}")),
                    "the mark still reads: {:?}",
                    grid.row_text(0)
                );
            }
        }
    }

    /// EL 0 that bisects a wide pair gives the orphaned half the BCE
    /// background, not default colours.
    #[test]
    fn erase_to_end_of_line_gives_the_wide_orphan_the_bce_background() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(
            grid.cell(0, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: col 0 is the wide lead"
        );

        let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
        grid.set_cursor_template(fill, None);

        grid.move_cursor_to(0, 1);
        grid.erase_to_end_of_line();

        let orphan = grid.cell(0, 0).unwrap();
        assert_eq!(orphan.char(), ' ', "the orphaned lead is blanked");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the orphan must take the BCE background like the erased span \
             (xterm ClearRight -> ClearCells)"
        );
        assert!(
            !is_empty_at(&grid, 0, 0),
            "a default-coloured orphan renders as a hole in the BCE run"
        );
    }

    /// The truecolor half: a BCE cell holds only an RGB marker, so the ring
    /// must extend to the orphan that now carries the marker.
    #[test]
    fn erase_to_end_of_line_extends_the_bce_truecolor_ring_to_the_wide_orphan() {
        use crate::{Cell, PackedColor};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );

        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(10, 20, 30));
        grid.set_cursor_template(fill, Some([10, 20, 30]));

        grid.move_cursor_to(0, 1);
        grid.erase_to_end_of_line();

        assert_eq!(
            grid.bg_rgb_at(0, 1),
            Some([10, 20, 30]),
            "precondition: the erased span resolves its truecolor background"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 0),
            Some([10, 20, 30]),
            "the orphan column must resolve the same truecolor background — the \
             cell carries only the RGB marker, so the ring has to reach it"
        );
    }

    /// EL 1 hits the right boundary instead: the continuation just past the
    /// erase is the orphan.
    #[test]
    fn erase_from_start_of_line_gives_the_wide_orphan_the_bce_background() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 4);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        assert!(
            grid.cell(0, 5)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: col 5 is the continuation"
        );

        let fill = Cell::bce_blank(PackedColors::with_indexed_bg(4));
        grid.set_cursor_template(fill, None);

        grid.move_cursor_to(0, 4);
        grid.erase_from_start_of_line();

        let orphan = grid.cell(0, 5).unwrap();
        assert_eq!(orphan.char(), ' ', "the orphaned continuation is blanked");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the right-hand orphan must take the BCE background too"
        );
    }

    /// The truecolor half of EL 1: the orphaned continuation sits one column
    /// past the erased range, so `fill_bce_rgb_range` runs to `span.end`.
    #[test]
    fn erase_from_start_of_line_extends_the_bce_truecolor_ring_to_the_wide_orphan() {
        use crate::{Cell, PackedColor};

        let (rows, cols) = (2u16, 10u16);
        let mut grid = Grid::new(rows, cols);

        let dead_bg = [7u8, 8, 9];
        grid.move_cursor_to(0, 4);
        grid.write_wide_char_styled(
            '中',
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(dead_bg[0], dead_bg[1], dead_bg[2]),
            CellFlags::empty(),
        );
        grid.extras_mut()
            .set_rgb_ring_range(0, 4, 6, None, Some(dead_bg), rows, cols);
        assert_eq!(
            grid.bg_rgb_at(0, 5),
            Some(dead_bg),
            "precondition: the continuation carries the glyph's own bytes"
        );

        let bce_bg = [10u8, 20, 30];
        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(bce_bg[0], bce_bg[1], bce_bg[2]));
        grid.set_cursor_template(fill, Some(bce_bg));

        grid.move_cursor_to(0, 4);
        grid.erase_from_start_of_line();

        assert_eq!(
            grid.bg_rgb_at(0, 4),
            Some(bce_bg),
            "precondition: the erased span resolves its truecolor background"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 5),
            Some(bce_bg),
            "the right-hand orphan must resolve the BCE truecolor background — \
             the cell carries only the RGB marker, so the ring has to reach one \
             column PAST the erased range or the orphan keeps the dead glyph's \
             bytes"
        );
    }

    /// EL 2 over a DECLRMM-margin row: the orphan just past the right margin
    /// takes the BCE blank, and the ring follows it.
    #[test]
    fn erase_line_within_margins_gives_the_wide_orphan_the_bce_background_and_ring() {
        use crate::{Cell, PackedColor};

        let (rows, cols) = (2u16, 12u16);
        let mut grid = Grid::new(rows, cols);

        let dead_bg = [7u8, 8, 9];
        grid.move_cursor_to(0, 6);
        grid.write_wide_char_styled(
            '中',
            PackedColor::DEFAULT_FG,
            PackedColor::rgb(dead_bg[0], dead_bg[1], dead_bg[2]),
            CellFlags::empty(),
        );
        grid.extras_mut()
            .set_rgb_ring_range(0, 6, 8, None, Some(dead_bg), rows, cols);
        assert!(
            grid.cell(0, 7)
                .unwrap()
                .flags()
                .contains(CellFlags::WIDE_CONTINUATION),
            "precondition: col 7 is the continuation, one past the right margin"
        );

        grid.set_horizontal_margins(2, 6);
        let bce_bg = [10u8, 20, 30];
        let fill = Cell::bce_blank_from_bg(PackedColor::rgb(bce_bg[0], bce_bg[1], bce_bg[2]));
        grid.set_cursor_template(fill, Some(bce_bg));

        grid.move_cursor_to(0, 4);
        grid.erase_line();

        let orphan = grid.cell(0, 7).unwrap();
        assert_eq!(orphan.char(), ' ', "the orphaned continuation is blanked");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the margin branch must give the orphan the BCE background too — \
             xterm reaches the same ClearRight -> ClearCells here"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 6),
            Some(bce_bg),
            "precondition: the erased span resolves its truecolor background"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 7),
            Some(bce_bg),
            "the orphan past the right margin must resolve it too — the ring \
             has to reach one column past the margin or the orphan keeps the \
             dead glyph's bytes behind a BCE marker"
        );
    }

    /// DECERA clears an orphaned wide half outside the rectangle to `Cell::EMPTY` without applying the BCE background.
    #[test]
    fn erase_rect_keeps_the_wide_orphan_empty_under_a_bce_background() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );

        grid.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(4)), None);

        grid.erase_rect(0, 1, 0, 5);

        assert!(
            is_empty_at(&grid, 0, 0),
            "#7522: the orphan one column LEFT of the rect must stay Cell::EMPTY, \
             not take the rect's fill colour"
        );
    }

    /// DECFRA's fill glyph must not bleed into the orphan one column outside the
    /// rectangle.
    #[test]
    fn fill_rect_keeps_the_wide_orphan_empty() {
        use crate::{Cell, PackedColors};

        let mut grid = Grid::new(2, 10);
        grid.move_cursor_to(0, 0);
        grid.write_wide_char_styled(
            '中',
            crate::PackedColor::DEFAULT_FG,
            crate::PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        grid.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(4)), None);

        let fill = Cell::new('X');
        grid.fill_rect(fill, 0, 1, 0, 5, None, None);

        assert_eq!(char_at(&grid, 0, 3), 'X', "the rect itself takes the fill");
        assert!(
            is_empty_at(&grid, 0, 0),
            "#7522: the DECFRA fill glyph must not bleed onto the orphan"
        );
    }

    #[test]
    fn test_fill_rect_preserves_cell_attributes() {
        use crate::{Cell, PackedColors};
        let mut grid = Grid::new(4, 10);
        let colors = PackedColors::with_indexed(196, 0);
        let flags = CellFlags::BOLD;
        let template = Cell::from_ascii_styled(b'X', colors, flags);
        grid.fill_rect(template, 1, 2, 2, 5, None, None);

        for row in 1..=2 {
            for col in 2..=5 {
                let cell = grid.cell(row, col).expect("cell should exist");
                assert_eq!(cell.char(), 'X', "({row},{col}) should be 'X'");
                assert!(
                    cell.flags().contains(CellFlags::BOLD),
                    "({row},{col}) should have BOLD flag (#7655)"
                );
                assert!(
                    cell.colors().fg_is_indexed(),
                    "({row},{col}) should have indexed FG color (#7655)"
                );
            }
        }
    }

    #[test]
    fn test_fill_rect_repopulates_truecolor_ring() {
        use crate::{Cell, PackedColors};
        let mut grid = Grid::new(4, 10);
        let colors = PackedColors::new().with_rgb_fg().with_rgb_bg();
        let fg = [30u8, 60, 90];
        let bg = [10u8, 20, 30];
        let template = Cell::from_ascii_styled(b'Z', colors, CellFlags::empty());
        grid.fill_rect(template, 1, 2, 2, 5, Some(fg), Some(bg));

        for row in 1..=2 {
            for col in 2..=5 {
                let cell = grid.cell(row, col).expect("cell should exist");
                assert_eq!(cell.char(), 'Z', "({row},{col}) should be 'Z'");
                assert!(
                    cell.colors().fg_is_rgb() && cell.colors().bg_is_rgb(),
                    "({row},{col}) should carry RGB markers"
                );
                assert_eq!(
                    grid.fg_rgb_at(row, col),
                    Some(fg),
                    "({row},{col}) fg truecolor must survive DECFRA"
                );
                assert_eq!(
                    grid.bg_rgb_at(row, col),
                    Some(bg),
                    "({row},{col}) bg truecolor must survive DECFRA"
                );
            }
        }
        assert_eq!(grid.fg_rgb_at(0, 2), None, "row above rect untouched");
        assert_eq!(grid.bg_rgb_at(1, 1), None, "col left of rect untouched");
    }

    #[test]
    fn test_copy_rect_repopulates_truecolor_and_complex_rings() {
        use crate::{Cell, CellCoord, PackedColors};
        let mut grid = Grid::new(4, 10);
        let rows = grid.rows();
        let cols = grid.cols();

        let fg = [200u8, 100, 50];
        let bg = [12u8, 34, 56];
        let rgb_colors = PackedColors::new().with_rgb_fg().with_rgb_bg();
        let truecolor_cell = Cell::from_ascii_styled(b'A', rgb_colors, CellFlags::empty());
        if let Some(r) = grid.row_mut(0) {
            r.set(1, truecolor_cell);
        }
        grid.extras_mut()
            .set_rgb_ring_range(0, 1, 2, Some(fg), Some(bg), rows, cols);

        let emoji = '\u{1F680}'; // 🚀
        let mut complex_cell = Cell::EMPTY;
        let flags = complex_cell.flags().union(CellFlags::COMPLEX);
        complex_cell.set_flags(flags);
        if let Some(r) = grid.row_mut(0) {
            r.set(3, complex_cell);
        }
        grid.extras_mut()
            .set_complex_char_ring(0, 3, emoji, rows, cols);

        assert_eq!(grid.fg_rgb_at(0, 1), Some(fg));
        assert_eq!(grid.bg_rgb_at(0, 1), Some(bg));
        assert_eq!(grid.complex_char_at(0, 3), Some(emoji));
        assert!(grid.extras().get(CellCoord::new(0, 1)).is_none());
        assert!(grid.extras().get(CellCoord::new(0, 3)).is_none());

        grid.copy_rect(0, 0, 0, 9, 2, 0);

        let dst_tc = grid.cell(2, 1).expect("dst truecolor cell exists");
        assert!(
            dst_tc.colors().fg_is_rgb() && dst_tc.colors().bg_is_rgb(),
            "dst cell keeps RGB markers"
        );
        assert_eq!(
            grid.fg_rgb_at(2, 1),
            Some(fg),
            "fg truecolor must survive DECCRA copy"
        );
        assert_eq!(
            grid.bg_rgb_at(2, 1),
            Some(bg),
            "bg truecolor must survive DECCRA copy"
        );
        assert!(
            grid.cell(2, 3)
                .expect("dst complex cell exists")
                .is_complex(),
            "dst cell keeps COMPLEX marker"
        );
        assert_eq!(
            grid.complex_char_at(2, 3),
            Some(emoji),
            "complex codepoint must survive DECCRA copy"
        );
    }

    #[test]
    fn test_copy_rect_clears_stale_dst_complex_ring() {
        use crate::Cell;
        let mut grid = Grid::new(4, 10);
        let rows = grid.rows();
        let cols = grid.cols();

        let mut complex_cell = Cell::EMPTY;
        let flags = complex_cell.flags().union(CellFlags::COMPLEX);
        complex_cell.set_flags(flags);
        if let Some(r) = grid.row_mut(2) {
            r.set(0, complex_cell);
        }
        grid.extras_mut()
            .set_complex_char_ring(2, 0, '\u{1F600}', rows, cols);
        assert_eq!(grid.complex_char_at(2, 0), Some('\u{1F600}'));

        grid.move_cursor_to(0, 0);
        write_text(&mut grid, "B");

        grid.copy_rect(0, 0, 0, 0, 2, 0);

        assert_eq!(char_at(&grid, 2, 0), 'B', "dst now holds the copied 'B'");
        assert!(
            !grid.cell(2, 0).expect("dst cell exists").is_complex(),
            "dst cell is no longer complex"
        );
        assert_eq!(
            grid.complex_char_at(2, 0),
            None,
            "stale dst complex codepoint must be cleared by copy_rect"
        );
    }

    /// Set a row's DEC line attribute the way `ESC#3` / `ESC#4` / `ESC#6` does:
    /// the row flag plus the grid-level fast-path flag.
    fn poison_row(grid: &mut Grid, row: u16, size: crate::LineSize) {
        if let Some(r) = grid.row_mut(row) {
            r.set_line_size(size);
        }
        grid.mark_has_double_width();
    }

    fn line_size_of(grid: &Grid, row: u16) -> crate::LineSize {
        grid.row(row)
            .map_or(crate::LineSize::SingleWidth, |r| r.line_size())
    }

    #[test]
    fn ed2_returns_every_erased_row_to_single_width() {
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 0, crate::LineSize::DoubleWidth);
        poison_row(&mut grid, 1, crate::LineSize::DoubleHeightTop);
        poison_row(&mut grid, 2, crate::LineSize::DoubleHeightBottom);

        grid.erase_screen();

        for row in 0..4 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::SingleWidth,
                "ED 2 erases complete lines, so row {row} is single-width again"
            );
        }
        assert!(
            !grid.has_any_double_width(),
            "no visible row is double any more, so the fast-path flag goes with them"
        );
    }

    #[test]
    fn ed2_gives_back_the_columns_a_double_width_row_had_halved() {
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 0, crate::LineSize::DoubleWidth);
        grid.set_cursor(0, 19);
        assert_eq!(
            grid.cursor().col,
            9,
            "while double-width, column 19 of a 20-column row clamps to the halfway mark"
        );

        grid.erase_screen();
        grid.set_cursor(0, 19);

        assert_eq!(
            grid.cursor().col,
            19,
            "after ED 2 the erased row is single-width, so all 20 columns are usable"
        );
    }

    #[test]
    fn ed0_resets_the_complete_rows_below_and_leaves_the_cursor_row_alone() {
        let mut grid = Grid::new(4, 20);
        for row in 0..4 {
            poison_row(&mut grid, row, crate::LineSize::DoubleWidth);
        }
        grid.set_cursor(1, 2);

        grid.erase_to_end_of_screen();

        assert_eq!(
            line_size_of(&grid, 0),
            crate::LineSize::DoubleWidth,
            "ED 0 does not reach the rows above the cursor"
        );
        assert_eq!(
            line_size_of(&grid, 1),
            crate::LineSize::DoubleWidth,
            "the cursor row is a partial erase and keeps its attribute"
        );
        for row in 2..4 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::SingleWidth,
                "row {row} was erased as a complete line"
            );
        }
        assert!(
            grid.has_any_double_width(),
            "rows 0 and 1 are still double, so the fast-path flag must stay raised"
        );
    }

    #[test]
    fn ed1_resets_the_complete_rows_above_and_leaves_the_cursor_row_alone() {
        let mut grid = Grid::new(4, 20);
        for row in 0..4 {
            poison_row(&mut grid, row, crate::LineSize::DoubleWidth);
        }
        grid.set_cursor(2, 2);

        grid.erase_from_start_of_screen();

        for row in 0..2 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::SingleWidth,
                "row {row} was erased as a complete line"
            );
        }
        assert_eq!(
            line_size_of(&grid, 2),
            crate::LineSize::DoubleWidth,
            "the cursor row is a partial erase and keeps its attribute"
        );
        assert_eq!(
            line_size_of(&grid, 3),
            crate::LineSize::DoubleWidth,
            "ED 1 does not reach the rows below the cursor"
        );
    }

    #[test]
    fn an_ed0_that_clears_the_last_double_row_lowers_the_fast_path_flag() {
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 3, crate::LineSize::DoubleWidth);
        grid.set_cursor(0, 0);

        grid.erase_to_end_of_screen();

        assert_eq!(line_size_of(&grid, 3), crate::LineSize::SingleWidth);
        assert!(
            !grid.has_any_double_width(),
            "the last double row is gone, so the rescan lowers the flag"
        );
    }

    #[test]
    fn el2_keeps_the_line_attribute_because_xterm_does() {
        let mut grid = Grid::new(4, 20);
        poison_row(&mut grid, 1, crate::LineSize::DoubleWidth);
        grid.set_cursor(1, 0);

        grid.erase_line();

        assert_eq!(
            line_size_of(&grid, 1),
            crate::LineSize::DoubleWidth,
            "EL 2 clears the characters, not the line attribute"
        );
    }

    #[test]
    fn decsed_preserves_line_attributes_on_every_row_it_erases() {
        let mut grid = Grid::new(4, 20);
        for row in 0..4 {
            poison_row(&mut grid, row, crate::LineSize::DoubleWidth);
        }

        grid.selective_erase_screen();

        for row in 0..4 {
            assert_eq!(
                line_size_of(&grid, row),
                crate::LineSize::DoubleWidth,
                "DECSED keeps row {row}'s line attribute"
            );
        }
    }
}
