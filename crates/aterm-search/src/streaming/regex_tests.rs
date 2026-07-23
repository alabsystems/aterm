// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::test_content::TestContent;
use super::*;

#[test]
fn regex_basic_pattern() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello123world", "abc456def", "no digits here"]);

    search.start_search(r"\d+", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 2);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 5);
    assert_eq!(search.results()[0].end_col, 8);
    assert_eq!(search.results()[1].row, 1);
    assert_eq!(search.results()[1].start_col, 3);
    assert_eq!(search.results()[1].end_col, 6);
    assert!(search.verify_all_invariants());
}

#[test]
fn regex_character_class() {
    let mut search = StreamingSearch::new();
    // Enable case-sensitive so [a-z]+ matches only lowercase (default is
    // case-insensitive which now correctly applies (?i) to the regex too).
    search.toggle_case_sensitive();
    let mut content = TestContent::new(vec!["apple BANANA cherry", "GRAPE orange"]);

    search.start_search(r"[a-z]+", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 3);
    // "apple" at row 0
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 5);
    // "cherry" at row 0
    assert_eq!(search.results()[1].row, 0);
    assert_eq!(search.results()[1].start_col, 13);
    assert_eq!(search.results()[1].end_col, 19);
    // "orange" at row 1
    assert_eq!(search.results()[2].row, 1);
    assert_eq!(search.results()[2].start_col, 6);
    assert_eq!(search.results()[2].end_col, 12);
    assert!(search.verify_all_invariants());
}

#[test]
fn regex_word_boundary() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["testing test tested", "contest pretest"]);

    search.start_search(r"\btest\b", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 8);
    assert!(search.verify_all_invariants());
}

#[test]
fn regex_invalid_pattern() {
    let mut search = StreamingSearch::new();
    let result = search.start_search(r"(unclosed", FilterMode::Regex);
    assert!(matches!(result, Err(SearchError::InvalidRegex(_))));
}

#[test]
fn regex_vs_literal_metacharacters() {
    let mut regex_search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["a.b", "axb", "ab"]);

    regex_search.start_search("a.b", FilterMode::Regex).unwrap();
    regex_search.scan_all(&mut content);
    assert_eq!(regex_search.results().len(), 2);

    let mut literal_search = StreamingSearch::new();
    literal_search
        .start_search("a.b", FilterMode::Literal)
        .unwrap();
    literal_search.scan_all(&mut content);
    assert_eq!(literal_search.results().len(), 1);
    assert_eq!(literal_search.results()[0].row, 0);
}

#[test]
fn regex_multiple_matches_per_row() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["cat dog cat bird cat"]);

    search.start_search("cat", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.results().len(), 3);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[1].start_col, 8);
    assert_eq!(search.results()[2].start_col, 17);
    assert!(search.verify_all_invariants());
}

#[test]
fn regex_multi_match_cjk_line_column_offsets_correct() {
    let mut search = StreamingSearch::new();
    let text = "日本".repeat(50);

    search.start_search("日", FilterMode::Regex).unwrap();
    let matched = search.scan_row(0, &text, 1);

    assert_eq!(matched, 50, "should find every regex match on the CJK row");
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 2);
    assert_eq!(search.results()[1].start_col, 4);
    assert_eq!(search.results()[1].end_col, 6);
    assert_eq!(search.results()[2].start_col, 8);
    assert!(search.verify_all_invariants());
}

#[test]
fn regex_alternation() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec![
        "error: file not found",
        "warning: deprecated",
        "info: ok",
    ]);

    search
        .start_search(r"error|warning", FilterMode::Regex)
        .unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 2);
    // "error" at row 0, col 0-5
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 5);
    // "warning" at row 1, col 0-7
    assert_eq!(search.results()[1].row, 1);
    assert_eq!(search.results()[1].start_col, 0);
    assert_eq!(search.results()[1].end_col, 7);
}

#[test]
fn regex_quantifiers() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["aaaa bbb cccccc", "a bb ccc"]);

    search.start_search(r"a{3,}", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(
        search.results()[0].end_col - search.results()[0].start_col,
        4
    );
}

