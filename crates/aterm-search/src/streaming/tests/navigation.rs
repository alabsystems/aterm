// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::test_content::TestContent;
use super::super::*;

#[test]
fn navigation_works() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["match here", "match there", "match everywhere"]);

    search
        .start_search("match", FilterMode::Literal)
        .expect("literal pattern should start successfully");
    search.scan_all(&mut content);

    assert_eq!(search.current_index(), 1);

    search.next_match();
    assert_eq!(search.current_index(), 2);

    search.next_match();
    assert_eq!(search.current_index(), 3);

    // Wrap around
    search.next_match();
    assert_eq!(search.current_index(), 1);

    search.prev_match();
    assert_eq!(search.current_index(), 3);

    assert!(search.verify_all_invariants());
}

#[test]
fn navigation_without_wrap() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        wrap_enabled: false,
        ..Default::default()
    });
    let mut content = TestContent::new(vec!["match", "match"]);

    search.start_search("match", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.current_index(), 1);

    search.next_match();
    assert_eq!(search.current_index(), 2);

    // Should not wrap
    search.next_match();
    assert_eq!(search.current_index(), 2);

    search.prev_match();
    assert_eq!(search.current_index(), 1);

    // Should not wrap backwards
    search.prev_match();
    assert_eq!(search.current_index(), 1);
}

#[test]
fn toggle_wrap_flips_setting_and_changes_navigation_behavior() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["match 1", "match 2"]);

    search.start_search("match", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);
    assert_eq!(search.current_index(), 1);
    assert!(search.wrap_enabled());

    search.toggle_wrap();
    assert!(!search.wrap_enabled());
    search.prev_match();
    assert_eq!(
        search.current_index(),
        1,
        "wrap disabled should clamp at start"
    );

    search.toggle_wrap();
    assert!(search.wrap_enabled());
    search.prev_match();
    assert_eq!(search.current_index(), 2, "wrap enabled should wrap to end");
}

#[test]
fn toggle_highlight_all_flips_setting() {
    let mut search = StreamingSearch::new();
    assert!(search.highlight_all());

    search.toggle_highlight_all();
    assert!(!search.highlight_all());

    search.toggle_highlight_all();
    assert!(search.highlight_all());
}

#[test]
fn jump_to_match() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["match", "match", "match"]);

    search.start_search("match", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    assert_eq!(search.current_index(), 1);

    search.jump_to_match(3);
    assert_eq!(search.current_index(), 3);

    search.jump_to_match(2);
    assert_eq!(search.current_index(), 2);

    // Invalid index ignored
    search.jump_to_match(10);
    assert_eq!(search.current_index(), 2);

    search.jump_to_match(0);
    assert_eq!(search.current_index(), 2);
}

#[test]
fn current_match_returns_correct_match() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec!["hello world"]);

    search.start_search("world", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    let current = search.current_match().unwrap();
    assert_eq!(current.row, 0);
    assert_eq!(current.start_col, 6);
}
