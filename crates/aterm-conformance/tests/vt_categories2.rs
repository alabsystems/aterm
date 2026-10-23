// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! VT conformance tests for insert mode, origin mode, DEC line drawing, reverse index, and full reset.

use aterm_conformance::{Screen, run};

#[test]
fn irm_insert_mode_shifts_right() {
    let s = run(b"abc\x1b[1;1H\x1b[4hXY");
    assert_eq!(s.row(0), "XYabc");
    assert_eq!(s.cursor(), (0, 2));
}

#[test]
fn dec_line_drawing_maps_glyphs() {
    let s = run(b"\x1b(0qqq\x1b(B");
    assert_eq!(s.row(0), "\u{2500}\u{2500}\u{2500}"); // ─── (box drawings light horizontal)
}

#[test]
fn ri_at_top_scrolls_down() {
    let mut s = Screen::new(3, 80);
    s.feed(b"A\r\nB\r\nC\x1b[1;1H\x1bM");
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
    let mut s = Screen::new(6, 80);
    s.feed(b"\x1b[2;4r\x1b[?6h\x1b[1;1HX");
    assert_eq!(s.row(1), "X");
}

#[test]
fn pending_wrap_is_cancelled_by_cr() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'a', 80).collect();
    input.extend_from_slice(b"\rZ");
    let s = run(&input);
    assert_eq!(s.row(0).chars().next(), Some('Z')); // Z overwrote col 0, no wrap
    assert_eq!(s.row(1), ""); // nothing wrapped to row 1
}
