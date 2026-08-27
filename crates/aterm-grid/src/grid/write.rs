// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Character writing operations for the terminal grid.
//!
//! This module provides methods for writing characters to the grid with
//! various styling and wrapping options. Includes:
//! - Basic character writes (`write_char`)
//! - Autowrap writes (`write_char_wrap`)
//! - Fast ASCII path (`write_ascii_blast`, `write_ascii_run_styled`)
//! - Wide character handling (via write_split.rs in production)
//! - Style-ID resolution (`resolve_style_to_colors`)
//!
//! Test-only convenience methods (`write_char_styled`, `write_char_with_style_id`,
//! etc.) are in the `#[cfg(test)]` block below. Production code uses the split
//! write/advance primitives in `write_split.rs`.

use super::{
    Cell, CellCoord, CellFlags, ColorType, ExtendedStyle, Grid, PackedColor, PackedColors, StyleId,
};

/// Collect columns in `[start, end)` whose current cell carries the
/// HAS_EXTRAS flag — i.e. cells whose extras-map entry must be removed when
/// a write overwrites them (#7456). Returns an empty `Vec` (no allocation)
/// when none do. Callers must only invoke this when the extras map is
/// non-empty (`!extras.is_empty()`), keeping plain writes branch-only.
#[cold]
#[inline(never)]
pub(super) fn stale_extras_cols(row: &crate::Row, start: u16, end: u16) -> Vec<u16> {
    let mut stale = Vec::new();
    for col in start..end {
        if row.get(col).is_some_and(Cell::has_extras) {
            stale.push(col);
        }
    }
    stale
}

impl Grid {
    /// Remove the extras-map entries captured by [`stale_extras_cols`] for a
    /// row that has just been overwritten (#7456).
    ///
    /// Overwriting a cell that carried extras (hyperlink, combining marks,
    /// underline color, RGB overflow) previously left its HashMap entry
    /// behind: invisible to readers (they gate on the cell's HAS_EXTRAS
    /// flag) but leaking memory until the row scrolled off or was erased —
    /// and resurfacing when a later styled write's `get_or_create` landed on
    /// the same coordinate and merged stale data (e.g. an old hyperlink)
    /// into the new cell.
    #[cold]
    #[inline(never)]
    pub(super) fn remove_stale_extras(&mut self, row: u16, cols: Vec<u16>) {
        for col in cols {
            self.storage.extras.remove(CellCoord::new(row, col));
        }
    }

    /// Probe the old cell pair at `(row, col[, col+1])` for the HAS_EXTRAS
    /// flag before a 1- or 2-cell overwrite (#7456). Returns which of the
    /// two columns carry a (potentially stale) extras-map entry.
    ///
    /// Cost model: one map-emptiness branch when the extras map is empty
    /// (the common plain-text case — pays nothing else); otherwise one
    /// cell-flag read per overwritten column, and a hash removal (via
    /// [`Self::remove_stale_extras_pair`]) only for columns actually flagged.
    #[inline]
    pub(crate) fn stale_extras_pair(&self, row: u16, col: u16, width: u16) -> (bool, bool) {
        if self.storage.extras.is_empty() {
            return (false, false);
        }
        self.stale_extras_pair_cold(row, col, width)
    }

    /// Out-of-line probe for [`Self::stale_extras_pair`] — only reached when
    /// the extras map is non-empty, keeping the hot write bodies lean.
    #[cold]
    #[inline(never)]
    fn stale_extras_pair_cold(&self, row: u16, col: u16, width: u16) -> (bool, bool) {
        let r = self.row(row);
        let probe = |c: u16| r.and_then(|r| r.get(c)).is_some_and(Cell::has_extras);
        (probe(col), width == 2 && probe(col.saturating_add(1)))
    }

    /// Remove the extras-map entries flagged by [`Self::stale_extras_pair`]
    /// after the overwrite landed (#7456). Call only when the write
    /// actually happened — a failed write must not drop live extras.
    #[inline]
    pub(crate) fn remove_stale_extras_pair(&mut self, row: u16, col: u16, stale: (bool, bool)) {
        if stale.0 || stale.1 {
            self.remove_stale_extras_pair_cold(row, col, stale);
        }
    }

