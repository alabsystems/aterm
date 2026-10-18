// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
// History line API ordering (ring buffer and tiered scrollback)

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

// Complex char overflow and cell extras

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
