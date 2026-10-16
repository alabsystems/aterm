// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Split write/advance primitives for the terminal handler.
//!
//! Writing at the cursor is separate from advancing it, so the handler can apply cell extras
//! (hyperlinks, non-BMP overflow, underline colours) at the written cell before the advance can
//! scroll the row away. All write damage goes through `mark_content_*`. Also the bulk extras
//! path for ASCII runs (`write_ascii_run_with_extras`).

use std::sync::Arc;

#[cfg(test)]
use super::StyleId;
use super::write::StaleExtras;
use super::{Cell, CellFlags, Grid, PackedColors};

impl Grid {
    /// Write a styled character at cursor with pre-computed packed colors.
    ///
    /// Avoids per-character `convert_legacy_colors` — caller pre-computes once.
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn write_char_at_cursor_packed(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        // Drop a stale extras-map entry before overwriting (and the entry of a
        // wide half the overwrite orphans), so a later
        // `cell_extra_mut_preflagged` here cannot resurrect old data (an old
        // hyperlink) via `get_or_create`.
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 1);
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char_packed(cursor_col, c, colors, flags);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
        // Drop a stale dense-RGB-ring entry too: this path puts the new cell's RGB in the
        // extras map (`apply_cell_extras_preflagged`), but reads consult the ring first (filled
        // by the bulk and BCE/erase paths), so a leftover ring colour would shadow the new one.
        self.storage
            .extras
            .clear_rgb_ring_cell(cursor_row, cursor_col, 1);
        self.storage.mark_content_cell(cursor_row, cursor_col);
    }

    /// Write a wide character at cursor with pre-computed packed colors.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
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
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
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

        // Drop stale extras-map entries for both halves, only once the write
        // succeeds.
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);
        if cursor_col.saturating_add(1) < effective_cols
            && let Some(row) = self.row_mut(cursor_row)
            && row.write_wide_char_packed(cursor_col, c, colors, flags)
        {
            self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
            // Drop stale dense-RGB-ring entries for both halves; see
            // `write_char_at_cursor_packed`.
            self.storage
                .extras
                .clear_rgb_ring_cell(cursor_row, cursor_col, 2);
            self.storage.mark_content_wide_cell(cursor_row, cursor_col);
            return true;
        }
        false
    }

    /// Pre-wrap for a wide char: wrap first if it won't fit at the cursor. Returns the
    /// effective columns of the (possibly new) cursor row.
    ///
    /// REQUIRES: pending wrap already resolved
    /// REQUIRES: self.storage.visible_rows > 0
    #[inline]
    pub fn pre_wrap_wide_ecols(&mut self, effective_cols: u16, fill: Cell) -> u16 {
        if self.storage.cursor.col.saturating_add(1) >= effective_cols {
            // The glyph won't fit at the last column: blank the skipped tail with the BCE
            // background and wrap (xterm; see `blank_wide_wrap_tail`).
            self.blank_wide_wrap_tail(effective_cols, fill);
            self.advance_autowrap_line();
            self.storage.effective_cols_for_row(self.storage.cursor.row)
        } else {
            effective_cols
        }
    }

    /// Blank, with BCE blanks, the tail cell(s) a wide glyph skips when it wraps off the last
    /// column, as xterm does, rather than strand the old content. Call with the cursor still
    /// on the old row, before `advance_autowrap_line()`.
    #[inline]
    pub(super) fn blank_wide_wrap_tail(&mut self, ecols: u16, fill: Cell) {
        let row = self.storage.cursor.row;
        let col = self.storage.cursor.col;
        // `None` only if the row does not exist; then nothing was blanked and
        // there is no span to give the ring either.
        let mut bce: Option<core::ops::Range<u16>> = None;
        if let Some(r) = self.storage.row_mut(row) {
            // A wide half bisected by this blanking takes the BCE blank (`fill`), not a
            // default-coloured one: xterm's `ClearCells` widens its run over the adjacent
            // `HIDDEN_CHAR` halves and writes the current colour pair. EL/ED share this rule;
            // only the rectangle ops use `Clear1Cell`. See `Row::clear_range_with_orphan`.
            bce = Some(r.clear_range_with_orphan(col, ecols, fill, fill));
            // Record the hole the wrap leaves, so a rewrap drops this cell and
            // only it (`RowFlags::EARLY_WRAP_HOLE`). Only a hole that is the
            // row's physical last cell: under DECLRMM or on a DECDWL row the
            // wrap edge is not the row end, and no rewrap merges such a row.
            let cols = r.cols();
            r.set_early_wrap_hole(ecols == cols && col.saturating_add(1) == cols);
        }
        // A truecolor BCE cell carries only an RGB marker; its bytes live in `RgbColorRing`, so
        // the blanked tail and orphan need ring entries or they resolve the dead glyph's bytes.
        // `fill_bce_rgb_range` reads them from the cursor template; the `bg_is_rgb` guard keeps
        // a caller whose `fill` disagrees with it from seeding the ring behind an indexed marker.
        if let Some(bce) = bce {
            // The blanked tail and orphan drop the extras they carried (a mark on
            // the last column's glyph), or a write there would adopt them.
            self.drop_unclaimed_extras(row, bce.clone());
            if fill.colors().bg_is_rgb() {
                self.fill_bce_rgb_range(row, bce.start, bce.end);
            }
        }
        self.storage.mark_content_row(row);
    }

    /// Advance the cursor 1 column without wrapping, stopping at the row's last column. A write
    /// there still arms `pending_wrap`: xterm sets `do_wrap = need_wrap` whenever a print fills
    /// to the margin regardless of DECAWM (charproc.c `dotext`), so re-enabling DECAWM lets the
    /// next printable wrap.
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

    /// Advance the cursor 1 column with deferred autowrap (xterm/VT220): at the last column,
    /// arm `pending_wrap` so the next printable wraps. The slow path handles the last column,
    /// DECDWL rows and DECLRMM margins.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
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

    /// Slow path for `advance_cursor_wrap`: handles DECDWL and DECLRMM margins.
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
    /// row's last column (half-width on a DECDWL/DECDHL row), or the DECSLRM right margin when
    /// `col` is inside the margin span, whichever comes first. The run writers derive the same
    /// clamp per row (`effective_cols.min(right + 1)`).
    #[inline]
    pub(super) fn last_print_col(&self, row: u16, col: u16) -> u16 {
        let max_col = self.storage.max_col_for_row(row);
        if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            if col >= margins.left && col <= margins.right {
                return margins.right.min(max_col);
            }
        }
        max_col
    }

    /// Get effective columns clamped by DECLRMM right margin.
    #[inline]
    fn margin_clamped_ecols(&self, row: u16) -> u16 {
        self.last_print_col(row, self.storage.cursor.col)
            .saturating_add(1)
    }

    /// Advance the cursor 2 columns (wide char) without wrapping. Filling to the margin arms
    /// `pending_wrap` even with autowrap off; see `advance_cursor_no_wrap`.
    #[inline]
    pub fn advance_cursor_wide_no_wrap_ecols(&mut self, effective_cols: u16) {
        let max_col = effective_cols.saturating_sub(1);
        let next = self.storage.cursor.col.saturating_add(2);
        if next > max_col {
            self.storage.mark_pending_wrap();
        }
        self.storage.cursor.col = next.min(max_col);
    }

    /// Advance cursor by 2 columns with deferred autowrap, using pre-computed
    /// effective column count. DECLRMM-aware.
    ///
    /// REQUIRES: self.storage.visible_rows > 0
    #[inline]
    pub fn advance_cursor_wide_wrap_ecols(&mut self, effective_cols: u16) {
        let wrap_cols = if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            if !margins.is_full(self.storage.cols)
                && self.storage.cursor.col >= margins.left
                && self.storage.cursor.col <= margins.right
            {
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
    /// Returns `true` if written.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode
    /// REQUIRES: `self.storage.visible_rows > 0`
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
            // The glyph won't fit at the last column: blank the skipped cell with the BCE
            // background and wrap (xterm; see `blank_wide_wrap_tail`).
            self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
            self.advance_autowrap_line();
            self.margin_clamped_ecols(self.storage.cursor.row)
        } else {
            ecols
        };

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        // As in `write_wide_char_at_cursor_packed_ecols`.
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);

        if let Some(row) = self.storage.row_mut(cursor_row) {
            if !row.write_wide_char_packed(cursor_col, c, colors, flags) {
                return false;
            }
        } else {
            return false;
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        // Row-level damage, as the ASCII bulk path: cheaper than per-cell min/max merges for
        // sequential wide writes.
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

    /// `write_wide_autowrap_fast` for a non-BMP wide char, also storing it in the complex-char
    /// ring.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode, COMPLEX flag set in flags
    /// REQUIRES: `self.storage.visible_rows > 0`
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

    /// Resolve-wrap + write + damage + advance for a width-1 char: the pair
    /// `write_char_at_cursor_packed` and `advance_cursor_wrap`, with row-level damage amortized
    /// over bulk runs.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode
    /// REQUIRES: `self.storage.visible_rows > 0`
    #[inline]
    pub fn write_narrow_autowrap_fast(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        // Resolve a pending wrap first, or a width-1 char after a wide char at end of line
        // overwrites its spacer half.
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        // Drop a stale extras-map entry before overwriting (and the entry of a
        // wide half the overwrite orphans).
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 1);

        if let Some(row) = self.storage.row_mut(cursor_row) {
            row.write_char_packed(cursor_col, c, colors, flags);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        self.storage.mark_content_row(cursor_row);

        // Fast advance unless DECDWL rows or DECLRMM margins are in play.
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
    ///
    /// All chars must be BMP (< U+10000) and width-2 (CJK, Hangul, etc.).
    /// Computes the ring-buffer index once per row instead of per character.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode
    /// REQUIRES: `self.storage.visible_rows > 0`
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
        // Hoisted per run: no per-cell cost when the extras map is empty.
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
                // Blank the skipped last-column tail as the single-char path does, so bulk
                // and per-char input fold to the same grid.
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
                // Too narrow for a wide char (1 column): skip it, or the loop never ends.
                pos += 1;
                continue;
            }

            let mut stale = StaleExtras::NONE;
            if let Some(row) = self.storage.row_mut(cursor_row) {
                // Capture stale-extras columns (both wide halves) before the
                // overwrite clobbers the HAS_EXTRAS flags.
                let end_col_run = cursor_col + (to_write as u16) * 2;
                if scan_stale {
                    stale = StaleExtras::probe(row, cursor_col, end_col_run);
                }
                // Bounds-check once for the run, then write unchecked.
                if (end_col_run as usize) <= row.cells_len() {
                    // The unchecked writes skip orphaned-wide-half cleanup, and only the run's
                    // edges can orphan a half: the first cell on the continuation of an
                    // existing wide char (cursor put there by CUP), and the cell past the run
                    // being the continuation of a wide base at `end_col_run - 1`.
                    let end_half_blanked =
                        row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for i in 0..to_write {
                        let col = cursor_col + (i as u16) * 2;
                        // SAFETY: (a) `col + 1 < end_col_run <= cells_len()`, checked above;
                        // (b) `col = cursor_col + 2i` is a wide-cell boundary; (c) the writes are
                        // sequential and disjoint, each covering `(col, col + 1)`.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, chars[pos + i], colors, flags);
                        }
                    }
                    // Not `update_len`, which only grows: the edge fixup can clear the row's
                    // last content, and a stale-high `len` adds a phantom trailing space to
                    // `row_text`, search and scrollback.
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

    /// `write_wide_run_autowrap` for non-BMP width-2 characters (emoji), also storing the
    /// codepoints in the complex-char ring (`set_complex_char_wide_run`). The comments there
    /// cover both.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode, COMPLEX flag set in flags
    /// REQUIRES: `self.storage.visible_rows > 0`
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
                        // SAFETY: (a) `col + 1 < end_col <= cells_len()`, checked above;
                        // (b) `col = cursor_col + 2j` is a wide-cell boundary; (c) the writes are
                        // sequential and disjoint, each covering `(col, col + 1)`.
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

    /// `write_wide_run_autowrap` for a mix of BMP and non-BMP width-2 characters; only the
    /// non-BMP chars get ring entries.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode
    /// REQUIRES: `self.storage.visible_rows > 0`
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
                        // SAFETY: (a) `col + 1 < end_col <= cells_len()`, checked above;
                        // (b) `col = cursor_col + 2j` is a wide-cell boundary; (c) the writes are
                        // sequential and disjoint, each covering `(col, col + 1)`.
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

            // Ring entries for the non-BMP chars only, batched per row.
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

    /// Write a wide character with autowrap using a style ID. Test-only; production uses
    /// `pre_wrap_wide_ecols` + `write_wide_char_at_cursor_packed_ecols` +
    /// `advance_cursor_wide_wrap_ecols`.
    #[cfg(test)]
    pub(crate) fn write_wide_char_wrap_with_style_id(
        &mut self,
        c: char,
        style_id: StyleId,
        extra_flags: CellFlags,
    ) -> bool {
        let (fg, bg, flags) = self.resolve_style_to_colors(style_id, extra_flags);
        self.write_wide_char_wrap_styled(c, fg, bg, flags)
    }

    /// Write a wide character without autowrap using a style ID. Test-only; production uses
    /// `write_wide_char_at_cursor_packed_ecols` + `advance_cursor_wide_no_wrap_ecols`.
    #[cfg(test)]
    pub fn write_wide_char_with_style_id(
        &mut self,
        c: char,
        style_id: StyleId,
        extra_flags: CellFlags,
    ) -> bool {
        let (fg, bg, flags) = self.resolve_style_to_colors(style_id, extra_flags);
        self.write_wide_char_styled(c, fg, bg, flags)
    }

    /// Write styled ASCII bytes whose style needs `CellExtras` (truecolor RGB, hyperlinks,
    /// underline colours, extended flags): the cells in bulk as `write_ascii_run_styled` does,
    /// then the extras over the written range via `set_range_uniform`, once per row.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, insert mode OFF, auto-wrap ON
    /// REQUIRES: the cursor is not outside a DECSLRM span
    /// (`Grid::cursor_outside_margin_span`): the right-margin clamp is derived
    /// once per row from the column the row's batch starts at
    /// ENSURES: result <= ascii.len(), self.storage.cursor.row < self.storage.visible_rows
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
        // The stale sweep keeps Phase 2's `set_range_uniform` from merging the new extras into
        // a stale entry (an old hyperlink on new text); skipped when the extras map is empty.
        let scan_stale = !self.storage.extras.is_empty();

        while !remaining.is_empty() {
            self.resolve_pending_wrap();

            let cursor_row = self.storage.cursor.row;
            let cursor_col = self.storage.cursor.col;

            // Pre-compute margin clamp before mutable borrow.
            let margin_clamp = if self.storage.has_horizontal_margins {
                let margins = self.storage.horizontal_margins();
                if !margins.is_full(self.storage.cols)
                    && cursor_col >= margins.left
                    && cursor_col <= margins.right
                {
                    Some(margins.right.saturating_add(1))
                } else {
                    None
                }
            } else {
                None
            };

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

            // Capture stale-extras columns before Phase 1 clobbers the
            // HAS_EXTRAS flags; remove them before Phase 2 creates fresh entries
            // so old extras never merge into the new cells.
            let stale = if scan_stale {
                StaleExtras::probe(row, cursor_col, cursor_col + to_write_u16)
            } else {
                StaleExtras::NONE
            };

            // Phase 1: the styled cells, in bulk.
            if let Some(target) = row.cells_mut_with_fixup(cursor_col, to_write_u16) {
                debug_assert_eq!(target.len(), to_write);
                // Set HAS_EXTRAS on each cell since extras will be applied in Phase 2.
                let colors_with_extras = colors.with_extras_flag();
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_styled(byte, colors_with_extras, flags);
                }
            }

            row.update_len(cursor_col + to_write_u16);

            self.remove_stale_extras(cursor_row, stale);

            // Phase 2: the extras over the written range, batched.
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

    /// A wrap-tail blank that bisects the wide pair to its left gives the orphaned head and the
    /// tail the BCE blank and its ring background, not a default colour or the dead glyph's
    /// bytes. Three rows, so the wrap does not scroll.
    #[test]
    fn wide_wrap_tail_paints_the_bisected_orphan_with_the_bce_blank_and_its_ring() {
        let (rows, cols) = (3u16, 10u16);
        let mut grid = Grid::new(rows, cols);

        // SGR 48;2;10;20;30, then a wide glyph at cols 8-9.
        let old_bg = [10u8, 20, 30];
        let old_colors = PackedColors::new().with_rgb_bg();
        grid.set_cursor_template(Cell::bce_blank(old_colors), Some(old_bg));
        grid.move_cursor_to(0, 8);
        assert!(grid.write_wide_autowrap_fast('\u{4E2D}', old_colors, CellFlags::empty()));
        // The emulator's write path stores truecolor bytes in the dense ring;
        // this grid-level fast path leaves that to its caller, so seed it here.
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

        // SGR 48;2;200;100;50, cursor to the last column, another wide glyph.
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
