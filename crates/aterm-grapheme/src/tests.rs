// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";

#[test]
#[rustfmt::skip]
fn grapheme_width_aggregates_clusters_and_cells() {
    for (case, text, clusters, cells, codepoints, emoji, combining, wide) in [
        ("ascii", "Hello", 5, 5, 5, false, false, false),
        ("empty", "", 0, 0, 0, false, false, false),
        ("cjk", "\u{4E2D}\u{6587}", 2, 4, 2, false, false, true),
        ("emoji", "\u{1F600}", 1, 2, 1, true, false, true),
        ("zwj family", FAMILY, 1, 2, 7, true, false, true),
        ("combining", "e\u{0301}", 1, 1, 2, false, true, false),
        ("stacked marks", "a\u{0300}\u{0301}\u{0302}\u{0303}", 1, 1, 5, false, true, false),
        ("flag", "\u{1F1FA}\u{1F1F8}", 1, 2, 2, true, false, true),
        ("skin tone", "\u{1F44B}\u{1F3FD}", 1, 2, 2, true, false, true),
        ("mixed", "Hello \u{4E16}\u{754C} \u{1F44B}", 10, 13, 10, true, false, true),
        ("vs16 heart", "I \u{2764}\u{FE0F} \u{65E5}\u{672C}", 6, 9, 7, true, false, true),
        ("CJK Extension G and I", "\u{30000}\u{30001}\u{2EBF0}", 3, 6, 3, false, false, true),
    ] {
        let info = grapheme_width(text);
        assert_eq!(
            (
                info.grapheme_count,
                info.display_width,
                info.codepoint_count,
                info.byte_count
            ),
            (clusters, cells, codepoints, text.len()),
            "{case}"
        );
        assert_eq!(
            (info.has_emoji, info.has_combining, info.has_wide),
            (emoji, combining, wide),
            "{case}"
        );
    }
}

#[test]
fn single_cluster_widths() {
    for (case, cluster, cells) in [
        ("CJK ideograph", "\u{4E2D}", 2),
        ("CJK block start", "\u{4E00}", 2),
        ("CJK block end", "\u{9FFF}", 2),
        ("hiragana", "\u{3042}", 2),
        ("katakana", "\u{30A2}", 2),
        ("hangul syllable", "\u{AC00}", 2),
        ("fullwidth A", "\u{FF21}", 2),
        ("fullwidth !", "\u{FF01}", 2),
        ("CJK Extension G end", "\u{3134A}", 2),
        ("CJK Extension H middle", "\u{31A00}", 2),
        ("CJK Extension I middle", "\u{2F000}", 2),
        ("past CJK Extension H", "\u{323B0}", 1),
        ("lone ZWJ", "\u{200D}", 0),
        ("lone ZWNJ", "\u{200C}", 0),
        ("e + acute", "e\u{0301}", 1),
        ("zwj family", FAMILY, 2),
        ("astronaut", "\u{1F468}\u{200D}\u{1F680}", 2),
        ("sun + VS16", "\u{2600}\u{FE0F}", 2),
        ("keycap", "0\u{FE0F}\u{20E3}", 2),
        ("US flag", "\u{1F1FA}\u{1F1F8}", 2),
        ("wave + light skin", "\u{1F44B}\u{1F3FB}", 2),
        ("grinning face", "\u{1F600}", 2),
        ("rocket", "\u{1F680}", 2),
        ("airplane, text by default", "\u{2708}", 1),
        ("airplane + VS16", "\u{2708}\u{FE0F}", 2),
        ("star", "\u{2B50}", 2),
        ("black large square (wide since Unicode 9)", "\u{2B1B}", 2),
        ("white large square", "\u{2B1C}", 2),
        ("ascii", "a", 1),
        ("space", " ", 1),
        ("precomposed e-acute", "\u{00E9}", 1),
        ("box drawing", "\u{253C}", 1),
        ("full block", "\u{2588}", 1),
        ("NUL", "\x00", 1),
        ("ESC", "\x1B", 1),
        ("DEL", "\x7F", 1),
        ("section sign (ambiguous)", "\u{00A7}", 1),
        ("copyright (ambiguous)", "\u{00A9}", 1),
    ] {
        assert_eq!(grapheme_width(cluster).grapheme_count, 1, "{case}");
        assert_eq!(grapheme_display_width(cluster), cells, "{case}");
    }
}

