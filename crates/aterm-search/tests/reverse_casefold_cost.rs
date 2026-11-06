// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Previous-match navigation and newest-edge batches on dense case-insensitive matches.

use crate::heap::allocated;

#[test]
#[ignore = "manual literal anchor-position comparison in both case modes"]
fn literal_anchor_cost() {
    const BYTES: usize = 1_048_576;
    const REPEATS: u32 = 64;
    let mut search = TerminalSearch::new();
    search.index_scrollback_line(&"a".repeat(BYTES));
    for case_sensitive in [true, false] {
        for col in [0, BYTES / 2, BYTES] {
            for direction in [SearchDirection::Forward, SearchDirection::Backward] {
                let began = std::time::Instant::now();
                for _ in 0..REPEATS {
                    let found = std::hint::black_box(&search)
                        .find_direction_opts(
                            "aa",
                            case_sensitive,
                            false,
                            DirectedFind {
                                anchor: (0, col),
                                direction,
                                inclusive: false,
                                wrap: false,
                            },
                        )
                        .unwrap();
                    let expected = match direction {
                        SearchDirection::Forward => {
                            col.checked_add(1).filter(|start| *start <= BYTES - 2)
                        }
                        SearchDirection::Backward => {
                            col.checked_sub(1).map(|start| start.min(BYTES - 2))
                        }
                        _ => unreachable!("fixture enumerates both directions"),
                    };
                    assert_eq!(found.map(|m| m.start_col), expected);
                }
                eprintln!(
                    "literal_anchor bytes={BYTES} case_sensitive={case_sensitive} col={col} direction={direction:?} repeats={REPEATS} ns_per_op={}",
                    began.elapsed().as_nanos() / u128::from(REPEATS)
                );
            }
        }
    }
}

#[test]
#[ignore = "manual backward anchor-position comparison"]
fn reverse_casefold_anchor_cost() {
    const BYTES: usize = 1_048_576;
    const REPEATS: u32 = 64;
    let mut search = TerminalSearch::new();
    search.index_scrollback_line(&"A".repeat(BYTES));
    for col in [0, 16, BYTES / 2, BYTES] {
        for inclusive in [false, true] {
            let ceiling = if inclusive {
                Some(col)
            } else {
                col.checked_sub(1)
            };
            let expected = ceiling.map(|c| c.min(BYTES - 2));
            let began = std::time::Instant::now();
            for _ in 0..REPEATS {
                let found = std::hint::black_box(&search)
                    .find_direction_opts(
                        "aa",
                        false,
                        false,
                        DirectedFind {
                            anchor: (0, col),
                            direction: SearchDirection::Backward,
                            inclusive,
                            wrap: false,
                        },
                    )
                    .unwrap();
                assert_eq!(found.map(|m| m.start_col), expected);
            }
            eprintln!(
                "reverse_casefold_anchor bytes={BYTES} col={col} inclusive={inclusive} repeats={REPEATS} ns_per_op={}",
                began.elapsed().as_nanos() / u128::from(REPEATS)
            );
        }
    }
}

#[test]
#[ignore = "manual ordinary case-insensitive workload comparison"]
fn ordinary_casefold_cost() {
    const ROWS: usize = 8192;
    const REPEATS: u32 = 64;
    let line = |row| {
        format!(
            "INFO SERVICE-WORKER STATUS=ready request={row:06} path=/var/log/app.log READY SERVICE started"
        )
    };
    let mut search = TerminalSearch::with_capacity(ROWS);
    for row in 0..ROWS {
        search.index_scrollback_line(&line(row));
    }
    for query in [
        "o",
        "re",
        "service-worker",
        "STATUS=READY",
        "ready service-worker",
    ] {
        let expected = line(0)
            .to_ascii_lowercase()
            .match_indices(&query.to_ascii_lowercase())
            .count()
            * ROWS;
        if query == "ready service-worker" {
            assert!(search.literal_candidate_bound(query, false).unwrap() > 0);
            assert_eq!(expected, 0);
        }
        for direction in [SearchDirection::Forward, SearchDirection::Backward] {
            let before = allocated();
            let began = std::time::Instant::now();
            for _ in 0..REPEATS {
                let found = std::hint::black_box(&search)
                    .search_results_opts_direction(
                        std::hint::black_box(query),
                        false,
                        false,
                        direction,
                    )
                    .unwrap();
                assert_eq!(found.matches.len(), expected);
            }
            let elapsed = began.elapsed();
            let bytes = allocated() - before;
            eprintln!(
                "ordinary_casefold rows={ROWS} query={query:?} direction={direction:?} repeats={REPEATS} ns_per_op={} bytes_per_op={}",
                elapsed.as_nanos() / u128::from(REPEATS),
                bytes / u64::from(REPEATS)
            );
        }
    }
}
use aterm_search::{DirectedFind, MAX_SEARCH_MATCHES, SearchDirection, TerminalSearch};

