// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! MIRI-only memory safety tests for aterm-grid.
//!
//! These tests exercise the unsafe paths in PageStore, PageSlice, and Row
//! directly (without the terminal FFI layer), catching undefined behavior
//! that unit tests and Kani cannot detect at runtime.
//!
//! Run with:
//!   cargo +nightly miri test -p aterm-grid --test grid_miri -- --test-threads=1
//!
//! Covers:
//! - PageStore allocation, free-list recycling, lazy zeroing
//! - Row::new / Row::resize PageSlice lifecycle
//! - Row write_char / write_char_styled unchecked access paths
//! - Row write_wide_char dual-cell unchecked write
//! - Wide char overwrite cleanup (fixup_wide_char_overwrite)
//! - Multi-row aliasing: writes to one row must not corrupt another
//! - Grid::resize reflow (PageSlice replacement under reallocation)
//!
//! Part of memory_verification phase: standalone aterm-grid MIRI coverage.

#![cfg(miri)]

use aterm_grid::{Cell, CellFlags, Grid, PackedColor, PageStore, Row};

// =========================================================================
// PageStore allocation and recycling
// =========================================================================

/// Allocate rows, drop them, reallocate — exercises PageSlice pointer
/// validity when new allocations share the same PageStore after prior
/// PageSlice references are dropped.
#[test]
fn miri_page_store_alloc_reuse() {
    let mut pages = PageStore::new();

    // Phase 1: Allocate rows that fill pages
    let mut rows: Vec<Row> = Vec::new();
    for _ in 0..8 {
        // SAFETY: `pages` outlives all rows in this scope.
        rows.push(unsafe { Row::new(80, &mut pages) });
    }

    // Write to each row to exercise the page data
    for (i, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + (i as u8 % 26));
        row.write_char(0, c);
        assert_eq!(row.get(0).unwrap().char(), c);
    }

    // Phase 2: Drop all row handles (pages remain in PageStore)
    drop(rows);

    // Phase 3: Allocate new rows from the same PageStore
    let mut rows2: Vec<Row> = Vec::new();
    for _ in 0..8 {
        // SAFETY: `pages` outlives all rows in this scope.
        rows2.push(unsafe { Row::new(80, &mut pages) });
    }

    // New cells should be initialized to empty (space)
    for row in &rows2 {
        for col in 0..80 {
            let cell = row.get(col).unwrap();
            assert_eq!(cell.char(), ' ', "new cell should be space");
        }
    }
}

/// Preheat allocates pages into the free list; subsequent alloc reuses them.
#[test]
fn miri_page_store_preheat_reuse() {
    let mut pages = PageStore::new();
    pages.preheat(2);

    // Allocate rows — these should come from preheated pages
    // SAFETY: `pages` outlives the row.
    let mut row = unsafe { Row::new(40, &mut pages) };
    row.write_char(0, 'P');
    assert_eq!(row.get(0).unwrap().char(), 'P');
}

// =========================================================================
// Row::new and basic read/write
// =========================================================================

/// Basic Row::new → write → read round-trip.
#[test]
fn miri_row_new_write_read() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(20, &mut pages) };

    // Fill with distinct chars
    for col in 0..20u16 {
        let c = char::from(b'a' + (col as u8));
        row.write_char(col, c);
    }

    // Read back — exercises PageSlice::as_slice through Row::get
    for col in 0..20u16 {
        let expected = char::from(b'a' + (col as u8));
        assert_eq!(row.get(col).unwrap().char(), expected);
    }
}

/// write_char_styled exercises the unsafe get_unchecked/get_unchecked_mut path.
#[test]
fn miri_row_write_char_styled() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    for col in 0..10u16 {
        let c = char::from(b'0' + (col as u8));
        row.write_char_styled(col, c, fg, bg, CellFlags::empty());
    }

    for col in 0..10u16 {
        let expected = char::from(b'0' + (col as u8));
        assert_eq!(row.get(col).unwrap().char(), expected);
    }
}

// =========================================================================
// Row::resize — PageSlice replacement
// =========================================================================