#[test]
fn column_and_byte_conversions_count_grid_cells() {
    let family_x = format!("{FAMILY}x");
    let columns: [(&str, &[(usize, usize)]); 10] = [
        ("Hello World", &[(0, 0), (5, 5), (10, 10), (15, 11)]),
        (
            "\u{4F60}\u{597D}world",
            &[(0, 0), (1, 0), (2, 1), (3, 1), (4, 2), (5, 3)],
        ),
        ("e\u{0301}x", &[(0, 0), (1, 2)]),
        (&family_x, &[(0, 0), (1, 0), (2, 7)]),
        ("\u{200D}x", &[(0, 1), (1, 2)]),
        ("a\u{200C}b", &[(0, 0), (1, 2)]),
        ("\u{0301}x", &[(0, 1), (1, 2)]),
        ("\u{200C}\u{200D}a", &[(0, 2), (1, 3)]),
        (
            "\u{4E16}\u{200B}\u{754C}",
            &[(0, 0), (1, 0), (2, 2), (3, 2), (4, 3)],
        ),
        ("ab\u{200B}", &[(0, 0), (1, 1), (2, 3)]),
    ];
    for (text, pairs) in columns {
        for &(column, index) in pairs {
            assert_eq!(
                column_to_char_index(text, column),
                index,
                "{text:?} column {column}"
            );
        }
    }
    let bytes: [(&str, &[(usize, usize)]); 3] = [
        ("Hello \u{4E16}\u{754C}", &[(0, 0), (5, 5), (6, 6), (9, 8)]),
        ("e\u{0301}x", &[(1, 1), (3, 1)]),
        (
            &family_x,
            &[(0, 0), (4, 2), (FAMILY.len(), 2), (FAMILY.len() + 1, 3)],
        ),
    ];
    for (text, pairs) in bytes {
        for &(byte, column) in pairs {
            assert_eq!(byte_to_column(text, byte), column, "{text:?} byte {byte}");
        }
    }
}

#[test]
fn skin_tone_grid_columns_follow_the_grids_fold() {
    for (s, cells) in [
        ("\u{1F44D}\u{1F3FD}", 2),
        ("\u{1F977}\u{1F3FD}", 2),
        ("\u{1F680}\u{1F3FD}", 4),
        ("\u{1F90D}\u{1F3FD}", 4),
        ("\u{261D}\u{1F3FD}", 3),
        ("\u{261D}\u{FE0F}\u{1F3FD}", 2),
        ("a\u{1F3FD}", 3),
        ("\u{1F680}\u{1F3FD}\u{1F3FD}", 6),
        ("\u{1F44D}\u{1F3FD}\u{1F3FD}", 2),
    ] {
        assert_eq!(str_grid_columns(s), cells, "{s:?}");
    }
}

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

