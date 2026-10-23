// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row storage for the terminal grid: a contiguous array of cells, a length (one past the last cell output wrote, printed blanks included; see [`Row::len`]) and [`RowFlags`].

use super::cell::Cell;
use super::page::{PageSlice, PageStore};

#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "guarded by value <= u16::MAX as usize before cast"
)]
const fn u16_from_usize(value: usize) -> u16 {
    if value > u16::MAX as usize {
        u16::MAX
    } else {
        value as u16
    }
}

/// Where the halves of wide pairs an ICH/DCH/ECH blanked with its BCE fill ended up once its shift was done (at most four), beyond the span it vacated or erased.
pub type BlankedHalves = [Option<u16>; 4];

/// A cell an early-wrap hole can still be: a blank (space) that is no half of
/// a wide glyph and no complex cluster.
#[inline]
const fn is_wrap_hole_blank(cell: &Cell) -> bool {
    use super::CellFlags;
    cell.char_data() == (b' ' as u16)
        && !cell.flags().intersects(
            CellFlags::WIDE
                .union(CellFlags::WIDE_CONTINUATION)
                .union(CellFlags::COMPLEX),
        )
}

/// A single row of terminal cells.
pub struct Row {
    /// Cell storage.
    cells: PageSlice<Cell>,
    /// One past the last cell output wrote (see [`Row::len`]).
    len: u16,
    /// Row flags.
    flags: RowFlags,
}

aterm_types::bitflags! {
    /// Row flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    #[repr(transparent)]
    pub struct RowFlags: u8 {
        /// This row is a continuation of the previous row (soft wrap).
        const WRAPPED = 1 << 0;
        /// This row is marked as dirty (needs re-render).
        const DIRTY = 1 << 1;
        /// Double-width line (DECDWL or DECDHL).
        const DOUBLE_WIDTH = 1 << 2;
        /// Double-height line, top half (DECDHL).
        const DOUBLE_HEIGHT_TOP = 1 << 3;
        /// Double-height line, bottom half (DECDHL).
        const DOUBLE_HEIGHT_BOTTOM = 1 << 4;
        /// At least one cell is a WIDE head.
        const HAS_WIDE_CHARS = 1 << 5;
        /// The row was filled from a history [`Line`](aterm_scrollback::Line)
        /// that ends two or more cells before the last column: its line goes on
        /// at the row's length when the next row continues it, as the line
        /// does in the tiered store, not at the last column an autowrap would
        /// have filled.
        const ENDS_AT_LEN = 1 << 6;
        /// The row's last cell is an early-wrap hole: a width-2 glyph that
        /// could not start there wrapped to the next row and left the cell
        /// unwritten (xterm blanks it with the current background; Ghostty's
        /// `spacer_head`).
        const EARLY_WRAP_HOLE = 1 << 7;
        /// Mask of DEC line attribute flags (DECDWL/DECDHL).
        const LINE_ATTRIBUTES = Self::DOUBLE_WIDTH.bits()
            | Self::DOUBLE_HEIGHT_TOP.bits()
            | Self::DOUBLE_HEIGHT_BOTTOM.bits();
    }
}

/// Line size attributes (DEC line height/width).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum LineSize {
    /// Single-width, single-height line (default).
    #[default]
    SingleWidth,
    /// Double-width line (single-height).
    DoubleWidth,
    /// Double-height line, top half (also double-width).
    DoubleHeightTop,
    /// Double-height line, bottom half (also double-width).
    DoubleHeightBottom,
}

