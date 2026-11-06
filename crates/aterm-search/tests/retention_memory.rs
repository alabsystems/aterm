// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dropping history should not allocate a second list of the discarded rows.

use crate::heap::allocated;
use aterm_search::SearchIndex;

#[test]
fn dropping_dense_and_sparse_prefixes_needs_no_row_scratch() {
    for spacing in [1, 10_000] {
        for rows in [128, 32_768] {
            let mut index = SearchIndex::with_capacity(rows);
            for row in 0..rows {
                index.index_line(row * spacing, "retained searchable row");
            }
            let first = rows / 2 * spacing;
            let before = allocated();
            index.drop_history_below(first);
            let bytes = allocated() - before;
            eprintln!("drop_prefix rows={rows} spacing={spacing} allocated_bytes={bytes}");
            assert!(
                bytes < 128,
                "{bytes} scratch bytes for {rows} rows, spacing {spacing}"
            );
            let found = index.search_with_positions("searchable");
            assert_eq!(found.len(), rows / 2);
            for (actual, row) in found.iter().zip(rows / 2..rows) {
                assert_eq!(actual.line, row * spacing);
            }
        }
    }
}
