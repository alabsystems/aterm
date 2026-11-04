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

/// NEGATIVE CONTROL: `is_ordered_cell_pair` rejects reversed rows and columns,
/// `cell_span_width` saturates to `0` when `start_col > end_col`, and
/// `encode_selection_bounds_le` / `decode_selection_bounds_le` round-trip signed rows and
/// unsigned columns in little-endian byte order.
#[test]
fn selection_bounds_helpers_order_span_and_roundtrip_le_bytes() {
    assert!(is_ordered_cell_pair(-5, usize::MAX, -4, 0));
    assert!(!is_ordered_cell_pair(-4, 0, -5, usize::MAX));
    assert!(is_ordered_cell_pair(7, 3, 7, 3));
    assert!(is_ordered_cell_pair(7, 3, 7, 4));
    assert!(!is_ordered_cell_pair(7, 4, 7, 3));
    assert!(is_ordered_cell_pair(i64::MIN, usize::MAX, i64::MAX, 0));
    assert!(!is_ordered_cell_pair(i64::MAX, 0, i64::MIN, usize::MAX));

    assert_eq!(cell_span_width(3, 10), 7);
    assert_eq!(cell_span_width(10, 10), 0);
    assert_eq!(cell_span_width(10, 3), 0);
    assert_eq!(cell_span_width(usize::MAX, 0), 0);
    assert_eq!(cell_span_width(0, usize::MAX), usize::MAX);

    let encoded = encode_selection_bounds_le(-2, 0x0102_0304, 0x0506_0708, u32::MAX);
    assert_eq!(
        encoded,
        [
            0xFE, 0xFF, 0xFF, 0xFF, // -2 in two's-complement LE
            0x04, 0x03, 0x02, 0x01, // start_col LE
            0x08, 0x07, 0x06, 0x05, // end_row LE
            0xFF, 0xFF, 0xFF, 0xFF, // end_col LE
        ]
    );
    for bounds in [
        (0, 0, 0, 0),
        (-1, 1, -1, u32::MAX),
        (i32::MIN, 0, i32::MAX, u32::MAX),
        (i32::MAX, u32::MAX, i32::MIN, 0),
        (-42, 120, 18, 7),
    ] {
        let bytes = encode_selection_bounds_le(bounds.0, bounds.1, bounds.2, bounds.3);
        assert_eq!(decode_selection_bounds_le(bytes), bounds);
    }
}
