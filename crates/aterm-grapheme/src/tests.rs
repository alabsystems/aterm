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
fn test_byte_to_column() {
    let text = "Hello \u{4E16}\u{754C}";

    assert_eq!(byte_to_column(text, 0), 0); // 'H'
    assert_eq!(byte_to_column(text, 5), 5); // space
    assert_eq!(byte_to_column(text, 6), 6); // '世'
    assert_eq!(byte_to_column(text, 9), 8); // '界' (after 世's 3 bytes)
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

/// `is_emoji_modifier_base` is exactly Unicode 16.0.0 `Emoji_Modifier_Base`
/// (134 code points). The hand-kept subset it replaced left out U+1F977 NINJA,
/// so the fully-qualified `🥷🏽` drew as a ninja and a separate swatch over four
/// cells, and took in U+1F90D/U+1F90E (white and brown heart), which are not
/// bases.
#[test]
fn emoji_modifier_base_is_exactly_the_ucd_property() {
    let count = (0..=0x10_FFFFu32)
        .filter_map(char::from_u32)
        .filter(|&c| is_emoji_modifier_base(c))
        .count();
    assert_eq!(count, 134);
    assert!(is_emoji_modifier_base('\u{1F977}'));
    assert!(!is_emoji_modifier_base('\u{1F90D}'));
    assert!(!is_emoji_modifier_base('\u{1F90E}'));
}

/// A skin-tone modifier folds into its base's cell only when the grid folds
/// it: the base is an `Emoji_Modifier_Base` drawn wide. After anything else
/// the grid draws the modifier as its own 2-cell glyph, and the measurer must
/// charge both, or every later column (find highlight, copy) lands left of
/// the text.
#[test]
fn skin_tone_grid_columns_follow_the_grids_fold() {
    for (s, cells) in [
        ("\u{1F44D}\u{1F3FD}", 2),          // thumbs up: a wide base
        ("\u{1F977}\u{1F3FD}", 2),          // ninja: a wide base
        ("\u{1F680}\u{1F3FD}", 4),          // rocket: not a base
        ("\u{1F90D}\u{1F3FD}", 4),          // white heart: not a base
        ("\u{261D}\u{1F3FD}", 3),           // index up: a base, drawn narrow
        ("\u{261D}\u{FE0F}\u{1F3FD}", 2),   // ...widened by VS16 first
        ("a\u{1F3FD}", 3),                  // not an emoji at all
        ("\u{1F680}\u{1F3FD}\u{1F3FD}", 6), // a modifier is not a base
        ("\u{1F44D}\u{1F3FD}\u{1F3FD}", 2), // both fold onto the base
    ] {
        assert_eq!(str_grid_columns(s), cells, "{s:?}");
    }
}

/// The measurer walks a cluster with the grid's own per-character rules.
/// VS16 widens every narrow base the grid widens (`is_vs16_emoji_capable`),
/// not only the pictograph blocks `is_emoji_char` names: `©️ ▶️ ℹ️ ‼️ ↔️` are
/// two cells. A character after a ZWJ folds into the cell before it whatever
/// that cell holds, so a ZWJ emoji on a narrow base and an explicit
/// Devanagari half form are one cell. VS15 narrows the glyph but not the
/// cursor, and a narrowed cell takes no skin-tone modifier.
#[test]
fn vs16_zwj_and_vs15_grid_columns_follow_the_grid() {
    for (s, cells) in [
        ("\u{A9}\u{FE0F}", 2),
        ("\u{25B6}\u{FE0F}", 2),
        ("\u{2139}\u{FE0F}", 2),
        ("\u{203C}\u{FE0F}", 2),
        ("\u{2194}\u{FE0F}", 2),
        ("a\u{FE0F}", 1),
        ("\u{231A}\u{FE0F}", 2),
        ("\u{2764}\u{200D}\u{1F525}", 1),
        ("\u{2764}\u{FE0F}\u{200D}\u{1F525}", 2),
        ("\u{915}\u{94D}\u{200D}\u{937}", 1),
        ("\u{1F680}\u{FE0E}\u{FE0F}", 2),
        ("\u{1F44D}\u{FE0E}\u{1F3FD}", 4),
    ] {
        assert_eq!(str_grid_columns(s), cells, "{s:?}");
    }
}

/// A UAX #29 GB9b Prepend cluster is measured at its spacing BASE's width, not
/// the leading zero-width Prepend sign's. Charging the first codepoint
/// reported these as width 0. The signs are real Arabic/Kaithi
/// number marks that prepend a digit (U+0600 ARABIC NUMBER SIGN, U+06DD ARABIC
/// END OF AYAH — common in Quranic text — U+110BD KAITHI NUMBER SIGN).
#[test]
fn prepend_cluster_is_measured_at_its_base() {
    for (s, display, columns) in [
        ("\u{0600}\u{0661}", 1, 1), // ARABIC NUMBER SIGN + ARABIC-INDIC DIGIT ONE
        ("\u{06DD}\u{0661}", 1, 1), // ARABIC END OF AYAH + ARABIC-INDIC DIGIT ONE
        ("\u{110BD}\u{11066}", 1, 1), // KAITHI NUMBER SIGN + KAITHI DIGIT ZERO
        ("\u{0600}\u{2764}\u{FE0F}", 2, 2), // the spacing base's VS16 presentation
        // Cluster presentation and the stateful grid deliberately differ on
        // a minimally-qualified narrow-base ZWJ sequence (GridColumns' docs).
        ("\u{0600}\u{2764}\u{200D}\u{1F525}", 2, 1),
        ("\u{0600}\u{1F469}\u{200D}\u{1F4BB}", 2, 2),
    ] {
        // GB9b: no break after a Prepend, so each is exactly one cluster.
        assert_eq!(split_graphemes(s).count(), 1, "{s:?} is one cluster");
        // Both domains follow the spacing base, retaining their own policy.
        let grid = str_grid_columns(s);
        assert_eq!(
            grapheme_display_width(s),
            display,
            "cluster presentation {s:?}"
        );
        assert_eq!(grid, columns, "{s:?} grid columns follow its spacing base");
        assert_eq!(grapheme_width(s).has_emoji, display == 2, "{s:?}");
        assert_eq!(
            split_graphemes(s).next().unwrap().is_emoji,
            display == 2,
            "{s:?}"
        );
    }
    // The existing coarse all-zero classification is unchanged.
    assert_eq!(grapheme_display_width("\u{FE0F}"), 0);
    assert!(grapheme_width("\u{FE0F}").has_emoji);
    let cjk = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    assert_eq!(grapheme_display_width("\u{0600}\u{00B0}"), 1);
    assert_eq!(
        grapheme_display_width_with_config("\u{0600}\u{00B0}", &cjk),
        2
    );
}

/// The grid's fold state crosses UAX #29 cluster boundaries, so the columns
/// are measured over the whole line, never one cluster at a time. Sinhala
/// writes yansaya and rakaransaya with a ZWJ that ENDS a cluster (its virama
/// is no conjunct linker), and the grid folds the next cluster's letter into
/// the same cell: `ක්‍ය` and `ශ්‍රී` are one cell. A doubled ZWJ (or a mark
/// between ZWJs) ends a cluster too, and the grid still folds the pictograph
/// after it; a regional indicator pairs past a mark on the first. ASCII
/// takes the grid's bulk path, which never folds.
#[test]
fn grid_columns_carry_fold_state_across_cluster_boundaries() {
    for (s, cells, clusters) in [
        ("\u{0D9A}\u{0DCA}\u{200D}\u{0DBA}", 1, [(0, 1), (0, 1)]),
        (
            "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}",
            1,
            [(0, 1), (0, 1)],
        ),
        ("\u{1F44D}\u{200D}\u{200D}\u{1F44D}", 2, [(0, 2), (0, 2)]),
        (
            "\u{1F44D}\u{200D}\u{0308}\u{200D}\u{1F44D}",
            2,
            [(0, 2), (0, 2)],
        ),
        ("\u{1F1FA}\u{0301}\u{1F1F8}", 2, [(0, 2), (0, 2)]),
        ("\u{1F44D}\u{200D}a", 3, [(0, 2), (2, 3)]),
        ("a\u{200D}b", 2, [(0, 1), (1, 2)]),
    ] {
        assert_eq!(str_grid_columns(s), cells, "{s:?}");
        let spans: Vec<_> = grid_graphemes(s)
            .map(|g| (g.start_col, g.end_col))
            .collect();
        assert_eq!(spans, clusters, "{s:?}");
    }
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

/// The two public position converters count GRID cells, the unit every caller
/// measures in: the soft-wrap join pads each row out to the grid width, the
/// double-click word walk asks whether a clicked cell lies past the text, and
/// the status line cuts a scraped prompt at the shell's start column. A
/// Devanagari conjunct is one cluster that looks one cell wide and occupies
/// three (`न्`, `द`, `ी`); charging its display width put every later column two
/// cells to the left of its text, so a double-click on the word after it
/// selected a single cell. A zero-width format character the grid folds into
/// the previous cell occupies no column.
#[test]
fn position_converters_count_grid_cells() {
    let conjunct = "\u{0928}\u{094D}\u{0926}\u{0940}";
    let text = format!("{conjunct}{conjunct} tail");
    let tail = text.find('t').unwrap();
    // Two 3-cell conjuncts and a space precede `tail`: column 7, char 9.
    assert_eq!(byte_to_column(&text, tail), 7);
    assert_eq!(column_to_char_index(&text, 7), 9);
    assert_eq!(column_to_char_index(&text, 4), 4, "the second conjunct");

    let corpus = [
        text.as_str(),
        "\u{0995}\u{09CD}\u{09B7}\u{09BF} x", // Bengali conjunct
        "a\u{200B}b",                         // ZWSP folds into `a`'s cell
        "1\u{FE0F}\u{20E3}=\u{2764}\u{FE0F}", // keycap, VS16 emoji
        "\u{1F468}\u{200D}\u{1F469}\u{1F1FA}\u{1F1F8}", // ZWJ pair, flag
        "e\u{0301}\u{4E2D}x",                 // combining mark, CJK
    ];
    for s in corpus {
        let mut column = 0usize;
        let mut chars = 0usize;
        for g in grid_graphemes(s) {
            assert_eq!(
                byte_to_column(s, g.byte_offset),
                column,
                "{s:?} at byte {}",
                g.byte_offset
            );
            for c in column..g.end_col {
                assert_eq!(column_to_char_index(s, c), chars, "{s:?} column {c}");
            }
            column = g.end_col;
            chars += g.text.chars().count();
        }
        assert_eq!(byte_to_column(s, s.len()), str_grid_columns(s), "{s:?}");
        assert_eq!(column_to_char_index(s, column), chars, "{s:?} past the end");
    }
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
fn test_empty_string() {
    let info = grapheme_width("");
    assert_eq!(info.grapheme_count, 0);
    assert_eq!(info.display_width, 0);

    assert_eq!(truncate_to_width("", 10), "");
    assert_eq!(pad_to_width("", 5), "     ");
}

// Zero-width grapheme column mapping tests. `column_to_char_index` counts GRID
// cells, and the grid gives a zero-width grapheme no cell of its own (it folds
// into the cell before it), so such a grapheme covers no column: a column at
// or past it names the grapheme after it.

#[test]
fn test_column_to_char_index_standalone_zwj() {
    // "\u{200D}x" = 2 graphemes: a lone ZWJ (no cell) and `x` (column 0).
    let text = "\u{200D}x";
    assert_eq!(column_to_char_index(text, 0), 1); // x at column 0
    assert_eq!(column_to_char_index(text, 1), 2); // past end
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
    // A standalone combining mark at the start of a string (no base character)
    // is its own grapheme cluster with no cell; `x` is at column 0.
    let text = "\u{0301}x"; // combining acute accent + x
    assert_eq!(column_to_char_index(text, 0), 1); // x at column 0
    assert_eq!(column_to_char_index(text, 1), 2); // past end
}

#[test]
fn test_column_to_char_index_multiple_zero_width() {
    // ZWNJ + ZWJ cluster into one grapheme per UAX #29.
    // "\u{200C}\u{200D}a" = 2 graphemes: "\u{200C}\u{200D}" (no cell, 2 chars) + "a".
    let text = "\u{200C}\u{200D}a";
    assert_eq!(column_to_char_index(text, 0), 2); // 'a' (char index 2, after ZWNJ and ZWJ)
    assert_eq!(column_to_char_index(text, 1), 3); // past end
}

#[test]
fn test_column_to_char_index_zero_width_between_cjk() {
    // Zero-width grapheme between CJK characters: the grid folds the ZWSP into
    // 世's cell, so 界 starts at column 2.
    let text = "\u{4E16}\u{200B}\u{754C}"; // 世 + ZWSP + 界
    assert_eq!(column_to_char_index(text, 0), 0); // 世 (column 0)
    assert_eq!(column_to_char_index(text, 1), 0); // 世 (column 1, second cell)
    assert_eq!(column_to_char_index(text, 2), 2); // 界 at column 2
    assert_eq!(column_to_char_index(text, 3), 2); // 界 (second cell)
    assert_eq!(column_to_char_index(text, 4), 3); // past end
}

#[test]
fn test_column_to_char_index_trailing_zero_width() {
    // Trailing zero-width grapheme: no cell, so column 2 is already past the text.
    let text = "ab\u{200B}";
    assert_eq!(column_to_char_index(text, 0), 0); // 'a'
    assert_eq!(column_to_char_index(text, 1), 1); // 'b'
    assert_eq!(column_to_char_index(text, 2), 3); // past end
}

// Width table verification tests

/// Well-known ambiguous-width characters are wide in CJK mode.
#[test]
fn test_char_width_cjk_widens_ambiguous() {
    for c in [
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
    ] {
        assert_eq!(crate::tables::width::char_width(c), 1, "U+{:04X}", c as u32);
        assert_eq!(
            crate::tables::width::char_width_cjk(c),
            2,
            "U+{:04X}",
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

    // Plane 14 (Tags, Variation Selectors Supplement) = zero-width
    assert_eq!(crate::tables::width::char_width('\u{E0001}'), 0);

    // CJK Extension G (U+30000-U+3134A) = wide
    assert_eq!(crate::tables::width::char_width('\u{30000}'), 2); // Extension G start
    assert_eq!(crate::tables::width::char_width('\u{3134A}'), 2); // Extension G end

    // CJK Extension H (U+31350-U+323AF) = wide
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

/// Plane 14 is Default_Ignorable_Code_Point throughout: the Tags block
/// (U+E0000..U+E007F, subdivision-flag letters), the Variation Selectors
/// Supplement (U+E0100..U+E01EF) and the reserved gaps. Every scalar is
/// zero-width in both ambiguous-width modes, although the generated table
/// stops at U+2FFFF, so 🏴 plus six tags occupies one cell.
#[test]
fn plane14_default_ignorables_are_zero_width() {
    for cp in [
        0xE0000_u32,
        0xE0001,
        0xE001F,
        0xE0020,
        0xE007F,
        0xE0080,
        0xE0100,
        0xE01EF,
        0xE01F0,
        0xE0FFF,
    ] {
        let c = char::from_u32(cp).expect("plane 14 scalar");
        assert_eq!(
            crate::tables::width::char_width(c),
            0,
            "U+{cp:05X} in default mode"
        );
        assert_eq!(
            crate::tables::width::char_width_cjk(c),
            0,
            "U+{cp:05X} in CJK mode"
        );
    }
    // The first scalar past the block stays narrow, Extension G stays wide,
    // and the BMP presentation selectors keep the zero width they had.
    assert_eq!(crate::tables::width::char_width('\u{E1000}'), 1);
    assert_eq!(crate::tables::width::char_width('\u{30000}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{FE0E}'), 0);
    assert_eq!(crate::tables::width::char_width('\u{FE0F}'), 0);
    // England: 🏴 + gbeng + CANCEL TAG is one two-cell grapheme.
    assert_eq!(
        crate::tables::width::str_width(
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}"
        ),
        2
    );
}

/// Plane 14 (tags, variation selectors supplement, unassigned) is zero-width
/// in both ambiguous-width modes — the table catch-all's answer for the plane.
#[test]
fn test_char_width_plane14_is_zero() {
    for cp in 0xE0000_u32..=0xE0FFF {
        let c = char::from_u32(cp).expect("plane 14 scalar");
        assert_eq!(crate::tables::width::char_width(c), 0, "U+{cp:05X}");
        assert_eq!(crate::tables::width::char_width_cjk(c), 0, "U+{cp:05X} CJK");
    }
}

/// Every conjoining vowel and final jamo (`Hangul_Syllable_Type` V and T) is
/// zero-width in both modes, Hangul Jamo Extended-B's (U+D7B0..U+D7C6,
/// U+D7CB..U+D7FB) as much as U+1160..U+11FF: each joins the syllable its
/// leading consonant starts (UAX #29 GB6-GB8), so an Old-Hangul syllable is
/// two cells, not four. glibc's utf8_gen.py and unicode-width rule the same.
#[test]
fn hangul_vowel_and_final_jamo_are_zero_width() {
    let v_and_t = (0x1160_u32..=0x11FF)
        .chain(0xD7B0..=0xD7C6)
        .chain(0xD7CB..=0xD7FB);
    for cp in v_and_t {
        let c = char::from_u32(cp).expect("BMP scalar");
        assert_eq!(crate::tables::width::char_width(c), 0, "U+{cp:04X}");
        assert_eq!(crate::tables::width::char_width_cjk(c), 0, "U+{cp:04X} CJK");
    }
    // The unassigned code points around them stay narrow, and the leading
    // consonants stay wide.
    for c in ['\u{D7AF}', '\u{D7C7}', '\u{D7CA}', '\u{D7FC}', '\u{D7FF}'] {
        assert_eq!(crate::tables::width::char_width(c), 1, "{c:?}");
        assert_eq!(crate::tables::width::char_width_cjk(c), 1, "{c:?} CJK");
    }
    assert_eq!(crate::tables::width::char_width('\u{A960}'), 2);
    assert_eq!(
        crate::tables::width::str_width("\u{A960}\u{D7B0}\u{D7CB}"),
        2
    );
}

// CJK aggregate functions

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

// CJK ambiguous-width tests

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
