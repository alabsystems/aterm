// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::test_content::TestContent;
use super::super::*;

#[test]
fn new_search_is_idle() {
    let search = StreamingSearch::new();
    assert_eq!(search.state(), SearchState::Idle);
    assert_eq!(search.current_index(), 0);
    assert!(search.results().is_empty());
    assert!(search.verify_all_invariants());
}

#[test]
fn config_getters_reflect_defaults_and_custom_values() {
    let search = StreamingSearch::new();
    assert!(search.wrap_enabled());
    assert_eq!(search.case_sensitive(), cfg!(kani));
    assert!(search.highlight_all());

    let configured = StreamingSearch::with_config(StreamingSearchConfig {
        wrap_enabled: false,
        case_sensitive: true,
        highlight_all: false,
        ..Default::default()
    });
    assert!(!configured.wrap_enabled());
    assert!(configured.case_sensitive());
    assert!(!configured.highlight_all());
}

#[test]
fn start_search_transitions_to_searching() {
    let mut search = StreamingSearch::new();
    search.start_search("hello", FilterMode::Literal).unwrap();

    assert_eq!(search.state(), SearchState::Searching);
    assert_eq!(search.pattern(), "hello");
    assert_eq!(search.scan_progress(), 0);
    assert!(search.verify_all_invariants());
}

#[test]
fn empty_pattern_returns_error() {
    let mut search = StreamingSearch::new();
    let result = search.start_search("", FilterMode::Literal);
    assert_eq!(result, Err(SearchError::EmptyPattern));
}

#[test]
fn cancel_clears_state() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello"]);

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.state(), SearchState::HasResults);

    search.cancel();

    assert_eq!(search.state(), SearchState::Idle);
    assert!(search.pattern().is_empty());
    assert!(search.results().is_empty());
    assert_eq!(search.current_index(), 0);
    assert!(search.verify_all_invariants());
}

#[test]
fn clear_pattern_returns_to_idle() {
    let mut search = StreamingSearch::new();

    search.start_search("hello", FilterMode::Literal).unwrap();
    search.update_pattern("").unwrap();

    assert_eq!(search.state(), SearchState::Idle);
}
