// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

#[test]
fn test_ascii_graphemes() {
    let info = grapheme_width("Hello");
    assert_eq!(info.grapheme_count, 5);
    assert_eq!(info.display_width, 5);
    assert_eq!(info.codepoint_count, 5);
    assert!(!info.has_emoji);
    assert!(!info.has_combining);
    assert!(!info.has_wide);
}

#[test]
fn test_cjk_graphemes() {
    // CJK characters are 2 cells wide
    let info = grapheme_width("中文");
    assert_eq!(info.grapheme_count, 2);
    assert_eq!(info.display_width, 4);
    assert!(!info.has_emoji);
    assert!(info.has_wide);
}

#[test]
fn test_emoji_graphemes() {
    // Simple emoji
    let info = grapheme_width("\u{1F600}");
    assert_eq!(info.grapheme_count, 1);
    assert_eq!(info.display_width, 2);
    assert!(info.has_emoji);
}

#[test]
fn test_emoji_zwj_sequence() {
    // Family emoji: man + ZWJ + woman + ZWJ + girl + ZWJ + boy
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let info = grapheme_width(family);
    assert_eq!(info.grapheme_count, 1); // Single grapheme cluster
    assert!(info.has_emoji);
    assert!(info.codepoint_count > 1); // Multiple codepoints
}

#[test]
fn test_combining_characters() {
    // e + combining acute accent
    let text = "e\u{0301}";
    let info = grapheme_width(text);
    assert_eq!(info.grapheme_count, 1);
    assert_eq!(info.display_width, 1);
    assert_eq!(info.codepoint_count, 2);
    assert!(info.has_combining);
}

#[test]
fn test_regional_indicators() {
    // US flag: regional indicator U + regional indicator S
    let flag = "\u{1F1FA}\u{1F1F8}";
    let info = grapheme_width(flag);
    assert_eq!(info.grapheme_count, 1);
    assert!(info.has_emoji);
}

#[test]
fn test_skin_tone_grapheme() {
    // Wave + medium skin tone
    let wave = "\u{1F44B}\u{1F3FD}";
    let info = grapheme_width(wave);
    assert_eq!(info.grapheme_count, 1);
    assert!(info.has_emoji);
    assert!(info.codepoint_count >= 2);
}

#[test]
fn test_mixed_text() {
    let text = "Hello \u{4E16}\u{754C} \u{1F44B}";
    let info = grapheme_width(text);
    assert_eq!(info.grapheme_count, 10); // H e l l o ' ' 世 界 ' ' 👋
    assert!(info.has_wide);
    assert!(info.has_emoji);
}

#[test]
fn test_split_graphemes() {
    let text = "Hello";
    let graphemes: Vec<_> = split_graphemes(text).collect();
    assert_eq!(graphemes.len(), 5);
    assert_eq!(graphemes[0].text, "H");
    assert_eq!(graphemes[0].byte_offset, 0);
    assert_eq!(graphemes[0].width, 1);
    assert!(graphemes[0].is_ascii());
}

#[test]
fn test_grapheme_at_byte() {
    let text = "Hello \u{4E16}\u{754C}";

    // ASCII portion
    let g = grapheme_at_byte(text, 0).unwrap();
    assert_eq!(g.text, "H");

    let g = grapheme_at_byte(text, 4).unwrap();
    assert_eq!(g.text, "o");

    // CJK portion (世 is at bytes 6-8 in UTF-8)
    let g = grapheme_at_byte(text, 6).unwrap();
    assert_eq!(g.text, "\u{4E16}");

    // Out of bounds
    assert!(grapheme_at_byte(text, 100).is_none());
}

#[test]
fn test_grapheme_at_column() {
    let text = "Hello \u{4E16}\u{754C}";

    // Column 0 is 'H'
    let g = grapheme_at_column(text, 0).unwrap();
    assert_eq!(g.text, "H");

    // Column 5 is space
    let g = grapheme_at_column(text, 5).unwrap();
    assert_eq!(g.text, " ");

    // Column 6 is '世' (wide char)
    let g = grapheme_at_column(text, 6).unwrap();
    assert_eq!(g.text, "\u{4E16}");

    // Column 7 is still '世' (second cell of wide char)
    let g = grapheme_at_column(text, 7).unwrap();
    assert_eq!(g.text, "\u{4E16}");

    // Column 8 is '界'
    let g = grapheme_at_column(text, 8).unwrap();
    assert_eq!(g.text, "\u{754C}");
}

