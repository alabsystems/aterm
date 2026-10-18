// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Basic grid writes: soft-wrap continuations and supplementary-plane chars.

use super::super::*;

#[test]
fn write_char_wrap_marks_continuation_after_full_screen_scroll() {
    // Autowrapping at the bottom of a full-screen scroll region recycles a
    // bottom row that receives the continuation; it stays marked `wrapped`, or
    // reflow/selection/copy would insert a hard break.
    let mut grid = Grid::new(2, 2);
    for c in "ABCDE".chars() {
        grid.write_char_wrap(c);
    }
    // "AB" scrolled into scrollback; visible row 0 == "CD" (continuation of "AB"),
    // visible row 1 == "E" (continuation of "CD") — both are soft-wrap rows.
    assert_eq!(grid.row(0).unwrap().to_string(), "CD");
    assert_eq!(grid.cell(1, 0).unwrap().char(), 'E');
    assert_eq!(grid.row_is_wrapped(0), Some(true));
    assert_eq!(
        grid.row_is_wrapped(1),
        Some(true),
        "the recycled bottom row holding the soft-wrap continuation must stay wrapped"
    );
}

/// A char past the BMP is stored as a COMPLEX cell whose codepoint lives in
/// the extras and reads back whole, through both write paths and between
/// ASCII; a BMP char (wide CJK included) is stored directly.
#[test]
fn supplementary_plane_chars_are_complex_and_read_back_whole() {
    for wrap in [false, true] {
        let mut grid = Grid::new(4, 40);
        let mut write = |c| {
            if wrap {
                grid.write_char_wrap(c);
            } else {
                grid.write_char(c);
            }
        };
        for c in "Hi \u{1F680} bye \u{4E16}".chars() {
            write(c);
        }
        assert_eq!(
            grid.row_text(0).as_deref(),
            Some("Hi \u{1F680} bye \u{4E16}"),
            "wrap {wrap}"
        );
        let row = grid.row(0).unwrap();
        assert!(row.get(3).unwrap().is_complex(), "wrap {wrap}: the rocket");
        assert!(!row.get(0).unwrap().is_complex(), "wrap {wrap}: ASCII");
        let cjk = (0..40)
            .find(|&c| row.get(c).unwrap().char() == '\u{4E16}')
            .expect("cjk");
        assert!(!row.get(cjk).unwrap().is_complex(), "wrap {wrap}: BMP CJK");
    }
}
