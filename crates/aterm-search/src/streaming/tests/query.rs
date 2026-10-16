// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::*;

#[test]
fn case_insensitive_search() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_row(0, "HELLO World HeLLo", 10);

    assert_eq!(search.results().len(), 2);
    assert_eq!(search.results()[0].start_col, 0, "HELLO at column 0");
    assert_eq!(search.results()[1].start_col, 12, "HeLLo at column 12");
}

#[test]
fn case_sensitive_search() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: true,
        ..Default::default()
    });

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_row(0, "HELLO World hello", 10);

    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].start_col, 12);
}

#[test]
fn fuzzy_match() {
    let mut search = StreamingSearch::new();

    search.start_search("hlo", FilterMode::Fuzzy).unwrap();
    search.scan_row(0, "hello world", 10);

    // "hlo" fuzzy matches "hello" (h...l...o in order)
    // Fuzzy mode highlights the entire row text (start=0, end=text display width)
    assert_eq!(search.results().len(), 1);
    assert_eq!(
        search.results()[0].start_col,
        0,
        "fuzzy match should start at column 0"
    );
    assert_eq!(
        search.results()[0].end_col,
        11,
        "fuzzy match highlights entire row text"
    );
}

#[test]
fn fuzzy_no_match() {
    let mut search = StreamingSearch::new();

    search.start_search("xyz", FilterMode::Fuzzy).unwrap();
    search.scan_row(0, "hello world", 10);

    assert!(search.results().is_empty());
}

// ============== Multi-match column mapping (#5672) ==============

/// Searching for a single char on a CJK line with many matches must produce
/// correct column offsets from the per-line ColumnMap (O(G + M·log G)).
#[test]
fn multi_match_cjk_line_column_offsets_correct() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: true,
        ..Default::default()
    });

    // Build a line with repeating CJK pattern. Each CJK char = 2 display cols.
    // "日" appears at grapheme indices 0, 3, 6, ... (3 bytes each).
    // Display columns: 日=0-1, 本=2-3, 日=4-5, 本=6-7, ...
    let text: String = "日本".repeat(50); // 100 chars, 200 display cols, 300 bytes
    search.start_search("日", FilterMode::Literal).unwrap();
    let matched = search.scan_row(0, &text, 1);

    assert_eq!(matched, 50, "should find 50 occurrences of 日");
    // Verify first few column offsets are correct (each CJK char = 2 cols wide).
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 2);
    assert_eq!(search.results()[1].start_col, 4); // after 日(2)+本(2)
    assert_eq!(search.results()[1].end_col, 6);
    assert_eq!(search.results()[2].start_col, 8);
}

/// Case-insensitive multi-match must still resolve columns correctly when
/// lowercasing shrinks byte lengths (Kelvin sign U+212A -> `k`).
#[test]
fn case_insensitive_multi_match_casefold_byte_shrink() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });

    // Kelvin sign is 3 UTF-8 bytes but lowercases to `k` (1 byte). Interleave
    // it with CJK so both the byte remap and column remap are exercised.
    // Columns: K=0-1, 日=1-3, K=3-4, 日=4-6, K=6-7.
    let text = "\u{212A}日\u{212A}日\u{212A}";
    search.start_search("k", FilterMode::Literal).unwrap();
    let matched = search.scan_row(0, text, 1);

    assert_eq!(
        matched, 3,
        "should find 3 Kelvin-sign matches via lowercase k"
    );
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 1);
    assert_eq!(search.results()[1].start_col, 3); // after K(1)+日(2)
    assert_eq!(search.results()[1].end_col, 4);
    assert_eq!(search.results()[2].start_col, 6); // after K(1)+日(2)+K(1)+日(2)
    assert_eq!(search.results()[2].end_col, 7);
}
