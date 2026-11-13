// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::merge_wallpaper_tint_ink;
use aterm_core::render::InkCell;
use aterm_core::terminal::{RenderCell, UnderlineStyle};

fn cell(ch: char, fg: [u8; 3], wide: bool) -> RenderCell {
    RenderCell {
        ch,
        fg,
        bg: [0, 0, 0],
        wide,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

fn tuples(ink: &[InkCell]) -> Vec<(u16, u16, [u8; 3])> {
    ink.iter()
        .map(|entry| (entry.row, entry.col, entry.color))
        .collect()
}

#[test]
fn merge_preserves_precedence_order() {
    let default_fg = [200, 200, 200];
    let cells = vec![
        vec![
            cell('a', default_fg, false),
            cell('b', default_fg, false),
            cell(' ', default_fg, false),
            cell('c', default_fg, true),
        ],
        vec![
            cell('d', default_fg, false),
            cell('e', [1, 2, 3], false),
            cell('f', default_fg, false),
            cell('g', default_fg, false),
        ],
    ];
    let tint = vec![
        Some([1, 1, 1]),
        Some([2, 2, 2]),
        Some([3, 3, 3]),
        Some([4, 4, 4]),
        Some([5, 5, 5]),
        Some([6, 6, 6]),
        None,
        Some([7, 7, 7]),
    ];
    let producers = vec![
        InkCell {
            row: 0,
            col: 1,
            color: [90, 90, 90],
        },
        InkCell {
            row: 1,
            col: 0,
            color: [80, 80, 80],
        },
        InkCell {
            row: 2,
            col: 0,
            color: [70, 70, 70],
        },
    ];
    let expected = vec![
        (0, 0, [1, 1, 1]),
        (0, 1, [90, 90, 90]),
        (1, 0, [80, 80, 80]),
        (1, 3, [7, 7, 7]),
        (2, 0, [70, 70, 70]),
    ];
    let mut ink = producers;
    let mut scratch = Vec::new();
    merge_wallpaper_tint_ink(&cells, 2, 4, default_fg, &tint, &mut ink, &mut scratch);
    assert_eq!(tuples(&ink), expected);
}
