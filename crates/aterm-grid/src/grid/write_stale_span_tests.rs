// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn unrelated_wide_cells_do_not_schedule_an_empty_extras_removal() {
    let mut grid = Grid::new(2, 80);
    grid.write_wide_run_autowrap(&['日'; 40], PackedColors::new(), CellFlags::empty());
    grid.cell_extra_mut(0, 79).add_combining('\u{0301}');
    let row = grid.row(0).unwrap();
    assert!(row.flags().contains(crate::row::RowFlags::HAS_WIDE_CHARS));
    assert!(!row.any_has_extras_in_range(0, 20));
    assert!(StaleExtras::probe(row, 0, 20).is_empty());
}
