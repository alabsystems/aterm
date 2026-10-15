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

/// The extras-map entries a write over `[start, end)` of `row` is about to
/// leave without a cell to claim them, probed before it lands (the write
/// clears the HAS_EXTRAS bits this reads).
///
/// Two kinds, removed differently once the write has landed
/// ([`Grid::remove_stale_extras`]):
///
/// - `overwritten`: cells in the span that carry HAS_EXTRAS. Their entries go
///   unconditionally: the write replaced the cell, and a writer that sets the
///   bit on its new cell (the extras run, the preflagged per-glyph path) must
///   not merge the old entry into it.
/// - `halves`: the wide halves outside the span that the write's fixup
///   blanks (`Row::orphaned_wide_halves`), when they carry HAS_EXTRAS. Their
///   entries go only if the cell lost the bit
///   ([`Grid::drop_unclaimed_extras`]): a narrow glyph over a toned emoji's
///   right half blanks the head, whose skin tone a truecolor write there
///   would otherwise adopt.
#[derive(Debug, Default)]
pub(super) struct StaleExtras {
    overwritten: Vec<u16>,
    halves: [Option<u16>; 2],
}

impl StaleExtras {
    /// Nothing to remove: the extras map is empty.
    pub(super) const NONE: Self = Self {
        overwritten: Vec::new(),
        halves: [None, None],
    };

    /// Probe `row` before a write over `[start, end)`. Call only when the
    /// extras map is non-empty, keeping plain writes branch-only; allocates
    /// only when an overwritten cell carries an entry.
    #[cold]
    #[inline(never)]
    pub(super) fn probe(row: &crate::Row, start: u16, end: u16) -> Self {
        let mut overwritten = Vec::new();
        for col in start..end {
            if row.get(col).is_some_and(Cell::has_extras) {
                if overwritten.is_empty() {
                    overwritten.reserve_exact(usize::from(end.saturating_sub(col)));
                }
                overwritten.push(col);
            }
        }
        Self {
            overwritten,
            halves: orphan_halves_with_extras(row, start, end),
        }
    }

    fn is_empty(&self) -> bool {
        self.overwritten.is_empty() && self.halves == [None, None]
    }
}

/// The wide halves outside `[start, end)` that a write over it orphans and
/// whose cells carry HAS_EXTRAS (see [`StaleExtras`]).
#[inline]
pub(super) fn orphan_halves_with_extras(
    row: &crate::Row,
    start: u16,
    end: u16,
) -> [Option<u16>; 2] {
    row.orphaned_wide_halves(start, end)
        .map(|half| half.filter(|&col| row.get(col).is_some_and(Cell::has_extras)))
}

/// [`StaleExtras`] for a one- or two-cell glyph write, held inline: the
/// per-glyph writers probe once per glyph, and rewriting cells that carry
/// extras (a truecolor redraw) must not allocate per glyph.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StaleGlyph {
    overwritten: [bool; 2],
    halves: [Option<u16>; 2],
}

impl Grid {
    /// Remove what [`StaleExtras::probe`] found for a row that has just been
    /// overwritten.
    ///
    /// Otherwise an overwritten cell's entry (hyperlink, combining marks,
    /// underline color, RGB overflow) would linger, and a later styled write's
    /// `get_or_create` at that coordinate would merge the stale data (e.g. an
    /// old hyperlink) into the new cell.
    #[inline]
    pub(super) fn remove_stale_extras(&mut self, row: u16, stale: StaleExtras) {
        if !stale.is_empty() {
            self.remove_stale_extras_cold(row, stale);
        }
    }

    #[cold]
    #[inline(never)]
    fn remove_stale_extras_cold(&mut self, row: u16, stale: StaleExtras) {
        for col in stale.overwritten {
            self.storage.extras.remove(CellCoord::new(row, col));
        }
        for col in stale.halves.into_iter().flatten() {
            self.drop_unclaimed_extras(row, col..col.saturating_add(1));
        }
    }

    /// [`StaleExtras`] for a 1- or 2-cell overwrite at `(row, col)`.
    ///
    /// Cost: one branch when the extras map is empty (the plain-text case);
    /// otherwise a flag read per overwritten column and per orphaned wide
    /// half, and a hash removal ([`Self::remove_stale_extras_pair`]) only for
    /// flagged columns.
    #[inline]
    pub(crate) fn stale_extras_pair(&self, row: u16, col: u16, width: u16) -> StaleGlyph {
        if self.storage.extras.is_empty() {
            return StaleGlyph::default();
        }
        self.stale_extras_pair_cold(row, col, width)
    }

