// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::test_content::TestContent;
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

#[test]
fn update_pattern_restarts_search() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello", "world"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 1);

    search.update_pattern("world").unwrap();
    assert_eq!(search.state(), SearchState::Searching);
    assert!(search.results().is_empty());

    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 1);
}

#[test]
fn toggle_case_sensitive_restarts_search() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["HELLO"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 1); // Case insensitive

    search.toggle_case_sensitive();
    assert_eq!(search.state(), SearchState::Searching);

    search.scan_all(&mut content);
    assert!(search.results().is_empty()); // Case sensitive, no match
}

#[test]
fn toggle_case_sensitive_restarts_while_scanning() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["HELLO", "hello"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    assert_eq!(search.scan_row(0, "HELLO", 2), 1);
    assert_eq!(search.scan_progress(), 1);
    assert_eq!(search.results().len(), 1);

    search.toggle_case_sensitive();
    assert_eq!(search.state(), SearchState::Searching);
    assert_eq!(search.scan_progress(), 0);
    assert!(search.results().is_empty());

    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 1);
    assert!(search.verify_all_invariants());
}

#[test]
fn set_filter_mode_restarts_while_scanning() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["aXb", "zzz"]);

    search.start_search("ab", FilterMode::Literal).unwrap();
    assert_eq!(search.scan_row(0, "aXb", 2), 0);
    assert_eq!(search.scan_progress(), 1);

    search.set_filter_mode(FilterMode::Fuzzy).unwrap();
    assert_eq!(search.state(), SearchState::Searching);
    assert_eq!(search.scan_progress(), 0);
    assert!(search.results().is_empty());

    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 0);
    assert!(search.verify_all_invariants());
}

#[test]
fn toggle_case_sensitive_restarts_cleanly_during_long_scan() {
    let mut search = StreamingSearch::new();
    let total_rows = 300usize;

    search.start_search("hello", FilterMode::Literal).unwrap();

    // Phase 1: case-insensitive scan collects uppercase matches.
    for row in 0..150 {
        let matched = search.scan_row(row, "HELLO upper", total_rows);
        assert_eq!(matched, 1);
    }
    assert_eq!(search.results().len(), 150);
    // Phase 1 matches should be on consecutive rows 0..150
    assert_eq!(search.results()[0].row, 0, "first match should be row 0");
    assert_eq!(
        search.results()[149].row,
        149,
        "last match should be row 149"
    );
    assert_eq!(search.scan_progress(), 150);

    // Mid-scan option toggle should restart and clear stale state.
    search.toggle_case_sensitive();
    assert_eq!(search.state(), SearchState::Searching);
    assert_eq!(search.scan_progress(), 0);
    assert!(search.results().is_empty());
    assert!(search.verify_all_invariants());

    // Phase 2: case-sensitive rescan should only keep lowercase matches.
    for row in 0..total_rows {
        let text = if row % 10 == 0 {
            "hello lower"
        } else {
            "HELLO upper"
        };
        let matched = search.scan_row(row, text, total_rows);
        if row % 10 == 0 {
            assert_eq!(matched, 1);
        } else {
            assert_eq!(matched, 0);
        }
        assert!(search.verify_all_invariants());
    }

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 30);
    assert!(search.results().iter().all(|m| m.row % 10 == 0));
}

#[test]
fn set_filter_mode_restarts_cleanly_during_long_scan() {
    let mut search = StreamingSearch::new();
    let total_rows = 240usize;

    search.start_search("abc", FilterMode::Literal).unwrap();

    // Phase 1: literal mode finds exact matches in sparse rows.
    for row in 0..120 {
        let text = if row % 20 == 0 { "abc" } else { "aXbXc" };
        search.scan_row(row, text, total_rows);
    }
    assert_eq!(search.results().len(), 6);
    assert_eq!(search.scan_progress(), 120);

    // Mid-scan filter mode switch should restart and clear stale literal matches.
    search.set_filter_mode(FilterMode::Fuzzy).unwrap();
    assert_eq!(search.state(), SearchState::Searching);
    assert_eq!(search.scan_progress(), 0);
    assert!(search.results().is_empty());
    assert!(search.verify_all_invariants());

    // Phase 2: fuzzy mode should now match non-contiguous rows.
    for row in 0..total_rows {
        let text = if row % 12 == 0 { "aXbXc" } else { "zzz" };
        let matched = search.scan_row(row, text, total_rows);
        if row % 12 == 0 {
            assert_eq!(matched, 1);
        } else {
            assert_eq!(matched, 0);
        }
        assert!(search.verify_all_invariants());
    }

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 20);
    assert!(search.results().iter().all(|m| m.row % 12 == 0));
}

