// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Replacing a visible row must not expand history-sized posting lists.

use crate::heap::allocated;
use aterm_search::SearchIndex;

const READY: &str = "common prefix service-worker status=ready";
const STOPPED: &str = "common prefix service-worker status=stopped";

#[test]
fn changed_row_scratch_is_independent_of_history() {
    for rows in [128, 32_768] {
        let mut index = SearchIndex::with_capacity(rows);
        for row in 0..rows {
            index.index_line(row, READY);
        }
        let edited = rows - 2;
        index.index_line(edited, STOPPED);
        index.index_line(edited, READY);
        for text in [STOPPED, READY, STOPPED, READY] {
            let before = allocated();
            index.index_line(edited, text);
            let bytes = allocated() - before;
            eprintln!("changed_row_scratch rows={rows} bytes={bytes}");
            assert!(
                bytes < 16_384,
                "rows={rows}: {bytes} scratch bytes for one changed row"
            );
            let results = index.search_with_positions("status=stopped");
            if text == STOPPED {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].line, edited);
            } else {
                assert!(results.is_empty());
            }
        }
    }
}

#[test]
#[ignore = "manual before/after changed-row measurement"]
fn changed_row_cost() {
    const REPEATS: u32 = 256;
    for rows in [128, 32_768] {
        let mut index = SearchIndex::with_capacity(rows);
        for row in 0..rows {
            index.index_line(row, READY);
        }
        let edited = rows - 2;
        index.index_line(edited, STOPPED);
        index.index_line(edited, READY);
        let before = allocated();
        let began = std::time::Instant::now();
        for step in 0..REPEATS {
            std::hint::black_box(&mut index).index_line(
                edited,
                std::hint::black_box(if step % 2 == 0 { STOPPED } else { READY }),
            );
        }
        let elapsed = began.elapsed();
        let bytes = allocated() - before;
        eprintln!(
            "changed_row_cost rows={rows} repeats={REPEATS} ns_per_op={} bytes_per_op={}",
            elapsed.as_nanos() / u128::from(REPEATS),
            bytes / u64::from(REPEATS)
        );
    }
}
