// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Navigation scratch must depend on the query, rather than retained history.

use crate::heap::allocated;

#[test]
fn repeated_query_posting_scratch_tracks_distinct_trigrams() {
    use aterm_search::SearchIndex;
    for pattern in [
        "a",
        "ab",
        "service-worker ",
        "abcdefghijklmnopqrstuvwxyz0123456789",
    ] {
        for repeats in [128, 8_192] {
            let query = pattern.repeat(repeats);
            let mut index = SearchIndex::new();
            index.index_line(0, &query);
            let before = allocated();
            let bound = index.literal_candidate_bound(&query, true);
            let bytes = allocated() - before;
            assert_eq!(bound, Some(1));
            eprintln!(
                "posting_scratch pattern={pattern:?} repeats={repeats} allocated_bytes={bytes}"
            );
            assert!(
                bytes < 4_096,
                "{bytes} scratch bytes for repeated pattern {pattern:?}"
            );
        }
    }
}

#[test]
#[ignore = "manual repeated-query posting selection comparison"]
fn repeated_query_posting_cost() {
    use aterm_search::SearchIndex;
    const REPEATS: u32 = 64;
    for pattern in ["a", "ab", "service-worker "] {
        let query = pattern.repeat(8_192);
        let mut index = SearchIndex::new();
        index.index_line(0, &query);
        let before = allocated();
        let began = std::time::Instant::now();
        for _ in 0..REPEATS {
            assert_eq!(
                std::hint::black_box(&index)
                    .literal_candidate_bound(std::hint::black_box(&query), true),
                Some(1)
            );
        }
        eprintln!(
            "posting_cost pattern={pattern:?} query_bytes={} repeats={REPEATS} ns_per_op={} bytes_per_op={}",
            query.len(),
            began.elapsed().as_nanos() / u128::from(REPEATS),
            (allocated() - before) / u64::from(REPEATS)
        );
    }
}

#[test]
#[ignore = "manual before/after navigation measurement"]
fn dense_navigation_cost() {
    const ROWS: usize = 32_768;
    const REPEATS: u32 = 256;
    let mut search = TerminalSearch::with_capacity(ROWS);
    for _ in 0..ROWS {
        search.index_scrollback_line("common prefix service-worker status=ready");
    }
    for (direction, anchor) in [
        (SearchDirection::Forward, (0, 0)),
        (SearchDirection::Backward, (ROWS - 1, usize::MAX)),
        (SearchDirection::Forward, (ROWS / 2, 0)),
        (SearchDirection::Backward, (ROWS / 2, usize::MAX)),
    ] {
        let before = allocated();
        let began = std::time::Instant::now();
        for _ in 0..REPEATS {
            let found = std::hint::black_box(&search)
                .find_direction_opts(
                    std::hint::black_box("service-worker"),
                    true,
                    false,
                    DirectedFind {
                        anchor,
                        direction,
                        inclusive: true,
                        wrap: false,
                    },
                )
                .unwrap()
                .unwrap();
            assert_eq!(found.line, anchor.0);
        }
        let elapsed = began.elapsed();
        let bytes = allocated() - before;
        eprintln!(
            "navigation_cost rows={ROWS} direction={direction:?} anchor={} repeats={REPEATS} ns_per_op={} bytes_per_op={}",
            anchor.0,
            elapsed.as_nanos() / u128::from(REPEATS),
            bytes / u64::from(REPEATS)
        );
    }
}
use aterm_search::{DirectedFind, SearchDirection, TerminalSearch};

#[test]
fn dense_navigation_has_history_independent_scratch() {
    for rows in [128, 32_768] {
        let mut search = TerminalSearch::with_capacity(rows);
        for _ in 0..rows {
            search.index_scrollback_line("common prefix service-worker status=ready");
        }
        for direction in [SearchDirection::Forward, SearchDirection::Backward] {
            for case_sensitive in [true, false] {
                let anchor = match direction {
                    SearchDirection::Forward => (0, 0),
                    _ => (rows - 1, usize::MAX),
                };
                let before = allocated();
                let found = search
                    .find_direction_opts(
                        "service-worker",
                        case_sensitive,
                        false,
                        DirectedFind {
                            anchor,
                            direction,
                            inclusive: true,
                            wrap: false,
                        },
                    )
                    .unwrap()
                    .unwrap();
                let bytes = allocated() - before;
                assert_eq!(found.line, anchor.0);
                assert!(
                    bytes < 8192,
                    "rows={rows} direction={direction:?} case_sensitive={case_sensitive}: {bytes} scratch bytes"
                );
                eprintln!(
                    "navigation_scratch rows={rows} direction={direction:?} case_sensitive={case_sensitive} bytes={bytes}"
                );
            }
        }
    }
}