/// Resize row larger: new cells should be empty, existing content preserved.
#[test]
fn miri_row_resize_grow() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    row.write_char(0, 'A');
    row.write_char(9, 'Z');

    // SAFETY: `pages` outlives `row` after resize.
    unsafe { row.resize(20, &mut pages) };

    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(9).unwrap().char(), 'Z');
    // New cells should be empty
    assert_eq!(row.get(15).unwrap().char(), ' ');
    assert_eq!(row.cols(), 20);
}

/// Resize row smaller: truncated content is gone, remaining is intact.
#[test]
fn miri_row_resize_shrink() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(20, &mut pages) };

    for col in 0..20u16 {
        row.write_char(col, char::from(b'A' + (col as u8)));
    }

    // SAFETY: `pages` outlives `row` after resize.
    unsafe { row.resize(5, &mut pages) };

    assert_eq!(row.cols(), 5);
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(4).unwrap().char(), 'E');
    assert!(row.get(5).is_none());
}

/// Multiple sequential resizes exercise alloc/dealloc cycling.
#[test]
fn miri_row_resize_sequence() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };
    row.write_char(0, 'X');

    for &new_cols in &[20u16, 5, 40, 3, 80, 10] {
        // SAFETY: `pages` outlives `row` after each resize.
        unsafe { row.resize(new_cols, &mut pages) };
        assert_eq!(row.cols(), new_cols);
        // Write to verify no stale pointers
        row.write_char(0, 'Y');
        assert_eq!(row.get(0).unwrap().char(), 'Y');
    }
}

// =========================================================================
// Wide character write paths (dual-cell get_unchecked)
// =========================================================================

/// write_wide_char exercises the dual-cell unsafe write path.
#[test]
fn miri_row_write_wide_char() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write a wide char at col 0 — occupies cols 0 and 1
    assert!(row.write_wide_char(0, '中', fg, bg, CellFlags::empty()));

    let cell0 = row.get(0).unwrap();
    assert_eq!(cell0.char(), '中');
    assert!(cell0.flags().contains(CellFlags::WIDE));

    let cell1 = row.get(1).unwrap();
    assert!(cell1.flags().contains(CellFlags::WIDE_CONTINUATION));
}

/// Wide char at edge: col_usize + 1 >= cells_len should return false.
#[test]
fn miri_row_write_wide_char_at_edge() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Wide char at last column — not enough room, should fail
    assert!(!row.write_wide_char(9, '中', fg, bg, CellFlags::empty()));
}

/// Overwrite first half of wide char with narrow char — exercises fixup path.
#[test]
fn miri_row_wide_char_overwrite_fixup() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write wide char at col 2-3
    row.write_wide_char(2, '中', fg, bg, CellFlags::empty());

    // Overwrite col 2 (first half) with narrow char — should clear continuation at col 3
    row.write_char_styled(2, 'X', fg, bg, CellFlags::empty());

    assert_eq!(row.get(2).unwrap().char(), 'X');
    // Col 3 (orphaned continuation) should be cleared to empty
    assert_eq!(row.get(3).unwrap().char(), ' ');
    assert!(
        !row.get(3)
            .unwrap()
            .flags()
            .contains(CellFlags::WIDE_CONTINUATION)
    );
}

/// Overwrite second half of wide char — exercises continuation fixup.
#[test]
fn miri_row_wide_char_overwrite_second_half() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // Write wide char at col 2-3
    row.write_wide_char(2, '中', fg, bg, CellFlags::empty());

    // Overwrite col 3 (second half) with narrow char — should clear wide at col 2
    row.write_char_styled(3, 'Y', fg, bg, CellFlags::empty());

    // Col 2 (orphaned wide) should be cleared
    assert_eq!(row.get(2).unwrap().char(), ' ');
    assert!(!row.get(2).unwrap().flags().contains(CellFlags::WIDE));
    assert_eq!(row.get(3).unwrap().char(), 'Y');
}

// =========================================================================
// Multi-row aliasing: writes to one row must not corrupt another
// =========================================================================

