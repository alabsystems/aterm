// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Refreshing unchanged cell coordinates reuses the cached run buffer.

use crate::heap::allocated;
use aterm_search::{LineCells, TerminalSearch};

#[test]
fn unchanged_cell_map_refresh_reuses_its_allocation() {
    for repeats in [8, 1_024] {
        let text = "a日".repeat(repeats);
        let mut cells = LineCells::new();
        for i in 0..repeats {
            cells.push(i * 4..i * 4 + 1, i * 3, 1);
            cells.push(i * 4 + 1..i * 4 + 4, i * 3 + 1, 2);
        }
        let mut search = TerminalSearch::new();
        search.index_numbered_cells_owned(std::iter::once((0, text.clone(), cells.clone())));
        let update = (0, text, cells);
        let before = allocated();
        search.index_numbered_cells_owned(std::iter::once(update));
        let bytes = allocated() - before;
        eprintln!("column_refresh repeats={repeats} allocated_bytes={bytes}");
        assert_eq!(bytes, 0, "unchanged columns should reuse their run storage");
        let found = search.search("日");
        assert_eq!(found.len(), repeats);
        for (actual, i) in found.iter().zip(0..repeats) {
            assert_eq!((actual.start_col, actual.end_col), (i * 3 + 1, i * 3 + 3));
        }
    }
}
