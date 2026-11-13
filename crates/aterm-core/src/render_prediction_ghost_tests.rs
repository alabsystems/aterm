// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::RenderInput;
use crate::terminal::{RenderCell, Rgb, Terminal};

fn frame(rows: u16, cols: u16, bytes: &[u8]) -> RenderInput {
    let mut term = Terminal::new(rows, cols);
    term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
    term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
    term.process(bytes);
    let mut f = RenderInput::empty();
    term.cell_frame_into(&mut f, rows.into(), cols.into());
    f
}

fn blank() -> RenderCell {
    RenderCell {
        fg: [0xC0, 0xC5, 0xCE],
        bg: [0x1B, 0x1D, 0x1E],
        ..RenderCell::default()
    }
}

fn only_changed(before: &RenderInput, after: &RenderInput, at: Option<(usize, usize)>) {
    for (r, (b, a)) in before.cells.iter().zip(&after.cells).enumerate() {
        for c in 0..b.len().max(a.len()) {
            if Some((r, c)) != at {
                assert_eq!(b.get(c), a.get(c), "({r}, {c}) changed");
            }
        }
    }
}

#[test]
fn a_ghost_changes_its_own_cell_and_no_other() {
    let before = frame(2, 10, b"ab\x1b[1;31mcde\x1b[0mfg");
    let mut after = before.clone();
    assert!(after.paint_prediction_ghost(0, 3, 'x', 10, blank()));
    only_changed(&before, &after, Some((0, 3)));
    let (old, new) = (before.cells[0][3], after.cells[0][3]);
    assert_eq!(new.ch, 'x');
    assert!(old.bold && !new.bold, "inherited SGR dropped");
    assert_eq!(new.bg, old.bg);
    assert_ne!(new.fg, old.fg, "dimmed toward the bg");
}

#[test]
fn a_ghost_never_paints_over_half_of_a_wide_glyph() {
    let before = frame(1, 10, "ab日c".as_bytes());
    assert!(
        before.cells[0][3].wide && !before.cells[0][2].wide,
        "fixture: the wide glyph's lead at 2, its continuation at 3"
    );
    for col in [2, 3] {
        let mut after = before.clone();
        assert!(
            !after.paint_prediction_ghost(0, col, 'x', 10, blank()),
            "column {col} is half of the wide glyph"
        );
        only_changed(&before, &after, None);
    }
    let mut after = before.clone();
    assert!(
        after.paint_prediction_ghost(0, 1, 'x', 10, blank()),
        "control: the narrow cell beside it paints"
    );
    only_changed(&before, &after, Some((0, 1)));
}

#[test]
fn a_ghost_lands_on_the_visual_cell_of_its_logical_column() {
    let mut before = frame(1, 10, b"cba");
    before.visual_to_logical = vec![vec![2, 1, 0]];
    let mut after = before.clone();
    assert!(after.paint_prediction_ghost(0, 0, 'x', 10, blank()));
    only_changed(&before, &after, Some((0, 2)));
    assert_eq!(after.cells[0][2].ch, 'x');
}

#[test]
fn a_ghost_drops_its_cells_combining_marks_only() {
    let before = frame(1, 10, "e\u{301}xo\u{308}".as_bytes());
    let marked: Vec<usize> = before.combining[0].iter().map(|&(c, _)| c).collect();
    assert_eq!(marked, [0, 2], "fixture: marks on columns 0 and 2");
    let mut after = before.clone();
    assert!(after.paint_prediction_ghost(0, 0, 'q', 10, blank()));
    let marked: Vec<usize> = after.combining[0].iter().map(|&(c, _)| c).collect();
    assert_eq!(marked, [2], "column 2 keeps its mark");
}

#[test]
fn a_ghost_s_refusal_can_be_asked_without_painting() {
    let before = frame(2, 6, "ab日c".as_bytes());
    for (row, col) in (0..3).flat_map(|r| (0..8).map(move |c| (r, c))) {
        let asked = before.prediction_ghost_paintable(row, col, 6);
        let mut after = before.clone();
        assert_eq!(
            asked,
            after.paint_prediction_ghost(row, col, 'x', 6, blank()),
            "({row}, {col})"
        );
    }
    let probed = before.clone();
    let _ = probed.prediction_ghost_paintable(0, 2, 6);
    assert_eq!(probed.cells, before.cells, "asking paints nothing");
}

#[test]
fn a_ghost_pads_a_short_row_and_clips_to_the_frame() {
    let mut f = frame(2, 6, b"$ ");
    assert_eq!(f.cells[0].len(), 2, "fixture: a trimmed row");
    assert!(f.paint_prediction_ghost(0, 4, 'x', 6, blank()));
    assert_eq!(f.cells[0].len(), 5);
    assert_eq!((f.cells[0][2], f.cells[0][3]), (blank(), blank()));
    assert_eq!(f.cells[0][4].ch, 'x');
    let before = f.clone();
    assert!(!f.paint_prediction_ghost(0, 6, 'y', 6, blank()));
    assert!(!f.paint_prediction_ghost(9, 0, 'y', 6, blank()));
    assert_eq!(f.cells, before.cells);
}

use super::SpanCell::{Moved, Typed, Vacated};

fn text(f: &RenderInput) -> String {
    f.cells[0].iter().map(|c| c.ch).collect()
}

