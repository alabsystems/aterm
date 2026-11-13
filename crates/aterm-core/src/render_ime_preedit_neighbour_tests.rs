// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::RenderInput;
use crate::terminal::{Rgb, Terminal, UnderlineStyle};

fn painted_frame(rows: u16, cols: u16, bytes: &[u8]) -> RenderInput {
    let mut term = Terminal::new(rows, cols);
    term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
    term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
    term.process(bytes);
    let mut f = RenderInput::empty();
    term.cell_frame_into(&mut f, rows.into(), cols.into());
    f
}

#[test]
fn a_full_row_keeps_its_length_and_still_seeds_from_its_own_cursor_cell() {
    let mut f = painted_frame(2, 10, b"abc\x1b[31;44mdefghij\r\x1b[4G");
    assert_eq!(f.cells[0].len(), 10, "fixture: the row is FULL");
    assert_eq!((f.cursor_row, f.cursor_col), (0, 3));
    let (fg, bg) = (f.cells[0][3].fg, f.cells[0][3].bg);
    assert_ne!(
        (fg, bg),
        ([0xC0, 0xC5, 0xCE], [0x1B, 0x1D, 0x1E]),
        "fixture: the cursor cell must NOT be wearing the frame defaults, or \
         this cannot tell the two seeds apart"
    );

    f.overlay_ime_preedit("XY", None, false);
    assert_eq!(f.cells[0].len(), 10, "materializing never grows a full row");
    assert_eq!(f.cells[0][3].ch, 'X');
    assert_eq!(f.cells[0][4].ch, 'Y');
    assert_eq!(
        (f.cells[0][3].fg, f.cells[0][3].bg),
        (fg, bg),
        "the cursor cell still wins the seed"
    );
    assert_eq!(f.cells[0][5].ch, 'f', "nothing right of it moved");
    assert_eq!(f.cursor_col, 5);
}

#[test]
fn a_composition_at_column_zero_of_an_unwritten_row_is_painted() {
    let mut f = painted_frame(3, 20, b"$ \r\n");
    assert!(
        f.cells[1].is_empty(),
        "fixture: the row has no written prefix at all"
    );
    assert_eq!((f.cursor_row, f.cursor_col), (1, 0));

    f.overlay_ime_preedit("日", None, false);
    assert_eq!(f.cells[1].len(), 2);
    assert_eq!(f.cells[1][0].ch, '日');
    assert!(f.cells[1][1].wide, "the continuation is materialized too");
    assert_eq!(f.cells[1][0].underline, UnderlineStyle::Single);
    assert_eq!(
        f.cells[1][0].bg,
        [0x1B, 0x1D, 0x1E],
        "an empty row has no last cell — the live default is the only seed"
    );
    assert_eq!(f.cursor_col, 2);
}

#[test]
fn a_wide_composition_with_one_column_left_paints_nothing_and_grows_nothing() {
    let mut f = painted_frame(2, 8, b"$ ");
    f.cursor_col = 7;
    let before = f.clone();
    f.overlay_ime_preedit("日", None, false);
    assert_eq!(f.cells, before.cells, "not one cell was touched");
    assert_eq!(f.cursor_col, 7, "and the caret stays on the cell it was on");
}

#[test]
fn the_platform_caret_at_offset_zero_anchors_at_the_composition_start() {
    let mut f = painted_frame(2, 20, b"$ ");
    f.overlay_ime_preedit("にほん", Some(0), false);
    assert_eq!(f.cursor_col, 2, "the composition's first column");
    assert_eq!(f.cells[0][2].ch, 'に', "and all of it is still painted");
    assert_eq!(f.cells[0][6].ch, 'ん');
}

#[test]
fn the_ambiguous_width_flag_widens_the_composition_it_is_given() {
    let mut narrow = painted_frame(2, 20, b"$ ");
    narrow.overlay_ime_preedit("±", None, false);
    assert_eq!(
        narrow.cells[0].len(),
        3,
        "EA-Ambiguous is narrow by default"
    );
    assert!(!narrow.cells[0][2].wide);

    let mut wide = painted_frame(2, 20, b"$ ");
    wide.overlay_ime_preedit("±", None, true);
    assert_eq!(
        wide.cells[0].len(),
        4,
        "and double under the CJK width mode"
    );
    assert!(wide.cells[0][3].wide);
    assert_eq!(wide.cursor_col, 4);
}

#[test]
fn composing_onto_a_wide_characters_lead_takes_its_continuation_too() {
    let mut f = painted_frame(2, 10, "日本\r".as_bytes());
    assert!(f.cells[0][1].wide, "fixture: 日 owns columns 0 and 1");
    assert_eq!(f.cursor_col, 0);

    f.overlay_ime_preedit("a", None, false);
    assert_eq!(f.cells[0][0].ch, 'a');
    assert!(!f.cells[0][1].wide, "the orphaned continuation is cleared");
    assert_eq!(f.cells[0][1].ch, ' ');
    assert_eq!(
        f.cells[0][1].underline,
        UnderlineStyle::None,
        "and it is not dressed as composition"
    );
    assert_eq!(f.cells[0][2].ch, '本', "the next character is untouched");
}

#[test]
fn composing_onto_a_wide_characters_continuation_takes_its_lead_too() {
    let mut f = painted_frame(2, 10, "日本\r\x1b[2G".as_bytes());
    assert_eq!(
        f.cursor_col, 1,
        "fixture: the caret sits on 日's continuation"
    );

    f.overlay_ime_preedit("a", None, false);
    assert_eq!(f.cells[0][1].ch, 'a');
    assert_eq!(
        f.cells[0][0].ch, ' ',
        "the lead cannot stay — it would be redrawn over the composition"
    );
    assert!(!f.cells[0][0].wide);
    assert_eq!(f.cells[0][2].ch, '本', "and 本 is untouched");
}

#[test]
fn composing_onto_a_graphemes_continuation_drops_the_cluster_at_its_lead() {
    let mut term = Terminal::new(2, 10);
    term.process("\u{1F1EF}\u{1F1F5}xy\r\u{1b}[2G".as_bytes());
    let mut f = RenderInput::empty();
    term.cell_frame_into(&mut f, 2, 10);
    assert_eq!(f.cluster_at(0, 0), Some("\u{1F1EF}\u{1F1F5}"));
    assert_eq!(
        f.cursor_col, 1,
        "fixture: caret on the flag's second column"
    );

    f.overlay_ime_preedit("a", None, false);
    assert_eq!(f.cells[0][1].ch, 'a');
    assert_eq!(
        f.cluster_at(0, 0),
        None,
        "the cleared lead's grapheme goes with it"
    );
}