#[test]
fn prepend_cluster_is_measured_at_its_base() {
    for (s, display, columns) in [
        ("\u{0600}\u{0661}", 1, 1),
        ("\u{06DD}\u{0661}", 1, 1),
        ("\u{110BD}\u{11066}", 1, 1),
        ("\u{0600}\u{2764}\u{FE0F}", 2, 2),
        ("\u{0600}\u{2764}\u{200D}\u{1F525}", 2, 1),
        ("\u{0600}\u{1F469}\u{200D}\u{1F4BB}", 2, 2),
    ] {
        assert_eq!(split_graphemes(s).count(), 1, "{s:?} is one cluster");
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
fn position_converters_count_grid_cells() {
    let conjunct = "\u{0928}\u{094D}\u{0926}\u{0940}";
    let text = format!("{conjunct}{conjunct} tail");
    let tail = text.find('t').unwrap();
    assert_eq!(byte_to_column(&text, tail), 7);
    assert_eq!(column_to_char_index(&text, 7), 9);
    assert_eq!(column_to_char_index(&text, 4), 4, "the second conjunct");

    let corpus = [
        text.as_str(),
        "\u{0995}\u{09CD}\u{09B7}\u{09BF} x",
        "a\u{200B}b",
        "1\u{FE0F}\u{20E3}=\u{2764}\u{FE0F}",
        "\u{1F468}\u{200D}\u{1F469}\u{1F1FA}\u{1F1F8}",
        "e\u{0301}\u{4E2D}x",
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
fn test_char_width_cjk_widens_ambiguous() {
    for c in [
        '\u{00A7}', '\u{00AE}', '\u{00B0}', '\u{00B1}', '\u{2013}', '\u{2014}', '\u{2018}',
        '\u{2019}', '\u{201C}', '\u{201D}', '\u{2026}',
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

#[test]
fn test_char_width_table_boundaries() {
    assert_eq!(crate::tables::width::char_width('\x00'), 0);
    assert_eq!(crate::tables::width::char_width('\x1F'), 0);
    assert_eq!(crate::tables::width::char_width('\x7F'), 0);
    assert_eq!(crate::tables::width::char_width('\u{9F}'), 0);

    assert_eq!(crate::tables::width::char_width(' '), 1);
    assert_eq!(crate::tables::width::char_width('A'), 1);
    assert_eq!(crate::tables::width::char_width('~'), 1);

    assert_eq!(crate::tables::width::char_width('\u{4E00}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{9FFF}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{AC00}'), 2);

    assert_eq!(crate::tables::width::char_width('\u{FF01}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{FF5E}'), 2);

    assert_eq!(crate::tables::width::char_width('\u{0300}'), 0);
    assert_eq!(crate::tables::width::char_width('\u{0301}'), 0);

    assert_eq!(crate::tables::width::char_width('\u{200B}'), 0);
    assert_eq!(crate::tables::width::char_width('\u{200D}'), 0);
    assert_eq!(crate::tables::width::char_width('\u{FEFF}'), 0);

    assert_eq!(crate::tables::width::char_width('\u{00AD}'), 1);

    assert_eq!(crate::tables::width::char_width('\u{E0001}'), 0);

    assert_eq!(crate::tables::width::char_width('\u{30000}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{3134A}'), 2);

    assert_eq!(crate::tables::width::char_width('\u{31350}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{323AF}'), 2);

    assert_eq!(crate::tables::width::char_width('\u{323B0}'), 1);

    assert_eq!(crate::tables::width::char_width('\u{2EBF0}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{2F7FF}'), 2);

    assert_eq!(crate::tables::width::char_width_cjk('\u{30000}'), 2);
    assert_eq!(crate::tables::width::char_width_cjk('\u{323AF}'), 2);
    assert_eq!(crate::tables::width::char_width_cjk('\u{323B0}'), 1);
    assert_eq!(crate::tables::width::char_width_cjk('\u{2EBF0}'), 2);
}

#[test]
fn plane14_default_ignorables_are_zero_width() {
    for cp in 0xE0000_u32..=0xE0FFF {
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
    assert_eq!(crate::tables::width::char_width('\u{E1000}'), 1);
    assert_eq!(crate::tables::width::char_width('\u{30000}'), 2);
    assert_eq!(crate::tables::width::char_width('\u{FE0E}'), 0);
    assert_eq!(crate::tables::width::char_width('\u{FE0F}'), 0);
    assert_eq!(
        crate::tables::width::str_width(
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}"
        ),
        2
    );
}

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

#[test]
fn test_split_graphemes_with_config_matches_aggregate() {
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

#[test]
fn ambiguous_double_widens_only_ambiguous_characters() {
    let single = TextShapingConfig::default();
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..Default::default()
    };
    for (cluster, single_cells, double_cells) in [
        ("\u{00B0}", 1, 2),
        ("\u{00A7}", 1, 2),
        ("\u{00AE}", 1, 2),
        ("\u{4E2D}", 2, 2),
        ("a", 1, 1),
    ] {
        assert_eq!(grapheme_display_width(cluster), single_cells, "{cluster:?}");
        assert_eq!(
            grapheme_display_width_with_config(cluster, &single),
            single_cells,
            "{cluster:?} single"
        );
        assert_eq!(
            grapheme_display_width_with_config(cluster, &double),
            double_cells,
            "{cluster:?} double"
        );
    }
    let info = grapheme_width_with_config("\u{00B0}test", &double);
    assert_eq!(
        (info.display_width, info.grapheme_count, info.has_wide),
        (6, 5, true)
    );
    let widths: Vec<usize> = split_graphemes_with_config("a\u{00B0}b", &double)
        .map(|g| g.width)
        .collect();
    assert_eq!(widths, [1, 2, 1]);
    let text = "Hello \u{4E16}\u{754C}!";
    assert_eq!(
        grapheme_width_with_config(text, &single),
        grapheme_width(text)
    );
}
