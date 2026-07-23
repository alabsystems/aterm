// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for `TextShapingConfig`-dependent width and `classify_grapheme`.

use crate::*;
use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

/// Verify grapheme_display_width_with_config returns bounded values.
///
/// This proof verifies:
/// 1. Result is always in [0, 2] for both AmbiguousWidth modes
/// 2. Single mode matches the non-config function
/// 3. Double mode returns >= Single mode (ambiguous chars are wider)
#[kani::proof]
#[kani::unwind(10)]
fn grapheme_display_width_with_config_bounded() {
    // Test representative characters covering different width categories
    const REPRESENTATIVES: [&str; 8] = [
        "a",        // ASCII (width 1)
        "中",       // CJK (width 2)
        "°",        // Ambiguous (U+00B0, width 1 or 2)
        "—",        // Em-dash (U+2014, ambiguous)
        "★",        // Black star (U+2605, ambiguous)
        "\u{2500}", // Box drawing (ambiguous)
        " ",        // Space (width 1)
        "\0",       // Control (width 0)
    ];

    let idx: usize = kani::any();
    kani::assume(idx < REPRESENTATIVES.len());
    let grapheme = REPRESENTATIVES[idx];

    let single_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Single,
        ..Default::default()
    };
    let double_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };

    let width_single = grapheme_display_width_with_config(grapheme, &single_config);
    let width_double = grapheme_display_width_with_config(grapheme, &double_config);

    // Both results must be in [0, 2]
    kani::assert(width_single <= 2, "Single mode width must be <= 2");
    kani::assert(width_double <= 2, "Double mode width must be <= 2");

    // Single mode must match non-config function
    let width_basic = grapheme_display_width(grapheme);
    kani::assert(
        width_single == width_basic,
        "Single mode must match basic function",
    );

    // Double mode width >= Single mode width (ambiguous chars may be wider)
    kani::assert(
        width_double >= width_single,
        "Double mode width must be >= Single mode",
    );
}

/// Verify grapheme_display_width_with_config behavior for ambiguous characters.
///
/// Ambiguous characters should have width 1 in Single mode and width 2
/// in Double (CJK) mode.
#[kani::proof]
fn grapheme_display_width_config_ambiguous_chars() {
    // Canonical ambiguous-width characters from different Unicode blocks
    const AMBIGUOUS: [&str; 4] = [
        "°",        // U+00B0 Degree sign
        "—",        // U+2014 Em-dash
        "★",        // U+2605 Black star
        "\u{2500}", // Box drawing horizontal
    ];
    let idx: usize = kani::any();
    kani::assume(idx < AMBIGUOUS.len());
    let grapheme = AMBIGUOUS[idx];

    let single_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Single,
        ..Default::default()
    };
    let double_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };

    let width_single = grapheme_display_width_with_config(grapheme, &single_config);
    let width_double = grapheme_display_width_with_config(grapheme, &double_config);

    // Verify expected behavior: ambiguous chars are 1 in Single, 2 in Double
    kani::assert(
        width_single == 1,
        "Ambiguous char must be width 1 in Single mode",
    );
    kani::assert(
        width_double == 2,
        "Ambiguous char must be width 2 in Double mode",
    );
}

/// Verify ASCII characters are unaffected by ambiguous_width config.
#[kani::proof]
fn grapheme_display_width_config_ascii_invariant() {
    const ASCII_CHARS: [&str; 5] = ["a", "Z", "0", "!", "~"];
    let idx: usize = kani::any();
    kani::assume(idx < ASCII_CHARS.len());
    let grapheme = ASCII_CHARS[idx];

    let single_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Single,
        ..Default::default()
    };
    let double_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };

    let width_single = grapheme_display_width_with_config(grapheme, &single_config);
    let width_double = grapheme_display_width_with_config(grapheme, &double_config);

    kani::assert(width_single == 1, "ASCII must be width 1 in Single mode");
    kani::assert(width_double == 1, "ASCII must be width 1 in Double mode");
}

/// Verify CJK characters are unaffected by ambiguous_width config.
#[kani::proof]
fn grapheme_display_width_config_cjk_invariant() {
    const CJK_CHARS: [&str; 4] = [
        "中",       // U+4E00
        "\u{9FFF}", // CJK end
        "\u{3041}", // Hiragana
        "\u{30A0}", // Katakana
    ];
    let idx: usize = kani::any();
    kani::assume(idx < CJK_CHARS.len());
    let grapheme = CJK_CHARS[idx];

    let single_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Single,
        ..Default::default()
    };
    let double_config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };

    let width_single = grapheme_display_width_with_config(grapheme, &single_config);
    let width_double = grapheme_display_width_with_config(grapheme, &double_config);

    kani::assert(width_single == 2, "CJK must be width 2 in Single mode");
    kani::assert(width_double == 2, "CJK must be width 2 in Double mode");
}

