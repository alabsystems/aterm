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
mod tests {
    use super::*;
    use crate::grid::test_support::wrapped_pair;

    /// Helper: read the character at (row, col), returning space for empty cells.
    fn char_at(grid: &Grid, row: u16, col: u16) -> char {
        grid.resolved_char(row, col).unwrap_or(' ')
    }

    /// Helper: write ASCII text at the current cursor position.
    fn write_text(grid: &mut Grid, text: &str) {
        for c in text.chars() {
            grid.write_char(c);
        }
    }

    /// Helper: read the trimmed text content of a row (trailing spaces stripped).
    fn row_trimmed(grid: &Grid, row: u16) -> String {
        grid.row_text(row)
            .unwrap_or_default()
            .trim_end()
            .to_string()
    }

    #[derive(Clone, Copy, Debug)]
    enum CharOp {
        Ich,
        Dch,
        Ech,
    }

    /// ICH shifts the rest of the cursor row right (dropping what passes the
    /// edge), DCH shifts it left (blank-filling the end), ECH blanks in place;
    /// each clamps its count to the row, leaves the other rows alone and
    /// consumes the pending wrap.
    #[test]
    fn char_ops_edit_only_the_cursor_row() {
        use CharOp::*;
        let cases: &[(CharOp, u16, &str, u16, u16, &str)] = &[
            (Ich, 10, "ABCDE", 2, 1, "AB CDE    "),
            (Ich, 10, "ABCDE", 1, 3, "A   BCDE  "),
            (Ich, 8, "HELLO", 0, 2, "  HELLO "),
            (Ich, 6, "ABCDEF", 4, 100, "ABCD  "),
            (Ich, 5, "ABCDE", 4, 1, "ABCD "),
            (Ich, 10, "", 0, 3, "          "),
            (Dch, 10, "ABCDE", 2, 1, "ABDE      "),
            (Dch, 10, "ABCDEFGH", 1, 3, "AEFGH     "),
            (Dch, 8, "ABCDEFGH", 0, 3, "DEFGH   "),
            (Dch, 6, "ABCDEF", 4, 100, "ABCD  "),
            (Dch, 5, "ABCDE", 4, 1, "ABCD "),
            (Dch, 10, "", 0, 3, "          "),
            (Ech, 10, "ABCDEFGH", 2, 3, "AB   FGH  "),
            (Ech, 10, "ABCDE", 2, 1, "AB DE     "),
            (Ech, 6, "ABCDEF", 3, 100, "ABC   "),
            (Ech, 8, "ABCDEFGH", 0, 4, "    EFGH"),
            (Ech, 10, "", 0, 5, "          "),
        ];
        for &(op, cols, text, col, count, want) in cases {
            let mut grid = Grid::new(3, cols);
            write_text(&mut grid, text);
            grid.move_cursor_to(1, 0);
            write_text(&mut grid, "ROW1");
            grid.move_cursor_to(0, col);
            grid.storage.mark_pending_wrap();
            match op {
                Ich => grid.insert_chars(count),
                Dch => grid.delete_chars(count),
                Ech => grid.erase_chars(count),
            }
            let got: String = (0..cols).map(|c| char_at(&grid, 0, c)).collect();
            let at = format!("{op:?} {count} at col {col} of {text:?}");
            assert_eq!(got, want, "{at}");
            assert_eq!(row_trimmed(&grid, 1), "ROW1", "{at}: another row changed");
            assert!(!grid.storage.pending_wrap, "{at}: the wrap survived");
        }
    }