    /// Out-of-line removal for [`Self::remove_stale_extras_pair`].
    #[cold]
    #[inline(never)]
    fn remove_stale_extras_pair_cold(&mut self, row: u16, col: u16, stale: (bool, bool)) {
        if stale.0 {
            self.storage.extras.remove(CellCoord::new(row, col));
        }
        if stale.1 {
            self.storage
                .extras
                .remove(CellCoord::new(row, col.saturating_add(1)));
        }
    }

    /// Mark a row as wrapped (continuation of previous line) if it exists.
    #[inline]
    fn mark_row_wrapped(&mut self, row: u16) {
        if let Some(next_row) = self.row_mut(row) {
            next_row.set_wrapped(true);
        }
    }

    /// Advance cursor to next visual line for autowrap.
    ///
    /// Mirrors `line_feed()` scroll-region behavior:
    /// - At region bottom: scroll region up
    /// - Above region bottom: move cursor down one row
    /// - Below region bottom: move down unless at screen bottom, then nothing
    ///   (xterm xtermIndex: `cur_row > bot_marg` is CursorDown, which clamps
    ///   at `max_row` — wrapping below the region NEVER scrolls the display;
    ///   output keeps overwriting the last row from the wrap column)
    ///
    /// Wrapped-line bookkeeping (DESTINATION convention — the row the wrapped
    /// content flows INTO is marked wrapped):
    /// - Mark wrapped when moving to a real next row.
    /// - Mark wrapped when the scroll region scrolls on wrap, for BOTH a
    ///   non-full region AND the full screen: the recycled destination row is a
    ///   soft-wrap continuation, so the flag must survive the scroll or reflow /
    ///   selection / copy will treat the continuation as a separate line.
    #[inline]
    pub(crate) fn advance_autowrap_line(&mut self) {
        // THE WRAP FACT (kitty-motion §4.1): this fn is the one funnel every
        // autowrap line advance resolves through, so the serial bumps HERE and
        // nowhere else — unconditionally at entry, because even the clamped
        // bottom-of-screen arm below (cursor pinned, column already reset to
        // the margin) is a wrap the caret observer must be able to see.
        self.storage.wrap_serial += 1;
        // Determine whether cursor was within horizontal margins BEFORE moving it.
        // When DECLRMM is active and cursor is within margins, wrap to the left
        // margin; otherwise wrap to column 0.  Bug fix: previously cursor.col was
        // set unconditionally to margins.left before the check, making the margin
        // test always true and causing incorrect margined scrolls (#7565).
        let margins = self.storage.horizontal_margins();
        let orig_col = self.storage.cursor.col;
        let in_margins = !margins.is_full(self.storage.cols)
            && orig_col >= margins.left
            && orig_col <= margins.right;

        self.storage.cursor.col = if in_margins { margins.left } else { 0 };

        let bottom = self.storage.scroll_region.bottom;
        match self.storage.cursor.row.cmp(&bottom) {
            std::cmp::Ordering::Less => {
                self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                self.mark_row_wrapped(self.storage.cursor.row);
            }
            std::cmp::Ordering::Equal => {
                // When DECLRMM is active and cursor was within margins,
                // use rectangular scroll so cells outside the margin
                // columns are not affected (#7454).
                if in_margins {
                    self.scroll_region_up_margined(1, margins.left, margins.right);
                } else {
                    self.scroll_region_up(1);
                }
                // The freshly-recycled destination row receives the soft-wrap
                // continuation, so mark it wrapped to preserve wrap continuity
                // for reflow / selection / copy — matching the `Less` branch.
                // (Bugfix: a full-screen bottom scroll previously skipped this,
                // breaking the continuation chain at the new bottom row, so copy
                // emitted a spurious newline and resize/reflow split one logical
                // line into two.)
                self.mark_row_wrapped(self.storage.cursor.row);
            }
            std::cmp::Ordering::Greater => {
                // Below the scroll region: cursor-down only, clamped at the
                // last screen row (xterm CursorDown) — never a display
                // scroll. Matches `line_feed()`.
                if self.storage.cursor.row < self.storage.visible_rows.saturating_sub(1) {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                    self.mark_row_wrapped(self.storage.cursor.row);
                }
            }
        }
    }

