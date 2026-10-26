// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Character writes: `write_char`, the ASCII bulk paths (`write_cell_run`, `write_ascii_blast`, `write_ascii_run_styled_packed`), and the stale-extras probe every writer shares.

#[cfg(test)]
use super::PackedColor;
use super::{Cell, CellCoord, CellFlags, Grid, PackedColors};

/// The extras-map entries a write over `start, end)` of `row` is about to leave without a cell to claim them, probed before it lands (the write clears the HAS_EXTRAS bits this reads).
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

    /// Probe `row` before a write over `start, end)`.
    #[inline]
    pub(super) fn probe(row: &crate::Row, start: u16, end: u16) -> Self {
        if !row.any_has_extras_in_range(start, end)
            && !row.flags().contains(crate::row::RowFlags::HAS_WIDE_CHARS)
        {
            return Self::NONE;
        }
        Self::probe_cold(row, start, end)
    }

    #[cold]
    #[inline(never)]
    fn probe_cold(row: &crate::Row, start: u16, end: u16) -> Self {
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

    #[inline]
    const fn is_empty(&self) -> bool {
        self.overwritten.is_empty() && matches!(self.halves, [None, None])
    }
}

#[cfg(clean_islands)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

verify_island! {
    mod stale_extras_probe_proofs {
        clean {
            section StaleExtrasProbeGate
            theorem FastSkipSoundWhenNoSpanExtrasAndNoWide :
                forall (span_has_extras row_has_wide : Prop),
                    Not span_has_extras ->
                    Not row_has_wide ->
                    Not (Or span_has_extras row_has_wide) :=
                fun (span_has_extras row_has_wide : Prop)
                    (h_no_span : Not span_has_extras)
                    (h_no_wide : Not row_has_wide) =>
                    fun (h_or : Or span_has_extras row_has_wide) =>
                        Or.elim h_or h_no_span h_no_wide

            -- When map_may_have_row is false (Not row_in_map), no entry in
            -- CellExtras lives on row, so removing stale extras for row is a
            -- no-op regardless of cell flag bits.
            theorem RowMapGateSkipsProbeWhenRowAbsent :
                forall (row_in_map stale_on_row : Prop),
                    (stale_on_row -> row_in_map) ->
                    Not row_in_map ->
                    Not stale_on_row :=
                fun (row_in_map stale_on_row : Prop)
                    (h_imp : stale_on_row -> row_in_map)
                    (h_no_row : Not row_in_map) =>
                    fun (h_stale : stale_on_row) =>
                        h_no_row (h_imp h_stale)
            end StaleExtrasProbeGate
        }
    }
}

/// The wide halves outside `start, end)` that a write over it orphans and whose cells carry HAS_EXTRAS (see `StaleExtras`).
#[inline]
pub(super) fn orphan_halves_with_extras(
    row: &crate::Row,
    start: u16,
    end: u16,
) -> [Option<u16>; 2] {
    row.orphaned_wide_halves(start, end)
        .map(|half| half.filter(|&col| row.get(col).is_some_and(Cell::has_extras)))
}

/// [`StaleExtras`] for a one- or two-cell glyph write, held inline: the per-glyph writers probe once per glyph, and rewriting cells that carry extras (a truecolor redraw) must not allocate per glyph.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StaleGlyph {
    overwritten: [bool; 2],
    halves: [Option<u16>; 2],
}

impl Grid {
    /// Remove what [`StaleExtras::probe`] found for a row that has just been overwritten, so a later styled write's `get_or_create` there cannot merge a stale entry (an old hyperlink, combining mark, underline colour, RGB overflow) into the new cell.
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
    #[inline]
    pub(crate) fn stale_extras_pair(&self, row: u16, col: u16, width: u16) -> StaleGlyph {
        if !self
            .storage
            .extras
            .map_may_have_row(row, usize::from(width).saturating_add(2))
        {
            return StaleGlyph::default();
        }
        self.stale_extras_pair_cold(row, col, width)
    }

    /// Out-of-line probe for [`Self::stale_extras_pair`], reached only when the extras map is
    /// non-empty.
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

    /// Remove the extras-map entries flagged by [`Self::stale_extras_pair`] after the overwrite landed.
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

    /// Drop the extras-map entries in `cols` of visible row `row` whose cells no longer claim one.
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

