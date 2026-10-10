// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Split write/advance primitives for the terminal handler.
//!
//! These separate "write at cursor" from "advance cursor", so the handler can
//! apply cell extras (hyperlinks, non-BMP overflow, underline colors) at the
//! written position before the cursor advances and possibly scrolls the row
//! away. All write damage goes through `mark_content_*`, so Unicode and styled
//! writes invalidate content-only readers just as the ASCII bulk path does.
//!
//! Also contains the bulk extras write path (`write_ascii_run_with_extras`)
//! for ASCII runs with RGB/hyperlink styles, avoiding per-character fallback.

use std::sync::Arc;

#[cfg(test)]
use super::StyleId;
use super::write::stale_extras_cols;
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
        // Drop a stale extras-map entry before overwriting, so a later
        // `cell_extra_mut_preflagged` here cannot resurrect old data (an old
        // hyperlink) via `get_or_create`.
        let scan_stale = !self.storage.extras.is_empty();
        let mut stale = (false, false);
        if let Some(row) = self.row_mut(cursor_row) {
            stale.0 = scan_stale && row.get(cursor_col).is_some_and(Cell::has_extras);
            row.write_char_packed(cursor_col, c, colors, flags);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
        // Drop a stale dense-RGB-ring entry before overwriting. This slow path
        // puts the new cell's RGB in the HashMap
        // (`apply_cell_extras_preflagged`), but reads consult the ring first
        // (it is filled by the bulk and BCE/erase paths), so a leftover ring
        // color would shadow the new one.
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

    /// Write a wide character at cursor with pre-computed effective column count.
    ///
    /// Avoids redundant `effective_cols_for_row` ring-buffer lookup when the
    /// caller has already computed it (e.g., during the wide char write pipeline).
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
            // Drop stale dense-RGB-ring entries for both wide halves before the
            // overwrite shows through. See `write_char_at_cursor_packed` for why
            // the slow path must clear this second truecolor store.
            self.storage
                .extras
                .clear_rgb_ring_cell(cursor_row, cursor_col, 2);
            self.storage.mark_content_wide_cell(cursor_row, cursor_col);
            return true;
        }
        false
    }

    /// Pre-wrap for wide character with pre-computed effective column count.
    ///
    /// Skips a redundant `resolve_pending_wrap` (the caller must have already
    /// resolved it) and reuses `effective_cols` to avoid a second ring-buffer
    /// lookup.
    ///
    /// Returns the effective_cols for the (possibly new) cursor row.
    ///
    /// REQUIRES: pending wrap already resolved
    /// REQUIRES: self.storage.visible_rows > 0
    #[inline]
    pub fn pre_wrap_wide_ecols(&mut self, effective_cols: u16, fill: Cell) -> u16 {
        if self.storage.cursor.col.saturating_add(1) >= effective_cols {
            // Wide glyph won't fit at the last column: blank the skipped tail
            // with the current BCE background before wrapping (matches xterm;
            // see blank_wide_wrap_tail).
            self.blank_wide_wrap_tail(effective_cols, fill);
            self.advance_autowrap_line();
            // Row changed — recompute effective_cols for new row
            self.storage.effective_cols_for_row(self.storage.cursor.row)
        } else {
            effective_cols
        }
    }

    /// Blank the tail cell(s) skipped when a wide glyph wraps off the last column.
    ///
    /// A width-2 glyph that won't fit at the right edge wraps to the next line;
    /// xterm fills the skipped cell(s) of the current row with BCE blanks (the
    /// glyph's background) instead of leaving the old content stranded. Call with
    /// the cursor still on the old row, before `advance_autowrap_line()`.
    #[inline]
    pub(super) fn blank_wide_wrap_tail(&mut self, ecols: u16, fill: Cell) {
        let row = self.storage.cursor.row;
        let col = self.storage.cursor.col;
        // `None` only if the row does not exist; then nothing was blanked and
        // there is no span to give the ring either.
        let mut bce: Option<core::ops::Range<u16>> = None;
        if let Some(r) = self.storage.row_mut(row) {
            // A wide half bisected by this blanking takes the BCE blank, not a
            // default-coloured one. In xterm a write bisecting a wide pair goes
            // `util.c` `WriteText` -> `DamagedCurCells` -> `ClearInLine` ->
            // `ClearInLine2` -> `screen.c` `ClearCells`, which widens its run
            // over the adjacent `HIDDEN_CHAR` halves and writes `' '` with
            // `ld->color[col] = xtermColorPair(xw)`, the current colour pair
            // (`fill` here). EL/ED share this rule; only the rectangle ops use
            // `Clear1Cell` instead. See `Row::clear_range_with_orphan`.
            bce = Some(r.clear_range_with_orphan(col, ecols, fill, fill));
            // Record the hole the wrap leaves, so a rewrap drops this cell and
            // only it (`RowFlags::EARLY_WRAP_HOLE`). Only a hole that is the
            // row's physical last cell: under DECLRMM or on a DECDWL row the
            // wrap edge is not the row end, and no rewrap merges such a row.
            let cols = r.cols();
            r.set_early_wrap_hole(ecols == cols && col.saturating_add(1) == cols);
        }
        // A truecolor BCE cell carries only an RGB marker; the bytes live in
        // `RgbColorRing`, so the blanked tail and the orphan column need ring
        // entries, or they resolve the dead glyph's bytes. `fill_bce_rgb_range`
        // takes the bytes from the cursor template, set from the same SGR state
        // as these `colors` (`handler_sgr`); the `bg_is_rgb` guard keeps a
        // caller that desynchronizes the two from seeding the ring behind an
        // indexed marker.
        if let Some(bce) = bce
            && fill.colors().bg_is_rgb()
        {
            self.fill_bce_rgb_range(row, bce.start, bce.end);
        }
        self.storage.mark_content_row(row);
    }

    /// Advance cursor by 1 column without wrapping.
    ///
    /// Stops at the last column of the current row. A write at the last
    /// column still arms `pending_wrap`: xterm sets `do_wrap = need_wrap`
    /// whenever a print fills to the margin regardless of WRAPAROUND
    /// (charproc.c dotext), so re-enabling DECAWM lets the next printable
    /// wrap. Fast path: uses `cols - 1` directly to avoid a row lookup.
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

    /// Advance cursor by 1 column with autowrap (deferred).
    ///
    /// At the last column, sets the `pending_wrap` flag instead of wrapping
    /// immediately. The actual wrap happens when the next character is written.
    /// This matches xterm/VT220 deferred wrap behavior.
    ///
    /// Fast path: uses `cols - 1` directly (avoids ring-buffer row lookup for
    /// double-width check and margin check). Falls back to the slow path when
    /// cursor is at or beyond `cols - 1`, when DECDWL is active, or when
    /// DECLRMM horizontal margins are active.
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
        let max_col = if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            if !margins.is_full(self.storage.cols)
                && self.storage.cursor.col >= margins.left
                && self.storage.cursor.col <= margins.right
            {
                margins.right
            } else {
                self.storage.max_col_for_row(self.storage.cursor.row)
            }
        } else {
            self.storage.max_col_for_row(self.storage.cursor.row)
        };

        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        } else {
            self.storage.mark_pending_wrap();
        }
    }

    /// Get effective columns clamped by DECLRMM right margin.
    #[inline]
    fn margin_clamped_ecols(&self, row: u16) -> u16 {
        let ecols = self.storage.effective_cols_for_row(row);
        if self.storage.has_horizontal_margins {
            let margins = self.storage.horizontal_margins();
            if !margins.is_full(self.storage.cols)
                && self.storage.cursor.col >= margins.left
                && self.storage.cursor.col <= margins.right
            {
                ecols.min(margins.right.saturating_add(1))
            } else {
                ecols
            }
        } else {
            ecols
        }
    }

    /// Advance cursor by 2 columns (for wide char) without wrapping,
    /// using pre-computed effective column count.
    ///
    /// A wide char filling to the margin arms `pending_wrap` even with autowrap
    /// off (xterm's `do_wrap = need_wrap` is mode-independent); see
    /// `advance_cursor_no_wrap`.
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
                // Wide char reached or exceeded end of line — defer wrap
                self.storage.cursor.col = wrap_cols.saturating_sub(1);
                self.storage.mark_pending_wrap();
            }
        }
    }

    /// Combined pre-wrap + write + damage + advance for BMP wide chars.
    ///
    /// One ring-buffer lookup and no redundant bounds checks, replacing four
    /// Grid calls on the per-char wide write path. Returns `true` if written.
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
        // Resolve pending wrap
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }

        // Effective cols clamped by DECLRMM margins.
        let ecols = self.margin_clamped_ecols(self.storage.cursor.row);

        // Pre-wrap wide: ensure room for 2 cells
        let ecols = if self.storage.cursor.col + 1 >= ecols {
            // The glyph won't fit at the last column (and no wrap was pending,
            // which take_pending_wrap would have consumed). As xterm does, blank
            // the skipped cell with the current BCE background before wrapping
            // the glyph, rather than stranding old content (e.g. ABCD<CUP last
            // col>中).
            self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
            self.advance_autowrap_line();
            self.margin_clamped_ecols(self.storage.cursor.row)
        } else {
            ecols
        };

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        // Drop stale extras-map entries for both halves, only once the write
        // succeeds.
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);

        // Single row_mut call — no separate bounds check (pre_wrap guarantees room)
        if let Some(row) = self.storage.row_mut(cursor_row) {
            if !row.write_wide_char_packed(cursor_col, c, colors, flags) {
                return false;
            }
        } else {
            return false;
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        // Row-level damage — same strategy as ASCII bulk path. Avoids per-char
        // min/max merge overhead of mark_wide_cell for sequential wide writes.
        self.storage.mark_content_row(cursor_row);

        // Advance cursor with pending wrap at end of line
        let new_col = cursor_col + 2;
        if new_col >= ecols {
            self.storage.cursor.col = ecols.saturating_sub(1);
            self.storage.mark_pending_wrap();
        } else {
            self.storage.cursor.col = new_col;
        }
        true
    }

    /// Combined pre-wrap + write + ring-buffer-set + damage + advance for
    /// non-BMP wide chars.
    ///
    /// Like `write_wide_autowrap_fast` but also stores the complex char in the
    /// dense ring buffer, replacing several Grid calls on the per-emoji path.
    /// The COMPLEX flag must already be set in `flags`.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode, COMPLEX flag set in flags
    /// REQUIRES: `self.storage.visible_rows > 0`
    #[inline]
    pub fn write_emoji_autowrap_fast(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        // Resolve pending wrap
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }

        // Effective cols clamped by DECLRMM margins.
        let ecols = self.margin_clamped_ecols(self.storage.cursor.row);

        // Pre-wrap wide: ensure room for 2 cells
        let ecols = if self.storage.cursor.col + 1 >= ecols {
            // Blank the skipped last cell before wrapping the glyph down (xterm;
            // see blank_wide_wrap_tail).
            self.blank_wide_wrap_tail(ecols, Cell::bce_blank(colors));
            self.advance_autowrap_line();
            self.margin_clamped_ecols(self.storage.cursor.row)
        } else {
            ecols
        };

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        // Drop stale extras-map entries for both halves, only once the write
        // succeeds.
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 2);

        // Write the wide char
        if let Some(row) = self.storage.row_mut(cursor_row) {
            if !row.write_wide_char_packed(cursor_col, c, colors, flags) {
                return;
            }
        } else {
            return;
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        // Row-level damage — same strategy as ASCII bulk path.
        self.storage.mark_content_row(cursor_row);

        // Store complex char codepoint in ring buffer (O(1) flat array, no Arc)
        let visible_rows = self.storage.visible_rows;
        let cols = self.storage.cols;
        self.storage.extras_mut().set_complex_char_ring(
            cursor_row,
            cursor_col,
            c,
            visible_rows,
            cols,
        );

        // Advance cursor with pending wrap at end of line
        let new_col = cursor_col + 2;
        if new_col >= ecols {
            self.storage.cursor.col = ecols.saturating_sub(1);
            self.storage.mark_pending_wrap();
        } else {
            self.storage.cursor.col = new_col;
        }
    }

    /// Combined resolve-wrap + write + damage + advance for width-1 chars.
    ///
    /// Replaces `write_char_at_cursor_packed` + `advance_cursor_wrap` and,
    /// unlike that pair, resolves a pending wrap first (otherwise a char after
    /// a wide char at the line boundary would overwrite).
    ///
    /// Uses row-level damage (like `write_wide_autowrap_fast`) instead of
    /// cell-level damage, amortizing min/max overhead across bulk runs.
    ///
    /// REQUIRES: auto_wrap enabled, no insert mode
    /// REQUIRES: `self.storage.visible_rows > 0`
    #[inline]
    pub fn write_narrow_autowrap_fast(&mut self, c: char, colors: PackedColors, flags: CellFlags) {
        // Resolve pending wrap (critical: without this, a width-1 char after a
        // wide char at end-of-line overwrites the spacer half)
        if self.storage.take_pending_wrap() {
            self.advance_autowrap_line();
        }

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;

        // Drop a stale extras-map entry before overwriting.
        let scan_stale = !self.storage.extras.is_empty();
        let mut stale = (false, false);

        // Write character — single row_mut call
        if let Some(row) = self.storage.row_mut(cursor_row) {
            stale.0 = scan_stale && row.get(cursor_col).is_some_and(Cell::has_extras);
            row.write_char_packed(cursor_col, c, colors, flags);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);

        // Row-level damage — amortized for bulk runs
        self.storage.mark_content_row(cursor_row);

        // Advance cursor with pending wrap at end of line.
        // Guard: double-width rows, DECLRMM margins.
        let fast_max = self.storage.cols.saturating_sub(1);
        if cursor_col < fast_max
            && !self.storage.any_double_width
            && !self.storage.has_horizontal_margins
        {
            self.storage.cursor.col = cursor_col + 1;
        } else {
            let max_col = if self.storage.has_horizontal_margins {
                let margins = self.storage.horizontal_margins();
                if !margins.is_full(self.storage.cols)
                    && cursor_col >= margins.left
                    && cursor_col <= margins.right
                {
                    margins.right
                } else {
                    self.storage.max_col_for_row(cursor_row)
                }
            } else {
                self.storage.max_col_for_row(cursor_row)
            };
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
            // Resolve pending wrap
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

            // Pre-wrap: ensure room for at least one wide char
            let (ecols, cursor_row, cursor_col) = if cursor_col + 1 >= ecols {
                // Match the single-char path: blank the skipped last-column tail
                // with a BCE blank before wrapping, so coalesced (bulk) and
                // fragmented (per-char) input fold to the identical grid.
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

            // How many wide chars fit on this row?
            let available_cols = ecols.saturating_sub(cursor_col);
            let max_chars = (available_cols / 2) as usize;
            let to_write = (chars.len() - pos).min(max_chars);
            if to_write == 0 {
                // Terminal too narrow for wide chars (e.g. 1 column).
                // Skip the unfittable character to avoid infinite loop.
                pos += 1;
                continue;
            }

            // Single row_mut call for all chars on this line
            let mut stale = Vec::new();
            if let Some(row) = self.storage.row_mut(cursor_row) {
                // Capture stale-extras columns (both wide halves) before the
                // overwrite clobbers the HAS_EXTRAS flags.
                let end_col_run = cursor_col + (to_write as u16) * 2;
                if scan_stale {
                    stale = stale_extras_cols(row, cursor_col, end_col_run);
                }
                // Bounds-check once for the whole run, then write unchecked (as
                // `write_emoji_run_autowrap` does): the run already settled the
                // wrap, row, margin clamp and damage mark, so per-glyph checks
                // would repeat the same answer.
                if (end_col_run as usize) <= row.cells_len() {
                    // The unchecked writes skip orphaned-wide-half cleanup, and a
                    // run has two places one can occur: the first cell landing on
                    // the continuation half of an existing wide char (cursor put
                    // there by CUP), and the cell just past the run being the
                    // continuation of a wide base at `end_col_run - 1`. Interior
                    // glyphs land only on cells this run wrote. Clean both edges.
                    row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for i in 0..to_write {
                        let col = cursor_col + (i as u16) * 2;
                        // SAFETY: (a) `col + 1 < cells.len()` because
                        // `end_col_run = cursor_col + to_write*2 <= cells_len()`
                        // and `col + 2 <= end_col_run`, so
                        // `col + 1 < end_col_run <= cells.len()`. (b) `col` is on
                        // a wide-cell boundary: writes land at `cursor_col + 0,
                        // 2, 4, ...`. (c) writes are sequential and
                        // non-overlapping: `i` strictly increases and each
                        // iteration covers exactly the two cells `(col, col+1)`.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, chars[pos + i], colors, flags);
                        }
                    }
                    // Not `update_len`, which only grows: the edge fixup above can
                    // clear the row's last content, and a stale-high `len` would
                    // add a phantom trailing space to `row_text`, search and
                    // scrollback
                    // (`wide_run_autowrap_tightens_len_when_the_run_orphans_the_row_tail`).
                    row.settle_len_after_wide_run(end_col_run);
                    row.mark_has_wide_chars();
                } else {
                    for i in 0..to_write {
                        let col = cursor_col + (i as u16) * 2;
                        row.write_wide_char_packed(col, chars[pos + i], colors, flags);
                    }
                }
            }
            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Row-level damage — once per row, not per char
            self.storage.mark_content_row(cursor_row);

            pos += to_write;

            // Advance cursor
            let end_col = cursor_col + (to_write as u16) * 2;
            if end_col >= ecols {
                self.storage.cursor.col = ecols.saturating_sub(1);
                self.storage.mark_pending_wrap();
            } else {
                self.storage.cursor.col = end_col;
            }
        }
    }

    /// Write a run of non-BMP width-2 characters (emoji) with autowrap, batched.
    ///
    /// Like `write_wide_run_autowrap` but also stores each char in the complex
    /// char ring buffer. Codepoints go into the ring via
    /// `set_complex_char_wide_run`, batched per row. Computes row_mut +
    /// effective_cols once per row.
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
        // Hoisted per run: no per-cell cost when the extras map is empty.
        let scan_stale = !self.storage.extras.is_empty();

        while pos < chars.len() {
            // Resolve pending wrap
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

            // Pre-wrap: ensure room for at least one wide char
            let (ecols, cursor_row, cursor_col) = if cursor_col + 1 >= ecols {
                // Match the single-char path: blank the skipped last-column tail
                // with a BCE blank before wrapping, so coalesced (bulk) and
                // fragmented (per-char) input fold to the identical grid.
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

            // How many wide chars fit on this row?
            let available_cols = ecols.saturating_sub(cursor_col);
            let max_chars = (available_cols / 2) as usize;
            let to_write = (chars.len() - pos).min(max_chars);
            if to_write == 0 {
                // Terminal too narrow for wide chars (e.g. 1 column).
                // Skip the unfittable character to avoid infinite loop.
                pos += 1;
                continue;
            }

            let end_col = cursor_col + (to_write as u16) * 2;

            // Single row_mut call for all chars on this line
            let mut stale = Vec::new();
            if let Some(row) = self.storage.row_mut(cursor_row) {
                // Capture stale-extras columns (both wide halves) before the
                // overwrite clobbers the HAS_EXTRAS flags.
                if scan_stale {
                    stale = stale_extras_cols(row, cursor_col, end_col);
                }
                // Bounds check once for the entire run, then use no-fixup writes.
                // Sequential emoji writes at even columns can't conflict.
                if (end_col as usize) <= row.cells_len() {
                    // The unchecked writes below skip orphaned-wide-half cleanup,
                    // but the run's first cell can land on the continuation half
                    // of an existing wide char (cursor put there via CUP), and the
                    // cell just past the run can be the continuation of a wide base
                    // at end_col-1. Clean both edges once (a no-op on rows without
                    // wide chars).
                    row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for j in 0..to_write {
                        let col = cursor_col + (j as u16) * 2;
                        // SAFETY: (a) `col + 1 < cells.len()` because
                        // `end_col = cursor_col + to_write*2 <= cells_len()` and
                        // `col + 2 <= end_col`, so `col + 1 < end_col <= cells.len()`.
                        // (b) `col` is on a wide-cell boundary: writes land at
                        // `cursor_col + 0, 2, 4, ...`. (c) writes are sequential and
                        // non-overlapping: `j` strictly increases and each iteration
                        // covers exactly `(col, col+1)`; the run starts at
                        // `cursor_col` on a freshly bounds-checked row.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, chars[pos + j], colors, flags);
                        }
                    }
                    // Not `update_len`, which only grows: the edge fixup above can
                    // clear the row's last content, and a stale-high `len` would
                    // add a phantom trailing space to `row_text`, search and
                    // scrollback.
                    row.settle_len_after_wide_run(end_col);
                    row.mark_has_wide_chars();
                } else {
                    for j in 0..to_write {
                        let col = cursor_col + (j as u16) * 2;
                        row.write_wide_char_packed(col, chars[pos + j], colors, flags);
                    }
                }
            }
            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Store complex char codepoints in ring buffer — batched, ring_row hoisted
            self.storage.extras_mut().set_complex_char_wide_run(
                cursor_row,
                cursor_col,
                &chars[pos..pos + to_write],
                visible_rows,
                cols,
            );

            // Row-level damage — once per row
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

    /// Write a mixed run of BMP and non-BMP width-2 characters with autowrap.
    ///
    /// One batched write for interleaved BMP (CJK, BMP emoji) and non-BMP
    /// (SMP emoji) wide chars (e.g. ✨🚀⚡🎉); only non-BMP chars get ring
    /// buffer entries.
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
                // Match the single-char path: blank the skipped last-column tail
                // before wrapping, so bulk and per-char folds are identical.
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

            let mut stale = Vec::new();
            if let Some(row) = self.storage.row_mut(cursor_row) {
                // Capture stale-extras columns (both wide halves) before the
                // overwrite clobbers the HAS_EXTRAS flags.
                if scan_stale {
                    stale = stale_extras_cols(row, cursor_col, end_col);
                }
                if (end_col as usize) <= row.cells_len() {
                    // As in write_emoji_run_autowrap: clean any orphaned wide half
                    // at the run's leading edge (cursor on a continuation) or just
                    // past its trailing edge before the unchecked writes (a no-op
                    // on rows without wide chars).
                    row.fixup_wide_chars_in_range(cursor_col, (to_write as u16) * 2);
                    for (j, &c) in slice.iter().enumerate() {
                        let col = cursor_col + (j as u16) * 2;
                        let fl = if (c as u32) > 0xFFFF {
                            nonbmp_flags
                        } else {
                            bmp_flags
                        };
                        // SAFETY: (a) `col + 1 < cells.len()` because
                        // `end_col = cursor_col + to_write*2 <= cells_len()` and
                        // `col + 2 <= end_col`, so `col + 1 < end_col <= cells.len()`.
                        // (b) `col` is on a wide-cell boundary: writes land at
                        // `cursor_col + 0, 2, 4, ...`. (c) writes are sequential and
                        // non-overlapping: `j` strictly increases and each iteration
                        // covers exactly `(col, col+1)`; the run starts at
                        // `cursor_col` on a freshly bounds-checked row.
                        unsafe {
                            row.write_wide_char_packed_no_fixup(col, c, bmp_colors, fl);
                        }
                    }
                    // Not `update_len`, which only grows: the edge fixup above can
                    // clear the row's last content, and a stale-high `len` would
                    // add a phantom trailing space to `row_text`, search and
                    // scrollback.
                    row.settle_len_after_wide_run(end_col);
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
            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Ring buffer writes — batched with hoisted ring_row for non-BMP chars
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

    /// Write a wide character with autowrap using a style ID.
    ///
    /// Test-only: delegates to the combined write+advance method in write.rs
    /// which is behind `#[cfg(test)]`. Production code uses the split primitives
    /// (`pre_wrap_wide_ecols` + `write_wide_char_at_cursor_packed_ecols` +
    /// `advance_cursor_wide_wrap_ecols`).
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

    /// Write a wide character without autowrap using a style ID.
    ///
    /// Test-only: delegates to the combined write+advance method in write.rs
    /// which is behind `#[cfg(test)]`. Production code uses the split primitives
    /// (`write_wide_char_at_cursor_packed_ecols` + `advance_cursor_wide_no_wrap_ecols`).
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

    /// Fast path: write styled ASCII bytes with extras (RGB, hyperlinks, etc.).
    ///
    /// For ASCII runs whose style needs `CellExtras` overflow (true-color RGB,
    /// hyperlinks, underline colors, extended flags). Writes cells in bulk like
    /// `write_ascii_run_styled`, then applies extras to the written range via
    /// `set_range_uniform`, amortizing style resolution, cursor arithmetic and
    /// damage marking per line instead of per character.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, insert mode OFF, auto-wrap ON
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
        // Hoisted per run (no per-cell cost when the extras map is empty).
        // Without the stale sweep, Phase 2's `set_range_uniform`/`get_or_create`
        // would merge the new extras into a stale entry (e.g. attach an old
        // hyperlink to new text).
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

            // Combined row access + effective cols: single ring-buffer lookup
            let Some((row, effective_cols)) = self.storage.row_mut_with_effective_cols(cursor_row)
            else {
                break;
            };

            // Clamp effective cols by right margin when DECLRMM is active.
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
                stale_extras_cols(row, cursor_col, cursor_col + to_write_u16)
            } else {
                Vec::new()
            };

            // Phase 1: Write styled cells directly to row (bulk)
            if let Some(target) = row.cells_mut_with_fixup(cursor_col, to_write_u16) {
                debug_assert_eq!(target.len(), to_write);
                // Set HAS_EXTRAS on each cell since extras will be applied in Phase 2.
                let colors_with_extras = colors.with_extras_flag();
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_styled(byte, colors_with_extras, flags);
                }
            }

            row.update_len(cursor_col + to_write_u16);
            // row borrow drops here

            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Phase 2: Apply extras to written range (batched)
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

    /// The wrap-tail blanking is a BCE erase that can bisect the wide pair to
    /// its left, and the orphaned head must get the BCE blank, resolving the
    /// BCE truecolor background.
    ///
    /// The shape a real terminal produces (`Terminal::new(3, 10)`):
    ///
    /// ```text
    ///   ESC[48;2;10;20;30m  ESC[1;9H   <wide glyph>   (cols 8-9)
    ///   ESC[48;2;200;100;50m ESC[1;10H <wide glyph>   (won't fit -> wraps)
    /// ```
    ///
    /// The second glyph cannot fit at the last column, so the tail `[9, 10)` is
    /// blanked with the new background and the glyph wraps to row 1, bisecting
    /// the first glyph and orphaning its head at col 8. Both col 8 and col 9
    /// must resolve the new background from the ring, not a default colour or
    /// the dead glyph's `[10, 20, 30]`.
    ///
    /// Several rows, since a single-row grid would scroll the evidence away.
    #[test]
    fn wide_wrap_tail_paints_the_bisected_orphan_with_the_bce_blank_and_its_ring() {
        let (rows, cols) = (3u16, 10u16);
        let mut grid = Grid::new(rows, cols);

        // --- SGR 48;2;10;20;30 then a wide glyph at cols 8-9.
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

        // --- SGR 48;2;200;100;50, cursor to the LAST column, another wide glyph.
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
