// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use super::*;
// History line API ordering (ring buffer and tiered scrollback)
// ========================================================================

#[test]
fn history_line_api_covers_ring_buffer_ordering() {
    let mut grid = Grid::with_scrollback(3, 4, 8);

    for marker in ['A', 'B', 'C', 'D', 'E'] {
        write_marker_line(&mut grid, marker);
    }

    assert_eq!(grid.history_line_count(), 3);

    let oldest = grid
        .get_history_line(0)
        .expect("history idx 0 should exist");
    let middle = grid
        .get_history_line(1)
        .expect("history idx 1 should exist");
    let newest = grid
        .get_history_line(2)
        .expect("history idx 2 should exist");

    assert_eq!(oldest.to_string().chars().next(), Some('A'));
    assert_eq!(middle.to_string().chars().next(), Some('B'));
    assert_eq!(newest.to_string().chars().next(), Some('C'));
    assert!(grid.get_history_line(3).is_none());
}

#[test]
fn try_history_line_ring_buffer_path_is_infallible() {
    let mut grid = Grid::with_scrollback(3, 4, 8);

    for marker in ['A', 'B', 'C', 'D', 'E'] {
        write_marker_line(&mut grid, marker);
    }

    assert_eq!(
        grid.tiered_scrollback_lines(),
        0,
        "fixture should exercise the ring-buffer-only path"
    );

    let oldest = grid
        .try_get_history_line(0)
        .expect("ring-buffer history read should not error")
        .expect("oldest ring-buffer history line should exist");
    let newest = grid
        .try_history_line_rev(0)
        .expect("reverse ring-buffer history read should not error")
        .expect("newest ring-buffer history line should exist");

    assert_eq!(oldest.to_string().chars().next(), Some('A'));
    assert_eq!(newest.to_string().chars().next(), Some('C'));
}

#[test]
fn history_line_api_covers_tiered_scrollback_ordering() {
    let scrollback = Scrollback::new(10, 100, 1_000_000);
    let mut grid = Grid::with_tiered_scrollback(2, 4, 0, scrollback);

    for marker in ['A', 'B', 'C', 'D'] {
        write_marker_line(&mut grid, marker);
    }

    assert_eq!(grid.storage.ring_buffer_scrollback(), 0);
    assert_eq!(grid.history_line_count(), 3);

    let oldest = grid
        .get_history_line(0)
        .expect("tiered history idx 0 should exist");
    let newest = grid
        .history_line_rev(0)
        .expect("reverse history idx 0 should exist");
    let second_newest = grid
        .history_line_rev(1)
        .expect("reverse history idx 1 should exist");

    assert_eq!(oldest.to_string().chars().next(), Some('A'));
    assert_eq!(newest.to_string().chars().next(), Some('C'));
    assert_eq!(second_newest.to_string().chars().next(), Some('B'));
    assert!(grid.history_line_rev(3).is_none());
}

// ========================================================================
// Complex char overflow and cell extras
// ========================================================================

#[test]
fn set_cell_complex_char_stores_overflow_string() {
    let mut grid = Grid::new(2, 2);
    let row = 1;
    let col = 1;
    let combining = "e\u{301}";

    grid.set_cell(row, col, Cell::new('e'));
    grid.set_cell_complex_char(row, col, combining);

    let cell = grid.cell(row, col).expect("cell should exist");
    assert!(cell.is_complex());

    let extra = grid
        .cell_extra(row, col)
        .expect("complex cell should have overflow data");
    assert_eq!(extra.complex_char().map(AsRef::as_ref), Some(combining));

    let rendered = grid.row_text(row).expect("row should exist");
    assert!(rendered.ends_with(combining));
}

#[test]
fn cell_extra_mut_creates_and_updates_extra_data() {
    let mut grid = Grid::new(2, 2);
    let row = 1;
    let col = 1;

    assert!(grid.cell_extra(row, col).is_none());

    let hyperlink: Arc<str> = Arc::from("https://example.test");
    let extra = grid.cell_extra_mut(row, col);
    extra.set_hyperlink(Some(hyperlink.clone()));
    extra.set_underline_color(Some([0x12, 0x34, 0x56]));

    let stored = grid
        .cell_extra(row, col)
        .expect("cell_extra_mut should create entry");
    assert_eq!(stored.hyperlink(), Some(&hyperlink));
    assert_eq!(stored.underline_color(), Some([0x12, 0x34, 0x56]));
}

// Algorithm audit: CellExtras shift_region boundary conditions
// ========================================================================