    /// Advance the cursor to the next line for autowrap, with `line_feed()`'s scroll-region
    /// behaviour:
    /// - at the region bottom: scroll the region up (only the rectangle between DECSLRM
    ///   margins, and nothing from a cursor outside them);
    /// - above it: move down one row;
    /// - below it: move down unless at the screen bottom (xterm `xtermIndex`: below the region
    ///   is CursorDown clamped at `max_row`, never a display scroll).
    #[inline]
    pub(crate) fn advance_autowrap_line(&mut self) {
        self.storage.wrap_serial += 1;
        // xterm `WrapLine`: the index reads the column the wrap was pending on (at the
        // bottom margin it scrolls only from within the margins), then the cursor goes
        // to the left margin wherever it came from.
        let margins = self.storage.horizontal_margins();
        let orig_col = self.storage.cursor.col;
        let margined = !margins.is_full(self.storage.cols);
        let in_margins = orig_col >= margins.left && orig_col <= margins.right;

        self.storage.cursor.col = if margined { margins.left } else { 0 };

        let bottom = self.storage.scroll_region.bottom;
        match self.storage.cursor.row.cmp(&bottom) {
            std::cmp::Ordering::Less => {
                self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                self.mark_row_wrapped(self.storage.cursor.row);
            }
            std::cmp::Ordering::Equal if margined && !in_margins => {
                // At the bottom margin from outside the margins: no scroll, no move.
            }
            std::cmp::Ordering::Equal => {
                // Inside DECLRMM margins, scroll the rectangle so cells outside them stay.
                if margined {
                    self.scroll_region_up_margined(1, margins.left, margins.right);
                } else {
                    self.scroll_region_up(1);
                }
                self.mark_row_wrapped(self.storage.cursor.row);
            }
            std::cmp::Ordering::Greater => {
                // Below the scroll region: cursor-down clamped at the last row, never a
                // scroll (as `line_feed()`).
                if self.storage.cursor.row < self.storage.visible_rows.saturating_sub(1) {
                    self.storage.cursor.row = self.storage.cursor.row.saturating_add(1);
                    self.mark_row_wrapped(self.storage.cursor.row);
                }
            }
        }
    }

    /// Write a character at the cursor and advance it, without wrapping.
    pub fn write_char(&mut self, c: char) {
        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
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

        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        }
        debug_assert!(
            self.storage.cursor.col <= self.storage.max_col_for_row(self.storage.cursor.row)
        );
    }

    #[doc(hidden)]
    #[cfg(test)]
    pub fn write_char_wrap(&mut self, c: char) {
        self.resolve_pending_wrap();

        let cursor_row = self.storage.cursor.row;
        let cursor_col = self.storage.cursor.col;
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

        let max_col = self.storage.max_col_for_row(cursor_row);
        if self.storage.cursor.col < max_col {
            self.storage.cursor.col += 1;
        } else {
            self.storage.mark_pending_wrap();
        }
        debug_assert!(self.storage.cursor.row < self.storage.visible_rows);
    }

    /// Fill `count` cells with one ASCII byte and style (`Row::fill_cell_run`, a slice fill), wrapping to the next line when the run passes the end of the current one.
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
        let scan_stale = !self.storage.extras.is_empty();

        while remaining > 0 {
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
            let available = usize::from(effective_cols.saturating_sub(cursor_col));
            let to_write = remaining.min(available);

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

            row.fill_cell_run(cursor_col, to_write_u16, template);

            self.remove_stale_extras(cursor_row, stale);

            self.storage.mark_content_row(cursor_row);

            *last_byte = Some(byte);
            written += to_write;
            remaining -= to_write;

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

    /// Write printable default-style ASCII bytes directly as cells.
    #[inline]
    pub fn write_ascii_blast(&mut self, ascii: &[u8]) -> usize {
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
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_fast(byte);
                }
            }

            row.update_len(cursor_col + to_write_u16);

            self.remove_stale_extras(cursor_row, stale);

            self.storage.mark_content_row(cursor_row);

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

    /// Write styled ASCII bytes with autowrap and deferred wrapping.
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

    /// Write styled ASCII bytes with autowrap and deferred wrapping, given packed colors.
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
                for (cell, &byte) in target.iter_mut().zip(remaining[..to_write].iter()) {
                    *cell = Cell::from_ascii_styled(byte, colors, flags);
                }
            }

            row.update_len(cursor_col + to_write_u16);

            self.remove_stale_extras(cursor_row, stale);

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

    /// Set a cell at the given position.
    #[cfg(test)]
    pub(crate) fn set_cell(&mut self, row: u16, col: u16, cell: Cell) {
        if let Some(r) = self.row_mut(row) {
            r.set(col, cell);
            self.storage.mark_content_cell(row, col);
        }
    }

    /// Mark an already-written cell COMPLEX and store `s` in its `CellExtras`: non-BMP characters (emoji) do not fit the cell's 16-bit char data.
    pub fn set_cell_complex_char(&mut self, row: u16, col: u16, s: &str) {
        use std::sync::Arc;

        if let Some(r) = self.row_mut(row)
            && let Some(cell) = r.get_mut(col)
        {
            let mut flags = cell.flags();
            flags.insert(CellFlags::COMPLEX);
            cell.set_flags(flags);
            cell.set_overflow_index(0);
            cell.set_has_extras(true);
        }

        let extra = self.storage.extras.get_or_create(CellCoord::new(row, col));
        extra.set_complex_char(Some(Arc::from(s)));

        self.storage.mark_content_cell(row, col);
    }
}
