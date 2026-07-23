// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Character and line insertion, deletion, and erase operations.
//!
//! Character-level: ICH (Insert Character), DCH (Delete Character),
//! ECH (Erase Character) — operate within a single row at the cursor.
//!
//! Line-level: IL (Insert Line), DL (Delete Line) — operate within
//! the scroll region, shifting rows vertically.

use super::{Grid, row_u16};
use crate::row::LineSize;

impl Grid {
    // -------------------------------------------------------------------------
    // Character-level operations (within a single row)
    // -------------------------------------------------------------------------

    /// Insert `count` blank characters at cursor position.
    ///
    /// Shifts existing characters right, discarding those that go past the edge.
    /// When horizontal margins are active (DECLRMM), shifting stops at the
    /// right margin instead of the row edge.
    /// This implements the ICH (Insert Character) CSI sequence.
    #[inline]
    pub fn insert_chars(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        let margins = self.storage.horizontal_margins();
        // When horizontal margins are narrower than effective_cols, use margin
        // boundary (right + 1) as the shift region. Per VT420, ICH respects
        // horizontal margins when DECLRMM is active.
        let right_bound = (margins.right.saturating_add(1)).min(effective_cols);
        let fill = self.storage.cursor_template;
        let is_margined = !margins.is_full(self.storage.cols);
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            if is_margined {
                if let Some(row) = self.row_mut(cursor_row) {
                    row.insert_chars_bounded_fill(cursor_col, count, right_bound, fill);
                }
            } else if let Some(row) = self.row_mut(cursor_row) {
                row.insert_chars_fill(cursor_col, count, fill);
            }
            self.storage
                .extras
                .shift_cols_right(cursor_row, cursor_col, count, right_bound);
            // Fill BCE RGB in vacated cells at cursor position after shift (#7685).
            self.fill_bce_rgb_range(cursor_row, cursor_col, cursor_col + count);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Delete `count` characters at cursor position (uses BCE cursor template).
    ///
    /// Shifts remaining characters left, filling the end with blanks.
    /// When horizontal margins are active (DECLRMM), shifting stops at the
    /// right margin instead of the row edge.
    /// This implements the DCH (Delete Character) CSI sequence.
    #[inline]
    pub fn delete_chars(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        let fill = self.storage.cursor_template;
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        let margins = self.storage.horizontal_margins();
        let right_bound = (margins.right.saturating_add(1)).min(effective_cols);
        let is_margined = !margins.is_full(self.storage.cols);
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            if is_margined {
                if let Some(row) = self.row_mut(cursor_row) {
                    row.delete_chars_bounded_fill(cursor_col, count, right_bound, fill);
                }
            } else if let Some(row) = self.row_mut(cursor_row) {
                row.delete_chars_fill(cursor_col, count, fill);
            }
            self.storage
                .extras
                .shift_cols_left(cursor_row, cursor_col, count, right_bound);
            // Fill BCE RGB in vacated cells at end of line after shift (#7685).
            self.fill_bce_rgb_range(cursor_row, right_bound - count, right_bound);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Margin-aware insert characters (DECLRMM).
    ///
    /// When `left_right_margin_mode` is true, shifts only within the
    /// horizontal margins. Otherwise falls back to `insert_chars`.
    /// Uses the BCE cursor template for fill (#7522).
    #[inline]
    pub fn insert_chars_margin(&mut self, count: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.insert_chars(count);
            return;
        }
        self.storage.clear_pending_wrap();
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        // Only bound by margins when cursor is inside them. When cursor is
        // outside the margin region, fall back to screen-edge bounding (#7580).
        let in_margins = cursor_col >= margins.left && cursor_col <= margins.right;
        let right_bound = if in_margins {
            margins.right.saturating_add(1).min(effective_cols)
        } else {
            effective_cols
        };
        let fill = self.storage.cursor_template;
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            if let Some(row) = self.row_mut(cursor_row) {
                if in_margins {
                    row.insert_chars_bounded_fill(cursor_col, count, right_bound, fill);
                } else {
                    row.insert_chars_fill(cursor_col, count, fill);
                }
            }
            self.storage
                .extras
                .shift_cols_right(cursor_row, cursor_col, count, right_bound);
            self.fill_bce_rgb_range(cursor_row, cursor_col, cursor_col + count);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Margin-aware delete characters (DECLRMM).
    ///
    /// When `left_right_margin_mode` is true, shifts only within the
    /// horizontal margins. Otherwise falls back to `delete_chars`.
    /// Uses the BCE cursor template for fill (#7522).
    #[inline]
    pub fn delete_chars_margin(&mut self, count: u16, left_right_margin_mode: bool) {
        if !left_right_margin_mode {
            self.delete_chars(count);
            return;
        }
        self.storage.clear_pending_wrap();
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.effective_cols_for_row(cursor_row);
        // Only bound by margins when cursor is inside them (#7580).
        let in_margins = cursor_col >= margins.left && cursor_col <= margins.right;
        let right_bound = if in_margins {
            margins.right.saturating_add(1).min(effective_cols)
        } else {
            effective_cols
        };
        let fill = self.storage.cursor_template;
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            if let Some(row) = self.row_mut(cursor_row) {
                if in_margins {
                    row.delete_chars_bounded_fill(cursor_col, count, right_bound, fill);
                } else {
                    row.delete_chars_fill(cursor_col, count, fill);
                }
            }
            self.storage
                .extras
                .shift_cols_left(cursor_row, cursor_col, count, right_bound);
            self.fill_bce_rgb_range(cursor_row, right_bound - count, right_bound);
            self.storage.mark_content_row(cursor_row);
        }
    }

    /// Erase `count` characters at cursor position without shifting.
    ///
    /// Replaces characters with blanks in place. Does not shift remaining characters.
    /// Uses the BCE cursor template for fill per VT420/xterm spec (#7522).
    /// This implements the ECH (Erase Character) CSI sequence.
    #[inline]
    pub fn erase_chars(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let mut right_bound = self.storage.effective_cols_for_row(cursor_row);
        // Per VT420/VT510 spec, ECH is bounded by the DECLRMM right margin
        // when horizontal margins are active AND cursor is within the margins.
        // When cursor is outside the margin region, use screen edge (#7491, #7580).
        if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            if cursor_col >= margins.left && cursor_col <= margins.right {
                right_bound = right_bound.min(margins.right + 1);
            }
        }
        if cursor_col < right_bound {
            let count = count.min(right_bound - cursor_col);
            let fill = self.storage.cursor_template;
            if let Some(row) = self.row_mut(cursor_row) {
                row.erase_chars_with(cursor_col, count, fill);
            }
            self.storage
                .extras
                .clear_range(cursor_row, cursor_col, cursor_col + count);
            self.fill_bce_rgb_range(cursor_row, cursor_col, cursor_col + count);
            self.storage.mark_content_row(cursor_row);
        }
    }

    // -------------------------------------------------------------------------
    // Line-level operations (within the scroll region)
    // -------------------------------------------------------------------------