    /// Out-of-line probe for [`Self::stale_extras_pair`] — only reached when
    /// the extras map is non-empty, keeping the hot write bodies lean.
    #[cold]
    #[inline(never)]
    fn stale_extras_pair_cold(&self, row: u16, col: u16, width: u16) -> StaleGlyph {
        let Some(r) = self.row(row) else {
            return StaleGlyph::default();
        };
        let probe = |c: u16| r.get(c).is_some_and(Cell::has_extras);
        StaleGlyph {
            overwritten: [probe(col), width == 2 && probe(col.saturating_add(1))],
            halves: orphan_halves_with_extras(r, col, col.saturating_add(width)),
        }
    }

    /// Remove the extras-map entries flagged by [`Self::stale_extras_pair`]
    /// after the overwrite landed. Call only when the write happened; a failed
    /// write must not drop live extras.
    #[inline]
    pub(crate) fn remove_stale_extras_pair(&mut self, row: u16, col: u16, stale: StaleGlyph) {
        if stale.overwritten != [false, false] || stale.halves != [None, None] {
            self.remove_stale_extras_pair_cold(row, col, stale);
        }
    }

    /// Out-of-line removal for [`Self::remove_stale_extras_pair`].
    #[cold]
    #[inline(never)]
    fn remove_stale_extras_pair_cold(&mut self, row: u16, col: u16, stale: StaleGlyph) {
        if stale.overwritten[0] {
            self.storage.extras.remove(CellCoord::new(row, col));
        }
        if stale.overwritten[1] {
            self.storage
                .extras
                .remove(CellCoord::new(row, col.saturating_add(1)));
        }
        for half in stale.halves.into_iter().flatten() {
            self.drop_unclaimed_extras(row, half..half.saturating_add(1));
        }
    }