#[test]
fn regex_set_filter_mode_switch() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["abc123xyz", "abc", "123"]);

    search.start_search("abc", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 2);
    // Literal "abc" matches row 0 ("abc123xyz") and row 1 ("abc")
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[1].row, 1);
    assert_eq!(search.results()[1].start_col, 0);

    search.set_filter_mode(FilterMode::Regex).unwrap();
    assert_eq!(search.state(), SearchState::Searching);
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 2);
    // Same "abc" pattern now as regex — same matches
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[1].row, 1);

    search.update_pattern(r"\d+").unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 2);
    // "\d+" matches "123" in row 0 and "123" in row 2
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 3);
    assert_eq!(search.results()[0].end_col, 6);
    assert_eq!(search.results()[1].row, 2);
    assert_eq!(search.results()[1].start_col, 0);
    assert_eq!(search.results()[1].end_col, 3);
}

#[test]
fn regex_update_pattern() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello world", "hello 123", "world 456"]);

    search.start_search(r"\d+", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 2);
    // "\d+" matches "123" in row 1 and "456" in row 2
    assert_eq!(search.results()[0].row, 1);
    assert_eq!(search.results()[0].start_col, 6);
    assert_eq!(search.results()[1].row, 2);
    assert_eq!(search.results()[1].start_col, 6);

    search.update_pattern(r"hello").unwrap();
    assert_eq!(search.state(), SearchState::Searching);
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 2);
    // "hello" matches in row 0 and row 1
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 5);
    assert_eq!(search.results()[1].row, 1);
    assert_eq!(search.results()[1].start_col, 0);
    assert_eq!(search.results()[1].end_col, 5);

    search.update_pattern("").unwrap();
    assert_eq!(search.state(), SearchState::Idle);
}

#[test]
fn regex_update_to_invalid() {
    let mut search = StreamingSearch::new();

    search.start_search(r"\d+", FilterMode::Regex).unwrap();
    search.scan_row(0, "123", 1);

    let result = search.update_pattern(r"[unclosed");
    assert!(matches!(result, Err(SearchError::InvalidRegex(_))));
    assert_eq!(search.pattern(), r"\d+");
}

#[test]
fn regex_set_filter_mode_invalid_regex_error() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello [world"]);

    // Start with a literal pattern containing invalid regex syntax
    search
        .start_search("[unclosed", FilterMode::Literal)
        .unwrap();
    search.scan_all(&mut content);

    // Switch to regex mode — the current pattern is invalid regex
    let result = search.set_filter_mode(FilterMode::Regex);
    assert!(matches!(result, Err(SearchError::InvalidRegex(_))));
    // Mode should NOT have changed since the operation failed
    assert_eq!(search.filter_mode(), FilterMode::Literal);
}

#[test]
fn regex_empty_match_handling() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello"]);

    search.start_search(r"x*", FilterMode::Regex).unwrap();
    search.scan_all(&mut content);
    assert!(search.verify_all_invariants());
    // `x*` on non-x input produces only zero-length matches, all filtered out.
    assert!(
        search.results().is_empty(),
        "`x*` on non-x input should produce 0 results after zero-length filtering, got {}",
        search.results().len(),
    );
}

#[test]
fn regex_zero_length_anchor_filtered() {
    let mut search = StreamingSearch::new();

    search.start_search(r"^", FilterMode::Regex).unwrap();
    search.scan_row(0, "hello world", 1);
    assert!(search.verify_all_invariants());
    assert!(
        search.results().is_empty(),
        "pure anchor `^` should produce 0 results after zero-length filtering, got {}",
        search.results().len(),
    );
}

#[test]
fn regex_preserves_nonzero_matches_alongside_zero_length() {
    let mut search = StreamingSearch::new();

    // `x*` on input containing `x` produces both zero-length and non-zero matches.
    // Only non-zero matches should survive.
    search.start_search(r"x*", FilterMode::Regex).unwrap();
    search.scan_row(0, "axb", 1);
    assert!(search.verify_all_invariants());
    assert!(
        !search.results().is_empty(),
        "`x*` on input containing `x` should return at least one non-zero match",
    );
    for m in search.results() {
        assert!(
            m.match_len > 0,
            "all returned matches must have positive length: row {} col {} match_len {}",
            m.row,
            m.start_col,
            m.match_len,
        );
    }
}

