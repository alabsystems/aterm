// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::test_content::TestContent;
use super::super::*;

#[test]
fn scan_finds_matches() {
    let mut search = StreamingSearch::new();
    search.start_search("hello", FilterMode::Literal).unwrap();

    let count = search.scan_row(0, "hello world", 10);
    assert_eq!(count, 1);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 5);
    assert!(search.verify_all_invariants());
}

#[test]
fn scan_all_completes_search() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello world", "goodbye world", "hello again"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 2);
    assert_eq!(search.current_index(), 1);
    assert!(search.verify_all_invariants());
}

#[test]
fn no_matches_transitions_to_no_results() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["foo", "bar", "baz"]);

    search.start_search("xyz", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::NoResults);
    assert!(search.results().is_empty());
    assert_eq!(search.current_index(), 0);
    assert!(search.verify_all_invariants());
}

#[test]
fn memory_bounded() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: 5,
        ..Default::default()
    });

    search.start_search("a", FilterMode::Literal).unwrap();

    // Create content with many matches
    for i in 0..20 {
        search.scan_row(i, "a a a a a", 20);
    }

    // Should be capped at 5 results
    assert!(search.results().len() <= 5);
    assert!(search.total_matches() > 5);
    assert!(search.verify_memory_bounded());
    assert!(search.verify_all_invariants());
}

#[test]
fn no_duplicate_results() {
    let mut search = StreamingSearch::new();

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_row(0, "hello hello", 10);

    // Should find 2 distinct matches
    assert_eq!(search.results().len(), 2);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[1].start_col, 6);
    assert!(search.verify_no_duplicates());
}

#[test]
fn scan_progress_consistent() {
    let mut search = StreamingSearch::new();

    // Idle state
    assert_eq!(search.scan_progress(), -1);
    assert!(search.verify_scan_progress_consistent());

    // Searching state
    search.start_search("hello", FilterMode::Literal).unwrap();
    assert!(search.scan_progress() >= 0);
    assert!(search.verify_scan_progress_consistent());

    // HasResults state
    search.scan_row(0, "hello", 1);
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.scan_progress(), -1);
    assert!(search.verify_scan_progress_consistent());
}

#[test]
fn content_added_in_no_results_transitions_to_has_results() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["foo", "bar", "baz"]);

    // Search for "xyz" — no matches in existing content
    search.start_search("xyz", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::NoResults);
    assert_eq!(search.current_index(), 0);

    // New content arrives that matches the query
    search.content_added(3, "xyz appears here");

    // Should transition to HasResults with the new match
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 3);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.current_index(), 1);
    assert!(search.verify_all_invariants());
}

#[test]
fn content_added_in_no_results_no_match_stays_no_results() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["foo", "bar"]);

    search.start_search("xyz", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::NoResults);

    // New content arrives but doesn't match
    search.content_added(2, "still no match here");

    // Should remain in NoResults
    assert_eq!(search.state(), SearchState::NoResults);
    assert!(search.results().is_empty());
    assert_eq!(search.current_index(), 0);
    assert!(search.verify_all_invariants());
}

#[test]
fn all_invariants_hold_after_operations() {
    let mut search = StreamingSearch::new();

    // After creation
    assert!(search.verify_all_invariants());

    // After start
    search.start_search("test", FilterMode::Literal).unwrap();
    assert!(search.verify_all_invariants());

    // After scanning
    search.scan_row(0, "test content test", 5);
    assert!(search.verify_all_invariants());

    search.scan_row(1, "more test here", 5);
    assert!(search.verify_all_invariants());

    // Complete search
    search.scan_row(2, "no match", 5);
    search.scan_row(3, "test again", 5);
    search.scan_row(4, "final test", 5);
    assert!(search.verify_all_invariants());

    // After navigation
    search.next_match();
    assert!(search.verify_all_invariants());

    search.prev_match();
    assert!(search.verify_all_invariants());

    // After content changes
    search.content_added(5, "new test row");
    assert!(search.verify_all_invariants());

    search.content_invalidated(0, 1);
    assert!(search.verify_all_invariants());

    // After cancel
    search.cancel();
    assert!(search.verify_all_invariants());
}

// =============================================================================
// Generation counter tests (#7271)
// =============================================================================

#[test]
fn streaming_generation_starts_at_zero() {
    let search = StreamingSearch::new();
    assert_eq!(search.generation(), 0);
}

#[test]
fn streaming_generation_bumps_on_start_search() {
    let mut search = StreamingSearch::new();
    search.start_search("test", FilterMode::Literal).unwrap();
    assert_eq!(search.generation(), 1);
}

#[test]
fn streaming_generation_bumps_on_cancel() {
    let mut search = StreamingSearch::new();
    search.start_search("test", FilterMode::Literal).unwrap();
    let gen_after_start = search.generation();

    search.cancel();
    assert!(
        search.generation() > gen_after_start,
        "cancel should bump generation"
    );
}

#[test]
fn streaming_generation_bumps_on_content_invalidated() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["test row 0", "test row 1", "test row 2"]);
    search.start_search("test", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);

    let gen_before = search.generation();
    search.content_invalidated(0, 0);
    assert!(
        search.generation() > gen_before,
        "content_invalidated with actual removals should bump generation"
    );
}

#[test]
fn streaming_generation_stable_on_noop_invalidation() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["test row 0"]);
    search.start_search("test", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    let gen_before = search.generation();
    // Invalidate a range with no results (row 5-10 when results are at row 0)
    search.content_invalidated(5, 10);
    assert_eq!(
        search.generation(),
        gen_before,
        "invalidating empty range should not bump generation"
    );
}

#[test]
fn streaming_generation_bumps_on_content_added_with_match() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["no match"]);
    search.start_search("xyz", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::NoResults);

    let gen_before = search.generation();
    search.content_added(1, "xyz here");
    assert!(
        search.generation() > gen_before,
        "content_added with new match should bump generation"
    );
}

#[test]
fn streaming_generation_stable_on_content_added_no_match() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["no match"]);
    search.start_search("xyz", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::NoResults);

    let gen_before = search.generation();
    search.content_added(1, "still no match");
    assert_eq!(
        search.generation(),
        gen_before,
        "content_added without new match should not bump generation"
    );
}

#[test]
fn streaming_generation_stable_on_navigation() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["test a", "test b", "test c"]);
    search.start_search("test", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    let gen_before = search.generation();
    search.next_match();
    assert_eq!(
        search.generation(),
        gen_before,
        "navigation should not bump generation"
    );
    search.prev_match();
    assert_eq!(
        search.generation(),
        gen_before,
        "navigation should not bump generation"
    );
}

// =============================================================================
// Wrapped-line coordinate remapping tests (#7572)
// =============================================================================

/// Non-wrapped rows pass through scan_row normally.
#[test]
fn non_wrapped_rows_unaffected_by_wrapping_logic() {
    use super::super::test_content::WrappedTestContent;

    let mut search = StreamingSearch::new();
    // All rows are independent (no wrapping).
    let mut content = WrappedTestContent::new(
        vec!["hello world", "goodbye world", "hello again"],
        vec![false, false, false],
    );

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 2);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[1].row, 2);
    assert_eq!(search.results()[1].start_col, 0);
    assert!(search.verify_all_invariants());
}
