// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Boundary condition and content mutation tests — algorithm audit (#1920).

use super::super::test_content::TestContent;
use super::super::*;

// =========================================================================
// CONTENT MUTATION TESTS
// =========================================================================

#[test]
fn content_added_finds_new_matches() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 1);

    // Add new content with a match
    search.content_added(1, "hello again");

    assert_eq!(search.results().len(), 2);
    assert!(search.verify_all_invariants());
}

#[test]
fn content_invalidated_removes_matches() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello 0", "hello 1", "hello 2"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 3);

    // Invalidate rows 0-1
    search.content_invalidated(0, 1);

    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 2);
    assert!(search.verify_all_invariants());
}

// =========================================================================
// BOUNDARY CONDITION TESTS — algorithm audit (#1920)
// =========================================================================

#[test]
fn scan_empty_content_transitions_to_no_results() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec![]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::NoResults);
    assert!(search.results().is_empty());
    assert!(search.verify_all_invariants());
}

#[test]
fn scan_single_empty_row() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec![""]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::NoResults);
    assert!(search.results().is_empty());
    assert!(search.verify_all_invariants());
}

#[test]
fn pattern_too_long_returns_error() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_pattern_len: 10,
        ..Default::default()
    });
    let result = search.start_search("12345678901", FilterMode::Literal);
    assert_eq!(
        result,
        Err(SearchError::PatternTooLong),
        "pattern of length 11 should exceed max_pattern_len 10"
    );
}

#[test]
fn pattern_at_max_length_is_accepted() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_pattern_len: 10,
        ..Default::default()
    });
    let result = search.start_search("1234567890", FilterMode::Literal);
    assert!(
        result.is_ok(),
        "pattern of exactly max_pattern_len should be accepted"
    );
}

#[test]
fn navigation_single_match_next_stays() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["only match here", "nothing", "nothing"]);

    search.start_search("only", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 1);
    assert_eq!(search.current_index(), 1);

    // next_match wraps around to the same single match
    search.next_match();
    assert_eq!(
        search.current_index(),
        1,
        "single match: next wraps to same match"
    );

    // prev_match also wraps to the same single match
    search.prev_match();
    assert_eq!(
        search.current_index(),
        1,
        "single match: prev wraps to same match"
    );

    assert!(search.verify_all_invariants());
}

#[test]
fn content_invalidated_single_row() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["aaa", "bbb", "ccc"]);

    search.start_search("bbb", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 1);

    // Invalidate just row 1
    search.content_invalidated(1, 1);

    assert!(
        search.results().is_empty(),
        "invalidating the only match row should leave no results"
    );
    assert!(search.verify_all_invariants());
}

#[test]
fn content_invalidated_removes_all_matches() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["match", "match", "match"]);

    search.start_search("match", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 3);
    // Verify each match is on a distinct row before invalidation
    for (i, m) in search.results().iter().enumerate() {
        assert_eq!(m.row, i, "match {i} should be on row {i}");
    }

    // Invalidate all rows
    search.content_invalidated(0, 2);

    assert!(
        search.results().is_empty(),
        "invalidating all rows should leave no results"
    );
    assert!(search.verify_all_invariants());
}

#[test]
fn content_added_in_no_results_with_match_transitions() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["no match"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    // After scan_all with no matches, state is NoResults.
    assert_eq!(search.state(), SearchState::NoResults);

    // New content arrives that matches — should transition to HasResults (#4518)
    search.content_added(1, "hello world");
    assert_eq!(
        search.state(),
        SearchState::HasResults,
        "content_added in NoResults should transition to HasResults when match found"
    );
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.current_index(), 1);
    assert!(search.verify_all_invariants());
}

#[test]
fn content_added_after_initial_match() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello first"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);

    // Now content_added should add new matches
    search.content_added(1, "hello second");
    assert_eq!(
        search.results().len(),
        2,
        "content_added in HasResults should add new match"
    );
    assert_eq!(search.results()[1].row, 1);
    assert!(search.verify_all_invariants());
}

#[test]
fn jump_to_match_boundary_zero_index() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["match", "match"]);

    search.start_search("match", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    // Jump to index 0 should be a no-op (1-based indexing)
    search.jump_to_match(0);
    assert_eq!(
        search.current_index(),
        1,
        "jump_to_match(0) should not change current index"
    );
    assert!(search.verify_all_invariants());
}

#[test]
fn jump_to_match_boundary_past_end() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["match", "match"]);

    search.start_search("match", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    // Jump past the last match (only 2 results)
    search.jump_to_match(10);
    assert_eq!(
        search.current_index(),
        1,
        "jump_to_match(10) past end should not change index"
    );
    assert!(search.verify_all_invariants());
}

#[test]
fn scan_row_with_max_rows_zero_is_noop() {
    let mut search = StreamingSearch::new();
    search.start_search("hello", FilterMode::Literal).unwrap();

    // scan_row with max_rows=0: row(0) >= max_rows(0) is true, so early return.
    let found = search.scan_row(0, "hello world", 0);

    assert_eq!(found, 0, "scan_row returns 0 when row >= max_rows");
    assert!(
        search.results().is_empty(),
        "no matches added when max_rows=0"
    );
    assert!(search.verify_all_invariants());
}

/// Case-insensitive match extent uses lowercased pattern byte length.
///
/// Regression test for #2046 MEDIUM: When case-insensitive, `to_lowercase()`
/// can change byte length for certain Unicode characters (e.g., Turkish İ).
/// Match end_col must use the lowercased pattern length, not the original.
#[test]
fn case_insensitive_match_extent_uses_lowered_length() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });
    search.start_search("abc", FilterMode::Literal).unwrap();

    // For ASCII, original and lowered lengths are identical
    let matches = search.find_matches_in_row(0, "xabcx");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].start_col, 1);
    assert_eq!(
        matches[0].end_col, 4,
        "match extent = start + lowered pattern len"
    );

    // Search for an uppercase pattern, match in lowercase text
    let mut search2 = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });
    search2.start_search("ABC", FilterMode::Literal).unwrap();

    let matches = search2.find_matches_in_row(0, "xxabcxx");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].start_col, 2);
    // "ABC".to_lowercase() = "abc" (3 bytes), so end_col = 2 + 3 = 5
    assert_eq!(
        matches[0].end_col, 5,
        "end_col based on lowered pattern len"
    );

    // Turkish İ: lowercasing changes byte length (2 bytes → 3 bytes).
    // After byte→column conversion, end_col reflects display columns.
    let mut search3 = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });
    // İ is U+0130 (2 bytes UTF-8), lowercases to i + combining dot (3 bytes)
    search3
        .start_search("\u{0130}", FilterMode::Literal)
        .unwrap();

    // Text contains the lowered form: 'i' + combining dot above (U+0307)
    // Graphemes: 'a'(col 0) 'a'(col 1) 'i̇'(col 2, width 1) 'b'(col 3) 'b'(col 4)
    let text = "aai\u{0307}bb";
    let matches = search3.find_matches_in_row(0, text);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].start_col, 2);
    // 'i' + combining dot is one grapheme (width 1), so end_col = 3
    assert_eq!(
        matches[0].end_col, 3,
        "Turkish İ: end_col is display column, not byte offset"
    );
}