    /// IL inserts blank rows at the cursor row and DL deletes rows there,
    /// shifting only the rest of the scroll region (rows past it are pushed
    /// out or blank-filled), clamping the count to the region, and homing the
    /// cursor column; a zero count or a cursor outside the region is a no-op.
    #[test]
    fn line_ops_shift_rows_within_the_region() {
        type Case<'a> = (
            bool,
            u16,
            Option<(u16, u16)>,
            (u16, u16),
            u16,
            &'a [&'a str],
        );
        let cases: &[Case] = &[
            (true, 5, None, (2, 0), 1, &["L0", "L1", "", "L2", "L3"]),
            (true, 6, None, (1, 0), 2, &["L0", "", "", "L1", "L2", "L3"]),
            (true, 4, None, (0, 0), 1, &["", "L0", "L1", "L2"]),
            (true, 4, None, (1, 5), 100, &["L0", "", "", ""]),
            (true, 5, None, (3, 0), 2, &["L0", "L1", "L2", "", ""]),
            (true, 1, None, (0, 0), 1, &[""]),
            (true, 4, None, (0, 3), 0, &["L0", "L1", "L2", "L3"]),
            (
                true,
                6,
                Some((1, 4)),
                (2, 0),
                1,
                &["L0", "L1", "", "L2", "L3", "L5"],
            ),
            (
                true,
                6,
                Some((1, 3)),
                (1, 0),
                1,
                &["L0", "", "L1", "L2", "L4", "L5"],
            ),
            (
                true,
                6,
                Some((2, 4)),
                (0, 3),
                1,
                &["L0", "L1", "L2", "L3", "L4", "L5"],
            ),
            (false, 5, None, (2, 0), 1, &["L0", "L1", "L3", "L4", ""]),
            (false, 6, None, (1, 0), 3, &["L0", "L4", "L5", "", "", ""]),
            (false, 4, None, (0, 0), 1, &["L1", "L2", "L3", ""]),
            (false, 4, None, (1, 5), 100, &["L0", "", "", ""]),
            (false, 5, None, (3, 0), 1, &["L0", "L1", "L2", "L4", ""]),
            (false, 1, None, (0, 0), 1, &[""]),
            (false, 4, None, (0, 3), 0, &["L0", "L1", "L2", "L3"]),
            (
                false,
                6,
                Some((1, 4)),
                (2, 0),
                1,
                &["L0", "L1", "L3", "L4", "", "L5"],
            ),
            (
                false,
                6,
                Some((1, 3)),
                (3, 0),
                1,
                &["L0", "L1", "L2", "", "L4", "L5"],
            ),
            (
                false,
                6,
                Some((2, 4)),
                (5, 3),
                1,
                &["L0", "L1", "L2", "L3", "L4", "L5"],
            ),
        ];
        for &(insert, rows, region, (row, col), count, want) in cases {
            let mut grid = Grid::new(rows, 10);
            for r in 0..rows {
                grid.move_cursor_to(r, 0);
                write_text(&mut grid, &format!("L{r}"));
            }
            if let Some((top, bottom)) = region {
                grid.set_scroll_region(top, bottom);
            }
            grid.move_cursor_to(row, col);
            if insert {
                grid.insert_lines(usize::from(count));
            } else {
                grid.delete_lines(usize::from(count));
            }
            let at = format!(
                "{} {count} at {:?} in {rows} rows, region {region:?}",
                if insert { "IL" } else { "DL" },
                (row, col)
            );
            let got: Vec<String> = (0..rows).map(|r| row_trimmed(&grid, r)).collect();
            assert_eq!(got, want, "{at}");
            let unchanged = want.iter().enumerate().all(|(r, w)| *w == format!("L{r}"));
            let want_col = if unchanged { col } else { 0 };
            assert_eq!(grid.cursor_col(), want_col, "{at}: cursor column");
        }
    }

    #[test]
    fn test_ech_through_the_last_column_breaks_the_wrap_as_el0_does() {
        for col in [0u16, 2, 4] {
            for count in [5 - col, 99] {
                let mut grid = wrapped_pair();
                grid.move_cursor_to(0, col);
                grid.erase_chars(count);
                assert!(
                    !grid.row(1).unwrap().is_wrapped(),
                    "ECH {count} at col {col} breaks the link"
                );
                assert_eq!(row_trimmed(&grid, 1), "FG", "row 1's cells survive");
            }
        }
    }

    #[test]
    fn test_ech_short_of_the_last_column_keeps_the_wrap() {
        for (col, count) in [(0u16, 3u16), (1, 1), (2, 2)] {
            let mut grid = wrapped_pair();
            grid.move_cursor_to(0, col);
            grid.erase_chars(count);
            assert!(
                grid.row(1).unwrap().is_wrapped(),
                "ECH {count} at col {col} keeps the link"
            );
            assert_eq!(char_at(&grid, 0, 4), 'E');
        }
    }

    #[test]
    fn test_ech_under_margins_runs_to_the_row_end_and_breaks_the_wrap_as_el0_does() {
        let mut grid = wrapped_pair();
        grid.set_horizontal_margins(0, 3);
        grid.move_cursor_to(0, 1);
        grid.erase_chars(99);
        assert_eq!(char_at(&grid, 0, 3), ' ', "erased through the margin");
        assert_eq!(char_at(&grid, 0, 4), ' ', "and past it, to the row end");
        assert!(!grid.row(1).unwrap().is_wrapped());
    }

    #[test]
    fn test_ech_under_margins_short_of_the_row_end_keeps_the_wrap() {
        let mut grid = wrapped_pair();
        grid.set_horizontal_margins(0, 3);
        grid.move_cursor_to(0, 1);
        grid.erase_chars(3);
        assert_eq!(char_at(&grid, 0, 3), ' ', "erased through the margin");
        assert_eq!(char_at(&grid, 0, 4), 'E', "the last column keeps its text");
        assert!(grid.row(1).unwrap().is_wrapped());
    }

    /// IL and DL with DECLRMM active are no-ops when the cursor column is
    /// outside the horizontal margins, even when the cursor row is inside the
    /// vertical scroll region.
    #[test]
    fn test_il_and_dl_margined_cursor_col_outside_margins_are_noops() {
        type Op = fn(&mut Grid);
        let ops: [(&str, Op); 2] = [
            ("IL", |g| g.insert_lines_margined(1, true)),
            ("DL", |g| g.delete_lines_margined(1, true)),
        ];
        for (name, op) in ops {
            let mut grid = Grid::new(24, 80);
            for r in 0..5u16 {
                grid.move_cursor_to(r, 0);
                write_text(&mut grid, &format!("ROW{r}"));
            }
            grid.set_horizontal_margins(10, 50);
            grid.move_cursor_to(2, 70);
            op(&mut grid);
            assert!(row_trimmed(&grid, 2).starts_with("ROW2"), "{name}");
            assert!(row_trimmed(&grid, 3).starts_with("ROW3"), "{name}");
            assert_eq!(
                grid.cursor_col(),
                70,
                "no-op {name} must not move cursor col"
            );
            assert_eq!(grid.cursor_row(), 2, "{name}");
        }
    }

    /// Inside both margins and region, margined IL still shifts (guard does not
    /// over-fire).
    #[test]
    fn test_il_margined_cursor_inside_margins_still_shifts() {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(0, 20);
        write_text(&mut grid, "AAA");
        grid.move_cursor_to(1, 20);
        write_text(&mut grid, "BBB");
        grid.set_horizontal_margins(10, 50);
        grid.move_cursor_to(0, 30);
        grid.insert_lines_margined(1, true);
        assert_eq!(grid.cursor_col(), 10);
        assert!(row_trimmed(&grid, 1).contains("AAA"));
    }

    /// Margined IL/DL and scroll_region_*_margined write cells via get_mut,
    /// bypassing len maintenance; when the margined copy blanks a row's tail,
    /// Row.len must still shrink, or row_text/search/scrollback emit phantom
    /// trailing spaces.
    #[test]
    fn test_dl_margined_shrinks_len_when_blanks_cover_tail() {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(0, 48);
        write_text(&mut grid, "END");
        assert_eq!(
            grid.row_len(0),
            Some(51),
            "precondition: row 0 content ends at col 50"
        );

        grid.set_horizontal_margins(10, 50);
        grid.move_cursor_to(0, 20);
        grid.delete_lines_margined(1, true);

        assert_eq!(
            grid.row_len(0),
            Some(0),
            "len must shrink to 0, not stay stale-high at 51 (phantom trailing spaces)"
        );
    }

    /// DECIC and DECDC shift the rows of the vertical scroll region (rows
    /// 0..=10) and are no-ops when the cursor row is outside it, even when the
    /// cursor column is within the horizontal margins; inside it DECIC still
    /// inserts (the guard does not over-fire).
    #[test]
    fn test_decic_and_decdc_act_only_inside_the_vertical_region() {
        type Op = fn(&mut Grid);
        let cases: [(&str, Op, u16, [&str; 2]); 3] = [
            (
                "DECIC outside",
                |g| g.insert_columns(3),
                15,
                ["ABCDE", "ABCDE"],
            ),
            (
                "DECDC outside",
                |g| g.delete_columns(3),
                15,
                ["ABCDE", "ABCDE"],
            ),
            (
                "DECIC inside",
                |g| g.insert_columns(2),
                5,
                ["AB  C", "ABCDE"],
            ),
        ];
        for (name, op, row, want) in cases {
            let mut grid = Grid::new(24, 80);
            for text_row in [5, 15] {
                grid.move_cursor_to(text_row, 0);
                write_text(&mut grid, "ABCDE");
            }
            grid.set_scroll_region(0, 10);
            grid.move_cursor_to(row, 2);
            op(&mut grid);
            let got = [5, 15].map(|r| (0..5).map(|col| char_at(&grid, r, col)).collect::<String>());
            assert_eq!(got, want.map(String::from), "{name}: rows 5 and 15");
        }
    }
}