    /// Drop the extras-map entries in `cols` of visible row `row` whose cells
    /// no longer claim one.
    ///
    /// A cell's HAS_EXTRAS bit is its claim on the entry keyed to its
    /// coordinate (`HAS_EXTRAS ⇔ entry`, which [`Self::assert_extras_claimed`]
    /// checks). An operation that blanks or replaces cells outside the writers'
    /// stale-entry probe clears the bit with the cell and leaves the entry
    /// keyed to it: a wide pair's orphaned half, a pair bisected by ICH, DCH,
    /// ECH or EL, a selective erase, a rectangle copy or shift, VS16 or VS15
    /// reshaping a pair. A later write there with an extras style adopts it
    /// (`get_or_create`), so an old combining mark, skin tone, link or underline
    /// colour reappears on new text, and the readers that do not gate on the
    /// bit (`cell_extra`, `hyperlink_at`, checkpoint serialization) see it at
    /// once. Every such operation calls this over the columns it may have
    /// blanked. A cell that kept the bit keeps its entry: a protected cell a
    /// selective erase skipped, a cell a shift moved together with its entry.
    /// A cell without the bit keeps only truecolor fg/bg bytes, which a BCE
    /// blank owns through the RGB marker in its packed colours (reflow harvests
    /// them from the dense ring into the map); stale bytes under a cell whose
    /// colours are not truecolor are never read.
    ///
    /// Ruling 2026-10-10 (r15 C2): the bit, not each caller's knowledge of
    /// which cells its fixups touched, decides; the one primitive serves every
    /// such operation, and the writers' probe ([`StaleExtras`]) reaches it for
    /// the halves their fixups blank.
    ///
    /// Cost: nothing while the map is empty or holds no entry on the row; else
    /// a flag read per column and a hash probe per column without the bit.
    pub fn drop_unclaimed_extras(&mut self, row: u16, cols: core::ops::Range<u16>) {
        if self.storage.extras.is_empty() || cols.is_empty() {
            return;
        }
        let width = usize::from(cols.end - cols.start);
        if !self.storage.extras.map_may_have_row(row, width) {
            return;
        }
        let Some(idx) = self.storage.row_index(row) else {
            return;
        };
        for col in cols {
            let claimed = self
                .storage
                .rows
                .get(idx)
                .and_then(|r| r.get(col))
                .is_some_and(Cell::has_extras);
            if !claimed {
                self.storage
                    .extras
                    .strip_unclaimed(CellCoord::new(row, col));
            }
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
    ///   (xterm xtermIndex: `cur_row > bot_marg` is CursorDown, clamped at
    ///   `max_row`, so wrapping below the region never scrolls the display)
    ///
    /// The row the wrapped content flows into is marked wrapped:
    /// - when moving to a real next row;
    /// - when the scroll region scrolls on wrap, for both a partial region and
    ///   the full screen: the recycled destination row is a soft-wrap
    ///   continuation, so reflow / selection / copy must see it as one line.
    #[inline]
    pub(crate) fn advance_autowrap_line(&mut self) {
        // The wrap fact: every autowrap line advance goes through this fn, so
        // the serial bumps here and nowhere else, unconditionally, since even
        // the clamped bottom-of-screen arm below is a wrap the caret observer
        // must see.
        self.storage.wrap_serial += 1;
        // Determine whether the cursor was within the horizontal margins before
        // moving it: with DECLRMM active and the cursor inside, wrap to the left
        // margin; otherwise to column 0.
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
                // With DECLRMM active and the cursor within the margins, use a
                // rectangular scroll so cells outside the margins are untouched.
                if in_margins {
                    self.scroll_region_up_margined(1, margins.left, margins.right);
                } else {
                    self.scroll_region_up(1);
                }
                // The recycled destination row receives the soft-wrap
                // continuation, so mark it wrapped (as in the `Less` branch), or
                // copy would add a newline and reflow would split the line.
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
        // Drop a stale extras-map entry before overwriting.
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
    #[cfg(test)]
    pub fn write_char_wrap(&mut self, c: char) {
        // Resolve any deferred wrap before writing
        self.resolve_pending_wrap();

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
        // Drop a stale extras-map entry before overwriting.
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

    /// Fast path: fill N consecutive cells with the same character and style.
    ///
    /// Creates a template `Cell` from the given ASCII byte and style, then
    /// bulk-writes it using `Row::fill_cell_run` (which lowers to a memset-like
    /// operation for 8-byte cells). Handles autowrap: when the run exceeds the
    /// current line, wraps to the next line and continues filling.
    ///
    /// Returns the total number of cells written.
    ///
    /// REQUIRES: `byte` is printable ASCII (0x20..=0x7E), insert mode OFF, auto-wrap ON
    /// REQUIRES: the cursor is not outside a DECSLRM span
    /// (`Grid::cursor_outside_margin_span`): the right-margin clamp is derived
    /// once per row from the column the row's batch starts at
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
        // hoisted per run — zero per-cell cost when the extras map
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

            // Capture stale-extras columns before the overwrite clobbers the
            // HAS_EXTRAS flags; the entries are removed after the row borrow
            // ends.
            let stale = if scan_stale {
                StaleExtras::probe(row, cursor_col, cursor_col + to_write_u16)
            } else {
                StaleExtras::NONE
            };

            // Bulk fill — Row::fill_cell_run uses slice::fill() (memset-like).
            row.fill_cell_run(cursor_col, to_write_u16, template);

            self.remove_stale_extras(cursor_row, stale);

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

    /// Fast path: write printable ASCII bytes directly as u64 cells.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, default style, insert mode OFF, auto-wrap ON
    /// REQUIRES: the cursor is not outside a DECSLRM span
    /// (`Grid::cursor_outside_margin_span`): the right-margin clamp is derived
    /// once per row from the column the row's batch starts at
    /// ENSURES: result <= ascii.len(), self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    pub fn write_ascii_blast(&mut self, ascii: &[u8]) -> usize {
        if ascii.is_empty() {
            return 0;
        }
        let mut written = 0;
        let mut remaining = ascii;
        // hoisted per run — zero per-cell cost when the extras map
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

            // Capture stale-extras columns before the overwrite clobbers the
            // HAS_EXTRAS flags; the entries are removed after the row borrow
            // ends.
            let stale = if scan_stale {
                StaleExtras::probe(row, cursor_col, cursor_col + to_write_u16)
            } else {
                StaleExtras::NONE
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

            self.remove_stale_extras(cursor_row, stale);

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

    /// Fast path: write styled ASCII bytes with autowrap and deferred wrapping.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, no RGB overflow, insert mode OFF
    /// ENSURES: result <= ascii.len(), self.storage.cursor.row < self.storage.visible_rows
    #[inline]
    #[cfg(test)]
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

    /// Fast path: write styled ASCII bytes with pre-computed packed colors.
    ///
    /// Like `write_ascii_run_styled` but accepts pre-computed `PackedColors`
    /// to avoid redundant `convert_colors` when the caller already has them.
    ///
    /// REQUIRES: all bytes 0x20..=0x7E, no RGB overflow, insert mode OFF
    /// REQUIRES: the cursor is not outside a DECSLRM span
    /// (`Grid::cursor_outside_margin_span`): the right-margin clamp is derived
    /// once per row from the column the row's batch starts at
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
        // hoisted per run — zero per-cell cost when the extras map
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

            // Capture stale-extras columns before the overwrite clobbers the
            // HAS_EXTRAS flags; the entries are removed after the row borrow
            // ends.
            let stale = if scan_stale {
                StaleExtras::probe(row, cursor_col, cursor_col + to_write_u16)
            } else {
                StaleExtras::NONE
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

            self.remove_stale_extras(cursor_row, stale);

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
