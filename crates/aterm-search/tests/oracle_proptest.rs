// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! E4 differential equivalence oracle (proptest).
//!
//! Property-tests the *verified* search path
//! ([`SearchIndex::search_with_positions`]) — which today reads the per-line
//! `FxHashMap<usize, String>` cache via `str::find` — against a brute-force
//! substring reference oracle. The E4 String-drop refactor must keep this
//! green byte-for-byte: same set of matched lines, and match columns that
//! recover the query at the reported span.
//!
//! Also serves as the build-environment proof that `proptest` resolves and
//! runs for `aterm-search` from the orc submodule.

use aterm_search::SearchIndex;
use proptest::prelude::*;

/// Brute-force reference: every indexed line whose text contains `query`.
fn reference_lines(lines: &[String], query: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, text)| text.contains(query))
        .map(|(i, _)| i)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// The verified search path must return exactly the lines a brute-force
    /// substring scan finds — no false negatives, no false positives — for
    /// queries of length >= 3 (the trigram-indexed regime).
    #[test]
    fn verified_search_matches_bruteforce(
        // ASCII printable so column index == byte index == char index.
        lines in prop::collection::vec("[ -~]{0,24}", 0..12usize),
        query in "[ -~]{3,6}",
    ) {
        let mut index = SearchIndex::new();
        for (i, text) in lines.iter().enumerate() {
            index.index_line(i, text);
        }

        let mut got: Vec<usize> = index
            .search_with_positions(&query)
            .into_iter()
            .map(|m| m.line)
            .collect();
        got.sort_unstable();
        got.dedup();

        let expected = reference_lines(&lines, &query);
        prop_assert_eq!(got, expected);
    }

    /// Every reported match span must recover the query from the source line
    /// (columns == byte offsets in the ASCII regime). Guards the str::find
    /// column computation the refactor must preserve.
    #[test]
    fn match_spans_recover_query(
        lines in prop::collection::vec("[ -~]{0,24}", 0..12usize),
        query in "[ -~]{3,6}",
    ) {
        let mut index = SearchIndex::new();
        for (i, text) in lines.iter().enumerate() {
            index.index_line(i, text);
        }

        for m in index.search_with_positions(&query) {
            let line = &lines[m.line];
            prop_assert!(m.start_col <= line.len());
            prop_assert!(m.end_col <= line.len());
            prop_assert_eq!(&line[m.start_col..m.end_col], query.as_str());
        }
    }
}
