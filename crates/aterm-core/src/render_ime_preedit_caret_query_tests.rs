// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{RenderInput, ime_preedit_caret_col};
use crate::terminal::{Rgb, Terminal};

fn prompt(cols: u16, cursor_col: usize) -> RenderInput {
    let mut term = Terminal::new(2, cols);
    term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
    term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
    term.process(b"$ ");
    let mut f = RenderInput::empty();
    term.cell_frame_into(&mut f, 2, cols.into());
    f.cursor_col = cursor_col;
    f
}

#[test]
fn the_caret_query_returns_exactly_the_column_the_paint_lands_on() {
    let cases: &[(&str, Option<usize>, bool, usize, u16)] = &[
        ("にほん", None, false, 2, 20),
        ("にほん", Some(0), false, 2, 20),
        ("にほん", Some(3), false, 2, 20),
        ("ab", Some(1), false, 3, 20),
        ("にほんご", None, false, 2, 8),
        ("日", None, false, 7, 8),
        ("e\u{0301}", None, false, 0, 20),
        ("±", None, true, 2, 20),
        ("±", None, false, 2, 20),
        ("", None, false, 2, 20),
        ("にほん", Some(999), false, 2, 20),
    ];
    for &(preedit, caret, cjk, start, cols) in cases {
        let mut f = prompt(cols, start);
        f.overlay_ime_preedit(preedit, caret, cjk);
        let queried = ime_preedit_caret_col(preedit, caret, cjk, start, usize::from(cols));
        assert_eq!(
            queried, f.cursor_col,
            "anchor and ink disagree for {preedit:?} caret={caret:?} cjk={cjk} \
             start={start} cols={cols}"
        );
    }
}
