// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_types::text_shaping::AmbiguousWidth;

use super::{Attrs, Color, MAX_JOINED, MAX_SIDE, Pos, Screen, Style, moves, sgr_bytes};
use crate::tty::Size;

fn screen(cols: u16) -> Screen {
    Screen::new(Size { cols, rows: 2 }, AmbiguousWidth::Single)
}

#[test]
fn put_lays_text_out_as_the_grid_does() {
    let mut s = screen(10);
    let plain = Style::default();
    assert_eq!(s.put(0, 0, "a中b", plain), 4);
    assert_eq!(s.row_text(0), "a中b      ");
    let mut s = screen(10);
    assert_eq!(s.put(0, 0, "\u{1F1FA}", plain), 2);
    assert_eq!(s.put(0, 2, "\u{1F1F8}x", plain), 3);
    assert_eq!(s.row_text(0), "\u{1F1FA}\u{1F1F8}x       ");
    let mut s = screen(3);
    assert_eq!(s.put(0, 1, "a中b", plain), 2);
    assert_eq!(s.row_text(0), " a ");
    let mut s = screen(6);
    assert_eq!(s.put(0, 0, "\u{231A}\u{FE0E}x", plain), 3);
    assert_eq!(s.cell(0, 0).unwrap().width(), 1);
    assert_eq!(s.row_text(0), "\u{231A}\u{FE0E} x   ");
    let mut s = screen(4);
    s.put(0, 0, "\t\x1b", plain);
    assert_eq!(s.row_text(0), "\u{FFFD}\u{FFFD}  ");
    let mut s = screen(4);
    let marks = "\u{301}".repeat(20);
    assert_eq!(s.put(0, 0, &format!("e{marks}"), plain), 1);
    assert_eq!(
        s.cell(0, 0).unwrap().grapheme().chars().count(),
        1 + MAX_JOINED
    );
    assert_eq!(s.put(0, 0, &format!("\u{2764}{marks}\u{FE0F}x"), plain), 2);
    assert_eq!(s.cell(0, 0).unwrap().width(), 1);
    assert_eq!(s.cell(0, 1).unwrap().grapheme(), "x");
    assert_eq!(
        s.put(0, 1, "\u{301}", plain),
        1,
        "a full cell takes nothing more"
    );
    assert_eq!(
        s.cell(0, 0).unwrap().grapheme().chars().count(),
        1 + MAX_JOINED
    );
    assert_eq!(s.cell(0, 1).unwrap().grapheme(), "x");
}

#[test]
fn a_cell_that_would_join_the_put_before_it_is_blanked() {
    let plain = Style::default();
    let mut s = screen(6);
    s.put(0, 2, "\u{1F1F8}", plain);
    s.put(0, 0, "\u{1F1FA}", plain);
    assert_eq!(s.row_text(0), "\u{1F1FA}    ");
    let mut s = screen(6);
    s.put(0, 0, "中文", plain);
    s.put(0, 1, "x", plain);
    assert_eq!(s.row_text(0), " x文  ");
    s.fill(0, 3, 4, plain);
    assert_eq!(s.row_text(0), " x    ");
}

#[test]
fn sgr_is_the_shorter_of_the_changes_and_a_reset() {
    let bold_red = Style {
        fg: Color::Indexed(1),
        attrs: Attrs::BOLD,
        ..Style::default()
    };
    assert_eq!(sgr_bytes(Style::default(), bold_red), b"\x1b[1;31m");
    assert_eq!(sgr_bytes(bold_red, Style::default()), b"\x1b[m");
    let dim_red = Style {
        attrs: Attrs::DIM,
        ..bold_red
    };
    assert_eq!(sgr_bytes(bold_red, dim_red), b"\x1b[22;2m");
    let grey = Style {
        bg: Color::Indexed(236),
        ..Style::default()
    };
    assert_eq!(sgr_bytes(Style::default(), grey), b"\x1b[48;5;236m");
    assert_eq!(sgr_bytes(grey, grey), b"");
}

#[test]
fn a_frame_is_clamped_to_the_grid_size_limit() {
    let huge = Size {
        cols: u16::MAX,
        rows: 2,
    };
    let mut s = Screen::new(huge, AmbiguousWidth::Single);
    assert_eq!(s.size().cols, MAX_SIDE);
    assert_eq!(s.size().rows, 2);
    assert!(s.cell(1, MAX_SIDE - 1).is_some());
    assert!(s.cell(1, MAX_SIDE).is_none());
    s.set_cursor(Some(Pos { row: 1, col: 2 }));
    s.resize(Size {
        cols: 3,
        rows: u16::MAX,
    });
    assert_eq!(s.size().rows, MAX_SIDE);
    assert_eq!(s.size().cols, 3);
    assert_eq!(s.cursor(), Some(Pos { row: 1, col: 2 }));
}

#[test]
fn moves_take_the_shortest_spelling() {
    let at = |row, col| Pos { row, col };
    assert_eq!(moves(None, at(0, 0)), b"\x1b[H");
    assert_eq!(moves(None, at(4, 0)), b"\x1b[5H");
    assert_eq!(moves(None, at(4, 9)), b"\x1b[5;10H");
    assert_eq!(moves(Some(at(4, 9)), at(4, 10)), b"\x1b[C");
    assert_eq!(moves(Some(at(4, 9)), at(4, 0)), b"\r");
    assert_eq!(moves(Some(at(4, 9)), at(5, 0)), b"\r\n");
    assert_eq!(moves(Some(at(4, 9)), at(6, 9)), b"\x1b[2B");
    assert_eq!(moves(Some(at(4, 9)), at(4, 9)), b"");
}
