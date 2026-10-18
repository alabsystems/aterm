// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reflow cost: row operations stay linear under extreme resize ratios.

use super::super::super::*;
use crate::test_counters::take_reflow_row_ops;

/// Reflow shrink from 100 cols to 1 visits each source row once, though one row expands to 100.
#[test]
fn reflow_shrink_extreme_ratio_is_linear_in_source_rows() {
    let _ = take_reflow_row_ops(); // reset counter

    let rows: u16 = 10;
    let old_cols: u16 = 100;
    let mut grid = Grid::new(rows, old_cols);

    grid.set_cursor(0, 0);
    for i in 0..old_cols {
        grid.write_char((b'A' + (i % 26) as u8) as char);
    }

    let _ = take_reflow_row_ops(); // reset after setup
    grid.resize(rows, 1); // extreme shrink: 100 → 1

    let ops = take_reflow_row_ops();
    assert_eq!(
        ops,
        usize::from(rows),
        "reflow shrink should visit each source row exactly once, got {ops}"
    );

    // The cursor (col 99 -> row 99) stays visible: rows 0..90 scroll off, so visible rows 0..9
    // hold original columns 90..99.
    for i in 0..10u16 {
        let char_index = 90 + i;
        let expected = (b'A' + (char_index % 26) as u8) as char;
        assert_eq!(
            grid.cell(i, 0).unwrap().char(),
            expected,
            "row {i} (original col {char_index}) should contain '{expected}'"
        );
    }

    grid.assert_invariants();
}