/// shift_region_up on a single-row region (top == bottom) should delete the row's extras
/// and not move anything (there's nothing below to shift up).
#[test]
fn shift_region_up_single_row_region() {
    use crate::extra::{CellCoord, CellExtras};

    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(5, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(4, 0))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(6, 0))
        .add_combining('\u{0303}');

    // Shift region [5,5] up by 1: row 5 is deleted, rows 4 and 6 are outside region
    extras.shift_region_up_by(5, 5, 1);

    assert!(
        extras.get(CellCoord::new(5, 0)).is_none(),
        "row at top==bottom should be deleted"
    );
    assert!(
        extras.get(CellCoord::new(4, 0)).is_some(),
        "row below region should be preserved"
    );
    assert!(
        extras.get(CellCoord::new(6, 0)).is_some(),
        "row above region should be preserved"
    );
}

/// shift_region_down on a single-row region (top == bottom) should delete the row's extras
/// and not move anything (there's nothing above to shift down).
#[test]
fn shift_region_down_single_row_region() {
    use crate::extra::{CellCoord, CellExtras};

    let mut extras = CellExtras::new();
    extras
        .get_or_create(CellCoord::new(5, 0))
        .add_combining('\u{0301}');
    extras
        .get_or_create(CellCoord::new(4, 0))
        .add_combining('\u{0302}');
    extras
        .get_or_create(CellCoord::new(6, 0))
        .add_combining('\u{0303}');

    // Shift region [5,5] down: row 5 (== bottom) is dropped, no rows in [top, bottom) to shift
    extras.shift_region_down_by(5, 5, 1);

    assert!(
        extras.get(CellCoord::new(5, 0)).is_none(),
        "row at top==bottom should be dropped"
    );
    assert!(
        extras.get(CellCoord::new(4, 0)).is_some(),
        "row below region should be preserved"
    );
    assert!(
        extras.get(CellCoord::new(6, 0)).is_some(),
        "row above region should be preserved"
    );
}

/// shift_region_up preserves extras at exactly top and bottom boundary rows correctly.
/// Row at top is deleted; row at bottom shifts to bottom-1.
#[test]
fn shift_region_up_boundary_rows() {
    use crate::extra::{CellCoord, CellExtras};

    let mut extras = CellExtras::new();
    // Place extras at top, middle, and bottom of region [2, 5]
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0301}'); // top - will be deleted
    extras
        .get_or_create(CellCoord::new(3, 0))
        .add_combining('\u{0302}'); // top+1 - shifts to row 2
    extras
        .get_or_create(CellCoord::new(5, 0))
        .add_combining('\u{0303}'); // bottom - shifts to row 4

    extras.shift_region_up_by(2, 5, 1);

    assert!(
        extras.get(CellCoord::new(2, 0)).is_some(),
        "old row 3 should now be at row 2"
    );
    assert_eq!(
        extras.get(CellCoord::new(2, 0)).unwrap().combining(),
        &['\u{0302}'],
        "row 2 should have old row 3's content"
    );
    assert!(
        extras.get(CellCoord::new(3, 0)).is_none(),
        "old row 3 position should be vacated"
    );
    assert_eq!(
        extras.get(CellCoord::new(4, 0)).unwrap().combining(),
        &['\u{0303}'],
        "old row 5 (bottom) should now be at row 4"
    );
    assert!(
        extras.get(CellCoord::new(5, 0)).is_none(),
        "bottom row position should be empty after shift"
    );
}

/// shift_region_down preserves extras at exactly top and bottom boundary rows correctly.
/// Row at bottom is dropped; row at top shifts to top+1.
#[test]
fn shift_region_down_boundary_rows() {
    use crate::extra::{CellCoord, CellExtras};

    let mut extras = CellExtras::new();
    // Place extras at top, middle, and bottom of region [2, 5]
    extras
        .get_or_create(CellCoord::new(2, 0))
        .add_combining('\u{0301}'); // top - shifts to row 3
    extras
        .get_or_create(CellCoord::new(4, 0))
        .add_combining('\u{0302}'); // bottom-1 - shifts to row 5
    extras
        .get_or_create(CellCoord::new(5, 0))
        .add_combining('\u{0303}'); // bottom - will be dropped

    extras.shift_region_down_by(2, 5, 1);

    assert!(
        extras.get(CellCoord::new(2, 0)).is_none(),
        "top row position should be empty (nothing shifts into it)"
    );
    assert_eq!(
        extras.get(CellCoord::new(3, 0)).unwrap().combining(),
        &['\u{0301}'],
        "old row 2 (top) should now be at row 3"
    );
    assert_eq!(
        extras.get(CellCoord::new(5, 0)).unwrap().combining(),
        &['\u{0302}'],
        "old row 4 (bottom-1) should now be at row 5 (bottom)"
    );
    // The original row 5 content should be gone
    // Note: row 5 now has old row 4's content, the original row 5 was dropped
}