    /// Insert `count` blank lines at cursor row.
    ///
    /// Lines below are shifted down within the scroll region, with lines
    /// at the bottom margin discarded. Per VT510, IL has no effect if
    /// cursor is outside the scroll region.
    ///
    /// This implements the IL (Insert Line) CSI sequence.
    ///
    /// REQUIRES: self.storage.scroll_region.top <= self.storage.scroll_region.bottom
    /// REQUIRES: self.storage.scroll_region.bottom < self.storage.visible_rows
    pub fn insert_lines(&mut self, count: usize) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;

        // Per VT510: IL has no effect if cursor is outside scroll region.
        // Check BEFORE moving cursor column — a no-op IL must not change
        // the cursor position at all (#7543).
        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        // Per ECMA-48 §8.3.67: IL moves the active position to the line
        // home position (column 0, or left margin when DECLRMM is active).
        // When DECLRMM is off, horizontal_margins().left is always 0, so
        // this is correct regardless of DECLRMM state.
        self.storage.cursor.col = self.storage.horizontal_margins().left;

        // Row index arithmetic requires display_offset == 0. Reset
        // defensively for callers that may not have done so. (#5019)
        self.reset_display_offset_with_damage();

        let start_row = usize::from(cursor_row);
        let end_row = usize::from(region.bottom) + 1; // Bottom margin (inclusive) + 1

        // Shift rows down within the scroll region using pre-computed physical indices.
        // display_offset == 0 is guaranteed by reset_display_offset_with_damage above.
        let shift_n = count.min(end_row - start_row);
        if end_row > start_row + shift_n {
            self.storage
                .shift_visible_rows_down(start_row, end_row - 1, shift_n);
        }

        // Batch shift CellExtras within [cursor_row, region.bottom]: O(E) regardless of count.
        // Insert lines shifts content DOWN, so shift extras down.
        let shift_n = row_u16(count.min(end_row - start_row));
        self.storage
            .extras
            .shift_region_down_by(cursor_row, region.bottom, shift_n);

        // Clear the inserted rows with BCE fill (#7522).
        // Reset line size to SingleWidth so DECDWL flags don't leak from
        // recycled rows that previously had double-width attributes.
        let fill = self.storage.cursor_template;
        let clear_end = (start_row + count).min(end_row);
        for row in start_row..clear_end {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }
        // Fill BCE RGB in vacated rows after shift (#7685).
        self.fill_bce_rgb_rows(cursor_row..row_u16(clear_end));

        // Invalidate selection — IL shifts visible content within the scroll
        // region, so selection coordinates become stale (same as scroll_region_down).
        self.storage.content_scroll_delta = i32::MAX;

        // Mark only affected rows: cursor_row through region bottom.
        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Margin-aware insert lines (DECLRMM).
    ///
    /// When `left_right_margin_mode` is true and horizontal margins are not
    /// full-width, shifts only the cells within the margin region on each row
    /// (rectangular shift). Otherwise falls back to full-width `insert_lines`.
    ///
    /// Per VT420: IL with DECLRMM active performs a rectangular shift within
    /// the intersection of the scroll region (vertical) and horizontal margins.
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
        // Rectangular IL: shift cells within [left, right] down by `count` rows
        // within [cursor_row, scroll_region.bottom].
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;
        // Per VT510: IL has no effect if cursor is outside scroll region.
        // Check BEFORE moving cursor column (#7530).
        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        // With DECLRMM active the scrolling region is the 2D rectangle
        // [top..bottom] x [left..right]; IL is a no-op when the cursor column
        // is outside the horizontal margins, mirroring DECIC/DECDC and xterm.
        let cursor_col = self.storage.cursor.col;
        if cursor_col < margins.left || cursor_col > margins.right {
            return;
        }

        self.storage.cursor.col = margins.left;
        self.reset_display_offset_with_damage();

        let top = usize::from(cursor_row);
        let bottom = usize::from(region.bottom);
        let region_size = bottom - top + 1;
        let n = count.min(region_size);
        let left_usize = usize::from(margins.left);
        let right_usize = usize::from(margins.right);
        let width = right_usize + 1 - left_usize;
        let cols = self.storage.cols as usize;

