// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Long periodic queries with dense overlapping occurrences.

use aterm_search::{BudgetedSearch, SearchDirection, SearchIndex};

#[test]
fn overlapping_matches_keep_columns_order_and_budgeted_parity() {
    for (text, query, expected) in [
        ("aaaaaaa", "aaaa", vec![(0, 4), (1, 5), (2, 6), (3, 7)]),
        ("ééééé", "ééé", vec![(0, 3), (1, 4), (2, 5)]),
        ("界界界界", "界界", vec![(0, 4), (2, 6), (4, 8)]),
        ("éabababé", "aba", vec![(1, 4), (3, 6)]),
        ("ΣςσσΣ", "σσ", vec![(0, 2), (1, 3), (2, 4), (3, 5)]),
    ] {
        let mut index = SearchIndex::new();
        index.index_line(0, text);
        index.index_line(1, text);
        for case_sensitive in [true, false] {
            let row = if case_sensitive && text == "ΣςσσΣ" {
                vec![(2, 4)]
            } else {
                expected.clone()
            };
            let want: Vec<_> = (0..2)
                .flat_map(|line| row.iter().map(move |&(start, end)| (line, start, end)))
                .collect();
            let forward = index
                .search_results_opts(query, case_sensitive, false)
                .unwrap();
            assert_eq!(
                forward
                    .matches
                    .iter()
                    .map(|m| (m.line, m.start_col, m.end_col))
                    .collect::<Vec<_>>(),
                want
            );
            let reverse = index
                .search_results_opts_direction(
                    query,
                    case_sensitive,
                    false,
                    SearchDirection::Backward,
                )
                .unwrap();
            assert_eq!(reverse, forward);
            let mut budgeted = BudgetedSearch::new(query, case_sensitive, false, 0, 2).unwrap();
            budgeted.feed_row_owned(text.to_owned());
            budgeted.feed_row_owned(text.to_owned());
            assert_eq!(budgeted.results(), forward);
            assert_eq!(
                index
                    .search_literal_narrowed(query, case_sensitive, None)
                    .results,
                forward
            );
        }
    }
}

#[test]
#[ignore = "manual before/after overlapping-match measurement"]
fn dense_overlap_cost() {
    const BYTES: usize = 32_768;
    const REPEATS: u32 = 64;
    let mut index = SearchIndex::new();
    index.index_line(0, &"a".repeat(BYTES));
    for query_bytes in [16, 256, 4096] {
        let query = "a".repeat(query_bytes);
        for case_sensitive in [true, false] {
            for direction in [SearchDirection::Forward, SearchDirection::Backward] {
                let began = std::time::Instant::now();
                for _ in 0..REPEATS {
                    let found = std::hint::black_box(&index)
                        .search_results_opts_direction(
                            std::hint::black_box(&query),
                            case_sensitive,
                            false,
                            direction,
                        )
                        .unwrap();
                    assert_eq!(found.matches.len(), BYTES - query_bytes + 1);
                }
                eprintln!(
                    "dense_overlap row_bytes={BYTES} query_bytes={query_bytes} case_sensitive={case_sensitive} direction={direction:?} repeats={REPEATS} ns_per_op={}",
                    began.elapsed().as_nanos() / u128::from(REPEATS)
                );
            }
        }
    }
}
