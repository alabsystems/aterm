// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Long periodic queries with dense overlapping occurrences.

use aterm_search::{BudgetedSearch, SearchDirection, SearchIndex};

#[test]
#[ignore = "manual before/after dense Unicode offset translation measurement"]
fn unicode_dense_offset_translation_cost() {
    const ROWS: usize = 32;
    const CHARS: usize = 2048;
    for width in [16, 256] {
        let query = "A".repeat(width);
        let mut index = SearchIndex::new();
        for row in 0..ROWS {
            index.index_line(row, &format!("İé{}", "a".repeat(CHARS)));
        }
        for direction in [SearchDirection::Forward, SearchDirection::Backward] {
            let allocated = crate::heap::allocated();
            let before = instructions();
            let result = index
                .search_results_opts_direction(&query, false, false, direction)
                .unwrap();
            let retired = before.zip(instructions()).map(|(a, b)| b - a);
            let bytes = crate::heap::allocated() - allocated;
            let per_row = CHARS - width + 1;
            assert_eq!(result.matches.len(), ROWS * per_row);
            for (i, found) in result.matches.iter().enumerate() {
                let start = 2 + i % per_row;
                assert_eq!(
                    (found.line, found.start_col, found.end_col),
                    (i / per_row, start, start + width)
                );
            }
            eprintln!(
                "dense_offset_translation width={width} direction={direction:?} rows={ROWS} instructions={retired:?} allocated_bytes={bytes}"
            );
        }
    }
}

#[test]
#[ignore = "manual before/after Unicode offset translation measurement"]
fn unicode_offset_translation_cost() {
    const ROWS: usize = 256;
    let query = format!("{}B", "A".repeat(15));
    for shape in ["ascii", "unicode", "alternating"] {
        for length in [64, 4096] {
            for hit in [false, true] {
                let prefix = match shape {
                    "ascii" => "x".repeat(length),
                    "unicode" => format!("é{}", "x".repeat(length)),
                    _ => "éx".repeat(length / 2),
                };
                let text = if hit {
                    format!("{prefix}{}b", "a".repeat(15))
                } else {
                    // Every query trigram exists, but the full query does not.
                    format!("{prefix}aab{}c", "a".repeat(15))
                };
                let start = aterm_search::display_columns(&prefix);
                let check = |result: &aterm_search::SearchResults| {
                    assert_eq!(result.matches.len(), if hit { ROWS } else { 0 });
                    for (row, found) in result.matches.iter().enumerate() {
                        assert_eq!(
                            (found.line, found.start_col, found.end_col),
                            (row, start, start + 16)
                        );
                    }
                };
                let rows = vec![text.clone(); ROWS];
                let mut search = BudgetedSearch::new(&query, false, false, 0, ROWS).unwrap();
                let allocated = crate::heap::allocated();
                let before = instructions();
                for row in rows {
                    search.feed_row_owned(row);
                }
                let feed = before.zip(instructions()).map(|(a, b)| b - a);
                let feed_bytes = crate::heap::allocated() - allocated;
                let result = search.results();
                check(&result);
                let retained = crate::heap::net();
                let before = instructions();
                drop(search);
                let dropped = before.zip(instructions()).map(|(a, b)| b - a);
                let freed = retained - crate::heap::net();
                let total = feed.zip(dropped).map(|(a, b)| a + b);
                eprintln!(
                    "offset_translation path=budgeted shape={shape} length={length} hit={hit} rows={ROWS} instructions={total:?} allocated_bytes={feed_bytes} state_freed_bytes={freed}"
                );
                let mut index = SearchIndex::new();
                for row in 0..ROWS {
                    index.index_line(row, &text);
                }
                assert_eq!(
                    index.literal_candidate_bound(&query, false),
                    Some(ROWS as u64)
                );
                for direction in [SearchDirection::Forward, SearchDirection::Backward] {
                    let allocated = crate::heap::allocated();
                    let before = instructions();
                    let result = index
                        .search_results_opts_direction(&query, false, false, direction)
                        .unwrap();
                    let retired = before.zip(instructions()).map(|(a, b)| b - a);
                    let bytes = crate::heap::allocated() - allocated;
                    check(&result);
                    eprintln!(
                        "offset_translation path=indexed direction={direction:?} shape={shape} length={length} hit={hit} rows={ROWS} instructions={retired:?} allocated_bytes={bytes}"
                    );
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the selected flavor initializes this complete buffer on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: success initialized the selected buffer above.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn instructions() -> Option<u64> {
    None
}

#[test]
#[ignore = "manual before/after incremental query preparation measurement"]
fn incremental_query_preparation_cost() {
    const ROWS: usize = 2048;
    for (case_sensitive, unicode_row) in [(true, false), (false, false), (false, true)] {
        for len in [1, 16, 256, 4096] {
            let needle = "a".repeat(len);
            let query = if case_sensitive {
                needle.clone()
            } else {
                needle.to_ascii_uppercase()
            };
            let prefix = if unicode_row { "é" } else { "" };
            let text = format!("{prefix}{needle}b");
            let rows = vec![text; ROWS];
            let mut search = BudgetedSearch::new(&query, case_sensitive, false, 0, ROWS).unwrap();
            let before = instructions();
            let began = std::time::Instant::now();
            for row in rows {
                search.feed_row_owned(row);
            }
            let elapsed = began.elapsed();
            let retired = before.zip(instructions()).map(|(a, b)| b - a);
            let result = search.results();
            assert_eq!(result.matches.len(), ROWS);
            for (row, found) in result.matches.iter().enumerate() {
                let start = usize::from(unicode_row);
                assert_eq!(
                    (found.line, found.start_col, found.end_col),
                    (row, start, start + len)
                );
            }
            eprintln!(
                "incremental_preparation query_bytes={len} case_sensitive={case_sensitive} unicode_row={unicode_row} rows={ROWS} instructions={retired:?} elapsed_ns={}",
                elapsed.as_nanos()
            );
        }
    }
}

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