        // Copy cells downward within [left, right] — bottom-to-top to avoid
        // overwriting source data (same pattern as scroll_region_down_margined).
        // Hoist buffer outside loop to avoid per-row heap allocation (#7468).
        let mut buf = vec![super::Cell::EMPTY; width];
        for dst_offset in (n..region_size).rev() {
            let dst_row = row_u16(top + dst_offset);
            let src_row = row_u16(top + dst_offset - n);
            buf.fill(super::Cell::EMPTY);
            if let Some(src) = self.row(src_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = src.get(row_u16(col)) {
                        buf[i] = *c;
                    }
                }
            }
            if let Some(dst) = self.row_mut(dst_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = dst.get_mut(row_u16(col)) {
                        *c = buf[i];
                    }
                }
                // Wide char fixup at rectangle boundaries (#7500).
                // Whole-row rect copy: no per-cell authoritative signal, keep the
                // char==' ' spacer heuristic (true).
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The get_mut writes above bypass len maintenance: copying blank
                // source cells over the dst row's tail content leaves len stale-high
                // (#7522 phantom trailing spaces via row_text/search/scrollback).
                // Recompute the true content extent per modified row (never
                // over-shrinks; margined IL/DL is off the hot path).
                dst.recompute_len();
            }
        }

        // Clear the top n rows within margins (the inserted blank lines) with BCE fill (#7522).
        let fill = self.storage.cursor_template;
        for clear_offset in 0..n {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                // Wide char fixup at rectangle boundaries (#7500).
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The BCE fill via get_mut bypasses len maintenance: an empty
                // cursor_template can orphan the tail (stale-high), a colored one
                // extends it (stale-low). Recompute the true extent (#7522).
                r.recompute_len();
            }
        }

        // Shift extras down within the margin rectangle to match the cell
        // shift, then clear the vacated top rows. Using shift_rect_down_by
        // preserves hyperlinks, RGB colors, combining marks, and underline
        // colors that would be destroyed by clear_rect (#7455).
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
        // Fill BCE RGB in vacated top rect after shift (#7685).
        #[allow(
            clippy::cast_possible_truncation,
            reason = "n <= region_size <= visible_rows, fits u16"
        )]
        self.fill_bce_rgb_rect(
            cursor_row..cursor_row.saturating_add(n as u16),
            margins.left..margins.right.saturating_add(1),
        );

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Delete `count` lines at cursor row.
    ///
    /// Lines below are shifted up within the scroll region, with blank lines
    /// inserted at the bottom margin. Per VT510, DL has no effect if cursor
    /// is outside the scroll region.
    ///
    /// This implements the DL (Delete Line) CSI sequence.
    ///
    /// REQUIRES: self.storage.scroll_region.top <= self.storage.scroll_region.bottom
    /// REQUIRES: self.storage.scroll_region.bottom < self.storage.visible_rows
    pub fn delete_lines(&mut self, count: usize) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;

        // Per VT510: DL has no effect if cursor is outside scroll region.
        // Check BEFORE moving cursor column — a no-op DL must not change
        // the cursor position at all (#7543).
        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        // Per ECMA-48 §8.3.32: DL moves the active position to the line
        // home position (column 0, or left margin when DECLRMM is active).
        // When DECLRMM is off, horizontal_margins().left is always 0, so
        // this is correct regardless of DECLRMM state.
        self.storage.cursor.col = self.storage.horizontal_margins().left;

        // Row index arithmetic requires display_offset == 0. Reset
        // defensively for callers that may not have done so. (#5019)
        self.reset_display_offset_with_damage();

        let start_row = usize::from(cursor_row);
        let end_row = usize::from(region.bottom) + 1; // Bottom margin (inclusive) + 1

        // Shift rows up within the scroll region using pre-computed physical indices.
        // display_offset == 0 is guaranteed by reset_display_offset_with_damage above.
        let shift_n = count.min(end_row - start_row);
        if end_row > start_row + shift_n {
            self.storage
                .shift_visible_rows_up(start_row, end_row - 1, shift_n);
        }

        // Batch shift CellExtras within [cursor_row, region.bottom]: O(E) regardless of count.
        // Delete lines shifts content UP, so shift extras up.
        let shift_n = row_u16(count.min(end_row - start_row));
        self.storage
            .extras
            .shift_region_up_by(cursor_row, region.bottom, shift_n);

        // Clear the bottom rows of the scroll region with BCE fill (#7522).
        // Reset line size to SingleWidth so DECDWL flags don't leak from
        // recycled rows that previously had double-width attributes.
        let fill = self.storage.cursor_template;
        let clear_start = end_row.saturating_sub(count).max(start_row);
        for row in clear_start..end_row {
            if let Some(r) = self.row_mut(row_u16(row)) {
                r.set_line_size(LineSize::SingleWidth);
                r.erase_with(fill);
            }
        }
        // Fill BCE RGB in vacated bottom rows after shift (#7685).
        self.fill_bce_rgb_rows(row_u16(clear_start)..row_u16(end_row));

        // Invalidate selection — DL shifts visible content within the scroll
        // region, so selection coordinates become stale (same as scroll_region_up).
        self.storage.content_scroll_delta = i32::MAX;

        // Mark only affected rows: cursor_row through region bottom.
        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    /// Margin-aware delete lines (DECLRMM).
    ///
    /// When `left_right_margin_mode` is true and horizontal margins are not
    /// full-width, shifts only the cells within the margin region on each row
    /// (rectangular shift). Otherwise falls back to full-width `delete_lines`.
    ///
    /// Per VT420: DL with DECLRMM active performs a rectangular shift within
    /// the intersection of the scroll region (vertical) and horizontal margins.
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
        // Rectangular DL: shift cells within [left, right] up by `count` rows
        // within [cursor_row, scroll_region.bottom].
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_row = self.storage.cursor.row;
        let region = self.storage.scroll_region;
        // Per VT510: DL has no effect if cursor is outside scroll region.
        // Check BEFORE moving cursor column (#7530).
        if cursor_row < region.top || cursor_row > region.bottom {
            return;
        }

        // With DECLRMM active the scrolling region is the 2D rectangle
        // [top..bottom] x [left..right]; DL is a no-op when the cursor column
        // is outside the horizontal margins, mirroring DECIC/DECDC and xterm.
        let cursor_col = self.storage.cursor.col;
        if cursor_col < margins.left || cursor_col > margins.right {
            return;
        }

        self.storage.cursor.col = margins.left;
        self.reset_display_offset_with_damage();

        let top = usize::from(cursor_row);
        let bottom = usize::from(region.bottom);
        let region_size = bottom - top + 1;
        let n = count.min(region_size);
        let left_usize = usize::from(margins.left);
        let right_usize = usize::from(margins.right);
        let width = right_usize + 1 - left_usize;
        let cols = self.storage.cols as usize;

        // Copy cells upward within [left, right] — top-to-bottom to avoid
        // overwriting source data (same pattern as scroll_region_up_margined).
        // Hoist buffer outside loop to avoid per-row heap allocation (#7468).
        let mut buf = vec![super::Cell::EMPTY; width];
        for dst_offset in 0..(region_size - n) {
            let dst_row = row_u16(top + dst_offset);
            let src_row = row_u16(top + dst_offset + n);
            buf.fill(super::Cell::EMPTY);
            if let Some(src) = self.row(src_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = src.get(row_u16(col)) {
                        buf[i] = *c;
                    }
                }
            }
            if let Some(dst) = self.row_mut(dst_row) {
                for (i, col) in (left_usize..=right_usize).enumerate() {
                    if let Some(c) = dst.get_mut(row_u16(col)) {
                        *c = buf[i];
                    }
                }
                // Wide char fixup at rectangle boundaries (#7500).
                // Whole-row rect copy: no per-cell authoritative signal, keep the
                // char==' ' spacer heuristic (true).
                dst.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The get_mut writes above bypass len maintenance: copying blank
                // source cells over the dst row's tail content leaves len stale-high
                // (#7522 phantom trailing spaces via row_text/search/scrollback).
                // Recompute the true content extent per modified row (never
                // over-shrinks; margined IL/DL is off the hot path).
                dst.recompute_len();
            }
        }

        // Clear the bottom n rows within margins (vacated by the upward shift)
        // with BCE fill (#7522).
        let fill = self.storage.cursor_template;
        for clear_offset in (region_size - n)..region_size {
            let clear_row = row_u16(top + clear_offset);
            if let Some(r) = self.row_mut(clear_row) {
                for col in left_usize..=right_usize {
                    if let Some(c) = r.get_mut(row_u16(col)) {
                        *c = fill;
                    }
                }
                // Wide char fixup at rectangle boundaries (#7500).
                r.fixup_wide_boundary(left_usize, right_usize, cols, true);
                // The BCE fill via get_mut bypasses len maintenance: an empty
                // cursor_template can orphan the tail (stale-high), a colored one
                // extends it (stale-low). Recompute the true extent (#7522).
                r.recompute_len();
            }
        }

        // Shift extras up within the margin rectangle to match the cell
        // shift. Using shift_rect_up_by preserves hyperlinks, RGB colors,
        // combining marks, and underline colors that would be destroyed
        // by clear_rect (#7455).
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
        // Fill BCE RGB in vacated bottom rect after shift (#7685).
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

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(cursor_row, region.bottom.saturating_add(1));
    }

    // -------------------------------------------------------------------------
    // Column-level operations (DECIC / DECDC — VT420+)
    // -------------------------------------------------------------------------

    /// Insert `count` blank columns at cursor column position.
    ///
    /// For each row in the scroll region, shifts cells right within
    /// [cursor_col, right_margin], discarding content past the right margin.
    /// Blank columns are inserted at the cursor column with BCE fill.
    ///
    /// This implements the DECIC (Insert Column) CSI sequence: CSI Pn ' }
    ///
    /// Per VT420: DECIC operates within the scroll region (vertical) and
    /// horizontal margins. The cursor position is not changed.
    pub fn insert_columns(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_col = self.storage.cursor.col;
        let region = self.storage.scroll_region;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.cols;

        // Per VT420 / DEC STD 070: DECIC is ignored when the cursor is outside
        // the scrolling margins — both the horizontal margins (column) and the
        // vertical scroll region (row). Mirrors DECBI/DECFI and IL/DL.
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

        // For each row in the scroll region, insert columns at cursor_col.
        for row_idx in region.top..=region.bottom {
            if let Some(row) = self.row_mut(row_idx) {
                row.insert_chars_bounded_fill(cursor_col, count, right_bound, fill);
                // Wide char fixup at rectangle boundaries. `insert_chars_bounded_fill`
                // already fixed genuine boundary orphans authoritatively, so suppress
                // the ambiguous spacer-clear (it would only blank a protected space
                // outside the region — codex round-7 finding).
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
        }

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// Delete `count` columns at cursor column position.
    ///
    /// For each row in the scroll region, shifts cells left within
    /// [cursor_col, right_margin], inserting blank columns at the right margin.
    ///
    /// This implements the DECDC (Delete Column) CSI sequence: CSI Pn ' ~
    ///
    /// Per VT420: DECDC operates within the scroll region (vertical) and
    /// horizontal margins. The cursor position is not changed.
    pub fn delete_columns(&mut self, count: u16) {
        self.storage.clear_pending_wrap();
        if count == 0 {
            return;
        }

        let cursor_col = self.storage.cursor.col;
        let region = self.storage.scroll_region;
        let margins = self.storage.horizontal_margins();
        let effective_cols = self.storage.cols;

        // Per VT420 / DEC STD 070: DECDC is ignored when the cursor is outside
        // the scrolling margins — both the horizontal margins (column) and the
        // vertical scroll region (row). Mirrors DECBI/DECFI and IL/DL.
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

        // For each row in the scroll region, delete columns at cursor_col.
        for row_idx in region.top..=region.bottom {
            if let Some(row) = self.row_mut(row_idx) {
                row.delete_chars_bounded_fill(cursor_col, count, right_bound, fill);
                // Wide char fixup at rectangle boundaries. `delete_chars_bounded_fill`
                // already fixed genuine boundary orphans authoritatively; suppress the
                // ambiguous spacer-clear (protected-space corruption — codex round-7).
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
        }

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// SL — Scroll Left (CSI Ps SP @): scroll the content of the scroll region
    /// left by `count` columns within the horizontal margins; blank columns
    /// appear at the right margin. Anchored at the LEFT MARGIN (independent of
    /// the cursor) — the only difference from `delete_columns`. Uses the current
    /// `cursor_template` as the BCE fill (set by the caller).
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
            if let Some(row) = self.row_mut(row_idx) {
                row.delete_chars_bounded_fill(left, count, right_bound, fill);
                // Genuine orphans already fixed by the bounded shift; suppress the
                // ambiguous spacer-clear (codex round-7: SL blanked a protected space
                // one column past the right margin).
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
        }
        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// SR — Scroll Right (CSI Ps SP A): scroll the content of the scroll region
    /// right by `count` columns within the horizontal margins; blank columns
    /// appear at the left margin. Anchored at the LEFT MARGIN — the only
    /// difference from `insert_columns`.
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
            if let Some(row) = self.row_mut(row_idx) {
                row.insert_chars_bounded_fill(left, count, right_bound, fill);
                // Genuine orphans already fixed by the bounded shift; suppress the
                // ambiguous spacer-clear (protected-space corruption — codex round-7).
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
        }
        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    // -------------------------------------------------------------------------
    // Single-column scroll operations (DECBI / DECFI — VT420+)
    // -------------------------------------------------------------------------

    /// DECBI — Back Index: scroll content right within margins.
    ///
    /// For each row in the scroll region, inserts 1 blank column at the left
    /// margin, shifting content right within [left, right]. Content past the
    /// right margin is discarded. The cursor position is not changed.
    ///
    /// Called when the cursor is at the left margin and DECBI (ESC 6) is
    /// received. The caller handles the non-margin case (simple cursor left).
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
            if let Some(row) = self.row_mut(row_idx) {
                row.insert_chars_bounded_fill(left, 1, right_bound, fill);
                // Genuine orphans already fixed by the bounded shift; suppress the
                // ambiguous spacer-clear (protected-space corruption — codex round-7).
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
        }

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }

    /// DECFI — Forward Index: scroll content left within margins.
    ///
    /// For each row in the scroll region, deletes 1 column at the left margin,
    /// shifting content left within [left, right]. A blank column is inserted
    /// at the right margin. The cursor position is not changed.
    ///
    /// Called when the cursor is at the right margin and DECFI (ESC 9) is
    /// received. The caller handles the non-margin case (simple cursor right).
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
            if let Some(row) = self.row_mut(row_idx) {
                row.delete_chars_bounded_fill(left, 1, right_bound, fill);
                // Genuine orphans already fixed by the bounded shift; suppress the
                // ambiguous spacer-clear (protected-space corruption — codex round-7).
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
        }

        self.storage.content_scroll_delta = i32::MAX;
        self.storage
            .mark_content_rows(region.top, region.bottom.saturating_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellFlags;
    use crate::cell_colors::PackedColor;

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

    // =========================================================================
    // ICH — insert_chars: insert N blank chars at cursor, shifting right
    // =========================================================================

    #[test]
    fn test_ich_basic_insert_one() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 2); // cursor at 'C'
        grid.insert_chars(1);
        // Expected: "AB CDE" (shifted right by 1, last char may fall off if row is full)
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), ' '); // inserted blank
        assert_eq!(char_at(&grid, 0, 3), 'C');
        assert_eq!(char_at(&grid, 0, 4), 'D');
        assert_eq!(char_at(&grid, 0, 5), 'E');
    }

    #[test]
    fn test_ich_insert_multiple() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 1); // cursor at 'B'
        grid.insert_chars(3);
        // Expected: "A   BCD" (E pushed past col 7 or truncated)
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), ' ');
        assert_eq!(char_at(&grid, 0, 2), ' ');
        assert_eq!(char_at(&grid, 0, 3), ' ');
        assert_eq!(char_at(&grid, 0, 4), 'B');
        assert_eq!(char_at(&grid, 0, 5), 'C');
        assert_eq!(char_at(&grid, 0, 6), 'D');
    }

    #[test]
    fn test_ich_at_beginning_of_line() {
        let mut grid = Grid::new(4, 8);
        write_text(&mut grid, "HELLO");
        grid.move_cursor_to(0, 0);
        grid.insert_chars(2);
        assert_eq!(char_at(&grid, 0, 0), ' ');
        assert_eq!(char_at(&grid, 0, 1), ' ');
        assert_eq!(char_at(&grid, 0, 2), 'H');
        assert_eq!(char_at(&grid, 0, 3), 'E');
        assert_eq!(char_at(&grid, 0, 4), 'L');
        assert_eq!(char_at(&grid, 0, 5), 'L');
        assert_eq!(char_at(&grid, 0, 6), 'O');
    }

    #[test]
    fn test_ich_count_exceeds_remaining() {
        // When count exceeds remaining space, it's clamped.
        let mut grid = Grid::new(4, 6);
        write_text(&mut grid, "ABCDEF");
        grid.move_cursor_to(0, 4); // cursor at 'E'
        grid.insert_chars(100); // way more than remaining 2 cols
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'C');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), ' '); // blanked
        assert_eq!(char_at(&grid, 0, 5), ' '); // blanked
    }

    #[test]
    fn test_ich_at_last_column() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 4); // last column
        grid.insert_chars(1);
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'C');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), ' '); // 'E' pushed off, blank inserted
    }

    #[test]
    fn test_ich_does_not_affect_other_rows() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ROW0ROW0");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "ROW1ROW1");
        grid.move_cursor_to(0, 2);
        grid.insert_chars(2);
        // Row 1 should be unchanged
        assert!(row_trimmed(&grid, 1).starts_with("ROW1ROW1"));
    }

    // =========================================================================
    // DCH — delete_chars: delete N chars at cursor, shifting left
    // =========================================================================

    #[test]
    fn test_dch_basic_delete_one() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 2); // cursor at 'C'
        grid.delete_chars(1);
        // Expected: "ABDE " (C removed, D and E shift left)
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'D');
        assert_eq!(char_at(&grid, 0, 3), 'E');
        assert_eq!(char_at(&grid, 0, 4), ' '); // blank fill at end
    }

    #[test]
    fn test_dch_delete_multiple() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDEFGH");
        grid.move_cursor_to(0, 1); // cursor at 'B'
        grid.delete_chars(3);
        // Expected: "AEFGH   " (B, C, D removed)
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'E');
        assert_eq!(char_at(&grid, 0, 2), 'F');
        assert_eq!(char_at(&grid, 0, 3), 'G');
        assert_eq!(char_at(&grid, 0, 4), 'H');
    }

    #[test]
    fn test_dch_at_beginning_of_line() {
        let mut grid = Grid::new(4, 8);
        write_text(&mut grid, "ABCDEFGH");
        grid.move_cursor_to(0, 0);
        grid.delete_chars(3);
        // Expected: "DEFGH   "
        assert_eq!(char_at(&grid, 0, 0), 'D');
        assert_eq!(char_at(&grid, 0, 1), 'E');
        assert_eq!(char_at(&grid, 0, 2), 'F');
        assert_eq!(char_at(&grid, 0, 3), 'G');
        assert_eq!(char_at(&grid, 0, 4), 'H');
        assert_eq!(char_at(&grid, 0, 5), ' ');
    }

    #[test]
    fn test_dch_count_exceeds_remaining() {
        let mut grid = Grid::new(4, 6);
        write_text(&mut grid, "ABCDEF");
        grid.move_cursor_to(0, 4); // cursor at 'E'
        grid.delete_chars(100);
        // Clamped: delete 2 chars from col 4 to end
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'C');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), ' ');
        assert_eq!(char_at(&grid, 0, 5), ' ');
    }

    #[test]
    fn test_dch_at_last_column() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 4); // last column
        grid.delete_chars(1);
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'C');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), ' '); // 'E' deleted, replaced with blank
    }

    #[test]
    fn test_dch_does_not_affect_other_rows() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ROW0DATA");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "ROW1DATA");
        grid.move_cursor_to(0, 2);
        grid.delete_chars(2);
        // Row 1 unchanged
        assert!(row_trimmed(&grid, 1).starts_with("ROW1DATA"));
    }

    // =========================================================================
    // ECH — erase_chars: blank N chars at cursor without shifting
    // =========================================================================

    #[test]
    fn test_ech_basic_erase() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDEFGH");
        grid.move_cursor_to(0, 2);
        grid.erase_chars(3);
        // Expected: "AB   FGH" (C, D, E erased in place)
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), ' ');
        assert_eq!(char_at(&grid, 0, 3), ' ');
        assert_eq!(char_at(&grid, 0, 4), ' ');
        assert_eq!(char_at(&grid, 0, 5), 'F');
        assert_eq!(char_at(&grid, 0, 6), 'G');
        assert_eq!(char_at(&grid, 0, 7), 'H');
    }

    #[test]
    fn test_ech_erase_one() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 2);
        grid.erase_chars(1);
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), ' ');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), 'E');
    }

    #[test]
    fn test_ech_count_exceeds_remaining() {
        let mut grid = Grid::new(4, 6);
        write_text(&mut grid, "ABCDEF");
        grid.move_cursor_to(0, 3);
        grid.erase_chars(100); // clamped to 3
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'C');
        assert_eq!(char_at(&grid, 0, 3), ' ');
        assert_eq!(char_at(&grid, 0, 4), ' ');
        assert_eq!(char_at(&grid, 0, 5), ' ');
    }

    #[test]
    fn test_ech_at_beginning() {
        let mut grid = Grid::new(4, 8);
        write_text(&mut grid, "ABCDEFGH");
        grid.move_cursor_to(0, 0);
        grid.erase_chars(4);
        assert_eq!(char_at(&grid, 0, 0), ' ');
        assert_eq!(char_at(&grid, 0, 1), ' ');
        assert_eq!(char_at(&grid, 0, 2), ' ');
        assert_eq!(char_at(&grid, 0, 3), ' ');
        assert_eq!(char_at(&grid, 0, 4), 'E');
        assert_eq!(char_at(&grid, 0, 5), 'F');
    }

    #[test]
    fn test_ech_does_not_shift_content() {
        // Key difference from DCH: content after erased region stays in place
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 1);
        grid.erase_chars(2);
        // "A  DE" — B and C erased, D and E stay at their columns
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), ' ');
        assert_eq!(char_at(&grid, 0, 2), ' ');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), 'E');
    }

    #[test]
    fn test_ech_does_not_affect_other_rows() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ROW0DATA");
        grid.move_cursor_to(1, 0);
        write_text(&mut grid, "ROW1DATA");
        grid.move_cursor_to(0, 0);
        grid.erase_chars(4);
        assert!(row_trimmed(&grid, 1).starts_with("ROW1DATA"));
    }

    // =========================================================================
    // IL — insert_lines: insert N blank lines at cursor row
    // =========================================================================

    #[test]
    fn test_il_basic_insert_one() {
        let mut grid = Grid::new(5, 10);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("LINE{r}"));
        }
        grid.move_cursor_to(2, 0);
        grid.insert_lines(1);
        // Row 2 should be blank, LINE2 shifted to row 3, LINE4 lost
        assert!(row_trimmed(&grid, 0).starts_with("LINE0"));
        assert!(row_trimmed(&grid, 1).starts_with("LINE1"));
        assert_eq!(row_trimmed(&grid, 2), ""); // inserted blank
        assert!(row_trimmed(&grid, 3).starts_with("LINE2"));
        assert!(row_trimmed(&grid, 4).starts_with("LINE3"));
    }

    #[test]
    fn test_il_insert_multiple() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.move_cursor_to(1, 0);
        grid.insert_lines(2);
        // L0 stays, rows 1-2 blank, L1 at row 3, L2 at row 4, L3 at row 5
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert_eq!(row_trimmed(&grid, 1), "");
        assert_eq!(row_trimmed(&grid, 2), "");
        assert!(row_trimmed(&grid, 3).starts_with("L1"));
        assert!(row_trimmed(&grid, 4).starts_with("L2"));
        assert!(row_trimmed(&grid, 5).starts_with("L3"));
    }

    #[test]
    fn test_il_at_top() {
        let mut grid = Grid::new(4, 10);
        for r in 0..4u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("R{r}"));
        }
        grid.move_cursor_to(0, 0);
        grid.insert_lines(1);
        assert_eq!(row_trimmed(&grid, 0), "");
        assert!(row_trimmed(&grid, 1).starts_with("R0"));
        assert!(row_trimmed(&grid, 2).starts_with("R1"));
        assert!(row_trimmed(&grid, 3).starts_with("R2"));
        // R3 pushed off bottom
    }

    #[test]
    fn test_il_count_exceeds_region() {
        let mut grid = Grid::new(4, 10);
        for r in 0..4u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("R{r}"));
        }
        grid.move_cursor_to(1, 0);
        grid.insert_lines(100); // way more than 3 remaining rows
        // All rows from cursor down should be blank
        assert!(row_trimmed(&grid, 0).starts_with("R0"));
        assert_eq!(row_trimmed(&grid, 1), "");
        assert_eq!(row_trimmed(&grid, 2), "");
        assert_eq!(row_trimmed(&grid, 3), "");
    }

    #[test]
    fn test_il_cursor_moves_to_col0() {
        let mut grid = Grid::new(4, 10);
        grid.move_cursor_to(1, 5);
        grid.insert_lines(1);
        assert_eq!(
            grid.storage.cursor.col, 0,
            "IL should move cursor to column 0"
        );
    }

    #[test]
    fn test_il_zero_count_is_noop() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "HELLO");
        grid.move_cursor_to(0, 3);
        let col_before = grid.storage.cursor.col;
        grid.insert_lines(0);
        assert_eq!(
            grid.storage.cursor.col, col_before,
            "IL 0 should be a no-op"
        );
        assert!(row_trimmed(&grid, 0).starts_with("HELLO"));
    }

    // =========================================================================
    // DL — delete_lines: delete N lines at cursor row
    // =========================================================================

    #[test]
    fn test_dl_basic_delete_one() {
        let mut grid = Grid::new(5, 10);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("LINE{r}"));
        }
        grid.move_cursor_to(2, 0);
        grid.delete_lines(1);
        // LINE2 deleted, LINE3 and LINE4 shift up, blank at bottom
        assert!(row_trimmed(&grid, 0).starts_with("LINE0"));
        assert!(row_trimmed(&grid, 1).starts_with("LINE1"));
        assert!(row_trimmed(&grid, 2).starts_with("LINE3"));
        assert!(row_trimmed(&grid, 3).starts_with("LINE4"));
        assert_eq!(row_trimmed(&grid, 4), ""); // blank at bottom
    }

    #[test]
    fn test_dl_delete_multiple() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.move_cursor_to(1, 0);
        grid.delete_lines(3);
        // L1, L2, L3 deleted; L4 moves to row 1, L5 to row 2, rows 3-5 blank
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert!(row_trimmed(&grid, 1).starts_with("L4"));
        assert!(row_trimmed(&grid, 2).starts_with("L5"));
        assert_eq!(row_trimmed(&grid, 3), "");
        assert_eq!(row_trimmed(&grid, 4), "");
        assert_eq!(row_trimmed(&grid, 5), "");
    }

    #[test]
    fn test_dl_at_top() {
        let mut grid = Grid::new(4, 10);
        for r in 0..4u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("R{r}"));
        }
        grid.move_cursor_to(0, 0);
        grid.delete_lines(1);
        assert!(row_trimmed(&grid, 0).starts_with("R1"));
        assert!(row_trimmed(&grid, 1).starts_with("R2"));
        assert!(row_trimmed(&grid, 2).starts_with("R3"));
        assert_eq!(row_trimmed(&grid, 3), "");
    }

    #[test]
    fn test_dl_count_exceeds_region() {
        let mut grid = Grid::new(4, 10);
        for r in 0..4u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("R{r}"));
        }
        grid.move_cursor_to(1, 0);
        grid.delete_lines(100);
        // All rows from cursor down blanked
        assert!(row_trimmed(&grid, 0).starts_with("R0"));
        assert_eq!(row_trimmed(&grid, 1), "");
        assert_eq!(row_trimmed(&grid, 2), "");
        assert_eq!(row_trimmed(&grid, 3), "");
    }

    #[test]
    fn test_dl_cursor_moves_to_col0() {
        let mut grid = Grid::new(4, 10);
        grid.move_cursor_to(1, 5);
        grid.delete_lines(1);
        assert_eq!(
            grid.storage.cursor.col, 0,
            "DL should move cursor to column 0"
        );
    }

    #[test]
    fn test_dl_zero_count_is_noop() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "HELLO");
        grid.move_cursor_to(0, 3);
        let col_before = grid.storage.cursor.col;
        grid.delete_lines(0);
        assert_eq!(
            grid.storage.cursor.col, col_before,
            "DL 0 should be a no-op"
        );
        assert!(row_trimmed(&grid, 0).starts_with("HELLO"));
    }

    // =========================================================================
    // Scroll region interaction: IL/DL within DECSTBM margins
    // =========================================================================

    #[test]
    fn test_il_within_scroll_region() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        // Set scroll region to rows 1-4 (0-indexed)
        grid.set_scroll_region(1, 4);
        grid.move_cursor_to(2, 0);
        grid.insert_lines(1);
        // L0 unchanged (outside region top), L5 unchanged (outside region bottom)
        // Within region [1,4]: L2 inserted blank at row 2, L2->3, L3->4, L4 lost
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert!(row_trimmed(&grid, 1).starts_with("L1"));
        assert_eq!(row_trimmed(&grid, 2), ""); // inserted blank
        assert!(row_trimmed(&grid, 3).starts_with("L2"));
        assert!(row_trimmed(&grid, 4).starts_with("L3"));
        assert!(row_trimmed(&grid, 5).starts_with("L5")); // outside region, preserved
    }

    #[test]
    fn test_dl_within_scroll_region() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.set_scroll_region(1, 4);
        grid.move_cursor_to(2, 0);
        grid.delete_lines(1);
        // Within [1,4]: L2 deleted, L3->2, L4->3, blank at 4
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert!(row_trimmed(&grid, 1).starts_with("L1"));
        assert!(row_trimmed(&grid, 2).starts_with("L3"));
        assert!(row_trimmed(&grid, 3).starts_with("L4"));
        assert_eq!(row_trimmed(&grid, 4), ""); // blank at bottom of region
        assert!(row_trimmed(&grid, 5).starts_with("L5")); // outside, preserved
    }

    #[test]
    fn test_il_outside_scroll_region_is_noop() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.set_scroll_region(2, 4);
        grid.move_cursor_to(0, 3); // row 0 is above scroll region
        grid.insert_lines(1);
        // Nothing should change; cursor col should be preserved
        for r in 0..6u16 {
            assert!(
                row_trimmed(&grid, r).starts_with(&format!("L{r}")),
                "row {r} should be unchanged"
            );
        }
    }

    #[test]
    fn test_dl_outside_scroll_region_is_noop() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.set_scroll_region(2, 4);
        grid.move_cursor_to(5, 3); // row 5 is below scroll region
        grid.delete_lines(1);
        for r in 0..6u16 {
            assert!(
                row_trimmed(&grid, r).starts_with(&format!("L{r}")),
                "row {r} should be unchanged"
            );
        }
    }

    #[test]
    fn test_il_at_top_of_scroll_region() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.set_scroll_region(1, 3);
        grid.move_cursor_to(1, 0);
        grid.insert_lines(1);
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert_eq!(row_trimmed(&grid, 1), ""); // inserted blank
        assert!(row_trimmed(&grid, 2).starts_with("L1"));
        assert!(row_trimmed(&grid, 3).starts_with("L2")); // L3 pushed off region bottom
        assert!(row_trimmed(&grid, 4).starts_with("L4")); // outside, preserved
    }

    #[test]
    fn test_dl_at_bottom_of_scroll_region() {
        let mut grid = Grid::new(6, 10);
        for r in 0..6u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.set_scroll_region(1, 3);
        grid.move_cursor_to(3, 0); // bottom of region
        grid.delete_lines(1);
        // L3 deleted from bottom row of region; it's blanked immediately
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert!(row_trimmed(&grid, 1).starts_with("L1"));
        assert!(row_trimmed(&grid, 2).starts_with("L2"));
        assert_eq!(row_trimmed(&grid, 3), ""); // was L3, now blank
        assert!(row_trimmed(&grid, 4).starts_with("L4"));
    }

    // =========================================================================
    // Wide character handling
    // =========================================================================

    #[test]
    fn test_ich_with_wide_char() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "A");
        grid.write_wide_char_styled(
            '\u{4E2D}', // CJK "middle" (wide, occupies 2 cells)
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        write_text(&mut grid, "B");
        // Row: A[4E2D][cont]B
        grid.move_cursor_to(0, 0);
        grid.insert_chars(1);
        // Shifted right by 1: " A[4E2D][cont]B" if space allows
        assert_eq!(char_at(&grid, 0, 0), ' '); // inserted blank
        assert_eq!(char_at(&grid, 0, 1), 'A');
    }

    #[test]
    fn test_dch_with_wide_char() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "AB");
        grid.write_wide_char_styled(
            '\u{4E2D}',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        write_text(&mut grid, "C");
        // Row: A B [4E2D] [cont] C
        grid.move_cursor_to(0, 0);
        grid.delete_chars(1);
        // A deleted, everything shifts left
        assert_eq!(char_at(&grid, 0, 0), 'B');
    }

    #[test]
    fn test_ech_on_wide_char() {
        let mut grid = Grid::new(4, 10);
        grid.write_wide_char_styled(
            '\u{4E2D}',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::empty(),
        );
        write_text(&mut grid, "XY");
        // Row: [4E2D][cont]XY
        grid.move_cursor_to(0, 0);
        grid.erase_chars(1);
        // Erase the first cell of the wide char
        assert_eq!(char_at(&grid, 0, 2), 'X');
        assert_eq!(char_at(&grid, 0, 3), 'Y');
    }

    // =========================================================================
    // Edge cases
    // =========================================================================

    #[test]
    fn test_ich_on_empty_row() {
        let mut grid = Grid::new(4, 10);
        grid.move_cursor_to(0, 0);
        grid.insert_chars(3);
        // Row was empty, inserting blanks into an empty row is fine
        assert_eq!(char_at(&grid, 0, 0), ' ');
        assert_eq!(char_at(&grid, 0, 1), ' ');
    }

    #[test]
    fn test_dch_on_empty_row() {
        let mut grid = Grid::new(4, 10);
        grid.move_cursor_to(0, 0);
        grid.delete_chars(3);
        assert_eq!(char_at(&grid, 0, 0), ' ');
    }

    #[test]
    fn test_ech_on_empty_row() {
        let mut grid = Grid::new(4, 10);
        grid.move_cursor_to(0, 0);
        grid.erase_chars(5);
        assert_eq!(char_at(&grid, 0, 0), ' ');
    }

    #[test]
    fn test_il_single_row_grid() {
        let mut grid = Grid::new(1, 10);
        write_text(&mut grid, "HELLO");
        grid.move_cursor_to(0, 0);
        grid.insert_lines(1);
        // On a 1-row grid, the single row is blanked
        assert_eq!(row_trimmed(&grid, 0), "");
    }

    #[test]
    fn test_dl_single_row_grid() {
        let mut grid = Grid::new(1, 10);
        write_text(&mut grid, "HELLO");
        grid.move_cursor_to(0, 0);
        grid.delete_lines(1);
        assert_eq!(row_trimmed(&grid, 0), "");
    }

    #[test]
    fn test_ich_clears_pending_wrap() {
        let mut grid = Grid::new(4, 5);
        write_text(&mut grid, "ABCDE"); // at col 4 (last col), pending_wrap set
        // The write_char does not set pending_wrap — it stops at max_col.
        // Use write_char_wrap to trigger deferred wrap.
        grid.move_cursor_to(0, 0);
        write_text(&mut grid, "ABCDE");
        // Force pending_wrap by writing at last column with wrap behavior
        grid.move_cursor_to(0, 4);
        grid.storage.mark_pending_wrap();
        assert!(grid.storage.pending_wrap);
        grid.insert_chars(1);
        assert!(!grid.storage.pending_wrap, "ICH should clear pending_wrap");
    }

    #[test]
    fn test_dch_clears_pending_wrap() {
        let mut grid = Grid::new(4, 5);
        grid.move_cursor_to(0, 4);
        grid.storage.mark_pending_wrap();
        assert!(grid.storage.pending_wrap);
        grid.delete_chars(1);
        assert!(!grid.storage.pending_wrap, "DCH should clear pending_wrap");
    }

    #[test]
    fn test_ech_clears_pending_wrap() {
        let mut grid = Grid::new(4, 5);
        grid.move_cursor_to(0, 4);
        grid.storage.mark_pending_wrap();
        assert!(grid.storage.pending_wrap);
        grid.erase_chars(1);
        assert!(!grid.storage.pending_wrap, "ECH should clear pending_wrap");
    }

    #[test]
    fn test_il_preserves_rows_above_cursor() {
        // Verify that rows above the cursor are completely untouched
        let mut grid = Grid::new(5, 10);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("ROW{r}"));
        }
        grid.move_cursor_to(3, 0);
        grid.insert_lines(2);
        // Rows 0-2 should be totally unchanged
        assert!(row_trimmed(&grid, 0).starts_with("ROW0"));
        assert!(row_trimmed(&grid, 1).starts_with("ROW1"));
        assert!(row_trimmed(&grid, 2).starts_with("ROW2"));
        assert_eq!(row_trimmed(&grid, 3), "");
        assert_eq!(row_trimmed(&grid, 4), "");
    }

    #[test]
    fn test_dl_preserves_rows_above_cursor() {
        let mut grid = Grid::new(5, 10);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("ROW{r}"));
        }
        grid.move_cursor_to(3, 0);
        grid.delete_lines(1);
        assert!(row_trimmed(&grid, 0).starts_with("ROW0"));
        assert!(row_trimmed(&grid, 1).starts_with("ROW1"));
        assert!(row_trimmed(&grid, 2).starts_with("ROW2"));
        assert!(row_trimmed(&grid, 3).starts_with("ROW4"));
        assert_eq!(row_trimmed(&grid, 4), "");
    }

    // =========================================================================
    // ICH/DCH inverse relationship
    // =========================================================================

    #[test]
    fn test_ich_then_dch_roundtrip() {
        let mut grid = Grid::new(4, 10);
        write_text(&mut grid, "ABCDE");
        grid.move_cursor_to(0, 2);
        grid.insert_chars(2); // "AB  CDE.."
        grid.move_cursor_to(0, 2);
        grid.delete_chars(2); // Should restore "ABCDE"
        assert_eq!(char_at(&grid, 0, 0), 'A');
        assert_eq!(char_at(&grid, 0, 1), 'B');
        assert_eq!(char_at(&grid, 0, 2), 'C');
        assert_eq!(char_at(&grid, 0, 3), 'D');
        assert_eq!(char_at(&grid, 0, 4), 'E');
    }

    #[test]
    fn test_il_then_dl_roundtrip() {
        let mut grid = Grid::new(5, 10);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("L{r}"));
        }
        grid.move_cursor_to(1, 0);
        grid.insert_lines(1);
        grid.move_cursor_to(1, 0);
        grid.delete_lines(1);
        // L4 was pushed off by IL and is gone, but L0-L3 should be restored
        assert!(row_trimmed(&grid, 0).starts_with("L0"));
        assert!(row_trimmed(&grid, 1).starts_with("L1"));
        assert!(row_trimmed(&grid, 2).starts_with("L2"));
        assert!(row_trimmed(&grid, 3).starts_with("L3"));
        assert_eq!(row_trimmed(&grid, 4), ""); // L4 was lost by IL
    }

    // =========================================================================
    // DECLRMM IL/DL horizontal-containment (#7530 follow-up)
    // =========================================================================

    /// IL with DECLRMM active is a no-op when the cursor column is outside the
    /// horizontal margins, even when the cursor row is inside the vertical
    /// scroll region. It must not shift content nor move the cursor column.
    #[test]
    fn test_il_margined_cursor_col_outside_margins_is_noop() {
        let mut grid = Grid::new(24, 80);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("ROW{r}"));
        }
        grid.set_horizontal_margins(10, 50);
        // Cursor row 2 is inside the (default full) vertical region, but col 70
        // is outside [10, 50].
        grid.move_cursor_to(2, 70);
        grid.insert_lines_margined(1, true);
        // No content shift: rows still in original order.
        assert!(row_trimmed(&grid, 2).starts_with("ROW2"));
        assert!(row_trimmed(&grid, 3).starts_with("ROW3"));
        // Cursor column unchanged (must NOT jump to margins.left == 10).
        assert_eq!(grid.cursor_col(), 70, "no-op IL must not move cursor col");
        assert_eq!(grid.cursor_row(), 2);
    }

    /// DL with DECLRMM active is a no-op when the cursor column is outside the
    /// horizontal margins.
    #[test]
    fn test_dl_margined_cursor_col_outside_margins_is_noop() {
        let mut grid = Grid::new(24, 80);
        for r in 0..5u16 {
            grid.move_cursor_to(r, 0);
            write_text(&mut grid, &format!("ROW{r}"));
        }
        grid.set_horizontal_margins(10, 50);
        grid.move_cursor_to(2, 70);
        grid.delete_lines_margined(1, true);
        assert!(row_trimmed(&grid, 2).starts_with("ROW2"));
        assert!(row_trimmed(&grid, 3).starts_with("ROW3"));
        assert_eq!(grid.cursor_col(), 70, "no-op DL must not move cursor col");
        assert_eq!(grid.cursor_row(), 2);
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
        grid.move_cursor_to(0, 30); // inside [10,50]
        grid.insert_lines_margined(1, true);
        // Row 0's margin cells moved down into row 1; cursor col moved to left margin.
        assert_eq!(grid.cursor_col(), 10);
        assert!(row_trimmed(&grid, 1).contains("AAA"));
    }

    /// Regression (codex round-17 review): margined IL/DL and scroll_region_*_
    /// margined write cells via get_mut, bypassing len maintenance. When the
    /// margined copy blanks a row's tail content, Row.len must still shrink —
    /// otherwise row_text/search/scrollback emit phantom trailing spaces (#7522).
    #[test]
    fn test_dl_margined_shrinks_len_when_blanks_cover_tail() {
        let mut grid = Grid::new(24, 80);
        // Row 0: content only within the margins at the tail (cols 48-50), nothing
        // beyond the right margin. Row 1 (the shift source) stays blank.
        grid.move_cursor_to(0, 48);
        write_text(&mut grid, "END");
        assert_eq!(
            grid.row_len(0),
            Some(51),
            "precondition: row 0 content ends at col 50"
        );

        grid.set_horizontal_margins(10, 50);
        grid.move_cursor_to(0, 20); // inside [10,50] → margined DL applies
        grid.delete_lines_margined(1, true);

        // Row 0 cols [10,50] were overwritten with row 1's blanks (the "END" tail is
        // gone) and cols outside the margins were already blank → row 0 is empty.
        assert_eq!(
            grid.row_len(0),
            Some(0),
            "len must shrink to 0, not stay stale-high at 51 (phantom trailing spaces)"
        );
    }

    // =========================================================================
    // DECIC/DECDC vertical-region containment (#7530 mirror)
    // =========================================================================

    /// DECIC is a no-op when the cursor row is outside the vertical scroll
    /// region, even when the cursor column is within the horizontal margins.
    #[test]
    fn test_decic_cursor_row_outside_region_is_noop() {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(15, 0);
        write_text(&mut grid, "ABCDE");
        grid.set_scroll_region(0, 10); // region rows [0, 10]
        grid.move_cursor_to(15, 2); // row 15 outside region; col inside full margins
        grid.insert_columns(3);
        // Row 15 must be untouched.
        assert_eq!(char_at(&grid, 15, 0), 'A');
        assert_eq!(char_at(&grid, 15, 1), 'B');
        assert_eq!(char_at(&grid, 15, 2), 'C');
        assert_eq!(char_at(&grid, 15, 3), 'D');
        assert_eq!(char_at(&grid, 15, 4), 'E');
    }

    /// DECDC is a no-op when the cursor row is outside the vertical scroll
    /// region.
    #[test]
    fn test_decdc_cursor_row_outside_region_is_noop() {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(15, 0);
        write_text(&mut grid, "ABCDE");
        grid.set_scroll_region(0, 10);
        grid.move_cursor_to(15, 2);
        grid.delete_columns(3);
        assert_eq!(char_at(&grid, 15, 0), 'A');
        assert_eq!(char_at(&grid, 15, 1), 'B');
        assert_eq!(char_at(&grid, 15, 2), 'C');
        assert_eq!(char_at(&grid, 15, 3), 'D');
        assert_eq!(char_at(&grid, 15, 4), 'E');
    }

    /// DECIC inside the vertical region still inserts (guard does not over-fire).
    #[test]
    fn test_decic_cursor_row_inside_region_still_inserts() {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(5, 0);
        write_text(&mut grid, "ABCDE");
        grid.set_scroll_region(0, 10);
        grid.move_cursor_to(5, 2); // inside region
        grid.insert_columns(2);
        // Two blank columns inserted at col 2: "AB  CDE".
        assert_eq!(char_at(&grid, 5, 0), 'A');
        assert_eq!(char_at(&grid, 5, 1), 'B');
        assert_eq!(char_at(&grid, 5, 2), ' ');
        assert_eq!(char_at(&grid, 5, 3), ' ');
        assert_eq!(char_at(&grid, 5, 4), 'C');
    }
}