#[test]
fn ascii_previous_match_scratch_does_not_grow_with_row_length() {
    for row_bytes in [4096, 1_048_576] {
        for query in ["a", "aa", "service-worker"] {
            let unit = format!("{} ", query.to_ascii_uppercase());
            let text = unit.repeat(row_bytes / unit.len());
            let mut search = TerminalSearch::new();
            search.index_scrollback_line(&text);
            let before = allocated();
            let found = search
                .find_direction_opts(
                    query,
                    false,
                    false,
                    DirectedFind {
                        anchor: (0, usize::MAX),
                        direction: SearchDirection::Backward,
                        inclusive: true,
                        wrap: false,
                    },
                )
                .unwrap()
                .unwrap();
            let bytes = allocated() - before;
            assert_eq!(found.start_col, text.len() - unit.len());
            assert!(
                bytes < 8192,
                "row_bytes={row_bytes} query={query}: {bytes} scratch bytes"
            );
        }
    }
}

#[test]
#[ignore = "manual before/after backward case-insensitive measurement"]
fn reverse_casefold_cost() {
    for row_bytes in [4096, 1_048_576] {
        for query in ["a", "aa", "service-worker"] {
            let unit = format!("{} ", query.to_ascii_uppercase());
            let text = if query.len() <= 2 {
                "A".repeat(row_bytes)
            } else {
                unit.repeat(row_bytes / unit.len())
            };
            let stride = if query.len() <= 2 { 1 } else { unit.len() };
            let last = if query.len() <= 2 {
                text.len() - query.len()
            } else {
                text.len() - unit.len()
            };
            let count = last / stride + 1;
            let mut search = TerminalSearch::new();
            search.index_scrollback_line(&text);
            const REPEATS: u32 = 64;
            let before = allocated();
            let began = std::time::Instant::now();
            for _ in 0..REPEATS {
                let found = std::hint::black_box(&search)
                    .find_direction_opts(
                        std::hint::black_box(query),
                        false,
                        false,
                        DirectedFind {
                            anchor: (0, usize::MAX),
                            direction: SearchDirection::Backward,
                            inclusive: true,
                            wrap: false,
                        },
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(found.start_col, last);
            }
            let elapsed = began.elapsed();
            let bytes = allocated() - before;
            eprintln!(
                "reverse_casefold_point row_bytes={} query={query} repeats={REPEATS} ns_per_op={} bytes_per_op={}",
                text.len(),
                elapsed.as_nanos() / u128::from(REPEATS),
                bytes / u64::from(REPEATS)
            );
            let before = allocated();
            let began = std::time::Instant::now();
            let found = search
                .search_results_opts_direction(query, false, false, SearchDirection::Backward)
                .unwrap();
            let elapsed = began.elapsed();
            let bytes = allocated() - before;
            let retained = count.min(MAX_SEARCH_MATCHES);
            assert_eq!(found.matches.len(), retained);
            assert_eq!(
                found.matches.first().unwrap().start_col,
                last - stride * (retained - 1)
            );
            assert_eq!(found.matches.last().unwrap().start_col, last);
            eprintln!(
                "reverse_casefold_batch row_bytes={} query={query} matches={retained} ns={} bytes={bytes}",
                text.len(),
                elapsed.as_nanos()
            );
        }
    }
}