/// Regression: regex matching a combining mark produces a match that is
/// non-empty in bytes but zero display-width (byte_to_column maps both
/// endpoints to the same column). INV-SEARCH-2c requires match_len > 0.
///
/// Text: "a\u{0301}b" → graphemes: ["à" (width 1), "b" (width 1)]
/// Regex `\u{0301}` matches bytes 1..3 (the combining accent), but
/// byte_to_column(text, 1) == byte_to_column(text, 3) == 1, so
/// start_col == end_col → match_len == 0.
#[test]
fn regex_combining_mark_zero_display_width_filtered() {
    let mut search = StreamingSearch::new();

    // U+0301 is the combining acute accent (2 UTF-8 bytes: 0xCC 0x81)
    search.start_search(r"\u{0301}", FilterMode::Regex).unwrap();
    search.scan_row(0, "a\u{0301}b", 1);

    // All results must have positive display width
    for m in search.results() {
        assert!(
            m.match_len > 0,
            "combining mark match should be filtered: row {} start_col {} end_col {} match_len {}",
            m.row,
            m.start_col,
            m.end_col,
            m.match_len,
        );
    }
}

#[test]
fn regex_cancel_clears_compiled() {
    let mut search = StreamingSearch::new();

    search.start_search(r"\d+", FilterMode::Regex).unwrap();
    search.scan_row(0, "123", 1);

    search.cancel();

    assert_eq!(search.state(), SearchState::Idle);
    assert!(search.pattern().is_empty());
    assert!(search.results().is_empty());
}

#[test]
fn regex_invariants_after_operations() {
    let mut search = StreamingSearch::new();

    search.start_search(r"\w+", FilterMode::Regex).unwrap();
    assert!(search.verify_all_invariants());
    assert_eq!(search.result_count(), 0);

    search.scan_row(0, "hello world", 5);
    assert!(search.verify_all_invariants());
    // "hello" and "world" are two \w+ matches on row 0
    assert_eq!(search.result_count(), 2);

    search.scan_row(1, "foo bar baz", 5);
    assert!(search.verify_all_invariants());
    // 3 more matches: "foo", "bar", "baz"
    assert_eq!(search.result_count(), 5);

    // next_match/prev_match are no-ops while state == Searching (not yet completed)
    assert_eq!(search.state(), SearchState::Searching);
    search.next_match();
    assert!(search.verify_all_invariants());

    search.prev_match();
    assert!(search.verify_all_invariants());

    search.content_added(5, "new words here");
    assert!(search.verify_all_invariants());

    search.content_invalidated(0, 0);
    assert!(search.verify_all_invariants());

    search.cancel();
    assert!(search.verify_all_invariants());
    assert_eq!(search.result_count(), 0);
    assert_eq!(search.state(), SearchState::Idle);
}

/// Regression test: regex search with case_sensitive=false (default) produces
/// case-insensitive matches.
///
/// Bug #2775: The `compile_regex` method was ignoring the `case_sensitive` flag,
/// always producing case-sensitive regexes. The fix prepends `(?i)` when
/// `case_sensitive` is false.
#[test]
fn regex_case_insensitive_default() {
    // Default config: case_sensitive=false
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["Hello WORLD", "hello world", "HELLO"]);

    search
        .start_search("hello", FilterMode::Regex)
        .expect("valid regex");
    search.scan_all(&mut content);

    // case_sensitive=false by default, so "hello" regex should match all three rows
    assert_eq!(
        search.results().len(),
        3,
        "case-insensitive regex 'hello' should match HELLO, hello, Hello"
    );
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[1].row, 1);
    assert_eq!(search.results()[2].row, 2);
    assert!(search.verify_all_invariants());
}

/// Regression test: toggling case_sensitive to true makes regex case-sensitive.
#[test]
fn regex_case_sensitive_toggled_on() {
    let mut search = StreamingSearch::new();
    search.toggle_case_sensitive(); // Now case_sensitive=true
    let mut content = TestContent::new(vec!["Hello WORLD", "hello world", "HELLO"]);

    search
        .start_search("hello", FilterMode::Regex)
        .expect("valid regex");
    search.scan_all(&mut content);

    // Only exact "hello" in row 1 should match (case-sensitive)
    assert_eq!(
        search.results().len(),
        1,
        "case-sensitive regex 'hello' should only match lowercase"
    );
    assert_eq!(search.results()[0].row, 1);
    assert!(search.verify_all_invariants());
}
