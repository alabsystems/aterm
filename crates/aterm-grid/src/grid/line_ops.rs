// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Character, line and column insertion, deletion and erase.

use super::{Grid, row_u16};
use crate::row::LineSize;

impl Grid {
    /// Whether ICH/DCH shift the cursor row's cells only inside
    /// `[cursor, right_bound)` (the bounded row ops) rather than through the
    /// row's end: under a DECSLRM margin (`margined`), and wherever
    /// `right_bound` stops short of the row, as on a DECDWL/DECDHL row, whose
    /// line is its first half (`effective_cols_for_row`).
    #[inline]
    const fn shift_is_bounded(&self, right_bound: u16, margined: bool) -> bool {
        margined || right_bound < self.storage.cols
    }

    /// Insert `count` blank characters at the cursor (ICH), shifting the rest right and discarding what passes the edge.
    #[inline]
    pub fn insert_chars(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        let margins = self.storage.horizontal_margins();
        let right_bound = (margins.right.saturating_add(1)).min(effective_cols);
        let fill = self.storage.cursor_template;
        let bounded = self.shift_is_bounded(right_bound, !margins.is_full(self.storage.cols));
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            let blanked = self.row_mut(cursor_row).map(|row| {
                if bounded {
                    row.insert_chars_bounded_fill(cursor_col, count, right_bound, fill)
                } else {
                    row.insert_chars_fill(cursor_col, count, fill)
                }
            });
            self.storage
                .extras
                .shift_cols_right(cursor_row, cursor_col, count, right_bound);
            self.drop_unclaimed_extras(
                cursor_row,
                cursor_col.saturating_sub(1)..right_bound.saturating_add(1),
            );
            self.fill_bce_rgb_range(cursor_row, cursor_col, cursor_col + count);
            self.fill_bce_rgb_cols(cursor_row, blanked.unwrap_or_default());
            self.damage_selection_visible_rows(cursor_row, cursor_row);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Delete `count` characters at the cursor (DCH), shifting the rest left and filling the end with the BCE cursor template.
    #[inline]
    pub fn delete_chars(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        let fill = self.storage.cursor_template;
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        let margins = self.storage.horizontal_margins();
        let right_bound = (margins.right.saturating_add(1)).min(effective_cols);
        let bounded = self.shift_is_bounded(right_bound, !margins.is_full(self.storage.cols));
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            let blanked = self.row_mut(cursor_row).map(|row| {
                if bounded {
                    row.delete_chars_bounded_fill(cursor_col, count, right_bound, fill)
                } else {
                    row.delete_chars_fill(cursor_col, count, fill)
                }
            });
            self.storage
                .extras
                .shift_cols_left(cursor_row, cursor_col, count, right_bound);
            self.drop_unclaimed_extras(
                cursor_row,
                cursor_col.saturating_sub(1)..right_bound.saturating_add(1),
            );
            self.fill_bce_rgb_range(cursor_row, right_bound - count, right_bound);
            self.fill_bce_rgb_cols(cursor_row, blanked.unwrap_or_default());
            self.damage_selection_visible_rows(cursor_row, cursor_row);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Margin-aware insert characters (DECLRMM).
    #[inline]
    pub fn insert_chars_margin(&mut self, count: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.insert_chars(count);
            return;
        }
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        // Outside the margins ICH changes nothing, the pending wrap included: xterm's
        // `InsertChar` returns before its `ResetWrap`.
        let in_margins = cursor_col >= margins.left && cursor_col <= margins.right;
        if !in_margins {
            return;
        }
        self.storage.clear_pending_wrap();
        let right_bound = margins.right.saturating_add(1).min(effective_cols);
        let fill = self.storage.cursor_template;
        let bounded = self.shift_is_bounded(right_bound, in_margins);
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            let blanked = self.row_mut(cursor_row).map(|row| {
                if bounded {
                    row.insert_chars_bounded_fill(cursor_col, count, right_bound, fill)
                } else {
                    row.insert_chars_fill(cursor_col, count, fill)
                }
            });
            self.storage
                .extras
                .shift_cols_right(cursor_row, cursor_col, count, right_bound);
            self.drop_unclaimed_extras(
                cursor_row,
                cursor_col.saturating_sub(1)..right_bound.saturating_add(1),
            );
            self.fill_bce_rgb_range(cursor_row, cursor_col, cursor_col + count);
            self.fill_bce_rgb_cols(cursor_row, blanked.unwrap_or_default());
            self.damage_selection_visible_rows(cursor_row, cursor_row);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Margin-aware delete characters (DECLRMM).
    #[inline]
    pub fn delete_chars_margin(&mut self, count: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.delete_chars(count);
            return;
        }
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        // Outside the margins DCH changes nothing, the pending wrap included: xterm's
        // `DeleteChar` returns before its `ResetWrap`.
        let in_margins = cursor_col >= margins.left && cursor_col <= margins.right;
        if !in_margins {
            return;
        }
        self.storage.clear_pending_wrap();
        let right_bound = margins.right.saturating_add(1).min(effective_cols);
        let fill = self.storage.cursor_template;
        let bounded = self.shift_is_bounded(right_bound, in_margins);
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            let blanked = self.row_mut(cursor_row).map(|row| {
                if bounded {
                    row.delete_chars_bounded_fill(cursor_col, count, right_bound, fill)
                } else {
                    row.delete_chars_fill(cursor_col, count, fill)
                }
            });
            self.storage
                .extras
                .shift_cols_left(cursor_row, cursor_col, count, right_bound);
            self.drop_unclaimed_extras(
                cursor_row,
                cursor_col.saturating_sub(1)..right_bound.saturating_add(1),
            );
            self.fill_bce_rgb_range(cursor_row, right_bound - count, right_bound);
            self.fill_bce_rgb_cols(cursor_row, blanked.unwrap_or_default());
            self.damage_selection_visible_rows(cursor_row, cursor_row);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Erase `count` characters at the cursor in place (ECH), with the BCE cursor template.
    #[inline]
    pub fn erase_chars(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let right_bound = self.storage.effective_cols_for_row(cursor_row);
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            let fill = self.storage.cursor_template;
            let blanked = self
                .row_mut(cursor_row)
                .map(|row| row.erase_chars_with(cursor_col, count, fill));
            self.storage
                .extras
                .clear_range(cursor_row, cursor_col, cursor_col + count);
            self.drop_unclaimed_extras(
                cursor_row,
                cursor_col.saturating_sub(1)..(cursor_col + count).saturating_add(1),
            );
            self.fill_bce_rgb_range(cursor_row, cursor_col, cursor_col + count);
            self.fill_bce_rgb_cols(cursor_row, blanked.unwrap_or_default());
            self.damage_selection_visible_rows(cursor_row, cursor_row);
            self.storage.mark_content_row(cursor_row);
            if cursor_col + count == right_bound {
                self.clear_wrap_into_next_row(cursor_row);
            }
        }
    }

    /// Insert `count` blank lines at the cursor row (IL).
    pub fn insert_lines(&mut self, count: usize) {
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;

        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        self.storage.clear_pending_wrap();

        self.storage.cursor.col = self.storage.horizontal_margins().left;

        self.reset_display_offset_with_damage();

        let start_row = usize::from(cursor_row);
        let end_row = usize::from(region.bottom) + 1;

        let shift_n = count.min(end_row - start_row);
        if end_row > start_row + shift_n {
            self.storage
                .shift_visible_rows_down(start_row, end_row - 1, shift_n);
        }

        self.storage
            .extras
            .shift_region_down_by(cursor_row, region.bottom, row_u16(shift_n));

        let fill = self.storage.cursor_template;
        let clear_end = (start_row + count).min(end_row);
        for row in start_row..clear_end {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }
        self.fill_bce_rgb_rows(cursor_row..row_u16(clear_end));

        match i16::try_from(shift_n) {
            Ok(delta) => {
                self.damage_selection_scroll_region_as_band(cursor_row, region.bottom, delta);
            }
            Err(_) => {
                self.damage_selection_scroll_region();
                let delta = i64::try_from(shift_n).unwrap_or(i64::MAX);
                self.anchors_band_move(cursor_row, region.bottom, delta);
            }
        }

        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Margin-aware insert lines (DECLRMM).
    pub fn insert_lines_margined(&mut self, count: usize, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.insert_lines(count);
            return;
        }
        let margins = self.storage.horizontal_margins();
        if margins.is_full(self.storage.cols) {
            self.insert_lines(count);
            return;
        }
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;
        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        let cursor_col = self.storage.cursor.col;
        if cursor_col < margins.left || cursor_col > margins.right {
            return;
        }

        self.storage.cursor.col = margins.left;
        self.storage.clear_pending_wrap();
        self.reset_display_offset_with_damage();

        let top = usize::from(cursor_row);
        let bottom = usize::from(region.bottom);
        let region_size = bottom - top + 1;
        let n = count.min(region_size);
        let left_usize = usize::from(margins.left);
        let right_usize = usize::from(margins.right);
        let width = right_usize + 1 - left_usize;
        let cols = self.storage.cols as usize;

        // Unpair what straddles a margin first, so the shift splits no glyph.
        self.unpair_margin_straddlers(top, bottom, left_usize, right_usize);
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

        #[allow(
            clippy::cast_possible_truncation,
            reason = "n <= region_size <= visible_rows, fits u16"
        )]
        self.storage.extras.shift_rect_down_by(
            cursor_row,
            region.bottom,
            margins.left,
            margins.right,
            n as u16,
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "n <= region_size <= visible_rows, fits u16"
        )]
        self.fill_bce_rgb_rect(
            cursor_row..cursor_row.saturating_add(n as u16),
            margins.left..margins.right.saturating_add(1),
        );

        for row in cursor_row..=region.bottom {
            self.drop_unclaimed_extras(
                row,
                margins.left.saturating_sub(1)..margins.right.saturating_add(2),
            );
        }

        self.damage_selection_scroll_region();
        self.lose_anchors();
        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Delete `count` lines at the cursor row (DL).
    pub fn delete_lines(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;

        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        self.storage.clear_pending_wrap();

        self.storage.cursor.col = self.storage.horizontal_margins().left;

        self.reset_display_offset_with_damage();

        let start_row = usize::from(cursor_row);
        let end_row = usize::from(region.bottom) + 1;

        let shift_n = count.min(end_row - start_row);
        if end_row > start_row + shift_n {
            self.storage
                .shift_visible_rows_up(start_row, end_row - 1, shift_n);
        }

        self.storage
            .extras
            .shift_region_up_by(cursor_row, region.bottom, row_u16(shift_n));

        let fill = self.storage.cursor_template;
        let clear_start = end_row.saturating_sub(count).max(start_row);
        for row in clear_start..end_row {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }
        self.fill_bce_rgb_rows(row_u16(clear_start)..row_u16(end_row));

        match i16::try_from(shift_n) {
            Ok(rows) => {
                self.damage_selection_scroll_region_as_band(cursor_row, region.bottom, -rows);
            }
            Err(_) => {
                self.damage_selection_scroll_region();
                let delta = i64::try_from(shift_n).unwrap_or(i64::MAX);
                self.anchors_band_move(cursor_row, region.bottom, -delta);
            }
        }

        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Margin-aware delete lines (DECLRMM).
    pub fn delete_lines_margined(&mut self, count: usize, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.delete_lines(count);
            return;
        }
        let margins = self.storage.horizontal_margins();
        if margins.is_full(self.storage.cols) {
            self.delete_lines(count);
            return;
        }
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;
        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        let cursor_col = self.storage.cursor.col;
        if cursor_col < margins.left || cursor_col > margins.right {
            return;
        }

        self.storage.cursor.col = margins.left;
        self.storage.clear_pending_wrap();
        self.reset_display_offset_with_damage();

        let top = usize::from(cursor_row);
        let bottom = usize::from(region.bottom);
        let region_size = bottom - top + 1;
        let n = count.min(region_size);
        let left_usize = usize::from(margins.left);
        let right_usize = usize::from(margins.right);
        let width = right_usize + 1 - left_usize;
        let cols = self.storage.cols as usize;

        // Unpair what straddles a margin first, so the shift splits no glyph.
        self.unpair_margin_straddlers(top, bottom, left_usize, right_usize);
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

        #[allow(
            clippy::cast_possible_truncation,
            reason = "n <= region_size <= visible_rows, fits u16"
        )]
        self.storage.extras.shift_rect_up_by(
            cursor_row,
            region.bottom,
            margins.left,
            margins.right,
            n as u16,
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "n <= region_size <= visible_rows, fits u16"
        )]
        {
            let vacated_start = row_u16(top + region_size - n);
            self.fill_bce_rgb_rect(
                vacated_start..region.bottom.saturating_add(1),
                margins.left..margins.right.saturating_add(1),
            );
        }

        for row in cursor_row..=region.bottom {
            self.drop_unclaimed_extras(
                row,
                margins.left.saturating_sub(1)..margins.right.saturating_add(2),
            );
        }

        self.damage_selection_scroll_region();
        self.lose_anchors();
        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Insert `count` blank columns at the cursor column (DECIC, `CSI Pn ' }`).
    pub fn insert_columns(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_col = self.storage.cursor.col;
        let region = self.storage.scroll_region;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.cols;

        if cursor_col < margins.left || cursor_col > margins.right {
            return;
        }
        if self.storage.cursor.row < region.top || self.storage.cursor.row > region.bottom {
            return;
        }

        let right_bound = margins.right.saturating_add(1).min(effective_cols);
        let count = count.min(right_bound.saturating_sub(cursor_col));
        if count == 0 {
            return;
        }

        let fill = self.storage.cursor_template;

        for row_idx in region.top..=region.bottom {
            let mut blanked = crate::row::BlankedHalves::default();
            if let Some(row) = self.row_mut(row_idx) {
                blanked = row.insert_chars_bounded_fill(cursor_col, count, right_bound, fill);
                row.fixup_wide_boundary(
                    usize::from(margins.left),
                    usize::from(margins.right),
                    usize::from(effective_cols),
                    false,
                );
            }
            self.storage
                .extras
                .shift_cols_right(row_idx, cursor_col, count, right_bound);
            self.fill_bce_rgb_range(row_idx, cursor_col, cursor_col + count);
            self.fill_bce_rgb_cols(row_idx, blanked);
            self.drop_unclaimed_extras(
                row_idx,
                margins.left.saturating_sub(1)..right_bound.saturating_add(1),
            );
        }

        self.damage_selection_scroll_region();
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// Delete `count` columns at the cursor column (DECDC, `CSI Pn ' ~`).
    pub fn delete_columns(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_col = self.storage.cursor.col;
        let region = self.storage.scroll_region;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.cols;

        if cursor_col < margins.left || cursor_col > margins.right {
            return;
        }
        if self.storage.cursor.row < region.top || self.storage.cursor.row > region.bottom {
            return;
        }

        let right_bound = margins.right.saturating_add(1).min(effective_cols);
        let count = count.min(right_bound.saturating_sub(cursor_col));
        if count == 0 {
            return;
        }

        let fill = self.storage.cursor_template;

        for row_idx in region.top..=region.bottom {
            let mut blanked = crate::row::BlankedHalves::default();
            if let Some(row) = self.row_mut(row_idx) {
                blanked = row.delete_chars_bounded_fill(cursor_col, count, right_bound, fill);
                row.fixup_wide_boundary(
                    usize::from(margins.left),
                    usize::from(margins.right),
                    usize::from(effective_cols),
                    false,
                );
            }
            self.storage
                .extras
                .shift_cols_left(row_idx, cursor_col, count, right_bound);
            self.fill_bce_rgb_range(row_idx, right_bound - count, right_bound);
            self.fill_bce_rgb_cols(row_idx, blanked);
            self.drop_unclaimed_extras(
                row_idx,
                margins.left.saturating_sub(1)..right_bound.saturating_add(1),
            );
        }

        self.damage_selection_scroll_region();
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// SL — Scroll Left (CSI Ps SP @).
    pub fn scroll_left(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }
        let region = self.storage.scroll_region;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.cols;
        let left = margins.left;
        let right_bound = margins.right.saturating_add(1).min(effective_cols);
        let count = count.min(right_bound.saturating_sub(left));
        if count == 0 {
            return;
        }
        let fill = self.storage.cursor_template;
        for row_idx in region.top..=region.bottom {
            let mut blanked = crate::row::BlankedHalves::default();
            if let Some(row) = self.row_mut(row_idx) {
                blanked = row.delete_chars_bounded_fill(left, count, right_bound, fill);
                row.fixup_wide_boundary(
                    usize::from(margins.left),
                    usize::from(margins.right),
                    usize::from(effective_cols),
                    false,
                );
            }
            self.storage
                .extras
                .shift_cols_left(row_idx, left, count, right_bound);
            self.fill_bce_rgb_range(row_idx, right_bound - count, right_bound);
            self.fill_bce_rgb_cols(row_idx, blanked);
            self.drop_unclaimed_extras(
                row_idx,
                margins.left.saturating_sub(1)..right_bound.saturating_add(1),
            );
        }
        self.damage_selection_scroll_region();
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// SR — Scroll Right (CSI Ps SP A).
    pub fn scroll_right(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }
        let region = self.storage.scroll_region;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.cols;
        let left = margins.left;
        let right_bound = margins.right.saturating_add(1).min(effective_cols);
        let count = count.min(right_bound.saturating_sub(left));
        if count == 0 {
            return;
        }
        let fill = self.storage.cursor_template;
        for row_idx in region.top..=region.bottom {
            let mut blanked = crate::row::BlankedHalves::default();
            if let Some(row) = self.row_mut(row_idx) {
                blanked = row.insert_chars_bounded_fill(left, count, right_bound, fill);
                row.fixup_wide_boundary(
                    usize::from(margins.left),
                    usize::from(margins.right),
                    usize::from(effective_cols),
                    false,
                );
            }
            self.storage
                .extras
                .shift_cols_right(row_idx, left, count, right_bound);
            self.fill_bce_rgb_range(row_idx, left, left + count);
            self.fill_bce_rgb_cols(row_idx, blanked);
            self.drop_unclaimed_extras(
                row_idx,
                margins.left.saturating_sub(1)..right_bound.saturating_add(1),
            );
        }
        self.damage_selection_scroll_region();
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// DECBI — Back Index.
    pub fn back_index(&mut self, left: u16, right: u16) {
        self.storage.clear_pending_wrap();
        let region = self.storage.scroll_region;
        let effective_cols = self.storage.cols;
        let right_bound = right.saturating_add(1).min(effective_cols);

        if left >= right_bound {
            return;
        }

        let fill = self.storage.cursor_template;

        for row_idx in region.top..=region.bottom {
            let mut blanked = crate::row::BlankedHalves::default();
            if let Some(row) = self.row_mut(row_idx) {
                blanked = row.insert_chars_bounded_fill(left, 1, right_bound, fill);
                row.fixup_wide_boundary(
                    usize::from(left),
                    usize::from(right),
                    usize::from(effective_cols),
                    false,
                );
            }
            self.storage
                .extras
                .shift_cols_right(row_idx, left, 1, right_bound);
            self.fill_bce_rgb_range(row_idx, left, left + 1);
            self.fill_bce_rgb_cols(row_idx, blanked);
            self.drop_unclaimed_extras(
                row_idx,
                left.saturating_sub(1)..right_bound.saturating_add(1),
            );
        }

        self.damage_selection_scroll_region();
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// DECFI — Forward Index.
    pub fn forward_index(&mut self, left: u16, right: u16) {
        self.storage.clear_pending_wrap();
        let region = self.storage.scroll_region;
        let effective_cols = self.storage.cols;
        let right_bound = right.saturating_add(1).min(effective_cols);

        if left >= right_bound {
            return;
        }

        let fill = self.storage.cursor_template;

        for row_idx in region.top..=region.bottom {
            let mut blanked = crate::row::BlankedHalves::default();
            if let Some(row) = self.row_mut(row_idx) {
                blanked = row.delete_chars_bounded_fill(left, 1, right_bound, fill);
                row.fixup_wide_boundary(
                    usize::from(left),
                    usize::from(right),
                    usize::from(effective_cols),
                    false,
                );
            }
            self.storage
                .extras
                .shift_cols_left(row_idx, left, 1, right_bound);
            self.fill_bce_rgb_range(row_idx, right_bound - 1, right_bound);
            self.fill_bce_rgb_cols(row_idx, blanked);
            self.drop_unclaimed_extras(
                row_idx,
                left.saturating_sub(1)..right_bound.saturating_add(1),
            );
        }

        self.damage_selection_scroll_region();
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }
}

#[cfg(test)]
#[path = "line_ops_tests.rs"]
mod tests;
