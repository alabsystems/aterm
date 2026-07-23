// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

// =============================================================================
// Kani Stub Constructors
// =============================================================================
//
// These constructors are optimized for Kani/CBMC verification by avoiding
// loops with symbolic bounds that cause state explosion.

#[cfg(kani)]
impl Row {
    /// Create a row for Kani verification without cell initialization loop.
    ///
    /// Unlike `Row::new`, this skips the `for cell in cells.iter_mut()` loop
    /// that initializes cells to `Cell::EMPTY`. The allocated page memory is
    /// already zeroed, which is sufficient for property verification.
    ///
    /// # Usage
    ///
    /// Use this in Kani proofs instead of `Row::new` when you don't need
    /// initialized cell content (e.g., cursor/bounds proofs).
    #[must_use]
    pub(crate) fn kani_stub(cols: u16, pages: &mut PageStore) -> Self {
        // Allocate cells without initialization loop.
        // PageStore provides zeroed memory, so cells contain zero bytes.
        // For verification, we only care about bounds and invariants,
        // not actual cell content.
        let cells = pages.alloc_slice::<Cell>(cols);
        Self {
            cells,
            len: 0,
            flags: RowFlags::DIRTY,
        }
    }

    /// Create a Row from a raw mutable Cell slice.
    ///
    /// This avoids PageStore allocation entirely, providing O(1) construction
    /// for Kani verification. The slice is typically backed by a static array.
    ///
    /// # Arguments
    ///
    /// * `cells_slice` - Mutable reference to a Cell slice
    ///
    /// # Safety
    ///
    /// The slice must remain valid for the lifetime of the Row.
    /// Only used in Kani proofs where lifetime is bounded by proof execution.
    #[must_use]
    pub(crate) fn kani_mock(cells_slice: &mut [Cell]) -> Self {
        let cells = PageSlice::from_raw(cells_slice);
        Self {
            cells,
            len: 0,
            flags: RowFlags::DIRTY,
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::super::super::CellFlags;
    use super::super::super::PackedColor;
    use super::*;

    /// write_char_styled uses unsafe get_unchecked after a bounds guard.
    /// Verify that for all symbolic col values, the method either succeeds
    /// (in-bounds) or returns false (out-of-bounds) without UB.
    #[kani::proof]
    fn row_write_char_styled_bounds_safe() {
        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;

        let result = row.write_char_styled(col, 'A', fg, bg, CellFlags::empty());

        if col < 8 {
            kani::assert(result, "write_char_styled returns true for in-bounds col");
        } else {
            kani::assert(
                !result,
                "write_char_styled returns false for out-of-bounds col",
            );
        }
    }

    /// write_wide_char uses unsafe get_unchecked for the primary cell and
    /// continuation. Verify it returns true (success) or false (reject) without UB.
    #[kani::proof]
    fn row_write_wide_char_bounds_safe() {
        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;

        let ok = row.write_wide_char(col, '\u{4E00}', fg, bg, CellFlags::empty());

        if col < 7 {
            kani::assert(ok, "write_wide_char returns true for in-bounds col");
        } else {
            kani::assert(!ok, "write_wide_char returns false for out-of-bounds col");
        }
    }

    /// clear_range with symbolic (start, end) pairs on a real row.
    /// Verify no panic and that row length remains bounded.
    #[kani::proof]
    #[kani::unwind(10)]
    fn row_clear_range_bounds_safe() {
        let start: u16 = kani::any();
        let end: u16 = kani::any();
        kani::assume(start <= 8 && end <= 8);

        let mut cells = [Cell::EMPTY; 8];
        for cell in cells.iter_mut() {
            cell.set_char('X');
        }
        let mut row = Row::kani_mock(&mut cells);
        row.len = 8;

        row.clear_range(start, end);

        kani::assert(
            (row.len as usize) <= 8,
            "len stays within row width after clear_range",
        );
    }

    /// insert_chars shifts cells right using copy_within. Verify no panic
    /// or UB and row length remains bounded for all symbolic (col, count).
    #[kani::proof]
    #[kani::unwind(10)]
    fn row_insert_chars_shift_bounds_safe() {
        let col: u16 = kani::any();
        let count: u16 = kani::any();
        kani::assume(col <= 8);
        kani::assume(count <= 8);

        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);
        row.write_char(0, 'A');
        row.write_char(1, 'B');

        row.insert_chars(col, count);

        kani::assert(
            (row.len as usize) <= 8,
            "len stays within row width after insert_chars",
        );
    }

    /// write_char (safe API) returns correct success/failure for all symbolic
    /// col values and updates len correctly on success.
    #[kani::proof]
    fn row_write_char_safe_api_bounds() {
        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let result = row.write_char(col, 'Z');

        if col < 8 {
            kani::assert(result, "write_char returns true for in-bounds col");
            kani::assert(
                row.len >= col + 1,
                "len is at least col+1 after successful write",
            );
        } else {
            kani::assert(!result, "write_char returns false for out-of-bounds col");
            kani::assert(row.len == 0, "len unchanged after failed write");
        }
    }

    /// write_wide_char with pre-existing wide chars exercises the unsafe
    /// fixup_wide_char_write path when overwriting wide character halves.
    #[kani::proof]
    fn row_write_wide_char_overlap_bounds_safe() {
        let col: u16 = kani::any();
        kani::assume(col <= 8);

        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;

        // Place a wide char at col 2 to create WIDE + WIDE_CONTINUATION cells
        row.write_wide_char(2, '\u{4E00}', fg, bg, CellFlags::empty());

        // Overwrite at symbolic col — may overlap the existing wide char
        let _ok = row.write_wide_char(col, '\u{4E8C}', fg, bg, CellFlags::empty());

        kani::assert(
            (row.len as usize) <= 8,
            "len stays within row width after overlapping wide write",
        );
    }

    // =========================================================================
    // StyleId write method proofs
    // =========================================================================
    //
    // These prove the unsafe get_unchecked / get_unchecked_mut calls in
    // style_id_write.rs are guarded by correct bounds checks.

    /// write_char_with_style_id uses unsafe get_unchecked after a bounds
    /// guard. Verify that for all symbolic col values, the method either
    /// succeeds (in-bounds) or returns false (out-of-bounds) without UB,
    /// and that len is updated correctly on success.
    #[kani::proof]
    fn row_write_char_with_style_id_bounds_safe() {
        use super::super::super::style::StyleId;

        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let style_id = StyleId::new(0);
        let result = row.write_char_with_style_id(col, 'Z', style_id, CellFlags::empty());

        if col < 8 {
            kani::assert(
                result,
                "write_char_with_style_id returns true for in-bounds col",
            );
            kani::assert(
                row.len >= col + 1,
                "len is at least col+1 after successful style_id write",
            );
        } else {
            kani::assert(
                !result,
                "write_char_with_style_id returns false for out-of-bounds col",
            );
            kani::assert(row.len == 0, "len unchanged after failed style_id write");
        }
    }

    /// write_wide_char_with_style_id uses unsafe get_unchecked for the
    /// primary cell and continuation. Verify correct success/failure for
    /// all symbolic col values and that len stays bounded.
    #[kani::proof]
    fn row_write_wide_char_with_style_id_bounds_safe() {
        use super::super::super::style::StyleId;

        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let style_id = StyleId::new(0);
        let ok = row.write_wide_char_with_style_id(col, '\u{4E00}', style_id, CellFlags::empty());

        if col < 7 {
            kani::assert(
                ok,
                "write_wide_char_with_style_id returns true for in-bounds col",
            );
            kani::assert(
                row.len >= col + 2,
                "len is at least col+2 after successful wide style_id write",
            );
        } else {
            kani::assert(
                !ok,
                "write_wide_char_with_style_id returns false for out-of-bounds col",
            );
        }
    }

    /// write_wide_char_with_style_id with pre-existing wide chars exercises
    /// the unsafe fixup path when overwriting wide character halves via the
    /// StyleId API.
    #[kani::proof]
    fn row_write_wide_char_with_style_id_overlap_safe() {
        use super::super::super::style::StyleId;

        let col: u16 = kani::any();
        kani::assume(col <= 8);

        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let style_id = StyleId::new(0);

        // Place a wide char at col 2 to create WIDE + WIDE_CONTINUATION cells
        row.write_wide_char_with_style_id(2, '\u{4E00}', style_id, CellFlags::empty());

        // Overwrite at symbolic col — may overlap the existing wide char
        let _ok = row.write_wide_char_with_style_id(col, '\u{4E8C}', style_id, CellFlags::empty());

        kani::assert(
            (row.len as usize) <= 8,
            "len stays within row width after overlapping wide style_id write",
        );
    }

    // =========================================================================
    // Direct get_unchecked / get_unchecked_mut proofs
    // =========================================================================
    //
    // These prove the unsafe unchecked accessors return the same result as
    // the safe checked API when the precondition (col < cols()) holds.

    /// For all symbolic col values satisfying col < cols(), verify that
    /// `get_unchecked(col)` returns the same cell as `get(col).unwrap()`.
    #[kani::proof]
    fn row_get_unchecked_equivalence() {
        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;

        // Write some content so cells aren't all identical
        row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
        row.write_char_styled(3, 'B', fg, bg, CellFlags::empty());
        row.write_char_styled(7, 'C', fg, bg, CellFlags::empty());

        kani::assume(col < row.cols());

        let safe_result = row.get(col).unwrap();
        // SAFETY: col < row.cols() is guaranteed by kani::assume above
        let unchecked_result = unsafe { row.get_unchecked(col) };

        kani::assert(
            core::ptr::eq(safe_result, unchecked_result),
            "get_unchecked must return a reference to the same cell as get().unwrap()",
        );
        kani::assert(
            *safe_result == *unchecked_result,
            "get_unchecked must return the same cell as get().unwrap()",
        );
    }

    /// For all symbolic col values satisfying col < cols(), verify that
    /// `get_unchecked_mut(col)` returns a mutable reference to the same
    /// cell as `get_mut(col).unwrap()`.
    #[kani::proof]
    fn row_get_unchecked_mut_equivalence() {
        let col: u16 = kani::any();
        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;

        row.write_char_styled(1, 'X', fg, bg, CellFlags::empty());
        row.write_char_styled(5, 'Y', fg, bg, CellFlags::empty());

        kani::assume(col < row.cols());

        let safe_ptr = {
            let safe_ref = row.get(col).unwrap();
            safe_ref as *const Cell
        };

        // SAFETY: col < row.cols() is guaranteed by kani::assume above
        let unchecked_ref = unsafe { row.get_unchecked_mut(col) };
        let unchecked_ptr = unchecked_ref as *mut Cell as *const Cell;

        kani::assert(
            unchecked_ptr == safe_ptr,
            "get_unchecked_mut must point to the same cell as get().unwrap()",
        );
        unchecked_ref.set_char('Z');
        kani::assert(
            row.get(col).unwrap().char() == 'Z',
            "mutation through get_unchecked_mut must be visible through the safe API",
        );
    }

    // =========================================================================
    // Row::new initialization proof
    // =========================================================================

    /// Verify that Row::new initializes all cells to Cell::EMPTY,
    /// sets len to 0, and cols() equals the requested width.
    #[kani::proof]
    #[kani::unwind(10)]
    fn row_new_initializes_all_cells_empty() {
        let cols: u16 = kani::any();
        kani::assume(cols > 0 && cols <= 8);

        let mut pages = PageStore::new();
        // SAFETY: pages outlives row within this proof scope
        let row = unsafe { Row::new(cols, &mut pages) };

        kani::assert(row.cols() == cols, "cols() must equal requested width");
        kani::assert(row.len() == 0, "len must be 0 after construction");

        let slice = row.as_slice();
        let mut i: usize = 0;
        while i < cols as usize {
            kani::assert(
                slice[i] == Cell::EMPTY,
                "every cell must be Cell::EMPTY after Row::new",
            );
            i += 1;
        }
    }

    /// Verify that Row::resize preserves content up to min(old, new) cols,
    /// clamps len to new_cols, and sets cols() to new_cols.
    #[kani::proof]
    #[kani::unwind(10)]
    fn row_resize_preserves_content_and_clamps_len() {
        let new_cols: u16 = kani::any();
        kani::assume(new_cols > 0 && new_cols <= 8);

        let mut pages = PageStore::new();
        // Start with a 4-column row with known content
        // SAFETY: pages outlives row within this proof scope
        let mut row = unsafe { Row::new(4, &mut pages) };

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;
        row.write_char_styled(0, 'A', fg, bg, CellFlags::empty());
        row.write_char_styled(1, 'B', fg, bg, CellFlags::empty());
        row.write_char_styled(2, 'C', fg, bg, CellFlags::empty());
        row.write_char_styled(3, 'D', fg, bg, CellFlags::empty());

        // Capture pre-resize content for preservation check
        let pre_0 = *row.get(0).unwrap();
        let pre_1 = *row.get(1).unwrap();
        let pre_2 = *row.get(2).unwrap();
        let pre_3 = *row.get(3).unwrap();

        // SAFETY: pages outlives row within this proof scope
        unsafe { row.resize(new_cols, &mut pages) };

        kani::assert(
            row.cols() == new_cols,
            "cols() must equal new_cols after resize",
        );
        kani::assert(
            row.len() == new_cols.min(4),
            "len must clamp to min(old_len, new_cols) for this fully populated row",
        );

        let mut col: usize = 0;
        while col < new_cols as usize {
            let cell = *row.get(col as u16).unwrap();
            if col == 0 {
                kani::assert(cell == pre_0, "cell 0 must be preserved after resize");
            } else if col == 1 {
                kani::assert(cell == pre_1, "cell 1 must be preserved after resize");
            } else if col == 2 {
                kani::assert(cell == pre_2, "cell 2 must be preserved after resize");
            } else if col == 3 {
                kani::assert(cell == pre_3, "cell 3 must be preserved after resize");
            } else {
                kani::assert(cell == Cell::EMPTY, "grown cells must be initialized empty");
            }
            col += 1;
        }
    }

    /// fixup_wide_chars_in_range iterates a cell range accessing cells[col]
    /// and cells[col+1]. Verify no panic with pre-existing wide characters
    /// and symbolic (start_col, count) inputs.
    #[kani::proof]
    #[kani::unwind(9)]
    fn row_fixup_wide_chars_in_range_bounds_safe() {
        let start_col: u16 = kani::any();
        let count: u16 = kani::any();
        kani::assume(start_col <= 8);
        kani::assume(count <= 8);

        let mut cells = [Cell::EMPTY; 8];
        let mut row = Row::kani_mock(&mut cells);

        let fg = PackedColor::DEFAULT_FG;
        let bg = PackedColor::DEFAULT_BG;

        // Place wide chars to exercise all fixup paths
        row.write_wide_char(0, '\u{4E00}', fg, bg, CellFlags::empty());
        row.write_wide_char(4, '\u{4E8C}', fg, bg, CellFlags::empty());

        row.fixup_wide_chars_in_range(start_col, count);

        // Row width preserved (fixup must not corrupt the cell slice)
        kani::assert(row.cols() == 8, "fixup must not change row width");

        // Verify no orphaned WIDE char at the left boundary:
        // If start_col > 0, cells[start_col - 1] must NOT be WIDE unless
        // cells[start_col] is still its WIDE_CONTINUATION partner.
        let start = start_col as usize;
        let cells = row.as_slice();
        if start > 0 && start < 8 {
            let pre = cells[start - 1].flags();
            if pre.contains(CellFlags::WIDE) {
                kani::assert(
                    cells[start].flags().contains(CellFlags::WIDE_CONTINUATION),
                    "WIDE at left boundary must have continuation intact after fixup",
                );
            }
        }

        // Verify no orphaned WIDE_CONTINUATION at the right boundary:
        // If actual_end < 8, cells[actual_end] must NOT be WIDE_CONTINUATION
        // unless cells[actual_end - 1] is its WIDE partner.
        let end = (start_col.saturating_add(count) as usize).min(8);
        if end > 0 && end < 8 {
            let post = cells[end].flags();
            if post.contains(CellFlags::WIDE_CONTINUATION) {
                kani::assert(
                    cells[end - 1].flags().contains(CellFlags::WIDE),
                    "WIDE_CONTINUATION at right boundary must have WIDE partner after fixup",
                );
            }
        }
    }
}
