// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_scrollback::Scrollback;

/// A grid whose tiered history starts with a corrupt warm block (three
/// lines that fail to decompress) ahead of one readable hot line.
fn grid_with_corrupted_scrollback() -> Grid {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    sb.push_str("Line00");
    sb.inject_corrupted_warm_block(3);
    let mut grid = Grid::new(5, 80);
    grid.attach_scrollback(sb);
    grid
}

/// A corrupted tier surfaces as an error on the fallible reads (by index
/// and by reverse index) and as `None`, never a panic, on the
/// compatibility wrapper.
#[test]
fn corrupted_scrollback_reads_fail_without_panicking() {
    let grid = grid_with_corrupted_scrollback();
    let result = grid.try_get_history_line(0);
    assert!(result.is_err(), "try_get_history_line: {result:?}");
    assert!(grid.get_history_line(0).is_none());
    let total = grid.scrollback_lines();
    let result = grid.try_history_line_rev(total.saturating_sub(1));
    assert!(result.is_err(), "try_history_line_rev: {result:?}");
}