#[test]
fn test_byte_to_column() {
    let text = "Hello \u{4E16}\u{754C}";

    assert_eq!(byte_to_column(text, 0), 0); // 'H'
    assert_eq!(byte_to_column(text, 5), 5); // space
    assert_eq!(byte_to_column(text, 6), 6); // '世'
    assert_eq!(byte_to_column(text, 9), 8); // '界' (after 世's 3 bytes)
}

#[test]
fn test_column_to_byte() {
    let text = "Hello \u{4E16}\u{754C}";

    assert_eq!(column_to_byte(text, 0), 0); // 'H'
    assert_eq!(column_to_byte(text, 5), 5); // space
    assert_eq!(column_to_byte(text, 6), 6); // '世'
    assert_eq!(column_to_byte(text, 7), 6); // still '世' (second cell)
    assert_eq!(column_to_byte(text, 8), 9); // '界'
}

#[test]
fn test_column_to_char_index_ascii() {
    let text = "Hello World";
    assert_eq!(column_to_char_index(text, 0), 0);
    assert_eq!(column_to_char_index(text, 5), 5);
    assert_eq!(column_to_char_index(text, 10), 10);
    assert_eq!(column_to_char_index(text, 15), 11); // past end
}

#[test]
fn test_column_to_char_index_wide_chars() {
    // CJK characters: "你好world" - each CJK char is 2 columns wide
    let text = "\u{4F60}\u{597D}world";
    assert_eq!(column_to_char_index(text, 0), 0);
    assert_eq!(column_to_char_index(text, 1), 0);
    assert_eq!(column_to_char_index(text, 2), 1);
    assert_eq!(column_to_char_index(text, 3), 1);
    assert_eq!(column_to_char_index(text, 4), 2);
    assert_eq!(column_to_char_index(text, 5), 3);
}

#[test]
fn test_column_to_char_index_combining() {
    let text = "e\u{0301}x"; // e + combining acute + x
    assert_eq!(column_to_char_index(text, 0), 0);
    assert_eq!(column_to_char_index(text, 1), 2);
}

#[test]
fn test_column_to_char_index_zwj_sequence() {
    let emoji = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let text = format!("{emoji}x");
    let emoji_chars = emoji.chars().count();
    assert_eq!(column_to_char_index(&text, 0), 0);
    assert_eq!(column_to_char_index(&text, 1), 0);
    assert_eq!(column_to_char_index(&text, 2), emoji_chars);
}

#[test]
fn test_byte_to_column_combining() {
    let text = "e\u{0301}x";
    let indices: Vec<usize> = text.char_indices().map(|(idx, _)| idx).collect();
    let byte_combining = indices[1];
    let byte_after_combining = indices[2];
    assert_eq!(byte_to_column(text, byte_combining), 1);
    assert_eq!(byte_to_column(text, byte_after_combining), 1);
}

#[test]
fn test_byte_to_column_zwj_sequence() {
    let emoji = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let text = format!("{emoji}x");
    let byte_inside = text.char_indices().nth(1).map(|(idx, _)| idx).unwrap();
    let byte_x = text.find('x').unwrap();
    assert_eq!(byte_to_column(&text, 0), 0);
    assert_eq!(byte_to_column(&text, byte_inside), 2);
    assert_eq!(byte_to_column(&text, byte_x), 2);
    assert_eq!(byte_to_column(&text, text.len()), 3);
}

#[test]
fn test_truncate_to_width() {
    // ASCII only
    assert_eq!(truncate_to_width("Hello World", 5), "Hello");

    // With wide chars - should not split wide char
    assert_eq!(
        truncate_to_width("Hello \u{4E16}\u{754C}", 8),
        "Hello \u{4E16}"
    );
    assert_eq!(truncate_to_width("Hello \u{4E16}\u{754C}", 7), "Hello "); // 世 needs 2 cols

    // Exact fit
    assert_eq!(truncate_to_width("Hello", 5), "Hello");
}

