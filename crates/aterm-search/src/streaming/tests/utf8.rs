// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Production-path UTF-8 tests — #2688.
//!
//! The Kani proofs (INV-SEARCH-2, INV-SEARCH-4) verify a `#[cfg(kani)]`
//! byte-level scanner, not the production `str::find()` path. These unit
//! tests exercise the production path with multi-byte UTF-8 content and
//! assert the same invariants.

use super::super::test_content::TestContent;
use super::super::*;

/// INV-SEARCH-2 (production path): result positions are valid with CJK text.
#[test]
fn utf8_cjk_result_positions_valid() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: true,
        ..Default::default()
    });

    // "你好hello世界" — CJK chars are 3 bytes each, display width 2
    search.start_search("hello", FilterMode::Literal).unwrap();
    let matches = search.find_matches_in_row(0, "你好hello世界");

    assert_eq!(matches.len(), 1);
    // 你(width 2) + 好(width 2) = column 4
    assert_eq!(matches[0].start_col, 4);
    assert_eq!(matches[0].end_col, 9); // 4 + 5 ASCII chars
    assert!(search.verify_result_positions_valid());
}

/// INV-SEARCH-2 (production path): multi-byte pattern in multi-byte text.
#[test]
fn utf8_multibyte_pattern_positions_valid() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: true,
        ..Default::default()
    });

    // Search for CJK pattern "你好" in text with two occurrences
    search.start_search("你好", FilterMode::Literal).unwrap();
    search.scan_row(0, "你好世界你好", 1);

    assert_eq!(search.results().len(), 2);
    // First: col 0, width 2 per CJK char → end_col 2
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[0].end_col, 4); // 2 CJK chars × width 2
    // Second: 世(w2)+界(w2) after first 你好(w4) → start at col 8
    assert_eq!(search.results()[1].start_col, 8);
    assert_eq!(search.results()[1].end_col, 12);

    assert!(search.verify_result_positions_valid());
    assert!(search.verify_no_duplicates());
}

/// INV-SEARCH-4 (production path): no duplicates with emoji boundaries.
#[test]
fn utf8_emoji_no_duplicate_results() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: true,
        ..Default::default()
    });

    // Emoji are 4 bytes each; search for ASCII between them
    search.start_search("ab", FilterMode::Literal).unwrap();
    search.scan_row(0, "🎉ab🎉ab🎉", 1);

    assert_eq!(search.results().len(), 2);
    assert!(search.verify_no_duplicates());
    assert!(search.verify_result_positions_valid());
    assert!(search.verify_all_invariants());
}

/// INV-SEARCH-2 + INV-SEARCH-4 (production path): overlapping ASCII matches.
///
/// The production path advances by one character (not match length) so
/// overlapping matches are found, same as the Kani byte-level scanner.
#[test]
fn overlapping_ascii_matches_production_path() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: true,
        ..Default::default()
    });

    search.start_search("aa", FilterMode::Literal).unwrap();
    search.scan_row(0, "aaaa", 1);

    // "aa" in "aaaa": positions 0, 1, 2 (overlapping)
    assert_eq!(search.results().len(), 3);
    assert_eq!(search.results()[0].start_col, 0);
    assert_eq!(search.results()[1].start_col, 1);
    assert_eq!(search.results()[2].start_col, 2);
    assert!(search.verify_no_duplicates());
    assert!(search.verify_result_positions_valid());
}

/// Production path: mixed ASCII and multi-byte with case-insensitive search.
#[test]
fn utf8_case_insensitive_mixed_content() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });

    // Case-insensitive "café" in mixed-script text
    search.start_search("café", FilterMode::Literal).unwrap();
    search.scan_row(0, "I love CAFÉ and café!", 1);

    assert_eq!(search.results().len(), 2);
    assert!(search.verify_result_positions_valid());
    assert!(search.verify_no_duplicates());
    assert!(search.verify_all_invariants());
}

/// Production path: all invariants hold with multi-row multi-byte content.
#[test]
fn utf8_multirow_all_invariants() {
    let mut search = StreamingSearch::new();
    let mut content = TestContent::new(vec![
        "日本語テスト",       // Japanese
        "hello world",        // ASCII
        "Ελληνικά test",      // Greek + ASCII
        "مرحبا test مرحبا",   // Arabic + ASCII
        "🦀 Rust 🦀 test 🦀", // Emoji + ASCII
    ]);

    search.start_search("test", FilterMode::Literal).unwrap();
    search.scan_all(&mut content);

    // "test" appears in rows: 2 (Ελληνικά test), 3 (مرحبا test مرحبا), 4 (🦀 Rust 🦀 test 🦀)
    assert_eq!(search.results().len(), 3);
    assert_eq!(search.results()[0].row, 2);
    assert_eq!(search.results()[1].row, 3);
    assert_eq!(search.results()[2].row, 4);

    assert!(search.verify_result_positions_valid());
    assert!(search.verify_no_duplicates());
    assert!(search.verify_all_invariants());
}

/// Regression: literal search for a combining mark produces a zero-display-width
/// match. INV-SEARCH-2c requires match_len > 0. Same bug as the regex path
/// (see `regex_combining_mark_zero_display_width_filtered`), but for literal mode.
#[test]
fn utf8_literal_combining_mark_zero_display_width_filtered() {
    let mut search = StreamingSearch::new();

    // U+0301 is the combining acute accent. Literal search for it in "a\u{0301}b".
    search
        .start_search("\u{0301}", FilterMode::Literal)
        .unwrap();
    search.scan_row(0, "a\u{0301}b", 1);

    // All results must have positive display width (zero-width filtered out)
    for m in search.results() {
        assert!(
            m.match_len > 0,
            "combining mark literal match should be filtered: start_col {} end_col {} match_len {}",
            m.start_col,
            m.end_col,
            m.match_len,
        );
    }
    assert!(search.verify_result_positions_valid());
}

/// Regression test: multi-byte case folding produces correct column positions.
///
/// Bug #2775: `map_lower_byte_to_original` must handle characters where
/// `to_lowercase()` changes the byte length. U+212A (Kelvin Sign) is 3
/// bytes but lowercases to 'k' (1 byte). Searching case-insensitively for "k"
/// in text containing the Kelvin sign must produce correct start/end columns.
#[test]
fn utf8_case_insensitive_kelvin_sign_case_folding() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        case_sensitive: false,
        ..Default::default()
    });

    // U+212A (Kelvin Sign) is 3 bytes in UTF-8, lowercases to 'k' (1 byte).
    // Text: "x\u{212A}y" — columns: x=0, Kelvin=1, y=2
    let text = "x\u{212A}y";
    search.start_search("k", FilterMode::Literal).unwrap();
    search.scan_row(0, text, 1);

    // Case-insensitive "k" should match the Kelvin sign
    assert_eq!(
        search.results().len(),
        1,
        "case-insensitive 'k' should match Kelvin sign U+212A"
    );
    // The Kelvin sign is at grapheme column 1
    assert_eq!(search.results()[0].start_col, 1);
    assert_eq!(search.results()[0].end_col, 2);
    assert!(search.verify_all_invariants());
}