#[test]
fn a_span_moves_text_in_its_own_style_and_types_its_glyph() {
    let before = frame(1, 10, b"ab\x1b[1;31mcd\x1b[0mef");
    let mut after = before.clone();
    let span = [
        (2, Typed('x')),
        (3, Moved { from: 2, ch: 'c' }),
        (4, Moved { from: 3, ch: 'd' }),
        (5, Moved { from: 4, ch: 'e' }),
        (6, Moved { from: 5, ch: 'f' }),
    ];
    assert!(after.paint_prediction_span(0, span, 10, blank()));
    assert_eq!(text(&after), "abxcdef");
    let (x, c) = (after.cells[0][2], after.cells[0][3]);
    assert!(
        !x.bold && x.fg != before.cells[0][2].fg,
        "the typed glyph: a ghost"
    );
    assert_eq!(c, before.cells[0][2], "`c` in its own style, moved");
    assert_eq!(after.cells[0][4], before.cells[0][3]);
    assert_eq!(
        after.cells[0][..2],
        before.cells[0][..2],
        "left of the span"
    );
}

#[test]
fn a_span_reads_every_source_before_writing_it() {
    let before = frame(1, 12, b"a\x1b[1;31mb\x1b[0mcdef\x1b[1;31mgh\x1b[0mij");
    let mut f = before.clone();
    let span = [
        (1, Typed('x')),
        (2, Moved { from: 1, ch: 'b' }),
        (5, Moved { from: 6, ch: 'g' }),
        (6, Moved { from: 7, ch: 'h' }),
        (7, Vacated),
    ];
    assert!(f.paint_prediction_span(0, span, 12, blank()));
    assert_eq!(text(&f), "axbdegh ij");
    for (to, from) in [(2, 1), (5, 6), (6, 7)] {
        assert!(before.cells[0][from].bold, "fixture: ({from}) is bold");
        assert_eq!(
            f.cells[0][to], before.cells[0][from],
            "({to}) is the cell it came from, in its own style"
        );
    }
}

#[test]
fn a_span_cell_moved_onto_itself_is_painted() {
    let before = frame(1, 10, b"ab\x1b[1;31mcd\x1b[0m");
    let mut same = before.clone();
    assert!(same.paint_prediction_span(0, [(2, Moved { from: 2, ch: 'c' })], 10, blank()));
    assert_eq!(same.cells, before.cells, "the cell it is");
    let mut other = before.clone();
    assert!(other.paint_prediction_span(0, [(2, Moved { from: 2, ch: 'z' })], 10, blank()));
    assert_eq!(text(&other), "abzd");
    assert!(!other.cells[0][2].bold, "a ghost of the expected glyph");
    only_changed(&before, &other, Some((0, 2)));
}

#[test]
fn a_span_never_splits_a_wide_glyph() {
    let before = frame(1, 10, "ab日c".as_bytes());
    assert!(before.cells[0][3].wide, "fixture: the continuation at 3");
    for span in [
        vec![(2, Typed('x'))],
        vec![(3, Typed('x'))],
        vec![(1, Moved { from: 2, ch: '日' })],
        vec![(1, Typed('x')), (3, Typed('y'))],
    ] {
        let mut after = before.clone();
        assert!(
            !before.prediction_span_paintable(0, span.clone(), 10),
            "{span:?}"
        );
        assert!(!after.paint_prediction_span(0, span.clone(), 10, blank()));
        assert_eq!(after.cells, before.cells, "{span:?}: nothing painted");
    }
    let mut after = before.clone();
    let whole = [(2, Typed('x')), (3, Typed('y'))];
    assert!(before.prediction_span_paintable(0, whole, 10));
    assert!(after.paint_prediction_span(0, whole, 10, blank()));
    assert_eq!(text(&after), "abxyc");
    assert!(!after.cells[0][3].wide, "the glyph replaced as a unit");
}

#[test]
fn a_span_is_refused_where_its_cells_are_not_what_it_names() {
    let plain = frame(2, 10, b"abcd");
    let mut reordered = plain.clone();
    reordered.visual_to_logical = vec![vec![1, 0, 2, 3], Vec::new()];
    let mut double = plain.clone();
    double.line_sizes = vec![
        crate::grid::LineSize::DoubleWidth,
        crate::grid::LineSize::SingleWidth,
    ];
    let marked = frame(1, 10, "ae\u{301}cd".as_bytes());
    let ok = [(1, Moved { from: 2, ch: 'c' })];
    assert!(plain.prediction_span_paintable(0, ok, 10), "control");
    assert!(!reordered.prediction_span_paintable(0, ok, 10), "BiDi");
    assert!(!double.prediction_span_paintable(0, ok, 10), "double width");
    assert!(
        !plain.prediction_span_paintable(0, [(10, Typed('x'))], 10),
        "cols"
    );
    assert!(!plain.prediction_span_paintable(5, ok, 10), "missing row");
    assert!(
        !plain.prediction_span_paintable(0, [(2, Vacated), (1, Vacated)], 10),
        "out of order"
    );
    assert!(
        !plain.prediction_span_paintable(
            0,
            [
                (0, Moved { from: 2, ch: 'c' }),
                (1, Moved { from: 1, ch: 'b' })
            ],
            10
        ),
        "reordered sources"
    );
    assert!(
        !marked.prediction_span_paintable(0, [(0, Moved { from: 1, ch: 'e' })], 10),
        "a marked source"
    );
}

#[test]
fn a_span_paints_its_own_text_whatever_the_frame_holds() {
    let mut f = frame(1, 10, "ae\u{301}b".as_bytes());
    let span = [
        (1, Moved { from: 2, ch: 'z' }),
        (2, Typed('q')),
        (5, Vacated),
    ];
    assert!(f.paint_prediction_span(0, span, 10, blank()));
    assert_eq!(text(&f), "azq   ");
    assert!(f.combining[0].is_empty(), "the mark on `e` went with it");
    assert_eq!(f.cells[0][3], blank(), "padded");
    let before = f.clone();
    assert!(f.paint_prediction_span(0, [], 10, blank()));
    assert_eq!(f.cells, before.cells);
}
