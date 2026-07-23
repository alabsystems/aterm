// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani verification proofs for grapheme operations.
//!
//! Split into:
//! - This file: width, position, container, and character classification proofs
//! - `config_proofs`: `TextShapingConfig`-dependent proofs and `classify_grapheme`

mod config_proofs;

use super::*;

/// Verify grapheme_width never exceeds 2 for representative characters.
///
/// Uses bounded reduction: test boundary characters from each width category
/// instead of all ~1.1M Unicode characters.
#[kani::proof]
#[kani::unwind(10)] // Enough for unicode-width library's internal loops
fn grapheme_display_width_bounded() {
    // Representative characters covering all width categories
    const REPRESENTATIVES: [char; 16] = [
        // Width 1 (ASCII)
        'a',
        'Z',
        '0',
        ' ',
        // Width 0 (control)
        '\0',
        '\x1F',
        // Width 2 (CJK)
        '\u{4E00}',
        '\u{9FFF}',
        '\u{3040}',
        // Width 2 (Emoji)
        '\u{1F600}',
        '\u{1F680}',
        // Width varies (special)
        '\u{00C0}',
        '\u{0370}',
        '\u{0600}',
        // Fullwidth
        '\u{FF00}',
        '\u{FFFF}',
    ];

    let idx: usize = kani::any();
    kani::assume(idx < REPRESENTATIVES.len());

    let c = REPRESENTATIVES[idx];
    let s = c.to_string();
    let width = grapheme_display_width(&s);
    kani::assert(width <= 2, "Grapheme width must be 0, 1, or 2");
}

/// Verify truncate_to_width returns valid UTF-8.
///
/// Uses fixed representative values to avoid loop unrolling explosion.
/// Tests boundary cases: empty truncate, partial truncate, no truncate.
///
/// NOTE: This harness is sensitive to unwind bounds because truncate_to_width
/// calls split_graphemes() → grapheme_indices(true) from unicode-segmentation.
/// The crate's grapheme boundary state machine creates exponential CBMC paths.
/// unwind(10) helps but may still timeout on slower machines; was 599.5s at
/// unwind(5). If this continues to timeout, consider a Kani-specific stub
/// for split_graphemes that bypasses unicode-segmentation.
#[kani::proof]
#[kani::unwind(10)]
fn truncate_preserves_utf8() {
    // Fixed (string, max_width) pairs covering:
    // - max_width 0: truncate to nothing
    // - max_width < string width: partial truncate
    // - max_width >= string width: no truncate needed
    const TEST_CASES: [(&str, usize); 4] = [
        ("a", 0),   // Truncate single char to nothing
        ("ab", 1),  // Truncate to partial
        ("abc", 5), // No truncate needed (width fits)
        ("", 3),    // Empty string edge case
    ];

    let case_idx: usize = kani::any();
    kani::assume(case_idx < TEST_CASES.len());
    let (s, max_width) = TEST_CASES[case_idx];

    let result = truncate_to_width(s, max_width);
    // Result is a valid string slice (guaranteed by &str)
    kani::assert(
        result.len() <= s.len(),
        "Truncated string not longer than original",
    );
}

/// Verify column_to_byte returns valid byte offset.
#[kani::proof]
fn column_to_byte_valid() {
    let test = "ab";
    let col: usize = kani::any();
    kani::assume(col <= 10);

    let byte = column_to_byte(test, col);
    kani::assert(byte <= test.len(), "Byte offset within bounds");
}

/// Verify byte_to_column returns monotonic values.
#[kani::proof]
fn byte_to_column_monotonic() {
    let test = "ab";
    let b1: usize = kani::any();
    let b2: usize = kani::any();
    kani::assume(b1 <= test.len());
    kani::assume(b2 <= test.len());
    kani::assume(b1 <= b2);

    let c1 = byte_to_column(test, b1);
    let c2 = byte_to_column(test, b2);
    kani::assert(c1 <= c2, "Column positions are monotonic");
}

/// For pure ASCII strings, byte_to_column(s, n) == n (identity mapping).
#[kani::proof]
#[kani::unwind(12)]
fn byte_to_column_ascii_identity() {
    let s = "abcdefghij";
    let n: usize = kani::any();
    kani::assume(n <= s.len());

    let col = byte_to_column(s, n);
    kani::assert(col == n, "ASCII: byte offset must equal column");
}

/// byte_to_column(s, 0) == 0 for any string: no graphemes precede offset 0.
#[kani::proof]
#[kani::unwind(14)]
fn byte_to_column_identity_at_zero() {
    const TEST_STRINGS: [&str; 4] = ["hello world", "", "A", "\u{4E00}B"];
    let idx: usize = kani::any();
    kani::assume(idx < TEST_STRINGS.len());

    let col = byte_to_column(TEST_STRINGS[idx], 0);
    kani::assert(col == 0, "byte_to_column(s, 0) must be 0");
}

/// byte_to_column(s, n) <= display_width(s) for any offset n within bounds.
#[kani::proof]
#[kani::unwind(14)]
fn byte_to_column_bounded_by_display_width() {
    // Use our internal str_width (generated from Unicode 16.0 tables) instead of
    // the external unicode-width crate: unicode-width is a dev-dependency and is
    // not visible to #[cfg(kani)] builds. Part of #7954 drift-hygiene rerun.
    use crate::tables::str_width;

    let s = "hello world";
    let n: usize = kani::any();
    kani::assume(n <= s.len() + 1);

    let col = byte_to_column(s, n);
    let display_width = str_width(s);
    kani::assert(
        col <= display_width,
        "column must not exceed total display width",
    );
}

