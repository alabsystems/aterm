// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn text_projection_omits_paint_streams() {
    let mut term = Terminal::new(4, 16);
    for row in 0..12 {
        term.process(format!("past {row}\r\n").as_bytes());
    }
    term.process(b"incoming");
    term.process("\x1b[1;1Hשלום e\u{301}".as_bytes());
    term.scroll_display(1);
    let painted = term.cell_frame(4, 16);
    assert_eq!(painted.display_offset, 1);
    assert!(
        painted.apron_row.present,
        "CONTROL: scrolled paint has an incoming row"
    );
    assert!(
        !painted.line_sizes.is_empty(),
        "CONTROL: paint has line metadata"
    );
    let mut text = TextFrame::default();
    term.text_frame_into(&mut text, 4, 16);
    assert!(text.input.line_sizes.is_empty());
    assert!(text.input.images.is_empty());
    assert!(text.input.clusters.is_empty());
    assert!(text.input.combining.is_empty());
    assert!(text.input.row_rev.is_empty());
    assert!(!text.input.apron_row.present);
}

#[test]
fn text_projection_matches_paint_across_refills_without_consuming_damage() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut rich = Terminal::new(6, 24);
    rich.process("\x1b[31;47mשלום (אב) e\u{301} 👩‍💻\r\n中 ♥︎ ❤️\r\n\x1b#6double".as_bytes());
    rich.text_selection_mut()
        .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    rich.text_selection_mut()
        .update_selection(1, 8, SelectionSide::Right);
    rich.text_selection_mut().complete_selection();
    let mut plain = Terminal::new(6, 24);
    plain.process(b"\x1b[?25lplain");
    let mut history = Terminal::new(6, 24);
    for _ in 0..12 {
        history.process(b"past screen\r\n");
    }
    history.process(b"incoming");
    history.scroll_display(1);
    let mut alternate = Terminal::new(6, 24);
    alternate.process(b"\x1b[?1049halternate\r\n\x1b#6wide");
    let mut text = TextFrame::default();
    for _ in 0..3 {
        for term in [&mut rich, &mut plain, &mut history, &mut alternate] {
            for (rows, cols) in [(6, 24), (3, 8), (8, 30)] {
                let damaged: Vec<_> = term.grid().damage().damaged_rows(term.rows()).collect();
                let full = term.grid().damage().is_full();
                let mut paint = term.cell_frame(rows, cols);
                term.text_frame_into(&mut text, rows, cols);
                assert_eq!(text.cells(), paint.cells);
                assert_eq!(
                    text.cursor(),
                    paint
                        .cursor_visible
                        .then_some((paint.cursor_row, paint.cursor_col))
                );
                assert_eq!(text.display_offset(), paint.display_offset);
                assert_eq!(text.any_selection(), paint.any_selection());
                for row in 0..rows {
                    assert!(
                        text.selection_row_keys(row)
                            .eq(paint.selection_row_keys(row))
                    );
                }
                assert_eq!(
                    damaged,
                    term.grid()
                        .damage()
                        .damaged_rows(term.rows())
                        .collect::<Vec<_>>()
                );
                assert_eq!(full, term.grid().damage().is_full());
                text.overlay_ime_preedit("仮e\u{301}", Some(3), false);
                paint.overlay_ime_preedit("仮e\u{301}", Some(3), false);
                paint.cursor_visible = true;
                assert_eq!(text.cells(), paint.cells);
                assert_eq!(text.cursor(), Some((paint.cursor_row, paint.cursor_col)));
            }
        }
    }
}
