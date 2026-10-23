// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the smart selection system.

use super::*;

#[test]
fn test_no_match_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "just some regular words";

    let m = smart.find_at(text, 4);
    assert!(m.is_none(), "whitespace position should not match any rule");
}

#[test]
fn test_word_boundaries_with_match() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Check https://example.com please";

    let bounds = smart.word_boundaries_at(text, 15);
    assert_eq!(bounds, Some((6, 25)));
}

#[test]
fn test_word_boundaries_fallback() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "hello world";

    let bounds = smart.word_boundaries_at(text, 2);
    assert_eq!(bounds, Some((0, 5)));
}

/// Each builtin rule selects the whole token under the cursor and excludes surrounding punctuation.
#[test]
fn builtin_rules_select_the_token_under_the_cursor() {
    let smart = SmartSelection::with_builtin_rules();
    for (text, pos, matched, rule) in [
        (
            "Check out https://example.com for more info",
            15,
            "https://example.com",
            "url",
        ),
        (
            "Visit https://example.com.",
            10,
            "https://example.com",
            "url",
        ),
        (
            "(see https://example.com)",
            10,
            "https://example.com",
            "url",
        ),
        (
            "Link: https://example.com/path?q=1&b=2",
            10,
            "https://example.com/path?q=1&b=2",
            "url",
        ),
        (
            "Contact support@example.com for help",
            12,
            "support@example.com",
            "email",
        ),
        (
            "Edit ./config.yaml to fix",
            10,
            "./config.yaml",
            "file_path",
        ),
        (
            "Check ../config/app.yaml file",
            10,
            "../config/app.yaml",
            "file_path",
        ),
        (
            "Server 192.168.1.1 is responding",
            10,
            "192.168.1.1",
            "ipv4",
        ),
        (
            r#"Run command "git status" now"#,
            15,
            r#""git status""#,
            "double_quoted",
        ),
        ("Set value='hello' here", 12, "'hello'", "single_quoted"),
        (
            "Run `git status` command",
            6,
            "`git status`",
            "backtick_quoted",
        ),
        (
            "Resource 550e8400-e29b-41d4-a716-446655440000 not found",
            20,
            "550e8400-e29b-41d4-a716-446655440000",
            "uuid",
        ),
    ] {
        let m = smart
            .find_at(text, pos)
            .unwrap_or_else(|| panic!("{text:?} @ {pos}: no match"));
        assert_eq!(
            (m.matched_text(), m.rule_name()),
            (matched, rule),
            "{text:?} @ {pos}"
        );
    }
}