/// Verify GraphemeSegmenter column advances correctly for representative strings.
#[kani::proof]
fn segmenter_column_advances() {
    const TEST_STRINGS: [&str; 4] = ["a", "Z", " ", "0"];
    let idx: usize = kani::any();
    kani::assume(idx < TEST_STRINGS.len());

    let mut seg = GraphemeSegmenter::new();
    let initial = seg.column();

    let test = TEST_STRINGS[idx];
    let info = seg.process_string(test);

    kani::assert(
        seg.column() == initial + info.display_width,
        "Column advances by display width",
    );
}

/// Verify GraphemeCells contains_col is correct.
#[kani::proof]
fn cells_contains_col_correct() {
    let start: usize = kani::any();
    let count: usize = kani::any();
    kani::assume(count >= 1 && count <= 2);
    kani::assume(start <= 1000);

    let cells = GraphemeCells {
        start_col: start,
        cell_count: count,
        is_wide: count == 2,
    };

    let test_col: usize = kani::any();
    kani::assume(test_col <= 1010);

    let contains = cells.contains_col(test_col);
    let expected = test_col >= start && test_col < start + count;
    kani::assert(contains == expected, "contains_col matches manual check");
}

/// Verify is_emoji_char covers expected ranges with representative characters.
///
/// Uses bounded reduction: test boundary values at emoji range boundaries
/// instead of all ~1.1M Unicode characters.
#[kani::proof]
#[kani::unwind(5)] // Simple function, minimal loops
fn emoji_char_detection() {
    // Test boundary values at each emoji range
    const EMOJI_BOUNDARIES: [char; 22] = [
        // Non-emoji for comparison
        'a',
        'Z',
        '\0',
        // Dingbats (0x2600-0x26FF)
        '\u{25FF}', // before
        '\u{2600}', // start (should be emoji)
        '\u{26FF}', // end (should be emoji)
        '\u{2700}', // after (misc symbols, should be emoji)
        // Misc symbols (0x2700-0x27BF)
        '\u{27BF}', // end
        '\u{27C0}', // after
        // Supplemental (0x1F300-0x1F5FF)
        '\u{1F2FF}', // before
        '\u{1F300}', // start
        '\u{1F5FF}', // end
        // Emoticons (0x1F600-0x1F64F)
        '\u{1F600}', // start
        '\u{1F64F}', // end
        // Transport (0x1F680-0x1F6FF)
        '\u{1F680}', // start
        '\u{1F6FF}', // end
        // Regional indicators (0x1F1E0-0x1F1FF)
        '\u{1F1E0}', // start
        '\u{1F1FF}', // end
        // Supplemental symbols (0x1F900-0x1F9FF)
        '\u{1F900}', // start
        '\u{1F9FF}', // end
        // Variation selector
        '\u{FE0F}', // emoji presentation
        // Other
        '\u{4E00}', // CJK (not emoji)
    ];

    let idx: usize = kani::any();
    kani::assume(idx < EMOJI_BOUNDARIES.len());

    let c = EMOJI_BOUNDARIES[idx];
    let result = is_emoji_char(c);
    // ASCII and CJK are not emoji
    if c == 'a' || c == 'Z' || c == '\0' || c == '\u{4E00}' {
        kani::assert(
            !result,
            "ASCII and CJK chars must not be classified as emoji",
        );
    }
    // Core emoji ranges must be detected
    if c >= '\u{1F600}' && c <= '\u{1F64F}' {
        kani::assert(result, "emoticons (U+1F600-1F64F) must be emoji");
    }
    if c >= '\u{1F680}' && c <= '\u{1F6FF}' {
        kani::assert(result, "transport symbols (U+1F680-1F6FF) must be emoji");
    }
}

/// Verify skin tone modifier detection is correct for Fitzpatrick scale.
#[kani::proof]
fn skin_tone_modifier_range() {
    let codepoint: u32 = kani::any();
    kani::assume(codepoint >= 0x1F3FB && codepoint <= 0x1F3FF);

    if let Some(c) = char::from_u32(codepoint) {
        kani::assert(
            is_skin_tone_modifier(c),
            "Fitzpatrick modifiers must be detected",
        );
    }
}

/// Verify regional indicator detection is correct.
#[kani::proof]
fn regional_indicator_range() {
    let codepoint: u32 = kani::any();
    kani::assume(codepoint >= 0x1F1E6 && codepoint <= 0x1F1FF);

    if let Some(c) = char::from_u32(codepoint) {
        kani::assert(
            is_regional_indicator(c),
            "Regional indicators must be detected",
        );
    }
}

/// Verify has_zwj detects ZWJ character in representative strings.
#[kani::proof]
fn has_zwj_with_zwj() {
    // Strings containing ZWJ must return true
    const ZWJ_STRINGS: [&str; 3] = [
        "x\u{200D}y",             // base + ZWJ + base
        "\u{200D}",               // bare ZWJ
        "👨\u{200D}👩\u{200D}👧", // family ZWJ sequence
    ];
    let idx: usize = kani::any();
    kani::assume(idx < ZWJ_STRINGS.len());
    kani::assert(
        has_zwj(ZWJ_STRINGS[idx]),
        "String with ZWJ must be detected",
    );
}

/// Verify has_zwj returns false for strings without ZWJ.
#[kani::proof]
fn has_zwj_without_zwj() {
    const NON_ZWJ_STRINGS: [&str; 4] = [
        "abc",       // plain ASCII
        "",          // empty
        "\u{4E00}",  // CJK
        "\u{0300}A", // combining mark + ASCII
    ];
    let idx: usize = kani::any();
    kani::assume(idx < NON_ZWJ_STRINGS.len());
    kani::assert(
        !has_zwj(NON_ZWJ_STRINGS[idx]),
        "String without ZWJ must not be detected",
    );
}
