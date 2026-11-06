// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Backward regex results retain a capped suffix even on a dense single row.

use crate::heap::{allocated, start_peak, take_peak};
use aterm_search::{MAX_SEARCH_MATCHES, SearchDirection, SearchIndex, SearchMatch};

#[test]
fn backward_regex_suffix_wraps_with_partial_row_budgets() {
    for remaining in [1, 2, 3, 17, MAX_SEARCH_MATCHES / 2, MAX_SEARCH_MATCHES] {
        let older = 3 * remaining + 5;
        let newer = MAX_SEARCH_MATCHES - remaining;
        let mut index = SearchIndex::new();
        index.index_line(0, &"a\u{301}".repeat(older));
        index.index_line(1, &"b".repeat(newer));
        let found = index
            .search_results_opts_direction("a|b|\u{301}", true, true, SearchDirection::Backward)
            .unwrap();
        assert_eq!(found.cut_short_lines, 0);
        assert_eq!(found.matches.len(), MAX_SEARCH_MATCHES);
        let expected = (older - remaining..older)
            .map(|col| (0, col))
            .chain((0..newer).map(|col| (1, col)));
        for (actual, (line, col)) in found.matches.iter().zip(expected) {
            assert_eq!(
                (actual.line, actual.start_col, actual.end_col),
                (line, col, col + 1)
            );
        }
    }
}

#[test]
fn backward_regex_peak_retention_is_bounded_by_the_result_cap() {
    for chars in [MAX_SEARCH_MATCHES + 17, MAX_SEARCH_MATCHES * 8 + 37] {
        let mut index = SearchIndex::new();
        index.index_line(0, &"a".repeat(chars));
        let before = start_peak();
        let found = index
            .search_results_opts_direction("a", true, true, SearchDirection::Backward)
            .unwrap();
        let peak = take_peak() - before;
        assert_eq!(found.matches.len(), MAX_SEARCH_MATCHES);
        assert_eq!(
            found.matches.first().unwrap().start_col,
            chars - MAX_SEARCH_MATCHES
        );
        assert_eq!(found.matches.last().unwrap().start_col, chars - 1);
        assert!(found.incomplete);
        assert_eq!(found.cut_short_lines, 0);
        assert!(
            peak < (4 * MAX_SEARCH_MATCHES * std::mem::size_of::<SearchMatch>()) as i64,
            "chars={chars}: {peak} peak bytes retained during a capped query"
        );
        eprintln!(
            "backward_regex_peak chars={chars} matches={} peak_bytes={peak}",
            found.matches.len()
        );
    }
}

#[test]
#[ignore = "manual before/after backward regex retention measurement"]
fn backward_regex_retention_cost() {
    for chars in [MAX_SEARCH_MATCHES + 17, MAX_SEARCH_MATCHES * 8 + 37] {
        let mut index = SearchIndex::new();
        index.index_line(0, &"a".repeat(chars));
        let allocated_before = allocated();
        let before = start_peak();
        let began = std::time::Instant::now();
        let found = std::hint::black_box(&index)
            .search_results_opts_direction("a", true, true, SearchDirection::Backward)
            .unwrap();
        let elapsed = began.elapsed();
        let peak = take_peak() - before;
        let bytes = allocated() - allocated_before;
        assert_eq!(found.matches.len(), MAX_SEARCH_MATCHES);
        assert_eq!(
            found.matches.first().unwrap().start_col,
            chars - MAX_SEARCH_MATCHES
        );
        assert_eq!(found.matches.last().unwrap().start_col, chars - 1);
        assert_eq!(found.cut_short_lines, 0);
        eprintln!(
            "backward_regex_retention chars={chars} matches={} peak_bytes={peak} allocated_bytes={bytes} ns={}",
            found.matches.len(),
            elapsed.as_nanos()
        );
    }
}