/// Two rows from same PageStore must not alias.
#[test]
fn miri_multi_row_no_aliasing() {
    let mut pages = PageStore::new();

    // SAFETY: `pages` outlives both rows.
    let mut row_a = unsafe { Row::new(20, &mut pages) };
    let mut row_b = unsafe { Row::new(20, &mut pages) };

    // Fill row_a with 'A'
    for col in 0..20u16 {
        row_a.write_char(col, 'A');
    }

    // Fill row_b with 'B'
    for col in 0..20u16 {
        row_b.write_char(col, 'B');
    }

    // Verify no cross-contamination
    for col in 0..20u16 {
        assert_eq!(
            row_a.get(col).unwrap().char(),
            'A',
            "row_a corrupted at col {col}"
        );
        assert_eq!(
            row_b.get(col).unwrap().char(),
            'B',
            "row_b corrupted at col {col}"
        );
    }
}

/// Many rows in same PageStore — exercises page boundary crossings.
#[test]
fn miri_many_rows_page_boundary() {
    let mut pages = PageStore::new();

    // 100 rows × 80 cols = 800 cells × 8 bytes = 6400 bytes per row
    // A 64KB page fits ~10 rows, so this crosses multiple page boundaries
    let mut rows: Vec<Row> = Vec::new();
    for _ in 0..20 {
        // SAFETY: `pages` outlives all rows.
        rows.push(unsafe { Row::new(80, &mut pages) });
    }

    // Write distinct patterns to each row
    for (i, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + (i as u8 % 26));
        for col in 0..80u16 {
            row.write_char(col, c);
        }
    }

    // Verify no aliasing across page boundaries
    for (i, row) in rows.iter().enumerate() {
        let expected = char::from(b'A' + (i as u8 % 26));
        for col in 0..80u16 {
            assert_eq!(
                row.get(col).unwrap().char(),
                expected,
                "row {i} corrupted at col {col}"
            );
        }
    }
}

// =========================================================================
// Grid::resize reflow
// =========================================================================

/// Grid resize triggers reflow which replaces PageSlice backing stores.
#[test]
fn miri_grid_resize_reflow() {
    let mut grid = Grid::new(4, 10);

    // Write content
    if let Some(row) = grid.row_mut(0) {
        for col in 0..10u16 {
            row.write_char(col, char::from(b'A' + (col as u8)));
        }
    }

    // Resize wider — triggers reflow unwrap
    grid.resize(4, 20);

    // Content should survive
    let row = grid.row(0).unwrap();
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(9).unwrap().char(), 'J');

    // Resize narrower — triggers reflow wrap
    grid.resize(4, 5);

    // First 5 chars should be on first row
    let row = grid.row(0).unwrap();
    assert_eq!(row.get(0).unwrap().char(), 'A');
    assert_eq!(row.get(4).unwrap().char(), 'E');
}

/// Multiple resizes stress the PageStore alloc/dealloc cycle.
#[test]
fn miri_grid_resize_stress() {
    let mut grid = Grid::new(4, 10);

    // Write some content
    if let Some(row) = grid.row_mut(0) {
        row.write_char(0, 'X');
    }

    // Rapid resize cycling
    for &(rows, cols) in &[(8, 20), (2, 5), (6, 40), (3, 8), (10, 10)] {
        grid.resize(rows, cols);

        // Verify grid is functional after each resize
        if let Some(row) = grid.row_mut(0) {
            row.write_char(0, 'Z');
            assert_eq!(row.get(0).unwrap().char(), 'Z');
        }
    }
}

// =========================================================================
// Row bulk operations: cells_mut and copy_from
// =========================================================================

/// cells_mut returns a mutable slice that is safe to write.
#[test]
fn miri_row_cells_mut_bulk_write() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row`.
    let mut row = unsafe { Row::new(10, &mut pages) };

    let cells = row.cells_mut();
    for (i, cell) in cells.iter_mut().enumerate() {
        *cell = Cell::new(char::from(b'0' + (i as u8)));
    }
    row.update_len(10);

    for col in 0..10u16 {
        assert_eq!(row.get(col).unwrap().char(), char::from(b'0' + (col as u8)));
    }
}

/// copy_from transfers content between rows with different backing pages.
#[test]
fn miri_row_copy_from() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives both rows.
    let mut src = unsafe { Row::new(10, &mut pages) };
    let mut dst = unsafe { Row::new(10, &mut pages) };

    for col in 0..10u16 {
        src.write_char(col, char::from(b'A' + (col as u8)));
    }

    dst.copy_from(&src);

    for col in 0..10u16 {
        assert_eq!(dst.get(col).unwrap().char(), char::from(b'A' + (col as u8)));
    }
}