impl Row {
    /// Create a new row with the given width.
    /// # Safety
    /// `pages` must outlive the returned row.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ResizeRowReuse",
            action = "Grow",
            project = "grid::tests::resize_row_reuse::resize_row_reuse_conforms"
        )
    )]
    pub unsafe fn new(cols: u16, pages: &mut PageStore) -> Self {
        let mut cells = pages.alloc_cells(cols);
        cells.fill(Cell::EMPTY);
        Self {
            cells,
            len: 0,
            flags: RowFlags::DIRTY,
        }
    }

    /// Return this removed row's cell storage for another row of the same width.
    /// # Safety
    /// `pages` must be the live PageStore that allocated this row's cells.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ResizeRowReuse",
            action = "Shrink",
            project = "grid::tests::resize_row_reuse::resize_row_reuse_conforms"
        )
    )]
    pub(crate) unsafe fn recycle(self, pages: &mut PageStore) {
        // SAFETY: the caller supplies the originating arena; consuming the Row transfers its non-Clone PageSlice and leaves no row owning the range.
        unsafe { pages.recycle_cells(self.cells) };
    }

    /// Get the column count.
    #[must_use]
    #[inline]
    pub fn cols(&self) -> u16 {
        u16_from_usize(self.cells.len())
    }

    /// Get the total number of cells (usize).
    #[must_use]
    #[inline]
    pub fn cells_len(&self) -> usize {
        self.cells.len()
    }

    /// Mark this row as containing wide characters.
    #[inline]
    pub fn mark_has_wide_chars(&mut self) {
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_WIDE_CHARS;
    }

    /// Get the page ID for this row's cell storage.
    #[cfg(test)]
    #[must_use]
    #[inline]
    pub(crate) const fn page_id(&self) -> super::page::PageId {
        self.cells.page_id()
    }

    /// One past the last cell output wrote on the row: a printed glyph, a printed blank included, or a mark attached to a cell.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> u16 {
        self.len
    }

    /// Check if the row is empty.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get row flags.
    #[must_use]
    #[inline]
    pub const fn flags(&self) -> RowFlags {
        self.flags
    }

    /// Check if this row is a continuation of the previous (soft wrap).
    #[must_use]
    #[inline]
    pub const fn is_wrapped(&self) -> bool {
        self.flags.contains(RowFlags::WRAPPED)
    }

    /// Set the wrapped flag.
    #[inline]
    pub fn set_wrapped(&mut self, wrapped: bool) {
        if wrapped {
            self.flags |= RowFlags::WRAPPED;
        } else {
            self.flags -= RowFlags::WRAPPED;
        }
    }

    /// Is this row's last cell an early-wrap hole ([`RowFlags::EARLY_WRAP_HOLE`])? The flag must be set and the cell must still hold the blank the wrap left: a glyph written there since (by any path) makes it content again.
    #[must_use]
    #[inline]
    pub fn has_early_wrap_hole(&self) -> bool {
        self.flags.contains(RowFlags::EARLY_WRAP_HOLE)
            && self.cells.last().is_some_and(is_wrap_hole_blank)
    }

    /// Record (or forget) that this row's last cell is an early-wrap hole.
    #[inline]
    pub(crate) fn set_early_wrap_hole(&mut self, hole: bool) {
        if hole {
            self.flags |= RowFlags::EARLY_WRAP_HOLE;
        } else {
            self.flags -= RowFlags::EARLY_WRAP_HOLE;
        }
    }

    /// The cells output wrote on this row, less a recorded early-wrap hole: the row's text as written.
    #[must_use]
    #[inline]
    pub fn written_len(&self) -> u16 {
        if self.has_early_wrap_hole() {
            self.len.min(self.cols().saturating_sub(1))
        } else {
            self.len
        }
    }

    /// How many cells this row contributes to its logical line when the next row continues it (soft wrap): autowrap filled it to its last column, so every cell is content, blanks included, except a recorded early-wrap hole.
    #[must_use]
    #[inline]
    pub(crate) fn continued_len(&self) -> u16 {
        if self.flags.contains(RowFlags::ENDS_AT_LEN) {
            return self.len;
        }
        self.cols()
            .saturating_sub(u16::from(self.has_early_wrap_hole()))
    }

    /// Record (or forget) that this row's line goes on at its length when the next row continues it ([`RowFlags::ENDS_AT_LEN`]).
    #[inline]
    pub(crate) fn set_ends_at_len(&mut self, short: bool) {
        if short {
            self.flags |= RowFlags::ENDS_AT_LEN;
        } else {
            self.flags -= RowFlags::ENDS_AT_LEN;
        }
    }

    /// The marks that say where a continued row's content ends, as a margined row move carries them from one row to another: a recorded early-wrap hole (only while the cell still holds the blank the wrap left) and [`RowFlags::ENDS_AT_LEN`].
    #[must_use]
    #[inline]
    pub(crate) fn end_marks(&self) -> RowFlags {
        let mut marks = self.flags & RowFlags::ENDS_AT_LEN;
        if self.has_early_wrap_hole() {
            marks |= RowFlags::EARLY_WRAP_HOLE;
        }
        marks
    }

    /// Replace this row's [`Self::end_marks`] with `marks`.
    #[inline]
    pub(crate) fn set_end_marks(&mut self, marks: RowFlags) {
        let both = RowFlags::EARLY_WRAP_HOLE | RowFlags::ENDS_AT_LEN;
        self.flags = (self.flags - both) | (marks & both);
    }

    /// A write that lands on the last cell (`end`, exclusive, reaches the row
    /// end) fills an early-wrap hole: the cell is content from now on, and the
    /// row is as full as an autowrap leaves it.
    #[inline]
    fn note_write_through(&mut self, end: usize) {
        if end >= self.cells.len() {
            self.flags -= RowFlags::EARLY_WRAP_HOLE | RowFlags::ENDS_AT_LEN;
        }
    }

    /// Check if this row is dirty (needs re-render).
    #[cfg(test)]
    #[must_use]
    #[inline]
    const fn is_dirty(&self) -> bool {
        self.flags.contains(RowFlags::DIRTY)
    }

    /// Get the current line size attribute.
    #[must_use]
    #[inline]
    pub const fn line_size(&self) -> LineSize {
        if self.flags.contains(RowFlags::DOUBLE_HEIGHT_TOP) {
            LineSize::DoubleHeightTop
        } else if self.flags.contains(RowFlags::DOUBLE_HEIGHT_BOTTOM) {
            LineSize::DoubleHeightBottom
        } else if self.flags.contains(RowFlags::DOUBLE_WIDTH) {
            LineSize::DoubleWidth
        } else {
            LineSize::SingleWidth
        }
    }

    /// Set the line size attribute.
    #[inline]
    pub fn set_line_size(&mut self, size: LineSize) {
        self.flags.remove(
            RowFlags::DOUBLE_WIDTH | RowFlags::DOUBLE_HEIGHT_TOP | RowFlags::DOUBLE_HEIGHT_BOTTOM,
        );
        match size {
            LineSize::SingleWidth => {}
            LineSize::DoubleWidth => {
                self.flags |= RowFlags::DOUBLE_WIDTH;
            }
            LineSize::DoubleHeightTop => {
                self.flags |= RowFlags::DOUBLE_WIDTH | RowFlags::DOUBLE_HEIGHT_TOP;
            }
            LineSize::DoubleHeightBottom => {
                self.flags |= RowFlags::DOUBLE_WIDTH | RowFlags::DOUBLE_HEIGHT_BOTTOM;
            }
        }
        if matches!(
            size,
            LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom
        ) {
            let cols = self.cols();
            let half = cols / 2;
            let start = usize::from(half.max(1));
            let old_len = self.len as usize;
            if start < self.cells.len() {
                for cell in &mut self.cells[start..] {
                    *cell = Cell::EMPTY;
                }
                if start < old_len {
                    self.recalculate_len_up_to(start);
                }
            }
        }
        self.flags |= RowFlags::DIRTY;
    }

    /// Get a cell at the given column.
    #[must_use]
    #[inline]
    pub fn get(&self, col: u16) -> Option<&Cell> {
        self.cells.get(col as usize)
    }

    /// Get a mutable cell at the given column.
    #[must_use]
    #[inline]
    pub fn get_mut(&mut self, col: u16) -> Option<&mut Cell> {
        self.cells.get_mut(col as usize)
    }

    /// Get a cell at the given column (unchecked).
    /// # Safety
    /// Column must be less than cols().
    #[must_use]
    #[inline]
    pub unsafe fn get_unchecked(&self, col: u16) -> &Cell {
        debug_assert!((col as usize) < self.cells.len());
        // SAFETY: Caller guarantees col < cols()
        unsafe { self.cells.get_unchecked(col as usize) }
    }

    /// Get a mutable cell at the given column (unchecked).
    /// # Safety
    /// Column must be less than cols().
    #[must_use]
    #[inline]
    pub unsafe fn get_unchecked_mut(&mut self, col: u16) -> &mut Cell {
        debug_assert!((col as usize) < self.cells.len());
        // SAFETY: Caller guarantees col < cols()
        unsafe { self.cells.get_unchecked_mut(col as usize) }
    }

    /// Get mutable access to a cell range after clearing any orphaned wide-char halves.
    #[must_use]
    #[inline]
    pub fn cells_mut_with_fixup(&mut self, start_col: u16, count: u16) -> Option<&mut [Cell]> {
        let start = usize::from(start_col);
        if start > self.cells.len() {
            return None;
        }

        let old_len = self.len as usize;
        let end_half_blanked = self.flags.contains(RowFlags::HAS_WIDE_CHARS)
            && self.fixup_wide_chars_in_range(start_col, count);
        let end = start
            .saturating_add(usize::from(count))
            .min(self.cells.len());
        if end > start {
            self.flags |= RowFlags::DIRTY;
            self.note_write_through(end);
        }
        if end_half_blanked && end + 1 == old_len {
            self.len = u16_from_usize(end);
        }
        Some(&mut self.cells[start..end])
    }

    /// Grow `len` to cover content written up to `end_col - 1` by a path that does not keep it (e.g. [`Self::cells_mut_with_fixup`]).
    #[inline]
    pub fn update_len(&mut self, end_col: u16) {
        if end_col > self.len {
            self.len = end_col.min(self.cols());
        }
    }

    /// Overwrite the cells from column `left` with `cells` (clipped to the row), keeping the row's hint as [`Self::set`] does: `HAS_WIDE_CHARS` for a copied wide head.
    #[inline]
    pub(crate) fn overwrite_span(&mut self, left: usize, cells: &[Cell]) {
        let Some(dst) = self.cells.get_mut(left..) else {
            return;
        };
        let n = cells.len().min(dst.len());
        dst[..n].copy_from_slice(&cells[..n]);
        let mut hints = RowFlags::DIRTY;
        if cells[..n]
            .iter()
            .any(|cell| cell.flags().contains(super::CellFlags::WIDE))
        {
            hints |= RowFlags::HAS_WIDE_CHARS;
        }
        self.flags |= hints;
    }

    /// Set a cell at the given column.
    #[inline]
    pub fn set(&mut self, col: u16, cell: Cell) -> bool {
        if let Some(c) = self.cells.get_mut(col as usize) {
            *c = cell;
            if col >= self.len && !cell.is_empty() {
                self.len = col.saturating_add(1);
            }
            let mut new_flags = RowFlags::DIRTY;
            if cell.flags().contains(super::CellFlags::WIDE) {
                new_flags |= RowFlags::HAS_WIDE_CHARS;
            }
            self.flags |= new_flags;
            self.note_write_through(usize::from(col) + 1);
            true
        } else {
            false
        }
    }

    /// Settle `len` after a bulk wide-char run written through [`Row::write_wide_char_packed_no_fixup`].
    pub fn settle_len_after_wide_run(&mut self, end_col: u16, end_half_blanked: bool) {
        self.note_write_through(usize::from(end_col));
        let old_len = usize::from(self.len);
        self.settle_len_after_write(old_len, usize::from(end_col), end_half_blanked);
    }

    fn recalculate_len_up_to(&mut self, end: usize) {
        let end = end.min(self.cells.len());
        self.len = self
            .cells
            .iter()
            .take(end)
            .rposition(|c| !c.is_empty())
            .map(|i| u16_from_usize(i) + 1)
            .unwrap_or(0);
    }

    /// Recompute `len` by scanning the entire row for the last non-empty cell.
    #[inline]
    pub(crate) fn recompute_len(&mut self) {
        self.recalculate_len_up_to(self.cells.len());
    }

    /// Resize the row to `new_cols`: new cells are empty, excess cells dropped.
    /// # Safety
    /// `pages` must outlive `self` after the resize, as for [`Row::new`].
    pub unsafe fn resize(&mut self, new_cols: u16, pages: &mut PageStore) {
        let old_cols = self.cols();
        if new_cols == old_cols {
            return;
        }

        let mut new_cells = pages.alloc_slice::<Cell>(new_cols);
        for cell in new_cells.iter_mut() {
            *cell = Cell::EMPTY;
        }
        let copy_len = (old_cols as usize).min(new_cols as usize);
        new_cells[..copy_len].copy_from_slice(&self.cells[..copy_len]);
        self.cells = new_cells;

        if self.len > new_cols {
            self.len = new_cols;
        }

        if new_cols < old_cols {
            let mut cleared = false;
            let last_col = new_cols.saturating_sub(1) as usize;
            if let Some(last_cell) = self.cells.get_mut(last_col)
                && last_cell.flags().contains(super::CellFlags::WIDE)
            {
                *last_cell = Cell::EMPTY;
                cleared = true;
            }
            if cleared {
                self.recalculate_len_up_to(self.cells.len());
            }
        }

        self.flags = (self.flags | RowFlags::DIRTY) - RowFlags::EARLY_WRAP_HOLE;
    }

    /// Get a mutable iterator over all cells.
    #[inline]
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Cell> {
        self.flags |= RowFlags::DIRTY;
        self.cells.iter_mut()
    }

    /// Get a slice of cells.
    #[must_use]
    #[inline]
    pub(crate) fn as_slice(&self) -> &[Cell] {
        &self.cells
    }

    /// Copy cells from another row.
    pub(crate) fn copy_from(&mut self, other: &Row) {
        let copy_len = self.cols().min(other.cols()) as usize;
        self.cells[..copy_len].copy_from_slice(&other.cells[..copy_len]);
        self.len = other.len.min(self.cols());
        self.flags = other.flags | RowFlags::DIRTY;
    }

    /// An owned copy of this row that no [`PageStore`] backs, for a holder that must outlive the grid's pages (the alt-screen resize undo: the grid replaces its store on a width change, an ED 3 and a ring eviction, so a kept `Row` would dangle).
    pub(crate) fn snapshot(&self) -> RowSnapshot {
        RowSnapshot {
            cells: self.cells.to_vec().into_boxed_slice(),
            len: self.len,
            flags: self.flags,
        }
    }

    /// Rebuild a row from a [`RowSnapshot`], `cols` wide (the snapshot is clamped or padded with empty cells when the widths differ).
    /// # Safety
    /// Same contract as [`Row::new`]: `pages` must outlive the returned row.
    pub(crate) unsafe fn from_snapshot(
        snapshot: &RowSnapshot,
        cols: u16,
        pages: &mut PageStore,
    ) -> Self {
        // SAFETY: forwarded from this function's contract.
        let mut row = unsafe { Self::new(cols, pages) };
        let copy_len = row.cells.len().min(snapshot.cells.len());
        row.cells[..copy_len].copy_from_slice(&snapshot.cells[..copy_len]);
        row.len = snapshot.len.min(row.cols());
        row.flags = snapshot.flags | RowFlags::DIRTY;
        row
    }
}

/// A row's cells, length and flags held outside any [`PageStore`] ([`Row::snapshot`]).
#[derive(Debug, Clone)]
pub(crate) struct RowSnapshot {
    cells: Box<[Cell]>,
    len: u16,
    flags: RowFlags,
}

impl RowSnapshot {
    #[inline]
    pub(crate) const fn heap_memory_used(&self) -> usize {
        self.cells.len().saturating_mul(std::mem::size_of::<Cell>())
    }
}

mod char_ops;
mod clear;
mod fmt;
mod write;

#[cfg(test)]
mod tests;