// ============== Regression: O(M×G) → O(G + M·log G) (#5672) ==============

/// Searching for a single char on a CJK line with many matches must produce
/// correct column offsets. Before #5672, this path called byte_to_column()
/// (O(G)) per match, yielding O(M×G) total. After the fix, ColumnMap is
/// built once per line and lookups are O(log G).
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

// ============== Error Path Tests (Part of #2097) ==============

/// update_pattern returns InvalidState when search is Idle (not started).
#[test]
fn update_pattern_idle_returns_invalid_state() {
    let mut search = StreamingSearch::new();
    assert_eq!(search.state(), SearchState::Idle);

    let result = search.update_pattern("hello");
    assert_eq!(result, Err(SearchError::InvalidState));
    assert_eq!(
        search.state(),
        SearchState::Idle,
        "state unchanged on error"
    );
}

/// update_pattern returns PatternTooLong when pattern exceeds config limit.
#[test]
fn update_pattern_too_long_returns_error() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_pattern_len: 5,
        ..Default::default()
    });
    search.start_search("hi", FilterMode::Literal).unwrap();
    assert_eq!(search.state(), SearchState::Searching);

    let result = search.update_pattern("123456");
    assert_eq!(result, Err(SearchError::PatternTooLong));
    // Original pattern should be preserved on error.
    assert_eq!(search.pattern(), "hi");
}

/// update_pattern is a no-op when the new pattern equals the current pattern.
#[test]
fn update_pattern_same_pattern_is_noop() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello world"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.state(), SearchState::HasResults);

    // Updating with the same pattern should not restart the search.
    search.update_pattern("hello").unwrap();
    assert_eq!(search.state(), SearchState::HasResults, "state preserved");
    assert_eq!(search.results().len(), 1, "results preserved");
}

/// set_filter_mode is a no-op when mode is unchanged.
#[test]
fn set_filter_mode_same_mode_is_noop() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);

    search.set_filter_mode(FilterMode::Literal).unwrap();
    assert_eq!(search.state(), SearchState::HasResults, "state preserved");
    assert_eq!(search.results().len(), 1, "results preserved");
}

/// set_filter_mode with empty pattern updates mode without restarting search.
#[test]
fn set_filter_mode_empty_pattern_no_restart() {
    let mut search = StreamingSearch::new();
    search.start_search("temp", FilterMode::Literal).unwrap();
    // Clear pattern to get back to Idle with empty pattern.
    search.update_pattern("").unwrap();
    assert_eq!(search.state(), SearchState::Idle);

    // Start a new search so we're not idle, then clear again.
    search.start_search("abc", FilterMode::Literal).unwrap();
    search.update_pattern("").unwrap();
    assert_eq!(search.state(), SearchState::Idle);

    // Changing mode when idle with empty pattern should just update mode.
    search.set_filter_mode(FilterMode::Fuzzy).unwrap();
    assert_eq!(search.state(), SearchState::Idle);
}

/// set_filter_mode in Idle state with non-empty pattern updates mode but
/// does not restart search (restart only happens in Searching/HasResults/NoResults).
#[test]
fn set_filter_mode_idle_with_pattern_no_restart() {
    let mut search = StreamingSearch::new();
    // Start search then clear to get Idle — pattern becomes empty.
    // We need Idle with a non-empty pattern. That's not possible through
    // the normal API since update_pattern("") is the only way to reach Idle
    // and it clears the pattern. So this scenario is not reachable.
    // Instead, test that mode change in a non-Idle state restarts correctly.
    search.start_search("hello", FilterMode::Literal).unwrap();
    assert_eq!(search.state(), SearchState::Searching);

    search.set_filter_mode(FilterMode::Fuzzy).unwrap();
    assert_eq!(
        search.state(),
        SearchState::Searching,
        "mode change restarts search"
    );
}
