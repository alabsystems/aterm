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
fn streaming_generation_bumps_on_update_pattern() {
    let mut search = StreamingSearch::new();
    search.start_search("test", FilterMode::Literal).unwrap();
    let gen_before = search.generation();

    search.update_pattern("test2").unwrap();
    assert!(
        search.generation() > gen_before,
        "update_pattern should bump generation"
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
// content_modified tests (#7244)
// =============================================================================

/// Modifying a row after it has been scanned removes stale matches and
/// re-scans with new content.
#[test]
fn content_modified_removes_stale_matches_during_scan() {
    let mut search = StreamingSearch::new();
    search.start_search("hello", FilterMode::Literal).unwrap();

    // Scan rows 0 and 1 (out of 5 total).
    search.scan_row(0, "hello world", 5);
    search.scan_row(1, "hello again", 5);
    assert_eq!(search.results().len(), 2);
    assert_eq!(search.state(), SearchState::Searching);

    // Row 0 is overwritten in-place — the old "hello world" is gone.
    search.content_modified(0, "goodbye world");

    // The stale match on row 0 must be gone; row 1 match survives.
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 1);
    assert!(search.verify_all_invariants());
}

/// Modifying a row that has NOT yet been scanned is a no-op during
/// `Searching` state (scan_progress hasn't reached it yet).
#[test]
fn content_modified_noop_for_unscanned_row() {
    let mut search = StreamingSearch::new();
    search.start_search("hello", FilterMode::Literal).unwrap();

    search.scan_row(0, "hello world", 5);
    assert_eq!(search.results().len(), 1);

    let gen_before = search.generation();

    // Row 3 hasn't been scanned yet — content_modified should be a no-op.
    search.content_modified(3, "something else");

    assert_eq!(search.results().len(), 1);
    assert_eq!(search.generation(), gen_before);
    assert!(search.verify_all_invariants());
}

/// Modifying a row after scan completes (HasResults) removes old matches
/// and rescans.
#[test]
fn content_modified_after_scan_complete() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello first", "no match", "hello third"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 2);

    // Row 0 is overwritten — loses its match.
    search.content_modified(0, "goodbye first");

    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 2);
    assert!(search.verify_all_invariants());
}

/// Modifying the only matching row so it no longer matches transitions
/// to NoResults.
#[test]
fn content_modified_removes_all_matches_transitions_to_no_results() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello only"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);

    search.content_modified(0, "no match anymore");

    assert_eq!(search.state(), SearchState::NoResults);
    assert!(search.results().is_empty());
    assert_eq!(search.current_index(), 0);
    assert!(search.verify_all_invariants());
}

/// Modifying a row replaces old matches with new matches from updated text.
#[test]
fn content_modified_replaces_with_new_matches() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["aaa", "bbb"]);

    search.start_search("aaa", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 0);

    // Overwrite row 1 so it now contains the search term.
    search.content_modified(1, "aaa");

    // Row 0 match survives; row 1 now also matches.
    assert_eq!(search.results().len(), 2);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.results()[1].row, 1);
    assert!(search.verify_all_invariants());
}

/// Modifying a row in NoResults state with matching content transitions
/// to HasResults.
#[test]
fn content_modified_in_no_results_transitions_to_has_results() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["no match", "still no match"]);

    search.start_search("xyz", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.state(), SearchState::NoResults);

    // Row 0 is overwritten with content that matches the query.
    search.content_modified(0, "xyz appears");

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.results()[0].row, 0);
    assert_eq!(search.current_index(), 1);
    assert!(search.verify_all_invariants());
}

/// Generation counter bumps on content_modified when matches change.
#[test]
fn content_modified_bumps_generation() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello row"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    let gen_before = search.generation();
    search.content_modified(0, "goodbye row");
    assert!(
        search.generation() > gen_before,
        "content_modified that removes matches should bump generation"
    );
}

/// content_modified is a no-op in Idle state.
#[test]
fn content_modified_noop_in_idle() {
    let mut search = StreamingSearch::new();
    let gen_before = search.generation();

    search.content_modified(0, "anything");

    assert_eq!(search.generation(), gen_before);
    assert_eq!(search.state(), SearchState::Idle);
}

/// current_index is clamped when the current match is on the modified row.
#[test]
fn content_modified_clamps_current_index() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["aaa", "aaa", "aaa"]);

    search.start_search("aaa", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.results().len(), 3);

    // Navigate to the last match (index 3).
    search.next_match();
    search.next_match();
    assert_eq!(search.current_index(), 3);

    // Remove last two rows' matches.
    search.content_modified(1, "bbb");
    search.content_modified(2, "bbb");

    // Only 1 result remains; current_index must be clamped.
    assert_eq!(search.results().len(), 1);
    assert_eq!(search.current_index(), 1);
    assert!(search.verify_all_invariants());
}

// =============================================================================
// Wrapped-line coordinate remapping tests (#7572)
// =============================================================================

/// Search match on a wrapped line has coordinates within the physical row width.
///
/// Before the fix, matches found in joined wrapped-line text could have
/// start_col/end_col values that exceeded the terminal's physical column count,
/// because coordinates were reported relative to the joined string rather than
/// remapped to the physical row.
#[test]
fn wrapped_line_match_coordinates_within_physical_width() {
    use super::super::test_content::WrappedTestContent;

    let mut search = StreamingSearch::new();
    // Two physical rows that form one logical line:
    // Row 0: "hello " (6 columns)
    // Row 1: "world" (5 columns, continuation of row 0)
    // Logical line: "hello world"
    // A search for "world" should produce a match on row 1, columns 0..5.
    let mut content = WrappedTestContent::new(
        vec!["hello ", "world"],
        vec![false, true], // row 1 is a continuation
    );

    search.start_search("world", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert_eq!(search.results().len(), 1);

    let m = &search.results()[0];
    // Match must be on physical row 1 (the continuation row).
    assert_eq!(m.row, 1, "match should be on physical row 1");
    assert_eq!(m.start_col, 0, "match should start at column 0 of row 1");
    assert_eq!(m.end_col, 5, "match should end at column 5 of row 1");

    // The end column must not exceed the physical row width (5 columns).
    assert!(
        m.end_col <= 5,
        "end_col {} exceeds physical row width 5",
        m.end_col
    );
    assert!(search.verify_all_invariants());
}

/// Match spanning a wrap boundary is clamped to the physical row width (#7572).
#[test]
fn wrapped_line_match_spanning_boundary_is_clamped() {
    use super::super::test_content::WrappedTestContent;

    let mut search = StreamingSearch::new();
    // Row 0: "hel" (3 columns)
    // Row 1: "lo world" (8 columns, continuation)
    // Logical line: "hello world"
    // Search for "hello" spans the boundary. The match starts on row 0 at col 0.
    // The match_len in the joined text is 5, but row 0 only has 3 columns,
    // so end_col should be clamped to 3.
    let mut content = WrappedTestContent::new(vec!["hel", "lo world"], vec![false, true]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);
    assert!(!search.results().is_empty());

    let m = &search.results()[0];
    assert_eq!(m.row, 0, "match should start on physical row 0");
    assert_eq!(m.start_col, 0);
    // end_col must be clamped to physical row width (3), not 5.
    assert!(
        m.end_col <= 3,
        "end_col {} exceeds row 0 physical width 3",
        m.end_col
    );
    assert!(search.verify_all_invariants());
}

/// Non-wrapped rows pass through scan_row normally (regression check).
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