#[test]
fn test_pad_to_width() {
    assert_eq!(pad_to_width("Hi", 5), "Hi   ");
    assert_eq!(pad_to_width("Hello", 5), "Hello");
    assert_eq!(pad_to_width("Hello World", 5), "Hello");
}

#[test]
fn test_is_ascii_only() {
    assert!(is_ascii_only("Hello World"));
    assert!(!is_ascii_only("Hello \u{4E16}\u{754C}"));
    assert!(!is_ascii_only("caf\u{00E9}"));
}

#[test]
fn test_ascii_width() {
    assert_eq!(ascii_width("Hello"), 5);
    assert_eq!(ascii_width("Hello\n"), 5); // newline is control
    assert_eq!(ascii_width(""), 0);
}

#[test]
fn test_grapheme_segmenter() {
    let mut seg = GraphemeSegmenter::new();

    let text = "Hello";
    let info = seg.process_string(text);
    assert_eq!(info.display_width, 5);
    assert_eq!(seg.column(), 5);
    assert_eq!(seg.index(), 5);

    // Process more text
    let info2 = seg.process_string(" \u{4E16}");
    assert_eq!(info2.display_width, 3);
    assert_eq!(seg.column(), 8);
}

#[test]
fn test_assign_cells() {
    let text = "a\u{4E16}b";
    let cells: Vec<_> = assign_cells(text, 0).collect();

    assert_eq!(cells.len(), 3);

    // 'a' at column 0, width 1
    assert_eq!(cells[0].0.text, "a");
    assert_eq!(cells[0].1.start_col, 0);
    assert_eq!(cells[0].1.cell_count, 1);
    assert!(!cells[0].1.is_wide);

    // '世' at column 1, width 2
    assert_eq!(cells[1].0.text, "\u{4E16}");
    assert_eq!(cells[1].1.start_col, 1);
    assert_eq!(cells[1].1.cell_count, 2);
    assert!(cells[1].1.is_wide);

    // 'b' at column 3, width 1
    assert_eq!(cells[2].0.text, "b");
    assert_eq!(cells[2].1.start_col, 3);
    assert_eq!(cells[2].1.cell_count, 1);
}

#[test]
fn test_grapheme_info_flags() {
    let g: Vec<_> = split_graphemes("Hello").collect();
    assert!(g[0].is_ascii());
    assert!(!g[0].is_whitespace());
    assert!(!g[0].is_control());

    let g: Vec<_> = split_graphemes(" ").collect();
    assert!(g[0].is_whitespace());

    let g: Vec<_> = split_graphemes("\n").collect();
    assert!(g[0].is_control());
}

#[test]
fn test_zwj_detection() {
    // Family emoji with ZWJ
    assert!(has_zwj(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"
    ));
    assert!(has_zwj("\u{1F468}\u{200D}\u{1F680}")); // Man astronaut

    // Simple emoji without ZWJ
    assert!(!has_zwj("\u{1F600}"));
    assert!(!has_zwj("\u{1F389}"));
    assert!(!has_zwj("A"));
}

#[test]
fn test_skin_tone_detection() {
    assert!(has_skin_tone("\u{1F44B}\u{1F3FD}")); // Wave with medium skin
    assert!(has_skin_tone("\u{1F44D}\u{1F3FB}")); // Thumbs up light skin
    assert!(!has_skin_tone("\u{1F44B}")); // Wave without modifier
    assert!(!has_skin_tone("\u{1F600}")); // No skin tone
}

#[test]
fn test_skin_tone_modifier() {
    assert!(is_skin_tone_modifier('\u{1F3FB}')); // Light
    assert!(is_skin_tone_modifier('\u{1F3FD}')); // Medium
    assert!(is_skin_tone_modifier('\u{1F3FF}')); // Dark
    assert!(!is_skin_tone_modifier('A'));
    assert!(!is_skin_tone_modifier('\u{1F600}'));
}

#[test]
fn test_regional_indicator() {
    assert!(is_regional_indicator('\u{1F1FA}')); // U
    assert!(is_regional_indicator('\u{1F1F8}')); // S
    assert!(!is_regional_indicator('A'));
    assert!(!is_regional_indicator('\u{1F600}'));
}