    /// Write a character at cursor position and advance cursor.
    ///
    /// Non-BMP characters (U+10000+) are stored in the overflow table
    /// with the COMPLEX flag set, preserving the full codepoint.
    ///
    /// REQUIRES: self.storage.cursor.row < self.storage.visible_rows
    /// ENSURES: self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
    pub fn write_char(&mut self, c: char) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        // Drop a stale extras-map entry before overwriting (#7456).
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 1);
        let is_non_bmp = (c as u32) > Cell::MAX_DIRECT_CODEPOINT;
        // Write a BMP placeholder for non-BMP chars (Row only handles u16).
        // The real character is stored in the overflow table below.
        let write_c = if is_non_bmp { '\u{FFFD}' } else { c };
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char(cursor_col, write_c);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
        self.storage.mark_content_cell(cursor_row, cursor_col);

        if is_non_bmp {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            self.set_cell_complex_char(cursor_row, cursor_col, s);
        }

        // Advance cursor
        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        }
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    /// Resolve a StyleId to PackedColor values and CellFlags.
    ///
    /// This helper method looks up the style and converts it to the format
    /// needed for cell writes. Returns (fg, bg, flags).
    #[inline]
    pub fn resolve_style_to_colors(
        &self,
        style_id: StyleId,
        extra_flags: CellFlags,
    ) -> (PackedColor, PackedColor, CellFlags) {
        if let Some(ext_style) = self.storage.styles.extended(style_id) {
            // Convert extended style back to PackedColor format
            let fg = match ext_style.fg_type {
                ColorType::Default => PackedColor::DEFAULT_FG,
                ColorType::Indexed => PackedColor::indexed(ext_style.fg_index),
                ColorType::Rgb => {
                    let rgb = ext_style.style.fg.to_rgb();
                    PackedColor::rgb(rgb.0, rgb.1, rgb.2)
                }
            };
            let bg = match ext_style.bg_type {
                ColorType::Default => PackedColor::DEFAULT_BG,
                ColorType::Indexed => PackedColor::indexed(ext_style.bg_index),
                ColorType::Rgb => {
                    let rgb = ext_style.style.bg.to_rgb();
                    PackedColor::rgb(rgb.0, rgb.1, rgb.2)
                }
            };
            let flags =
                ExtendedStyle::attrs_to_cell_flags(ext_style.style.attrs).union(extra_flags);
            (fg, bg, flags)
        } else {
            // Fallback to default style
            (
                PackedColor::DEFAULT_FG,
                PackedColor::DEFAULT_BG,
                extra_flags,
            )
        }
    }

    #[doc(hidden)] // Write char with deferred autowrap. Pub for crate benchmarks only.
    pub fn write_char_wrap(&mut self, c: char) {
        // Resolve any deferred wrap before writing
        self.resolve_pending_wrap();

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        // Drop a stale extras-map entry before overwriting (#7456).
        let stale = self.stale_extras_pair(cursor_row, cursor_col, 1);
        let is_non_bmp = (c as u32) > Cell::MAX_DIRECT_CODEPOINT;
        let write_c = if is_non_bmp { '\u{FFFD}' } else { c };
        if let Some(row) = self.row_mut(cursor_row) {
            row.write_char(cursor_col, write_c);
        }
        self.remove_stale_extras_pair(cursor_row, cursor_col, stale);
        self.storage.mark_content_cell(cursor_row, cursor_col);

        if is_non_bmp {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            self.set_cell_complex_char(cursor_row, cursor_col, s);
        }

        // Advance cursor with deferred wrap
        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        } else {
            self.storage.mark_pending_wrap();
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// FAST PATH: Fill N consecutive cells with the same character and style.
    ///
    /// Creates a template `Cell` from the given ASCII byte and style, then
    /// bulk-writes it using `Row::fill_cell_run` (which lowers to a memset-like
    /// operation for 8-byte cells). Handles autowrap: when the run exceeds the
    /// current line, wraps to the next line and continues filling.
    ///
    /// Returns the total number of cells written.
    ///
    /// REQUIRES: `byte` is printable ASCII (0x20..=0x7E), insert mode OFF, auto-wrap ON
    /// ENSURES: result <= count, self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn write_cell_run(
        &mut self,
        byte: u8,
        count: usize,
        colors: PackedColors,
        flags: CellFlags,
        last_byte: &mut Option<u8>,
    ) -> usize {
        if count == 0 {
            return 0;
        }

        let template = Cell::from_ascii_styled(byte, colors, flags);
        let mut written = 0;
        let mut remaining = count;
        // #7456: hoisted per run — zero per-cell cost when the extras map
        // is empty (the common plain-text case).
        let scan_stale = !self.storage.extras.is_empty();

        while remaining > 0 {
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
            let available = usize::from(effective_cols.saturating_sub(cursor_col));
            let to_write = remaining.min(available);

            if to_write == 0 {
                self.advance_autowrap_line();
                continue;
            }

            let to_write_u16 = u16::try_from(to_write).unwrap_or(u16::MAX);

            // #7456: capture stale-extras columns before the fill clobbers
            // the HAS_EXTRAS flags; entries are removed after the row
            // borrow ends.
            let stale = if scan_stale {
                stale_extras_cols(row, cursor_col, cursor_col + to_write_u16)
            } else {
                Vec::new()
            };

            // Bulk fill — Row::fill_cell_run uses slice::fill() (memset-like).
            row.fill_cell_run(cursor_col, to_write_u16, template);

            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Mark damage for the row (row-level is faster than per-cell).
            self.storage.mark_content_row(cursor_row);

            *last_byte = Some(byte);
            written += to_write;
            remaining -= to_write;

            // Advance cursor with deferred wrap at end of line.
            let new_col = cursor_col + to_write_u16;
            if new_col > max_col {
                if remaining == 0 {
                    self.storage.cursor.col = max_col;
                    self.storage.mark_pending_wrap();
                } else {
                    self.advance_autowrap_line();
                }
            } else {
                self.storage.cursor.col = new_col;
            }
        }

        debug_assert!(written <= count);
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
        written
    }

    /// FAST PATH: Write printable ASCII bytes directly as u64 cells (400+ MB/s).
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, default style, insert mode OFF, auto-wrap ON
    /// ENSURES: result <= ascii.len(), self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn write_ascii_blast(&mut self, ascii: &[u8]) -> usize {
        if ascii.is_empty() {
            return 0;
        }
        let mut written = 0;
        let mut remaining = ascii;
        // #7456: hoisted per run — zero per-cell cost when the extras map
        // is empty (the common plain-text case).
        let scan_stale = !self.storage.extras.is_empty();
        while !remaining.is_empty() {
            self.resolve_pending_wrap(); // deferred wrap
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
            // How many chars can we write on this line?
            let available = (effective_cols.saturating_sub(cursor_col)) as usize;
            let to_write = remaining.len().min(available);

            if to_write == 0 {
                // At end of line, need to wrap
                self.advance_autowrap_line();
                continue;
            }

            // to_write bounded by effective_cols (u16) — saturate for safety
            let to_write_u16 = u16::try_from(to_write).unwrap_or(u16::MAX);

            // #7456: capture stale-extras columns before the overwrite
            // clobbers the HAS_EXTRAS flags; entries are removed after the
            // row borrow ends. The hot write loop below stays untouched.
            let stale = if scan_stale {
                stale_extras_cols(row, cursor_col, cursor_col + to_write_u16)
            } else {
                Vec::new()
            };

            // Write directly to row cells
            if let Some(target) = row.cells_mut_with_fixup(cursor_col, to_write_u16) {
                debug_assert_eq!(target.len(), to_write);
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_fast(byte);
                }
            }

            // Update row len to include written content
            row.update_len(cursor_col + to_write_u16);

            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Mark damage for the row (mark_row is faster than per-cell)
            self.storage.mark_content_row(cursor_row);

            written += to_write;
            remaining = &remaining[to_write..];

            // Advance cursor with deferred wrap at end of line
            let new_col = cursor_col + to_write_u16;
            if new_col > max_col {
                if remaining.is_empty() {
                    // Last batch — defer wrap (cursor stays at last col)
                    self.storage.cursor.col = max_col;
                    self.storage.mark_pending_wrap();
                } else {
                    // More data coming — wrap now to continue writing
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

    /// FAST PATH: Write styled ASCII bytes with autowrap and deferred wrapping.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, no RGB overflow, insert mode OFF
    /// ENSURES: result <= ascii.len(), self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn write_ascii_run_styled(
        &mut self,
        ascii: &[u8],
        fg: PackedColor,
        bg: PackedColor,
        flags: CellFlags,
        last_byte: &mut Option<u8>,
    ) -> usize {
        let colors = Cell::convert_colors(fg, bg);
        self.write_ascii_run_styled_packed(ascii, colors, flags, last_byte)
    }

    /// FAST PATH: Write styled ASCII bytes with pre-computed packed colors.
    ///
    /// Like `write_ascii_run_styled` but accepts pre-computed `PackedColors`
    /// to avoid redundant `convert_colors` when the caller already has them.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, no RGB overflow, insert mode OFF
    /// ENSURES: result <= ascii.len(), self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn write_ascii_run_styled_packed(
        &mut self,
        ascii: &[u8],
        colors: PackedColors,
        flags: CellFlags,
        last_byte: &mut Option<u8>,
    ) -> usize {
        if ascii.is_empty() {
            return 0;
        }

        let mut written = 0;
        let mut remaining = ascii;
        // #7456: hoisted per run — zero per-cell cost when the extras map
        // is empty (the common plain-text case).
        let scan_stale = !self.storage.extras.is_empty();

        while !remaining.is_empty() {
            // Resolve deferred wrap before writing next batch
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

            // How many chars can we write on this line?
            let available = (effective_cols.saturating_sub(cursor_col)) as usize;
            let to_write = remaining.len().min(available);

            if to_write == 0 {
                // At end of line, need to wrap
                self.advance_autowrap_line();
                continue;
            }

            // to_write bounded by effective_cols (u16) — saturate for safety
            let to_write_u16 = u16::try_from(to_write).unwrap_or(u16::MAX);

            // #7456: capture stale-extras columns before the overwrite
            // clobbers the HAS_EXTRAS flags; entries are removed after the
            // row borrow ends.
            let stale = if scan_stale {
                stale_extras_cols(row, cursor_col, cursor_col + to_write_u16)
            } else {
                Vec::new()
            };

            // Write styled cells directly to row
            if let Some(target) = row.cells_mut_with_fixup(cursor_col, to_write_u16) {
                debug_assert_eq!(target.len(), to_write);
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_styled(byte, colors, flags);
                }
            }

            // Update row len to include written content
            row.update_len(cursor_col + to_write_u16);

            if !stale.is_empty() {
                self.remove_stale_extras(cursor_row, stale);
            }

            // Mark damage for the row (mark_row is faster than per-cell)
            self.storage.mark_content_row(cursor_row);

            // Track last byte for REP
            if let Some(&last) = remaining[..to_write].last() {
                *last_byte = Some(last);
            }

            written += to_write;
            remaining = &remaining[to_write..];

            // Advance cursor with deferred wrap at end of line
            let new_col = cursor_col + to_write_u16;
            if new_col > max_col {
                if remaining.is_empty() {
                    // Last batch — defer wrap (cursor stays at last col)
                    self.storage.cursor.col = max_col;
                    self.storage.mark_pending_wrap();
                } else {
                    // More data coming — wrap now to continue writing
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

    /// Set a cell at the given position.
    ///
    /// REQUIRES: row < self.storage.visible_rows
    /// REQUIRES: col < self.storage.cols
    #[cfg(test)]
    pub(crate) fn set_cell(&mut self, row: u16, col: u16, cell: Cell) {
        if let Some(r) = self.row_mut(row) {
            r.set(col, cell);
            self.storage.mark_content_cell(row, col);
        }
    }

    /// Mark a cell as complex and store the character string in overflow.
    ///
    /// This is used for non-BMP characters (emoji, etc.) that cannot fit
    /// in the 16-bit char_data field of the packed Cell.
    ///
    /// The cell at (row, col) should already have been written. This method
    /// sets the COMPLEX flag and stores the string in CellExtras.
    ///
    /// REQUIRES: row < self.storage.visible_rows
    /// REQUIRES: col < self.storage.cols
    pub fn set_cell_complex_char(&mut self, row: u16, col: u16, s: &str) {
        use std::sync::Arc;

        if let Some(r) = self.row_mut(row)
            && let Some(cell) = r.get_mut(col)
        {
            // Set COMPLEX flag and clear char_data (we'll use overflow)
            let mut flags = cell.flags();
            flags.insert(CellFlags::COMPLEX);
            cell.set_flags(flags);
            cell.set_overflow_index(0); // Not used when COMPLEX; string is in overflow
            cell.set_has_extras(true);
        }

        // Store the string in the overflow table
        let extra = self.storage.extras.get_or_create(CellCoord::new(row, col));
        extra.set_complex_char(Some(Arc::from(s)));

        self.storage.mark_content_cell(row, col);
    }
}

// write_test_helpers declared in parent mod.rs to keep module paths stable
// for cross-crate consumers (aterm-core tests import through grid::, not grid::write::).
