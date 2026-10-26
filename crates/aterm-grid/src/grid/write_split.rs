// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Split write/advance primitives and bulk ASCII extras path for the terminal handler.

use std::sync::Arc;

use super::write::StaleExtras;
use super::{Cell, CellFlags, Grid, PackedColors};

impl Grid {
    /// Write a styled character at cursor with pre-computed packed colors.
    #[inline]
    pub fn write_char_at_cursor_packed(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 1);
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char_packed(cursor_col, c, colors, flags);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
        self.storage
            .extras
            .clear_rgb_ring_cell(cursor_row, cursor_col, 1);
        self.storage.mark_content_cell(cursor_row, cursor_col);
    }

    /// Write a wide character at cursor with pre-computed packed colors.
    #[inline]
    #[cfg(test)]
    pub fn write_wide_char_at_cursor_packed(
        &mut self,
        c: char,
        colors: PackedColors,
        flags: CellFlags,
    ) -> bool {
        let effective_cols = self.storage.effective_cols_for_row(self.storage.cursor.row);
        self.write_wide_char_at_cursor_packed_ecols(c, colors, flags, effective_cols)
    }

    /// Write a wide character at the cursor, given the row's effective column count.
    #[inline]
    pub fn write_wide_char_at_cursor_packed_ecols(
        &mut self,
        c: char,
        colors: PackedColors,
        flags: CellFlags,
        effective_cols: u16,
    ) -> bool {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);
        if cursor_col.saturating_add(1) < effective_cols
            && let Some(row) = self.row_mut(cursor_row)
            && row.write_wide_char_packed(cursor_col, c, colors, flags)
        {
            self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
            self.storage
                .extras
                .clear_rgb_ring_cell(cursor_row, cursor_col, 2);
            self.storage.mark_content_wide_cell(cursor_row, cursor_col);
            return true;
        }
        false
    }

    /// Pre-wrap for a wide char.
    #[inline]
    pub fn pre_wrap_wide_ecols(&mut self, effective_cols: u16, fill: Cell) -> u16 {
        if self.storage.cursor.col.saturating_add(1) >= effective_cols {
            self.blank_wide_wrap_tail(effective_cols, fill);
            self.advance_autowrap_line();
            self.storage.effective_cols_for_row(self.storage.cursor.row)
        } else {
            effective_cols
        }
    }

    /// Blank, with BCE blanks.
    #[inline]
    pub(super) fn blank_wide_wrap_tail(&mut self, ecols: u16, fill: Cell) {
        let row = self.storage.cursor.row;
        let col = self.storage.cursor.col;
        let mut bce: Option<core::ops::Range<u16>> = None;
        if let Some(r) = self.storage.row_mut(row) {
            bce = Some(r.clear_range_with_orphan(col, ecols, fill, fill));
            let cols = r.cols();
            r.set_early_wrap_hole(ecols == cols && col.saturating_add(1) == cols);
        }
        if let Some(bce) = bce {
            self.drop_unclaimed_extras(row, bce.clone());
            if fill.colors().bg_is_rgb() {
                self.fill_bce_rgb_range(row, bce.start, bce.end);
            }
        }
        self.storage.mark_content_row(row);
    }

    /// Advance the cursor 1 column without wrapping, stopping at the row's last column.
    #[inline]
    pub fn advance_cursor_no_wrap(&mut self) {
        let fast_max = self.storage.cols.saturating_sub(1);
        if self.storage.cursor.col < fast_max && !self.storage.any_double_width {
            self.storage.cursor.col += 1;
        } else {
            let max_col = self.storage.max_col_for_row(self.storage.cursor.row);
            if self.storage.cursor.col < max_col {
                self.storage.cursor.col += 1;
            } else {
                self.storage.mark_pending_wrap();
            }
        }
    }

    /// Advance the cursor 1 column with deferred autowrap (xterm/VT220).
    #[inline]
    pub fn advance_cursor_wrap(&mut self) {
        let fast_max = self.storage.cols.saturating_sub(1);
        if self.storage.cursor.col < fast_max
            && !self.storage.any_double_width
            && !self.storage.has_horizontal_margins
        {
            self.storage.cursor.col += 1;
        } else {
            self.advance_cursor_wrap_slow();
        }
    }

    /// Slow path for `advance_cursor_wrap`.
    #[inline(never)]
    fn advance_cursor_wrap_slow(&mut self) {
        let max_col = self.last_print_col(self.storage.cursor.row, self.storage.cursor.col);

        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        } else {
            self.storage.mark_pending_wrap();
        }
    }

    /// The last column a glyph printed at `col` of `row` reaches before its deferred wrap: the
    /// DECSLRM right margin unless `col` is past it (xterm `dotext`), else the row's last column.
    #[inline]
    pub(super) fn last_print_col(&self, row: u16, col: u16) -> u16 {
        let max_col = self.storage.max_col_for_row(row);
        if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            if col <= margins.right {
                return margins.right.min(max_col);
            }
        }
        max_col
    }

    /// Wrap first when the cursor sits past its row's last printable column.
    #[inline]
    pub fn wrap_if_past_print_edge(&mut self) {
        let (row, col) = (self.storage.cursor.row, self.storage.cursor.col);
        if self.storage.any_double_width && col > self.last_print_col(row, col) {
            self.advance_autowrap_line();
        }
    }

    /// Get effective columns clamped by DECLRMM right margin.
    #[inline]
    fn margin_clamped_ecols(&self, row: u16) -> u16 {
        self.last_print_col(row, self.storage.cursor.col)
            .saturating_add(1)
    }

    /// Advance the cursor 2 columns (wide char) without wrapping.
    #[inline]
    pub fn advance_cursor_wide_no_wrap_ecols(&mut self, effective_cols: u16) {
        let max_col = effective_cols.saturating_sub(1);
        let next = self.storage.cursor.col.saturating_add(2);
        if next > max_col {
            self.storage.mark_pending_wrap();
        }
        self.storage.cursor.col = next.min(max_col);
    }

    /// Advance cursor by 2 columns with deferred autowrap.
    #[inline]
    pub fn advance_cursor_wide_wrap_ecols(&mut self, effective_cols: u16) {
        let wrap_cols = if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            // Not past the right margin, as `last_print_col`.
            if !margins.is_full(self.storage.cols) && self.storage.cursor.col <= margins.right {
                effective_cols.min(margins.right.saturating_add(1))
            } else {
                effective_cols
            }
        } else {
            effective_cols
        };

        match (self.storage.cursor.col.saturating_add(2)).cmp(&wrap_cols) {
            std::cmp::Ordering::Less => {
                self.storage.cursor.col += 2;
            }
            std::cmp::Ordering::Equal | std::cmp::Ordering::Greater => {
                self.storage.cursor.col = wrap_cols.saturating_sub(1);
                self.storage.mark_pending_wrap();
            }
        }
    }

    /// Pre-wrap + write + damage + advance for a BMP wide char, with one ring-buffer lookup.
    #[inline]
    pub fn write_wide_autowrap_fast(
        &mut self,
        c: char,
        colors: PackedColors,
        flags: CellFlags,
    ) -> bool {
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }

        let ecols = self.margin_clamped_ecols(self.storage.cursor.row);

        let ecols = if self.storage.cursor.col + 1 >= ecols {
            self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
            self.advance_autowrap_line();
            self.margin_clamped_ecols(self.storage.cursor.row)
        } else {
            ecols
        };

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);

        if let Some(row) = self.storage.row_mut(cursor_row) {
            if !row.write_wide_char_packed(cursor_col, c, colors, flags) {
                return false;
            }
        } else {
            return false;
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        self.storage.mark_content_row(cursor_row);

        let new_col = cursor_col + 2;
        if new_col >= ecols {
            self.storage.cursor.col = ecols.saturating_sub(1);
            self.storage.mark_pending_wrap();
        } else {
            self.storage.cursor.col = new_col;
        }
        true
    }

    /// `write_wide_autowrap_fast` for a non-BMP wide char.
    #[inline]
    pub fn write_emoji_autowrap_fast(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }

        let ecols = self.margin_clamped_ecols(self.storage.cursor.row);

        let ecols = if self.storage.cursor.col + 1 >= ecols {
            self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
            self.advance_autowrap_line();
            self.margin_clamped_ecols(self.storage.cursor.row)
        } else {
            ecols
        };

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);

        if let Some(row) = self.storage.row_mut(cursor_row) {
            if !row.write_wide_char_packed(cursor_col, c, colors, flags) {
                return;
            }
        } else {
            return;
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        self.storage.mark_content_row(cursor_row);

        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        self.storage.extras_mut().set_complex_char_ring(
            cursor_row,
            cursor_col,
            c,
            visible_rows,
            cols,
        );

        let new_col = cursor_col + 2;
        if new_col >= ecols {
            self.storage.cursor.col = ecols.saturating_sub(1);
            self.storage.mark_pending_wrap();
        } else {
            self.storage.cursor.col = new_col;
        }
    }

    /// Resolve-wrap + write + damage + advance for a width-1 char.
    #[inline]
    pub fn write_narrow_autowrap_fast(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        // Resolve a pending wrap first, or a width-1 char after a wide char at end of line
        // overwrites its spacer half.
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }
        self.wrap_if_past_print_edge();

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        let stale = self.stale_extras_pair(cursor_row, cursor_col, 1);

        if let Some(row) = self.storage.row_mut(cursor_row) {
            row.write_char_packed(cursor_col, c, colors, flags);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        self.storage.mark_content_row(cursor_row);

        let fast_max = self.storage.cols.saturating_sub(1);
        if cursor_col < fast_max
            && !self.storage.any_double_width
            && !self.storage.has_horizontal_margins
        {
            self.storage.cursor.col = cursor_col + 1;
        } else {
            let max_col = self.last_print_col(cursor_row, cursor_col);
            if cursor_col < max_col {
                self.storage.cursor.col = cursor_col + 1;
            } else {
                self.storage.mark_pending_wrap();
            }
        }
    }

    /// Write a run of BMP width-2 characters with autowrap, batched per row.
    #[inline]
    pub fn write_wide_run_autowrap(
        &mut self,
        chars: &[char],
        colors: PackedColors,
        flags: CellFlags,
    ) {
        if chars.is_empty() {
            return;
        }

        let simple_mode = !self.storage.has_horizontal_margins && !self.storage.any_double_width;
        let cols = self.storage.cols;
        let mut pos = 0;
        let scan_stale = !self.storage.extras.is_empty();

        while pos < chars.len() {
            if self.storage.take_pending_wrap() {
                self.advance_autowrap_line();
            }

            let ecols = if simple_mode {
                cols
            } else {
                self.margin_clamped_ecols(self.storage.cursor.row)
            };
            let cursor_row = self.storage.cursor.row;
            let cursor_col = self.storage.cursor.col;

            let (ecols, cursor_row, cursor_col) = if cursor_col + 1 >= ecols {
                self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
                self.advance_autowrap_line();
                let new_ecols = if simple_mode {
                    cols
                } else {
                    self.margin_clamped_ecols(self.storage.cursor.row)
                };
                (new_ecols, self.storage.cursor.row, self.storage.cursor.col)
            } else {
                (ecols, cursor_row, cursor_col)
            };

            let available_cols = ecols.saturating_sub(cursor_col);
            let max_chars = (available_cols / 2) as usize;
            let to_write = (chars.len() - pos).min(max_chars);
            if to_write == 0 {
                pos += 1;
                continue;
            }

            let mut stale = StaleExtras::NONE;
            if let Some(row) = self.storage.row_mut(cursor_row) {
                let end_col_run = cursor_col + (to_write as u16) * 2;
                if scan_stale {
                    stale = StaleExtras::probe(row, cursor_col, end_col_run);
                }
                if (end_col_run as usize) <= row.cells_len() {
                    let end_half_blanked =
                        row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for i in 0..to_write {
                        let col = cursor_col + (i as u16) * 2;
                        // SAFETY: `col + 1 < cells_len()` on a wide boundary; writes are disjoint.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, chars[pos + i], colors, flags);
                        }
                    }
                    row.settle_len_after_wide_run(end_col_run, end_half_blanked);
                    row.mark_has_wide_chars();
                } else {
                    for i in 0..to_write {
                        let col = cursor_col + (i as u16) * 2;
                        row.write_wide_char_packed(col, chars[pos + i], colors, flags);
                    }
                }
            }
            self.remove_stale_extras(cursor_row, stale);

            self.storage.mark_content_row(cursor_row);

            pos += to_write;

            let end_col = cursor_col + (to_write as u16) * 2;
            if end_col >= ecols {
                self.storage.cursor.col = ecols.saturating_sub(1);
                self.storage.mark_pending_wrap();
            } else {
                self.storage.cursor.col = end_col;
            }
        }
    }

    /// `write_wide_run_autowrap` for non-BMP width-2 characters (emoji).
    #[inline]
    #[cfg(test)]
    pub fn write_emoji_run_autowrap(
        &mut self,
        chars: &[char],
        colors: PackedColors,
        flags: CellFlags,
    ) {
        if chars.is_empty() {
            return;
        }

        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        let simple_mode = !self.storage.has_horizontal_margins && !self.storage.any_double_width;
        let mut pos = 0;
        let scan_stale = !self.storage.extras.is_empty();

        while pos < chars.len() {
            if self.storage.take_pending_wrap() {
                self.advance_autowrap_line();
            }

            let ecols = if simple_mode {
                cols
            } else {
                self.margin_clamped_ecols(self.storage.cursor.row)
            };
            let cursor_row = self.storage.cursor.row;
            let cursor_col = self.storage.cursor.col;

            let (ecols, cursor_row, cursor_col) = if cursor_col + 1 >= ecols {
                self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
                self.advance_autowrap_line();
                let new_ecols = if simple_mode {
                    cols
                } else {
                    self.margin_clamped_ecols(self.storage.cursor.row)
                };
                (new_ecols, self.storage.cursor.row, self.storage.cursor.col)
            } else {
                (ecols, cursor_row, cursor_col)
            };

            let available_cols = ecols.saturating_sub(cursor_col);
            let max_chars = (available_cols / 2) as usize;
            let to_write = (chars.len() - pos).min(max_chars);
            if to_write == 0 {
                pos += 1;
                continue;
            }

            let end_col = cursor_col + (to_write as u16) * 2;

            let mut stale = StaleExtras::NONE;
            if let Some(row) = self.storage.row_mut(cursor_row) {
                if scan_stale {
                    stale = StaleExtras::probe(row, cursor_col, end_col);
                }
                if (end_col as usize) <= row.cells_len() {
                    let end_half_blanked =
                        row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for j in 0..to_write {
                        let col = cursor_col + (j as u16) * 2;
                        // SAFETY: `col + 1 < cells_len()` on a wide boundary; writes are disjoint.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, chars[pos + j], colors, flags);
                        }
                    }
                    row.settle_len_after_wide_run(end_col, end_half_blanked);
                    row.mark_has_wide_chars();
                } else {
                    for j in 0..to_write {
                        let col = cursor_col + (j as u16) * 2;
                        row.write_wide_char_packed(col, chars[pos + j], colors, flags);
                    }
                }
            }
            self.remove_stale_extras(cursor_row, stale);

            self.storage.extras_mut().set_complex_char_wide_run(
                cursor_row,
                cursor_col,
                &chars[pos..pos + to_write],
                visible_rows,
                cols,
            );

            self.storage.mark_content_row(cursor_row);

            pos += to_write;
            if end_col >= ecols {
                self.storage.cursor.col = ecols.saturating_sub(1);
                self.storage.mark_pending_wrap();
            } else {
                self.storage.cursor.col = end_col;
            }
        }
    }

    /// `write_wide_run_autowrap` for a mix of BMP and non-BMP width-2 characters.
    #[inline]
    pub fn write_mixed_wide_run_autowrap(
        &mut self,
        chars: &[char],
        bmp_colors: PackedColors,
        bmp_flags: CellFlags,
        nonbmp_flags: CellFlags,
    ) {
        if chars.is_empty() {
            return;
        }

        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        let simple_mode = !self.storage.has_horizontal_margins && !self.storage.any_double_width;
        let mut pos = 0;
        let scan_stale = !self.storage.extras.is_empty();

        while pos < chars.len() {
            if self.storage.take_pending_wrap() {
                self.advance_autowrap_line();
            }

            let ecols = if simple_mode {
                cols
            } else {
                self.margin_clamped_ecols(self.storage.cursor.row)
            };
            let cursor_row = self.storage.cursor.row;
            let cursor_col = self.storage.cursor.col;

            let (ecols, cursor_row, cursor_col) = if cursor_col + 1 >= ecols {
                self.blank_wide_wrap_tail(ecols, Cell::bce_blank(bmp_colors));
                self.advance_autowrap_line();
                let new_ecols = if simple_mode {
                    cols
                } else {
                    self.margin_clamped_ecols(self.storage.cursor.row)
                };
                (new_ecols, self.storage.cursor.row, self.storage.cursor.col)
            } else {
                (ecols, cursor_row, cursor_col)
            };

            let available_cols = ecols.saturating_sub(cursor_col);
            let max_chars = (available_cols / 2) as usize;
            let to_write = (chars.len() - pos).min(max_chars);
            if to_write == 0 {
                pos += 1;
                continue;
            }

            let end_col = cursor_col + (to_write as u16) * 2;
            let slice = &chars[pos..pos + to_write];

            let mut stale = StaleExtras::NONE;
            if let Some(row) = self.storage.row_mut(cursor_row) {
                if scan_stale {
                    stale = StaleExtras::probe(row, cursor_col, end_col);
                }
                if (end_col as usize) <= row.cells_len() {
                    let end_half_blanked =
                        row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for (j, &c) in slice.iter().enumerate() {
                        let col = cursor_col + (j as u16) * 2;
                        let fl = if (c as u32) > 0xFFFF {
                            nonbmp_flags
                        } else {
                            bmp_flags
                        };
                        // SAFETY: `col + 1 < cells_len()` on a wide boundary; writes are disjoint.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, c, bmp_colors, fl);
                        }
                    }
                    row.settle_len_after_wide_run(end_col, end_half_blanked);
                    row.mark_has_wide_chars();
                } else {
                    for (j, &c) in slice.iter().enumerate() {
                        let col = cursor_col + (j as u16) * 2;
                        let fl = if (c as u32) > 0xFFFF {
                            nonbmp_flags
                        } else {
                            bmp_flags
                        };
                        row.write_wide_char_packed(col, c, bmp_colors, fl);
                    }
                }
            }
            self.remove_stale_extras(cursor_row, stale);

            self.storage.extras_mut().set_mixed_wide_ring(
                cursor_row,
                cursor_col,
                slice,
                visible_rows,
                cols,
            );

            self.storage.mark_content_row(cursor_row);

            pos += to_write;
            if end_col >= ecols {
                self.storage.cursor.col = ecols.saturating_sub(1);
                self.storage.mark_pending_wrap();
            } else {
                self.storage.cursor.col = end_col;
            }
        }
    }

    /// Write styled ASCII bytes whose style needs `CellExtras`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn write_ascii_run_with_extras(
        &mut self,
        ascii: &[u8],
        colors: PackedColors,
        flags: CellFlags,
        fg_rgb: Option<[u8; 3]>,
        bg_rgb: Option<[u8; 3]>,
        underline_color: Option<u32>,
        extended_flags_bits: u16,
        hyperlink_url: Option<&Arc<str>>,
        hyperlink_id: Option<&Arc<str>>,
        last_byte: &mut Option<u8>,
    ) -> usize {
        if ascii.is_empty() {
            return 0;
        }

        let mut written = 0;
        let mut remaining = ascii;
        let scan_stale = !self.storage.extras.is_empty();

        while !remaining.is_empty() {
            self.resolve_pending_wrap();

            let cursor_row = self.storage.cursor.row;
            let cursor_col = self.storage.cursor.col;

            let margin_clamp = if self.storage.has_horizontal_margins {
                let margins = self.storage.horizontal_margins();
                // Not past the right margin (`last_print_col`).
                if !margins.is_full(self.storage.cols) && cursor_col <= margins.right {
                    Some(margins.right.saturating_add(1))
                } else {
                    None
                }
            } else {
                None
            };

            let row_may_have_extras = scan_stale
                && self
                    .storage
                    .extras
                    .map_may_have_row(cursor_row, usize::from(self.storage.cols));

            let Some((row, effective_cols)) = self.storage.row_mut_with_effective_cols(cursor_row)
            else {
                break;
            };

            let effective_cols = if let Some(clamp) = margin_clamp {
                effective_cols.min(clamp)
            } else {
                effective_cols
            };

            let max_col = effective_cols.saturating_sub(1);

            let available = (effective_cols.saturating_sub(cursor_col)) as usize;
            let to_write = remaining.len().min(available);

            if to_write == 0 {
                self.advance_autowrap_line();
                continue;
            }

            let to_write_u16 = u16::try_from(to_write).unwrap_or(u16::MAX);

            let stale = if row_may_have_extras {
                StaleExtras::probe(row, cursor_col, cursor_col + to_write_u16)
            } else {
                StaleExtras::NONE
            };

            if let Some(target) = row.cells_mut_with_fixup(cursor_col, to_write_u16) {
                debug_assert_eq!(target.len(), to_write);
                let colors_with_extras = colors.with_extras_flag();
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_styled(byte, colors_with_extras, flags);
                }
            }

            row.update_len(cursor_col + to_write_u16);

            self.remove_stale_extras(cursor_row, stale);

            {
                use crate::UniformExtras;
                let vals = UniformExtras {
                    fg_rgb,
                    bg_rgb,
                    underline_color,
                    extended_flags: extended_flags_bits,
                    hyperlink: hyperlink_url,
                    hyperlink_id,
                };
                let vr = self.storage.visible_rows;
                let c = self.storage.cols;
                self.storage.extras.set_range_uniform(
                    cursor_row,
                    cursor_col,
                    cursor_col + to_write_u16,
                    &vals,
                    vr,
                    c,
                );
            }

            self.storage.mark_content_row(cursor_row);

            if let Some(&last) = remaining[..to_write].last() {
                *last_byte = Some(last);
            }

            written += to_write;
            remaining = &remaining[to_write..];

            let new_col = cursor_col + to_write_u16;
            if new_col > max_col {
                if remaining.is_empty() {
                    self.storage.cursor.col = max_col;
                    self.storage.mark_pending_wrap();
                } else {
                    self.advance_autowrap_line();
                }
            } else {
                self.storage.cursor.col = new_col;
            }
        }

        debug_assert!(written <= ascii.len());
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        written
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, CellFlags, Grid, PackedColors};

    #[test]
    fn wide_wrap_tail_paints_the_bisected_orphan_with_the_bce_blank_and_its_ring() {
        let (rows, cols) = (3u16, 10u16);
        let mut grid = Grid::new(rows, cols);

        let old_bg = [10u8, 20, 30];
        let old_colors = PackedColors::new().with_rgb_bg();
        grid.set_cursor_template(Cell::bce_blank(old_colors), Some(old_bg));
        grid.move_cursor_to(0, 8);
        assert!(grid.write_wide_autowrap_fast('\u{4E2D}', old_colors, CellFlags::empty()));
        grid.extras_mut()
            .set_rgb_ring_range(0, 8, 10, None, Some(old_bg), rows, cols);
        assert!(
            grid.cell(0, 8).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: col 8 is the wide lead"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 8),
            Some(old_bg),
            "precondition: the ring holds the first glyph's bytes"
        );

        let new_bg = [200u8, 100, 50];
        let new_colors = PackedColors::new().with_rgb_bg();
        grid.set_cursor_template(Cell::bce_blank(new_colors), Some(new_bg));
        grid.move_cursor_to(0, 9);
        assert!(grid.write_wide_autowrap_fast('\u{4E2D}', new_colors, CellFlags::empty()));

        assert!(
            grid.cell(1, 0).unwrap().flags().contains(CellFlags::WIDE),
            "precondition: the second glyph wrapped to row 1"
        );

        let fill = Cell::bce_blank(new_colors);
        let orphan = grid.cell(0, 8).unwrap();
        assert!(
            !orphan.flags().contains(CellFlags::WIDE),
            "the orphaned wide head must be blanked"
        );
        assert_eq!(orphan.char(), ' ', "the orphan becomes a space");
        assert_eq!(
            orphan.colors(),
            fill.colors(),
            "the orphan carries the BCE blank, as xterm's ClearCells writes it"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 8),
            Some(new_bg),
            "the orphan resolves the new truecolor background from the ring"
        );
        assert_eq!(
            grid.cell(0, 9).unwrap().colors(),
            fill.colors(),
            "the blanked tail itself carries the BCE blank"
        );
        assert_eq!(
            grid.bg_rgb_at(0, 9),
            Some(new_bg),
            "the blanked tail resolves the new background from the ring"
        );
    }
}
