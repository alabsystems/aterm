// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{COLOR_UNSET, RenderInput};
use crate::terminal::{RenderCell, Rgb, Terminal, UnderlineStyle};

fn prompt_frame(rows: u16, cols: u16) -> RenderInput {
    let mut term = Terminal::new(rows, cols);
    term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
    term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
    term.process(b"$ ");
    let mut f = RenderInput::empty();
    term.cell_frame_into(&mut f, rows.into(), cols.into());
    f
}

#[test]
fn a_composition_at_the_prompt_caret_is_painted_not_swallowed() {
    let mut f = prompt_frame(4, 20);
    f.overlay_ime_preedit("日本", None, false);
    let row: String = f.cells[0].iter().map(|c| c.ch).collect();
    assert_eq!(row, "$ 日 本 ", "the composition lands right of the prompt");
    assert_eq!(f.cells[0][2].ch, '日');
    assert!(
        f.cells[0][3].wide,
        "the wide continuation is materialized too"
    );
    assert_eq!(f.cells[0][4].ch, '本');
    assert!(f.cells[0][5].wide);
    assert_eq!(f.cursor_col, 6, "and the caret trails the text it can see");
}

#[test]
fn the_caret_never_advances_past_what_was_actually_drawn() {
    let mut f = prompt_frame(4, 20);
    f.overlay_ime_preedit("にほん", None, false);
    assert_eq!(f.cursor_col, 8);
    assert!(
        f.cells[0].len() >= f.cursor_col,
        "every column the caret crossed is materialized: len={} caret={}",
        f.cells[0].len(),
        f.cursor_col
    );
}

#[test]
fn the_style_seed_falls_back_to_the_live_default_colors() {
    let mut f = prompt_frame(4, 20);
    f.overlay_ime_preedit("ab", None, false);
    assert_eq!(
        f.cells[0][2].fg,
        [0xC0, 0xC5, 0xCE],
        "live default foreground"
    );
    assert_eq!(
        f.cells[0][2].bg,
        [0x1B, 0x1D, 0x1E],
        "live default background"
    );
    assert_ne!(
        f.cells[0][2].fg, f.cells[0][2].bg,
        "composing text must not be painted onto its own colour"
    );
    assert_eq!(f.cells[0][2].underline, UnderlineStyle::Single);
}

#[test]
fn an_unresolved_frame_seeds_from_the_rows_last_real_cell() {
    let mut f = RenderInput {
        rows: 2,
        cols: 10,
        cells: vec![
            vec![RenderCell {
                ch: '$',
                fg: [11, 22, 33],
                bg: [44, 55, 66],
                ..Default::default()
            }],
            Vec::new(),
        ],
        cursor_row: 0,
        cursor_col: 1,
        ..Default::default()
    };
    assert_eq!(f.default_bg, COLOR_UNSET, "the unresolved producer case");
    f.overlay_ime_preedit("x", None, false);
    assert_eq!(f.cells[0][1].ch, 'x');
    assert_eq!(f.cells[0][1].fg, [11, 22, 33]);
    assert_eq!(f.cells[0][1].bg, [44, 55, 66]);
}

#[test]
fn the_gap_before_the_caret_is_padded_with_undecorated_blanks() {
    let mut f = prompt_frame(4, 20);
    f.cursor_col = 5;
    f.overlay_ime_preedit("z", None, false);
    assert_eq!(f.cells[0].len(), 6);
    for col in 2..5 {
        assert_eq!(f.cells[0][col].ch, ' ', "col {col} is a blank, not a glyph");
        assert_eq!(
            f.cells[0][col].underline,
            UnderlineStyle::None,
            "col {col} is padding, not composition"
        );
        assert_eq!(f.cells[0][col].bg, [0x1B, 0x1D, 0x1E], "in the live ground");
    }
    assert_eq!(f.cells[0][5].ch, 'z');
    assert_eq!(f.cells[0][5].underline, UnderlineStyle::Single);
}

#[test]
fn a_truncated_composition_materializes_only_what_it_drew() {
    let mut f = prompt_frame(2, 5);
    f.overlay_ime_preedit("日本", None, false);
    assert_eq!(f.cells[0].len(), 4, "the straddling char extended nothing");
    assert_eq!(f.cells[0][2].ch, '日');
    assert_eq!(f.cursor_col, 4, "the caret clamps inside the row");
}

#[test]
fn composing_over_a_grapheme_drops_its_stale_cluster() {
    let mut term = Terminal::new(2, 10);
    term.process("\u{1F1EF}\u{1F1F5}xy\r".as_bytes());
    let mut f = RenderInput::empty();
    term.cell_frame_into(&mut f, 2, 10);
    assert_eq!(f.cluster_at(0, 0), Some("\u{1F1EF}\u{1F1F5}"));
    assert_eq!(
        (f.cursor_row, f.cursor_col),
        (0, 0),
        "CR parked at the flag"
    );

    f.overlay_ime_preedit("a", None, false);
    assert_eq!(f.cells[0][0].ch, 'a');
    assert_eq!(
        f.cluster_at(0, 0),
        None,
        "the covered cell's grapheme goes with it"
    );
    assert_eq!(f.cluster_at(0, 2), None, "and nothing else is disturbed");
}
