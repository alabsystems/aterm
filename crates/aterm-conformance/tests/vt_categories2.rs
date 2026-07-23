// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// VT conformance, batch 3 — insert mode, origin mode, DEC line-drawing charset,
// reverse index scroll, and full reset. feed bytes → read screen → assert.

use aterm_conformance::{Screen, run};

#[test]
fn irm_insert_mode_shifts_right() {
    // type abc, home, enable insert mode (IRM), type XY -> "XYabc"
    let s = run(b"abc\x1b[1;1H\x1b[4hXY");
    assert_eq!(s.row(0), "XYabc");
    assert_eq!(s.cursor(), (0, 2));
}

#[test]
fn irm_off_overwrites() {
    // without insert mode, typing overwrites
    let s = run(b"abc\x1b[1;1HXY");
    assert_eq!(s.row(0), "XYc");
}

#[test]
fn dec_line_drawing_maps_glyphs() {
    // select DEC special graphics for G0, draw 'qqq' -> three horizontal lines,
    // then restore ASCII (G0 = B)
    let s = run(b"\x1b(0qqq\x1b(B");
    assert_eq!(s.row(0), "\u{2500}\u{2500}\u{2500}"); // ─── (box drawings light horizontal)
}

#[test]
fn ri_at_top_scrolls_down() {
    // 3-row screen with A/B/C, cursor home, Reverse Index scrolls the screen down
    let mut s = Screen::new(3, 80);
    s.feed(b"A\r\nB\r\nC\x1b[1;1H\x1bM");
    // a blank line is pulled in at the top; A/B move down, C falls off
    assert_eq!(s.row(0), "");
    assert_eq!(s.row(1), "A");
    assert_eq!(s.row(2), "B");
}

#[test]
fn ris_full_reset_clears_and_homes() {
    let s = run(b"hello\r\nworld\x1bc");
    assert_eq!(s.screen(), "");
    assert_eq!(s.cursor(), (0, 0));
}

#[test]
fn decom_origin_mode_homes_into_scroll_region() {
    // scroll region rows 2..4 (1-based); origin mode on; CUP 1;1 lands at region top
    let mut s = Screen::new(6, 80);
    s.feed(b"\x1b[2;4r\x1b[?6h\x1b[1;1HX");
    // region top is screen row 2 (1-based) == row 1 (0-based)
    assert_eq!(s.row(1), "X");
}

#[test]
fn pending_wrap_is_cancelled_by_cr() {
    // fill the last column (cursor in pending-wrap), CR returns to col 0 same row
    let mut input: Vec<u8> = std::iter::repeat_n(b'a', 80).collect();
    input.extend_from_slice(b"\rZ");
    let s = run(&input);
    assert_eq!(s.row(0).chars().next(), Some('Z')); // Z overwrote col 0, no wrap
    assert_eq!(s.row(1), ""); // nothing wrapped to row 1
}
