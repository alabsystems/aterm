// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grid fixtures shared by the grid's unit-test modules.

use super::Grid;

/// Write `n` numbered lines `L{start}..L{start+n-1}` (CR + text + LF each).
pub(crate) fn feed_numbered_lines(grid: &mut Grid, start: usize, n: usize) {
    for i in start..start + n {
        grid.carriage_return();
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
}

/// Every history row oldest→newest, read WITHOUT draining (the `line` /
/// `search` path), trailing blanks trimmed.
pub(crate) fn history_trimmed(grid: &Grid) -> Vec<String> {
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// Write a single marker char on a new line.
pub(crate) fn write_marker_line(grid: &mut Grid, marker: char) {
    grid.carriage_return();
    grid.write_char(marker);
    grid.carriage_return();
    grid.line_feed();
}

/// Row 0 `ABCDE` autowrapped into row 1 `FG` (a continuation) on a 5-col grid.
pub(crate) fn wrapped_pair() -> Grid {
    let mut grid = Grid::new(4, 5);
    for c in "ABCDEFG".chars() {
        grid.write_char_wrap(c);
    }
    assert!(
        grid.row(1).is_some_and(|r| r.is_wrapped()),
        "precondition: row 1 continues row 0"
    );
    grid
}