/// Verify classify_grapheme doesn't panic for representative characters.
///
/// Uses bounded reduction: test boundary values from each category
/// instead of all ~1.1M Unicode characters. Unlike the other bounded
/// proofs, this harness avoids symbolic string construction (which can
/// defeat slicing and explode solver state) by calling classify_grapheme
/// with a small, fixed set of representative `&'static str` inputs.
///
/// Categories tested:
/// - ASCII printable (0x20-0x7E)
/// - ASCII control (0x00-0x1F, 0x7F)
/// - CJK wide characters
/// - Basic emoji
/// - Regional indicators
/// - Skin tone modifiers
/// - Combining marks
/// - ZWJ sequences
/// - Various Unicode blocks (Latin, Greek, Arabic, etc.)
#[kani::proof]
#[kani::unwind(32)] // Covers UTF-8 scanning / pattern loops for the longest fixed inputs
fn classify_grapheme_no_panic() {
    fn assert_valid_classification(grapheme: &str, expected: Option<GraphemeType>) {
        let result = classify_grapheme(grapheme);
        // Exhaustive match proves all variants are handled
        let code = match result {
            GraphemeType::Ascii => 0u8,
            GraphemeType::Wide => 1,
            GraphemeType::Emoji => 2,
            GraphemeType::ZwjSequence => 3,
            GraphemeType::Flag => 4,
            GraphemeType::Combining => 5,
            GraphemeType::Control => 6,
            GraphemeType::Other => 7,
        };
        kani::assert(code <= 7, "classification must be a known variant");
        if let Some(exp) = expected {
            kani::assert(
                std::mem::discriminant(&result) == std::mem::discriminant(&exp),
                "classification must match expected type",
            );
        }
    }

    // ASCII printable boundaries — must classify as Ascii
    assert_valid_classification(" ", Some(GraphemeType::Ascii));
    assert_valid_classification("!", Some(GraphemeType::Ascii));
    assert_valid_classification("A", Some(GraphemeType::Ascii));
    assert_valid_classification("z", Some(GraphemeType::Ascii));
    assert_valid_classification("~", Some(GraphemeType::Ascii));

    // ASCII control — must classify as Control
    assert_valid_classification("\0", Some(GraphemeType::Control));
    assert_valid_classification("\x1B", Some(GraphemeType::Control));
    assert_valid_classification("\x7F", Some(GraphemeType::Control));

    // Latin extended
    assert_valid_classification("\u{00C0}", None); // À
    assert_valid_classification("\u{00FF}", None); // ÿ

    // CJK (wide characters) — must classify as Wide
    assert_valid_classification("\u{4E00}", Some(GraphemeType::Wide)); // 一
    assert_valid_classification("\u{9FFF}", Some(GraphemeType::Wide));
    assert_valid_classification("\u{3041}", Some(GraphemeType::Wide)); // Hiragana ぁ
    assert_valid_classification("\u{30A0}", Some(GraphemeType::Wide)); // Katakana start

    // Emoji / symbols
    assert_valid_classification("\u{2600}", None); // ☀
    assert_valid_classification("\u{26FF}", None);
    assert_valid_classification("\u{1F300}", None); // 🌀
    assert_valid_classification("\u{1F600}", None); // 😀
    assert_valid_classification("\u{1F64F}", None); // 🙏
    assert_valid_classification("\u{1F680}", None); // 🚀

    // Flag emoji (regional indicator pair) — must classify as Flag
    assert_valid_classification("🇺🇸", Some(GraphemeType::Flag));

    // Skin tone modifier sequence
    assert_valid_classification("👋🏽", None);

    // Variation selector (emoji presentation)
    assert_valid_classification("✌️", None);

    // ZWJ sequence — must classify as ZwjSequence
    assert_valid_classification("👨\u{200D}👩\u{200D}👧", Some(GraphemeType::ZwjSequence));
    assert_valid_classification("x\u{200D}y", None);

    // Combining marks — must classify as Combining
    assert_valid_classification("e\u{0301}", Some(GraphemeType::Combining));

    // Other Unicode blocks
    assert_valid_classification("\u{0370}", None); // Greek
    assert_valid_classification("\u{0600}", None); // Arabic
    assert_valid_classification("\u{0900}", None); // Devanagari
    assert_valid_classification("\u{3000}", None); // Ideographic space
    assert_valid_classification("\u{FF00}", None); // Fullwidth forms
    assert_valid_classification("\u{FFFF}", None); // Last BMP scalar
}
