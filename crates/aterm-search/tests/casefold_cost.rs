// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Repetitive rows whose trigrams pass the index but whose long query is absent.

use aterm_search::SearchIndex;

#[test]
#[ignore = "manual before/after case-insensitive narrowing measurement"]
fn repetitive_casefold_narrowing_cost() {
    let text = format!("{}BAAAC", "A".repeat(65_536));
    let mut index = SearchIndex::new();
    index.index_line(0, &text);
    const REPEATS: u32 = 16;
    for prefix in [64, 1024, 4096] {
        let query = format!("{}b{}c", "a".repeat(prefix), "a".repeat(prefix));
        assert!(
            index.might_contain(&query),
            "exercise a trigram false positive"
        );
        let began = std::time::Instant::now();
        for _ in 0..REPEATS {
            let found = std::hint::black_box(&index).search_literal_narrowed(
                std::hint::black_box(&query),
                false,
                None,
            );
            assert!(found.results.matches.is_empty());
            assert!(found.occurrence_lines.unwrap().is_empty());
        }
        eprintln!(
            "casefold_narrowing row_bytes={} query_bytes={} repeats={REPEATS} ns_per_op={}",
            text.len(),
            query.len(),
            began.elapsed().as_nanos() / u128::from(REPEATS)
        );
    }
}
