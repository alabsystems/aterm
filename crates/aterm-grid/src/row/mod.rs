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
        /// propagated by Row::set; cleared by clear()/erase(). Enables
        /// extract_row_extras to skip the per-cell style_id scan on plain-text
        /// rows even when other rows in the grid use style interning (#7872).
        const HAS_STYLE_ID = 1 << 6;
        /// Mask of DEC line attribute flags (DECDWL/DECDHL).
        ///
        /// Erase operations clear character content but must preserve these
        /// line attributes per VT420/VT510 spec and xterm behavior (#7497).
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
    pub unsafe fn new(cols: u16, pages: &mut PageStore) -> Self {
        let mut cells = pages.alloc_slice::<Cell>(cols);
        for cell in cells.iter_mut() {
            *cell = Cell::EMPTY;
        }
        Self {
            cells,
            len: 0,
            flags: RowFlags::DIRTY,
        }
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

    /// Mark this row as containing at least one cell with a `StyleId`.
    ///
    /// Set on any cell write whose `CellFlags::USES_STYLE_ID` bit is set so
    /// `extract_row_extras` can skip the per-cell scan on style-free rows
    /// (#7872). Cleared by [`Row::clear`] and [`Row::erase`].
    #[inline]
    pub fn mark_has_style_id(&mut self) {
        self.flags |= RowFlags::DIRTY | RowFlags::HAS_STYLE_ID;
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

    /// Restore serializable row flags from a checkpoint byte.
    ///
    /// Sets WRAPPED, DOUBLE_WIDTH, DOUBLE_HEIGHT_TOP, DOUBLE_HEIGHT_BOTTOM,
    /// and HAS_WIDE_CHARS from the serialized bits. DIRTY is always set
    /// (restored rows need re-render). Internal-only flags not present in
    /// the serialized byte are unaffected.
    ///
    /// This avoids `set_line_size()` which has side effects (clearing the
    /// second half of cells for double-width rows) that would destroy
    /// already-deserialized cell data.
    #[inline]
    pub fn restore_checkpoint_flags(&mut self, bits: u8) {
        // Mask to only the flags that are serialized (exclude DIRTY).
        const SERIALIZABLE: u8 = RowFlags::WRAPPED.bits()
            | RowFlags::DOUBLE_WIDTH.bits()
            | RowFlags::DOUBLE_HEIGHT_TOP.bits()
            | RowFlags::DOUBLE_HEIGHT_BOTTOM.bits()
            | RowFlags::HAS_WIDE_CHARS.bits();
        // Clear serializable flags, then set from checkpoint bits.
        self.flags -= RowFlags::from_bits_retain(SERIALIZABLE);
        self.flags |= RowFlags::from_bits_retain(bits & SERIALIZABLE);
        // Restored rows always need re-render.
        self.flags |= RowFlags::DIRTY;
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
        self.fixup_wide_chars_in_range(start_col, count);
        let end = start
            .saturating_add(usize::from(count))
            .min(self.cells.len());
        if end > start {
            self.flags |= RowFlags::DIRTY;
        }
        // #7522 (write/fill path): fixup_wide_chars_in_range can clear a wide
        // continuation at index `end` — ONE column past the returned slice — when
        // the last covered cell is a WIDE head. Bulk callers only GROW len via
        // update_len() and never see that past-range clear, so if the orphan was
        // the row's tail content len is left stale-high (phantom trailing space in
        // row_text/search/scrollback). Tighten here when a fixup emptied the old
        // tail; the caller's subsequent update_len() still grows len for any
        // content it writes into the range. Gated on had_wide (fixup only clears
        // when HAS_WIDE_CHARS) so the ASCII-blast hot path stays branch-free, and
        // recalculate_len_up_to never over-shrinks (never below real content).
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
            // Propagate HAS_STYLE_ID so extract_row_extras can short-circuit
            // on plain-text rows even after reflow/checkpoint restore copies
            // style-interned cells via Row::set (#7872).
            if cell.flags().contains(super::CellFlags::USES_STYLE_ID) {
                new_flags |= RowFlags::HAS_STYLE_ID;
            }
            self.flags |= new_flags;
            true
        } else {
            false
        }
    }

    /// Recalculate the len field by scanning up to `end`.
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
    /// For callers that write cells via [`Row::set`] (whose len update is
    /// grow-only) and may have overwritten the row's tail with blanks — e.g.
    /// DECCRA `copy_rect`, which copies verbatim source blanks and blanks
    /// orphaned wide halves. `set` never shrinks len, so such a caller must
    /// recompute the true content extent afterward or the emptied tail is left
    /// inside `len` (#7522 stale-high → phantom trailing spaces). Never
    /// over-shrinks (it finds real content anywhere in the row).
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
            // Note: We don't clear WIDE_CONTINUATION at position 0 because:
            // 1. A true spacer cell would be at position 1 (following WIDE at 0)
            // 2. WIDE_CONTINUATION shares bit with PROTECTED, so we'd incorrectly
            //    clear protected cells (see #1286)
            if cleared {
                self.recalculate_len_up_to(self.cells.len());
            }
        }

        self.flags |= RowFlags::DIRTY;
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
}

mod char_ops;
mod clear;
mod fmt;
mod write;

#[cfg(any(test, kani, feature = "testing"))]
mod style_id_write;

#[cfg(test)]
mod tests;

#[cfg(kani)]
mod kani_proofs;