#[test]
fn test_flag_emoji() {
    assert!(is_flag_emoji("\u{1F1FA}\u{1F1F8}")); // US flag
    assert!(is_flag_emoji("\u{1F1EF}\u{1F1F5}")); // Japan flag
    assert!(is_flag_emoji("\u{1F1EC}\u{1F1E7}")); // UK flag
    assert!(!is_flag_emoji("\u{1F600}")); // Not a flag
    assert!(!is_flag_emoji("A")); // Not a flag
    assert!(!is_flag_emoji("\u{1F1FA}")); // Single regional indicator
}

#[test]
fn test_classify_grapheme() {
    assert_eq!(classify_grapheme("a"), GraphemeType::Ascii);
    assert_eq!(classify_grapheme("Z"), GraphemeType::Ascii);
    assert_eq!(classify_grapheme(" "), GraphemeType::Ascii);
    assert_eq!(classify_grapheme("\n"), GraphemeType::Control);
    assert_eq!(classify_grapheme("\x00"), GraphemeType::Control);
    assert_eq!(classify_grapheme("\u{4E2D}"), GraphemeType::Wide);
    assert_eq!(classify_grapheme("\u{65E5}"), GraphemeType::Wide);
    assert_eq!(classify_grapheme("\u{1F600}"), GraphemeType::Emoji);
    assert_eq!(classify_grapheme("\u{1F389}"), GraphemeType::Emoji);
    assert_eq!(
        classify_grapheme("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
        GraphemeType::ZwjSequence
    );
    assert_eq!(classify_grapheme("\u{1F1FA}\u{1F1F8}"), GraphemeType::Flag);
    assert_eq!(classify_grapheme("e\u{0301}"), GraphemeType::Combining); // combining accent
}

#[test]
fn test_classify_empty() {
    assert_eq!(classify_grapheme(""), GraphemeType::Control);
}

#[test]
fn test_empty_string() {
    let info = grapheme_width("");
    assert_eq!(info.grapheme_count, 0);
    assert_eq!(info.display_width, 0);

    assert_eq!(truncate_to_width("", 10), "");
    assert_eq!(pad_to_width("", 5), "     ");
}

#[test]
fn test_grapheme_contains_col() {
    let cells = GraphemeCells {
        start_col: 5,
        cell_count: 2,
        is_wide: true,
    };

    assert!(!cells.contains_col(4));
    assert!(cells.contains_col(5));
    assert!(cells.contains_col(6));
    assert!(!cells.contains_col(7));
}

// Zero-width grapheme column mapping tests (#7605)

#[test]
fn test_column_to_char_index_standalone_zwj() {
    // Standalone ZWJ (U+200D) at the start: zero-width grapheme occupies 1 column
    // for mapping purposes, consistent with column_to_byte and grapheme_at_column.
    let text = "\u{200D}x";
    // ZWJ is grapheme 0 (1 char, 0 display width but treated as 1 for mapping)
    // x is grapheme 1 (1 char)
    assert_eq!(column_to_char_index(text, 0), 0); // ZWJ at column 0
    assert_eq!(column_to_char_index(text, 1), 1); // x at column 1
}

#[test]
fn test_column_to_char_index_zwnj_clusters_with_base() {
    // ZWNJ (U+200C) clusters with the preceding character per UAX #29.
    // "a\u{200C}b" = 2 graphemes: "a\u{200C}" (width 1, 2 chars) + "b" (width 1, 1 char)
    let text = "a\u{200C}b";
    assert_eq!(column_to_char_index(text, 0), 0); // 'a'+ZWNJ cluster
    assert_eq!(column_to_char_index(text, 1), 2); // 'b' (char index 2, after 'a' and ZWNJ)
}

#[test]
fn test_column_to_char_index_leading_combining_mark() {
    // A standalone combining mark at the start of a string (no base character).
    // Unicode segmentation treats this as its own grapheme cluster with width 0.
    let text = "\u{0301}x"; // combining acute accent + x
    assert_eq!(column_to_char_index(text, 0), 0); // combining mark at column 0
    assert_eq!(column_to_char_index(text, 1), 1); // x at column 1
}

#[test]
fn test_column_to_char_index_multiple_zero_width() {
    // ZWNJ + ZWJ cluster into one grapheme per UAX #29.
    // "\u{200C}\u{200D}a" = 2 graphemes: "\u{200C}\u{200D}" (width 0, 2 chars) + "a" (width 1)
    // The zero-width cluster occupies 1 effective column for mapping.
    let text = "\u{200C}\u{200D}a";
    assert_eq!(column_to_char_index(text, 0), 0); // ZWNJ+ZWJ cluster
    assert_eq!(column_to_char_index(text, 1), 2); // 'a' (char index 2, after ZWNJ and ZWJ)
}

#[test]
fn test_column_to_char_index_zero_width_between_cjk() {
    // Zero-width grapheme between CJK characters
    let text = "\u{4E16}\u{200B}\u{754C}"; // 世 + ZWSP + 界
    assert_eq!(column_to_char_index(text, 0), 0); // 世 (column 0)
    assert_eq!(column_to_char_index(text, 1), 0); // 世 (column 1, second cell)
    assert_eq!(column_to_char_index(text, 2), 1); // ZWSP at column 2
    assert_eq!(column_to_char_index(text, 3), 2); // 界 at column 3
    assert_eq!(column_to_char_index(text, 4), 2); // 界 (second cell)
}

#[test]
fn test_column_to_char_index_trailing_zero_width() {
    // Trailing zero-width grapheme
    let text = "ab\u{200B}";
    assert_eq!(column_to_char_index(text, 0), 0); // 'a'
    assert_eq!(column_to_char_index(text, 1), 1); // 'b'
    assert_eq!(column_to_char_index(text, 2), 2); // ZWSP
    assert_eq!(column_to_char_index(text, 3), 3); // past end
}

#[test]
fn test_column_to_char_index_consistency_with_column_to_byte() {
    // Verify that column_to_char_index and column_to_byte agree on
    // which grapheme a column maps to, even with zero-width graphemes.
    let text = "a\u{200C}b\u{200D}c";
    for col in 0..6 {
        let byte_offset = column_to_byte(text, col);
        let char_idx = column_to_char_index(text, col);
        // Both should map to the same grapheme
        let grapheme_from_byte = grapheme_at_byte(text, byte_offset);
        let chars_before_byte: usize = text[..byte_offset].chars().count();
        assert_eq!(
            char_idx, chars_before_byte,
            "column {col}: char_index {char_idx} should equal chars before byte offset {byte_offset} = {chars_before_byte}"
        );
        // Also verify grapheme_at_byte finds something for valid offsets
        if byte_offset < text.len() {
            assert!(
                grapheme_from_byte.is_some(),
                "column {col}: byte offset {byte_offset} should find a grapheme"
            );
        }
    }
}

// Width table verification tests (#7736)

/// Verify our generated char_width tables match unicode-width for the vast majority
/// of BMP codepoints.
///
/// Our tables target Unicode 16.0 while unicode-width 0.2.2 targets Unicode 17.0,
/// so we allow a small number of differences. The tolerance threshold is set to
/// catch regressions while permitting version-related drift.
///
/// Known difference categories:
/// - Soft hyphen (U+00AD): our tables treat as narrow, unicode-width as zero-width
/// - Arabic/Indic format/spacing marks: classification differences between versions
/// - Newly assigned codepoints in Unicode 17.0
#[test]
fn test_char_width_matches_unicode_width_bmp() {
    use unicode_width::UnicodeWidthChar;

    let mut mismatches = Vec::new();
    for cp in 0x00A0_u32..=0xFFFF {
        if let Some(c) = char::from_u32(cp) {
            let ours = crate::tables::width::char_width(c);
            let theirs = UnicodeWidthChar::width(c).unwrap_or(0);
            if ours != theirs {
                mismatches.push((cp, ours, theirs));
            }
        }
    }
    // Allow up to 200 mismatches due to Unicode 16.0 vs 17.0 differences.
    // As of this writing: ~160 differences (Indic spacing marks, Arabic Cf,
    // soft hyphen, newly assigned chars).
    assert!(
        mismatches.len() <= 200,
        "Too many char_width mismatches vs unicode-width: {} (limit 200).\n\
         First 20:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(20)
            .map(|(cp, ours, theirs)| format!("  U+{cp:04X}: ours={ours}, unicode-width={theirs}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Verify our tables match unicode-width for SMP emoji and CJK ranges.
///
/// Allows a small tolerance for Unicode version differences (16.0 vs 17.0).
#[test]
fn test_char_width_matches_unicode_width_smp_sample() {
    use unicode_width::UnicodeWidthChar;

    let mut mismatches = Vec::new();
    // CJK Unified Ideographs Extension B (sample)
    for cp in 0x20000_u32..=0x20100 {
        if let Some(c) = char::from_u32(cp) {
            let ours = crate::tables::width::char_width(c);
            let theirs = UnicodeWidthChar::width(c).unwrap_or(0);
            if ours != theirs {
                mismatches.push((cp, ours, theirs));
            }
        }
    }
    // Emoji ranges
    for cp in 0x1F300_u32..=0x1F9FF {
        if let Some(c) = char::from_u32(cp) {
            let ours = crate::tables::width::char_width(c);
            let theirs = UnicodeWidthChar::width(c).unwrap_or(0);
            if ours != theirs {
                mismatches.push((cp, ours, theirs));
            }
        }
    }
    // Hangul Syllables
    for cp in 0xAC00_u32..=0xAC00 + 100 {
        if let Some(c) = char::from_u32(cp) {
            let ours = crate::tables::width::char_width(c);
            let theirs = UnicodeWidthChar::width(c).unwrap_or(0);
            if ours != theirs {
                mismatches.push((cp, ours, theirs));
            }
        }
    }
    // Allow a few mismatches due to Unicode 16.0 vs 17.0 emoji additions
    assert!(
        mismatches.len() <= 10,
        "Too many SMP char_width mismatches vs unicode-width: {} (limit 10).\n\
         Mismatches:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(20)
            .map(|(cp, ours, theirs)| format!("  U+{cp:04X}: ours={ours}, unicode-width={theirs}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Verify our char_width_cjk tables match unicode-width CJK mode for well-known
/// ambiguous-width characters that are stable across Unicode versions.
#[test]
fn test_char_width_cjk_matches_unicode_width() {
    use unicode_width::UnicodeWidthChar;

    // Test well-known ambiguous-width characters stable across Unicode versions
    let ambiguous_samples: &[char] = &[
        '\u{00A7}', // SECTION SIGN
        '\u{00AE}', // REGISTERED SIGN
        '\u{00B0}', // DEGREE SIGN
        '\u{00B1}', // PLUS-MINUS SIGN
        '\u{2013}', // EN DASH
        '\u{2014}', // EM DASH
        '\u{2018}', // LEFT SINGLE QUOTATION MARK
        '\u{2019}', // RIGHT SINGLE QUOTATION MARK
        '\u{201C}', // LEFT DOUBLE QUOTATION MARK
        '\u{201D}', // RIGHT DOUBLE QUOTATION MARK
        '\u{2026}', // HORIZONTAL ELLIPSIS
    ];
    for &c in ambiguous_samples {
        let ours_cjk = crate::tables::width::char_width_cjk(c);
        let theirs_cjk = UnicodeWidthChar::width_cjk(c).unwrap_or(0);
        assert_eq!(
            ours_cjk, theirs_cjk,
            "char_width_cjk mismatch at U+{:04X}: ours={ours_cjk}, unicode-width={theirs_cjk}",
            c as u32
        );
    }
}

/// Verify table boundaries: control chars, zero-width, wide, and narrow.
#[test]
fn test_char_width_table_boundaries() {
    // Control chars return 0 from tables
    assert_eq!(crate::tables::width::char_width('\x00'), 0);
    assert_eq!(crate::tables::width::char_width('\x1F'), 0);
    assert_eq!(crate::tables::width::char_width('\x7F'), 0);
    assert_eq!(crate::tables::width::char_width('\u{9F}'), 0);

    // Regular ASCII = 1
    assert_eq!(crate::tables::width::char_width(' '), 1);
    assert_eq!(crate::tables::width::char_width('A'), 1);
    assert_eq!(crate::tables::width::char_width('~'), 1);

    // CJK = 2
    assert_eq!(crate::tables::width::char_width('\u{4E00}'), 2); // CJK Unified start
    assert_eq!(crate::tables::width::char_width('\u{9FFF}'), 2); // CJK Unified end
    assert_eq!(crate::tables::width::char_width('\u{AC00}'), 2); // Hangul syllable start

    // Fullwidth = 2
    assert_eq!(crate::tables::width::char_width('\u{FF01}'), 2); // Fullwidth !
    assert_eq!(crate::tables::width::char_width('\u{FF5E}'), 2); // Fullwidth ~

    // Combining marks = 0
    assert_eq!(crate::tables::width::char_width('\u{0300}'), 0); // Combining grave accent
    assert_eq!(crate::tables::width::char_width('\u{0301}'), 0); // Combining acute accent

    // Zero-width special chars
    assert_eq!(crate::tables::width::char_width('\u{200B}'), 0); // ZWSP
    assert_eq!(crate::tables::width::char_width('\u{200D}'), 0); // ZWJ
    assert_eq!(crate::tables::width::char_width('\u{FEFF}'), 0); // BOM

    // Soft hyphen = narrow (not zero-width)
    assert_eq!(crate::tables::width::char_width('\u{00AD}'), 1);

    // Beyond table range = narrow
    assert_eq!(crate::tables::width::char_width('\u{E0001}'), 1);

    // CJK Extension G (U+30000-U+3134A) = wide (#7775)
    assert_eq!(crate::tables::width::char_width('\u{30000}'), 2); // Extension G start
    assert_eq!(crate::tables::width::char_width('\u{3134A}'), 2); // Extension G end

    // CJK Extension H (U+31350-U+323AF) = wide (#7775)
    assert_eq!(crate::tables::width::char_width('\u{31350}'), 2); // Extension H start
    assert_eq!(crate::tables::width::char_width('\u{323AF}'), 2); // Extension H end

    // Beyond Extension H = narrow
    assert_eq!(crate::tables::width::char_width('\u{323B0}'), 1);

    // CJK Extension I (U+2EBF0-U+2F7FF) = wide (in table)
    assert_eq!(crate::tables::width::char_width('\u{2EBF0}'), 2); // Extension I start
    assert_eq!(crate::tables::width::char_width('\u{2F7FF}'), 2); // Extension I end

    // CJK mode also returns wide for Extensions G/H/I
    assert_eq!(crate::tables::width::char_width_cjk('\u{30000}'), 2);
    assert_eq!(crate::tables::width::char_width_cjk('\u{323AF}'), 2);
    assert_eq!(crate::tables::width::char_width_cjk('\u{323B0}'), 1);
    assert_eq!(crate::tables::width::char_width_cjk('\u{2EBF0}'), 2);
}

// CJK aggregate function tests (#7605)

#[test]
fn test_grapheme_width_with_config_single() {
    // Default (single width) mode: same as grapheme_width
    let config = TextShapingConfig::default();
    let text = "Hello \u{4E16}\u{754C}!";
    let info_basic = grapheme_width(text);
    let info_config = grapheme_width_with_config(text, &config);
    assert_eq!(info_basic, info_config);
}

#[test]
fn test_grapheme_width_with_config_double() {
    // CJK (double width) mode: ambiguous characters become 2 cells
    let config = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    // Degree sign (U+00B0) is ambiguous-width
    let text = "\u{00B0}test";
    let info = grapheme_width_with_config(text, &config);
    // degree(2) + t(1) + e(1) + s(1) + t(1) = 6
    assert_eq!(info.display_width, 6);
    assert_eq!(info.grapheme_count, 5);
    assert!(info.has_wide); // degree sign becomes wide in CJK mode
}

#[test]
fn test_grapheme_width_with_config_cjk_unchanged() {
    // CJK ideographs are always 2 cells regardless of config
    let single = TextShapingConfig::default();
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    let text = "\u{4E2D}\u{6587}"; // 中文
    assert_eq!(grapheme_width_with_config(text, &single).display_width, 4);
    assert_eq!(grapheme_width_with_config(text, &double).display_width, 4);
}

#[test]
fn test_grapheme_width_with_config_multiple_ambiguous() {
    // Multiple ambiguous-width characters in a row.
    // Note: not all symbols are ambiguous per unicode-width's tables.
    // U+00A7 (section sign) and U+00AE (registered) are ambiguous; U+00B0 (degree) too.
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    // Section sign + registered + degree sign (all actually ambiguous in unicode-width)
    let text = "\u{00A7}\u{00AE}\u{00B0}";
    let info_single = grapheme_width(text); // default = single
    let info_double = grapheme_width_with_config(text, &double);
    assert_eq!(info_single.display_width, 3); // each 1 cell
    assert_eq!(info_double.display_width, 6); // each 2 cells in CJK mode
}

#[test]
fn test_split_graphemes_with_config_basic() {
    let config = TextShapingConfig::default();
    let text = "a\u{4E2D}b";
    let graphemes: Vec<_> = split_graphemes_with_config(text, &config).collect();
    assert_eq!(graphemes.len(), 3);
    assert_eq!(graphemes[0].text, "a");
    assert_eq!(graphemes[0].width, 1);
    assert_eq!(graphemes[1].text, "\u{4E2D}");
    assert_eq!(graphemes[1].width, 2);
    assert_eq!(graphemes[2].text, "b");
    assert_eq!(graphemes[2].width, 1);
}

#[test]
fn test_split_graphemes_with_config_ambiguous_double() {
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    let text = "a\u{00B0}b"; // a + degree sign + b
    let graphemes: Vec<_> = split_graphemes_with_config(text, &double).collect();
    assert_eq!(graphemes.len(), 3);
    assert_eq!(graphemes[0].width, 1); // 'a'
    assert_eq!(graphemes[1].width, 2); // degree sign in CJK mode
    assert_eq!(graphemes[1].text, "\u{00B0}");
    assert_eq!(graphemes[2].width, 1); // 'b'
}

#[test]
fn test_split_graphemes_with_config_matches_aggregate() {
    // Verify split_graphemes_with_config produces widths consistent with
    // grapheme_width_with_config aggregate.
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    let text = "Hello \u{00B0}\u{4E2D}\u{1F600}!";
    let aggregate = grapheme_width_with_config(text, &double);
    let split: Vec<_> = split_graphemes_with_config(text, &double).collect();
    let split_width: usize = split.iter().map(|g| g.width).sum();
    let split_count = split.len();
    let split_codepoints: usize = split.iter().map(|g| g.codepoint_count).sum();
    assert_eq!(split_width, aggregate.display_width);
    assert_eq!(split_count, aggregate.grapheme_count);
    assert_eq!(split_codepoints, aggregate.codepoint_count);
}

// CJK ambiguous-width tests (#1371)

#[test]
fn test_ambiguous_width_degree_sign() {
    // Degree sign (deg, U+00B0) is ambiguous-width
    let single = TextShapingConfig::default();
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    assert_eq!(grapheme_display_width_with_config("\u{00B0}", &single), 1);
    assert_eq!(grapheme_display_width_with_config("\u{00B0}", &double), 2);
}

#[test]
fn test_ambiguous_width_cjk_unchanged() {
    // CJK ideographs are always 2 cells regardless of mode
    let single = TextShapingConfig::default();
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    assert_eq!(grapheme_display_width_with_config("\u{4E2D}", &single), 2);
    assert_eq!(grapheme_display_width_with_config("\u{4E2D}", &double), 2);
}

#[test]
fn test_ambiguous_width_ascii_unchanged() {
    // ASCII is always 1 cell regardless of mode
    let single = TextShapingConfig::default();
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    assert_eq!(grapheme_display_width_with_config("a", &single), 1);
    assert_eq!(grapheme_display_width_with_config("a", &double), 1);
}

#[test]
fn test_config_default_matches_basic() {
    // Default config should match the simple function
    let config = TextShapingConfig::default();
    assert_eq!(
        grapheme_display_width_with_config("a", &config),
        grapheme_display_width("a")
    );
    assert_eq!(
        grapheme_display_width_with_config("\u{4E2D}", &config),
        grapheme_display_width("\u{4E2D}")
    );
}
