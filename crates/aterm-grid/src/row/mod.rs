// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row storage for terminal grid.
//!
//! ## Design
//!
//! A Row is a contiguous array of cells with a length tracking the last
//! non-empty cell (for efficient iteration and rendering).
//!
//! Lines can be soft-wrapped (automatic) or hard-wrapped (explicit newline).
//!
//! ## Module organization
//!
//! - `write.rs`: Single/wide character write operations with wide-char fixup
//! - `char_ops.rs`: ICH (insert), DCH (delete), ECH (erase) operations
//! - `clear.rs`: Full/partial/selective row clearing

use super::cell::Cell;
use super::page::{PageSlice, PageStore};

#[inline]
fn u16_from_usize(value: usize) -> u16 {
    value.try_into().unwrap_or(u16::MAX)
}

/// A cell an early-wrap hole can still be: a blank (space) that is no half of
/// a wide glyph and no complex cluster. Its colours do not matter, since xterm
/// blanks the hole with the current background (BCE).
#[inline]
fn is_wrap_hole_blank(cell: &Cell) -> bool {
    use super::CellFlags;
    cell.char_data() == u16::from(b' ')
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
    /// Index of the last non-empty cell + 1 (for efficient iteration).
    /// If 0, the row is entirely empty.
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
        /// This row contains at least one WIDE (double-column) character.
        /// Set by write_wide_char; cleared by clear(). Enables
        /// fixup_wide_chars_in_range to skip the scan when no wide chars exist.
        const HAS_WIDE_CHARS = 1 << 5;
        /// This row contains at least one cell written with a StyleId
        /// (cell flag USES_STYLE_ID). Set by StyleId write paths and
        /// propagated by Row::set; cleared by clear()/erase(). Lets
        /// extract_row_extras skip the per-cell style_id scan on plain rows.
        const HAS_STYLE_ID = 1 << 6;
        /// The row's last cell is an early-wrap hole: a width-2 glyph that
        /// could not start there wrapped to the next row and left the cell
        /// unwritten (xterm blanks it with the current background). Ghostty
        /// calls the cell a `spacer_head`, Alacritty a
        /// `LEADING_WIDE_CHAR_SPACER`. It is not content: a rewrap that rejoins
        /// the row with its continuation drops this one cell and nothing else,
        /// so a space or glyph a program really wrote in the last column is
        /// kept ([`Row::has_early_wrap_hole`]).
        ///
        /// Set where a hole is made: the write path's early wrap
        /// (`Grid::blank_wide_wrap_tail`), the visible rewrap's chunker when it
        /// backs a chunk off a wide glyph, and a row filled from a history
        /// [`Line`](aterm_scrollback::Line) that ends one cell short (a line
        /// stores content only, so its hole is the cell it leaves unfilled).
        /// Cleared by a write into the last cell (a glyph, a wide glyph's
        /// spacer, a `Row::set`, a fill run, a DECFRA glyph) and by a shift of
        /// it (ICH/DCH), and by every whole-row clear/erase/reset; a width
        /// change without rewrap drops it. An erase of the cell (DECERA, or
        /// ECH and EL, which also end the soft wrap there) keeps it: the hole
        /// stays a hole, blanked again.
        const EARLY_WRAP_HOLE = 1 << 7;
        /// Mask of DEC line attribute flags (DECDWL/DECDHL).
        ///
        /// Erase operations clear character content but keep these line
        /// attributes, per VT420/VT510 and xterm.
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
    ///
    /// # Safety
    ///
    /// `pages` must outlive the returned row. If the row is stored inside
    /// another owner, that owner must not outlive the backing page store.
    #[must_use]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
    /// Snapshots/extras must already own any content that survives the removal.
    ///
    /// # Safety
    ///
    /// `pages` must be the live PageStore that allocated this row's cells.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "ResizeRowReuse",
            action = "Shrink",
            project = "grid::tests::resize_row_reuse::resize_row_reuse_conforms"
        )
    )]
    pub(crate) unsafe fn recycle(self, pages: &mut PageStore) {
        // SAFETY: the caller supplies the originating arena; consuming the Row
        // transfers its non-Clone PageSlice and leaves no row owning the range.
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

    /// Check if this row contains any cell with a `StyleId`.
    #[must_use]
    #[inline]
    pub fn has_style_id(&self) -> bool {
        self.flags.contains(RowFlags::HAS_STYLE_ID)
    }

    /// Get the page ID for this row's cell storage.
    ///
    /// Used for pin invalidation tracking.
    #[must_use]
    #[inline]
    pub(crate) fn page_id(&self) -> super::page::PageId {
        self.cells.page_id()
    }

    /// Get the length (last non-empty cell + 1).
    #[must_use]
    #[inline]
    pub fn len(&self) -> u16 {
        self.len
    }

    /// Check if the row is empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get row flags.
    #[must_use]
    #[inline]
    pub fn flags(&self) -> RowFlags {
        self.flags
    }

    /// Check if this row is a continuation of the previous (soft wrap).
    #[must_use]
    #[inline]
    pub fn is_wrapped(&self) -> bool {
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

    /// Is this row's last cell an early-wrap hole
    /// ([`RowFlags::EARLY_WRAP_HOLE`])? The flag must be set and the cell must
    /// still hold the blank the wrap left: a glyph written there since (by any
    /// path) makes it content again.
    #[must_use]
    #[inline]
    pub fn has_early_wrap_hole(&self) -> bool {
        self.flags.contains(RowFlags::EARLY_WRAP_HOLE)
            && self.cells.last().is_some_and(is_wrap_hole_blank)
    }

    /// Record (or forget) that this row's last cell is an early-wrap hole.
    /// Not a visual change, so the row is not marked dirty.
    #[inline]
    pub(crate) fn set_early_wrap_hole(&mut self, hole: bool) {
        if hole {
            self.flags |= RowFlags::EARLY_WRAP_HOLE;
        } else {
            self.flags -= RowFlags::EARLY_WRAP_HOLE;
        }
    }

    /// How many cells this row contributes to its logical line when the next
    /// row continues it (soft wrap): autowrap filled it to its last column, so
    /// every cell is content, blanks included, except a recorded early-wrap
    /// hole. Single-width rows only; the caller checks the continuation.
    #[must_use]
    #[inline]
    pub(crate) fn continued_len(&self) -> u16 {
        self.cols()
            .saturating_sub(u16::from(self.has_early_wrap_hole()))
    }

    /// A write that lands on the last cell (`end`, exclusive, reaches the row
    /// end) fills an early-wrap hole: the cell is content from now on.
    #[inline]
    fn note_write_through(&mut self, end: usize) {
        if end >= self.cells.len() {
            self.flags -= RowFlags::EARLY_WRAP_HOLE;
        }
    }

    /// Check if this row is dirty (needs re-render).
    #[cfg(test)]
    #[must_use]
    #[inline]
    fn is_dirty(&self) -> bool {
        self.flags.contains(RowFlags::DIRTY)
    }

    /// Get the current line size attribute.
    #[must_use]
    #[inline]
    pub fn line_size(&self) -> LineSize {
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
    ///
    /// Returns None if column is out of bounds.
    #[must_use]
    #[inline]
    pub fn get(&self, col: u16) -> Option<&Cell> {
        self.cells.get(col as usize)
    }

    /// Get a mutable cell at the given column.
    ///
    /// Returns None if column is out of bounds.
    #[must_use]
    #[inline]
    pub fn get_mut(&mut self, col: u16) -> Option<&mut Cell> {
        self.cells.get_mut(col as usize)
    }

    /// Get a cell at the given column (unchecked).
    ///
    /// # Safety
    ///
    /// Column must be less than cols().
    #[must_use]
    #[inline]
    pub unsafe fn get_unchecked(&self, col: u16) -> &Cell {
        debug_assert!((col as usize) < self.cells.len());
        // SAFETY: Caller guarantees col < cols()
        unsafe { self.cells.get_unchecked(col as usize) }
    }

    /// Get a mutable cell at the given column (unchecked).
    ///
    /// # Safety
    ///
    /// Column must be less than cols().
    #[must_use]
    #[inline]
    pub unsafe fn get_unchecked_mut(&mut self, col: u16) -> &mut Cell {
        debug_assert!((col as usize) < self.cells.len());
        // SAFETY: Caller guarantees col < cols()
        unsafe { self.cells.get_unchecked_mut(col as usize) }
    }

    /// Get mutable access to all cells in this row.
    ///
    /// This is the fast path for bulk cell writes (e.g., ASCII blast).
    /// After modifying cells, call `update_len()` if the content length changed.
    #[cfg(miri)]
    #[must_use]
    #[inline]
    pub fn cells_mut(&mut self) -> &mut [Cell] {
        self.flags |= RowFlags::DIRTY;
        &mut self.cells[..]
    }

    /// Get mutable access to a cell range after clearing any orphaned wide-char halves.
    ///
    /// Returns a clipped slice when `count` would extend past the row end.
    /// Returns `None` only when `start_col` is past the row bounds.
    #[must_use]
    #[inline]
    pub fn cells_mut_with_fixup(&mut self, start_col: u16, count: u16) -> Option<&mut [Cell]> {
        let start = usize::from(start_col);
        if start > self.cells.len() {
            return None;
        }

        let old_len = self.len as usize;
        let had_wide = self.flags.contains(RowFlags::HAS_WIDE_CHARS);
        if had_wide {
            self.fixup_wide_chars_in_range(start_col, count);
        }
        let end = start
            .saturating_add(usize::from(count))
            .min(self.cells.len());
        if end > start {
            self.flags |= RowFlags::DIRTY;
            self.note_write_through(end);
        }
        // `fixup_wide_chars_in_range` can clear a wide continuation at `end`,
        // one past the returned slice, when the last covered cell is a WIDE
        // head. Bulk callers only grow len via update_len(), so if that orphan
        // was the tail content len would stay stale-high (a phantom trailing
        // space in row_text/search/scrollback). Tighten here; the caller's
        // update_len() still grows len for what it writes. Gated on had_wide so
        // the ASCII path stays branch-free; recalculate_len_up_to never
        // over-shrinks.
        if had_wide && old_len > 0 && self.cells[old_len - 1].is_empty() {
            self.recalculate_len_up_to(old_len);
        }
        Some(&mut self.cells[start..end])
    }

    /// Update the row's len to include all written content up to `end_col`.
    ///
    /// Call this after using `cells_mut()` to write content at positions up to `end_col - 1`.
    /// This ensures `visible_content()` and iteration include the new content.
    #[inline]
    pub fn update_len(&mut self, end_col: u16) {
        if end_col > self.len {
            self.len = end_col.min(self.cols());
        }
    }

    /// Set a cell at the given column.
    ///
    /// Returns true if successful, false if out of bounds.
    #[inline]
    pub fn set(&mut self, col: u16, cell: Cell) -> bool {
        if let Some(c) = self.cells.get_mut(col as usize) {
            *c = cell;
            // Update len if we wrote past the current end
            if col >= self.len && !cell.is_empty() {
                self.len = col.saturating_add(1);
            }
            let mut new_flags = RowFlags::DIRTY;
            // Propagate HAS_WIDE_CHARS so fixup_wide_char_overwrite is not
            // bypassed after reflow copies wide cells via this method.
            if cell.flags().contains(super::CellFlags::WIDE) {
                new_flags |= RowFlags::HAS_WIDE_CHARS;
            }
            // Propagate HAS_STYLE_ID so extract_row_extras can short-circuit on
            // plain rows even after reflow/checkpoint restore copies
            // style-interned cells via Row::set.
            if cell.flags().contains(super::CellFlags::USES_STYLE_ID) {
                new_flags |= RowFlags::HAS_STYLE_ID;
            }
            self.flags |= new_flags;
            self.note_write_through(usize::from(col) + 1);
            true
        } else {
            false
        }
    }

    /// Settle `len` after a bulk wide-char run written through
    /// [`Row::write_wide_char_packed_no_fixup`].
    ///
    /// A wide write can orphan an existing wide char's continuation; the
    /// fixup clears it, and if it was the row's last content `len` must shrink
    /// or a phantom trailing space shows in `row_text`, search and scrollback.
    /// [`Row::update_len`] only grows, so run lanes call this instead.
    ///
    /// Once per run equals once per glyph: glyphs advance left to right over
    /// cells the run owns, and `recalculate_len_up_to` rescans, reaching the
    /// same answer from the final state.
    ///
    /// The wide-run counterpart of the rule [`Row::cells_mut_with_fixup`]
    /// applies for narrow bulk writes; the wide lanes call
    /// `fixup_wide_chars_in_range` directly.
    pub fn settle_len_after_wide_run(&mut self, end_col: u16) {
        // The run's unchecked writes skip `note_write_through`; a run that
        // reached the last cell wrote over any early-wrap hole there.
        self.note_write_through(usize::from(end_col));
        self.update_len(end_col);
        let len = self.len as usize;
        if len > 0 && self.cells.get(len - 1).is_some_and(Cell::is_empty) {
            self.recalculate_len_up_to(len);
        }
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
    ///
    /// For callers writing cells via [`Row::set`] (grow-only len) that may
    /// have overwritten the tail with blanks, e.g. DECCRA `copy_rect` copying
    /// source blanks and blanking orphaned wide halves; otherwise the emptied
    /// tail stays inside `len` (phantom trailing spaces). Never over-shrinks.
    #[inline]
    pub(crate) fn recompute_len(&mut self) {
        self.recalculate_len_up_to(self.cells.len());
    }

    /// Resize the row to a new column count.
    ///
    /// If growing, new cells are empty.
    /// If shrinking, excess cells are discarded.
    ///
    /// # Safety
    ///
    /// `pages` must outlive `self` after the resize completes. If `self` is
    /// stored inside another owner, that owner must not outlive the backing
    /// page store.
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

        // Clamp len
        if self.len > new_cols {
            self.len = new_cols;
        }

        if new_cols < old_cols {
            let mut cleared = false;
            let last_col = new_cols.saturating_sub(1) as usize;
            if let Some(last_cell) = self.cells.get_mut(last_col)
                && last_cell.flags().contains(super::CellFlags::WIDE)
            {
                // Wide char at last column can't display its second cell,
                // replace with space to maintain valid grid state
                *last_cell = Cell::EMPTY;
                cleared = true;
            }
            // WIDE_CONTINUATION at position 0 is not cleared: a true spacer
            // would sit at position 1 (after WIDE at 0), and the bit is shared
            // with PROTECTED, so clearing it would corrupt protected cells.
            if cleared {
                self.recalculate_len_up_to(self.cells.len());
            }
        }

        // The old last cell is no longer the last: a recorded early-wrap hole
        // names a cell this width does not end on.
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
    #[cfg(miri)]
    pub fn copy_from(&mut self, other: &Row) {
        let copy_len = self.cols().min(other.cols()) as usize;
        self.cells[..copy_len].copy_from_slice(&other.cells[..copy_len]);
        self.len = other.len.min(self.cols());
        self.flags = other.flags | RowFlags::DIRTY;
    }

    /// Copy cells from another row.
    #[cfg(not(miri))]
    pub(crate) fn copy_from(&mut self, other: &Row) {
        let copy_len = self.cols().min(other.cols()) as usize;
        self.cells[..copy_len].copy_from_slice(&other.cells[..copy_len]);
        self.len = other.len.min(self.cols());
        self.flags = other.flags | RowFlags::DIRTY;
    }

    /// An owned copy of this row that no [`PageStore`] backs, for a holder that
    /// must outlive the grid's pages (the alt-screen resize undo: the grid
    /// replaces its store on a width change, an ED 3 and a ring eviction, so a
    /// kept `Row` would dangle).
    pub(crate) fn snapshot(&self) -> RowSnapshot {
        RowSnapshot {
            cells: self.cells.to_vec().into_boxed_slice(),
            len: self.len,
            flags: self.flags,
        }
    }

    /// Rebuild a row from a [`RowSnapshot`], `cols` wide (the snapshot is
    /// clamped or padded with empty cells when the widths differ).
    ///
    /// # Safety
    ///
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

/// A row's cells, length and flags held outside any [`PageStore`]
/// ([`Row::snapshot`]).
#[derive(Debug, Clone)]
pub(crate) struct RowSnapshot {
    cells: Box<[Cell]>,
    len: u16,
    flags: RowFlags,
}

impl RowSnapshot {
    pub(crate) fn heap_memory_used(&self) -> usize {
        self.cells.len().saturating_mul(std::mem::size_of::<Cell>())
    }
}

mod char_ops;
mod clear;
mod fmt;
mod write;

#[cfg(test)]
mod style_id_write;

#[cfg(test)]
mod tests;
