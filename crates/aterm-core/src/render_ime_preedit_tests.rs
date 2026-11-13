// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::RenderInput;
use crate::terminal::{RenderCell, UnderlineStyle};

fn frame(rows: usize, cols: usize, cursor: (usize, usize)) -> RenderInput {
    let blank = RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [10, 10, 10],
        ..Default::default()
    };
    RenderInput {
        rows,
        cols,
        cells: vec![vec![blank; cols]; rows],
        cursor_row: cursor.0,
        cursor_col: cursor.1,
        cursor_visible: true,
        ..Default::default()
    }
}

#[test]
fn ascii_preedit_lands_underlined_at_the_cursor_and_the_caret_trails() {
    let mut f = frame(4, 10, (1, 3));
    f.overlay_ime_preedit("ab", None, false);
    assert_eq!(f.cells[1][3].ch, 'a');
    assert_eq!(f.cells[1][4].ch, 'b');
    assert_eq!(f.cells[1][3].underline, UnderlineStyle::Single);
    assert_eq!(
        f.cells[1][3].fg,
        [200, 200, 200],
        "cursor cell's colors seed the style"
    );
    assert_eq!(f.cursor_col, 5, "caret parks after the composition");
    assert_eq!(
        f.cells[1][2].ch, ' ',
        "nothing left of the cursor is touched"
    );
}

#[test]
fn the_platform_caret_offset_places_the_cursor_mid_composition() {
    let mut f = frame(2, 10, (0, 0));
    f.overlay_ime_preedit("ab", Some(1), false);
    assert_eq!(f.cursor_col, 1);
}

#[test]
fn an_empty_preedit_is_byte_identical() {
    let mut f = frame(2, 10, (0, 4));
    let before = f.clone();
    f.overlay_ime_preedit("", None, false);
    assert_eq!(f, before);
}

#[test]
fn zero_width_combining_marks_advance_nothing() {
    let mut f = frame(2, 10, (0, 0));
    f.overlay_ime_preedit("e\u{0301}", None, false);
    assert_eq!(f.cells[0][0].ch, 'e');
    assert_eq!(f.cursor_col, 1);
}
