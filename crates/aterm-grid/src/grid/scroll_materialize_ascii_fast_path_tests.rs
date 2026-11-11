// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{GraphemeUnit, advance_grapheme_unit_scan, advance_grapheme_unit_wide};

fn fields(u: GraphemeUnit) -> (usize, bool, bool, bool) {
    (u.chars, u.wide, u.vs16_widened, u.force_narrow)
}

#[test]
fn the_search_walk_lays_a_line_out_as_materialize_does() {
    use aterm_rle::Rle;

    use super::{CellAttrs, CellFlags, Line, StoredCells, stored_line_cells};

    let pieces = [
        "ab",
        "a\u{301}",
        "\u{301}",
        "\t",
        "\u{65e5}",
        "\u{2714}\u{fe0f}",
        "\u{231a}\u{fe0e}",
        "\u{1f44d}\u{200d}a",
        "\u{1f1fa}\u{1f1f8}",
        "\u{23bf}",
        " ",
    ];
    let mut texts = Vec::new();
    for a in pieces {
        for b in pieces {
            for c in pieces {
                texts.push(format!("{a}x{b}yz{c}"));
            }
        }
    }
    for text in &texts {
        let plain = Line::from_bytes(text.as_bytes());
        let mut wide_attrs = Rle::new();
        let wide = CellAttrs {
            flags: CellFlags::WIDE.bits(),
            ..CellAttrs::DEFAULT
        };
        wide_attrs.push(wide);
        wide_attrs.extend_with(
            CellAttrs::DEFAULT,
            u32::try_from(text.chars().count() - 1).unwrap(),
        );
        let styled = Line::with_attrs(text, wide_attrs);
        for line in [&plain, &styled] {
            for cols in 1..=u16::try_from(text.len() + 2).unwrap() {
                let per_cell: Vec<_> = StoredCells::new(line, text, cols)
                    .map(|cell| (cell.byte, cell.text.len(), cell.col, cell.wide))
                    .collect();
                let mut runs = Vec::new();
                for (bytes, col, width, count) in stored_line_cells(line, text, cols) {
                    for k in 0..count {
                        let len = bytes.len();
                        runs.push((
                            bytes.start + usize::from(k) * len,
                            len,
                            col + k * width,
                            width == 2,
                        ));
                    }
                }
                assert_eq!(runs, per_cell, "{text:?} at {cols} columns");
            }
        }
    }
}

#[test]
fn ascii_fast_path_matches_the_scan() {
    for c in (0u8..0x80).map(char::from) {
        assert_eq!(
            super::is_zero_width(c),
            aterm_grapheme::char_width(c) == 0,
            "{c:?}"
        );
        if (' '..='~').contains(&c) {
            assert_eq!(aterm_grapheme::char_width(c), 1, "{c:?}");
        }
    }
    let mut seconds: Vec<Option<char>> = (0u8..0x80).map(|b| Some(char::from(b))).collect();
    seconds.extend(
        [
            '\u{0301}',
            '\u{200D}',
            '\u{FE0F}',
            '\u{FE0E}',
            '\u{1F3FD}',
            '\u{1F1FA}',
            '\u{E0001}',
            '\u{65E5}',
            '\u{00E9}',
            '\u{0080}',
        ]
        .map(Some),
    );
    seconds.push(None);
    for first in (0u8..0x80).map(char::from) {
        for &second in &seconds {
            for tail in ["", "\u{0301}", "\u{200D}A", "x"] {
                for prefix in ["", "q"] {
                    let mut text = String::from(prefix);
                    text.push(first);
                    text.extend(second);
                    text.push_str(tail);
                    let (mut fast_idx, mut scan_idx) = (prefix.len(), prefix.len());
                    let fast = advance_grapheme_unit_wide(&text, &mut fast_idx);
                    let scan = advance_grapheme_unit_scan(&text, &mut scan_idx, usize::MAX);
                    assert_eq!(
                        (fields(fast), fast_idx),
                        (fields(scan), scan_idx),
                        "fast path disagrees with the scan on {text:?} at {}",
                        prefix.len()
                    );
                }
            }
        }
    }
}
