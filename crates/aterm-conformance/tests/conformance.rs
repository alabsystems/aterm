// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// VT/ANSI conformance — the gold standard for "is this a correct terminal."
// Each case: feed input bytes → read the screen via the engine API → assert.
// A failure here is either a wrong expectation or a real engine bug; both are
// worth catching.

use aterm_conformance::run;

#[test]
fn plain_text_lands_on_the_top_row() {
    let s = run(b"hello");
    assert_eq!(s.row(0), "hello");
    assert_eq!(s.cursor(), (0, 5));
}

#[test]
fn crlf_moves_to_next_row_col0() {
    let s = run(b"ab\r\ncd");
    assert_eq!(s.row(0), "ab");
    assert_eq!(s.row(1), "cd");
    assert_eq!(s.cursor(), (1, 2));
}

#[test]
fn carriage_return_returns_to_col0_overwriting() {
    let s = run(b"abc\rX");
    assert_eq!(s.row(0), "Xbc");
    assert_eq!(s.cursor(), (0, 1));
}

#[test]
fn backspace_moves_cursor_left() {
    let s = run(b"abc\x08X");
    assert_eq!(s.row(0), "abX");
}

#[test]
fn cup_positions_cursor_1based() {
    // CSI 2 ; 3 H -> row 2, col 3 (1-based) == (1, 2) 0-based
    let s = run(b"\x1b[2;3HX");
    assert_eq!(s.row(1), "  X");
    assert_eq!(s.cursor(), (1, 3));
}

#[test]
fn sgr_color_does_not_corrupt_text() {
    let s = run(b"\x1b[31mRED\x1b[0m\x1b[1;38;5;208mBOLD\x1b[0m");
    assert_eq!(s.row(0), "REDBOLD");
}

#[test]
fn sgr_interleaved_attrs_and_color_keep_text() {
    // Exercises the flags-only fast path (\x1b[1m, \x1b[22m), the color fast path
    // (\x1b[31m), and reset (\x1b[0m) interleaved between printable chars. The
    // text must remain "ABC" regardless of style transitions.
    let s = run(b"\x1b[1m\x1b[31mA\x1b[22mB\x1b[0mC");
    assert_eq!(s.row(0), "ABC");
}

#[test]
fn sgr_bold_then_color_equals_color_then_bold() {
    // Order of an attribute SGR vs a color SGR must not affect the final style:
    // the flags-only fast path and the color fast path must compose identically.
    let bold_then_color = run(b"\x1b[1m\x1b[31m").style_fingerprint();
    let color_then_bold = run(b"\x1b[31m\x1b[1m").style_fingerprint();
    assert_eq!(
        bold_then_color, color_then_bold,
        "bold-then-color and color-then-bold must yield the same final cell style"
    );
    // And both must actually be bold + red (not silently dropped).
    let single_shot = run(b"\x1b[1;31m").style_fingerprint();
    assert_eq!(bold_then_color, single_shot);
}

#[test]
fn sgr_attr_reset_returns_to_default_style() {
    // Setting then clearing each attribute via its reset form must restore the
    // exact default style fingerprint (flags-only fast path round-trip).
    let default = run(b"").style_fingerprint();
    // bold(1)/un-bold(22), reverse(7)/un-reverse(27), underline(4)/un-underline(24).
    let round_trip = run(b"\x1b[1m\x1b[7m\x1b[4mX\x1b[22m\x1b[27m\x1b[24m").style_fingerprint();
    assert_eq!(
        round_trip, default,
        "setting then clearing every attribute must restore the default style"
    );
}

#[test]
fn el0_erases_from_cursor_to_end_of_line() {
    // write abcdef, move to col 4 (1-based), erase to EOL
    let s = run(b"abcdef\x1b[1;4H\x1b[K");
    assert_eq!(s.row(0), "abc");
}

#[test]
fn ed2_erases_the_whole_screen() {
    let mut s = run(b"line1\r\nline2");
    s.feed(b"\x1b[2J");
    assert_eq!(s.screen(), "");
}

#[test]
fn tab_advances_to_next_8col_stop() {
    let s = run(b"\tX");
    assert_eq!(s.cursor(), (0, 9));
    assert_eq!(s.row(0), "        X");
}

#[test]
fn autowrap_wraps_at_the_right_margin() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'a', 80).collect();
    input.push(b'b');
    let s = run(&input);
    assert_eq!(s.row(0).len(), 80);
    assert_eq!(s.row(1), "b");
}

#[test]
fn cursor_up_down_forward_back() {
    // start at (0,0); CUD 3, CUF 5, CUU 1, CUB 2 -> (2, 3)
    let s = run(b"\x1b[3B\x1b[5C\x1b[1A\x1b[2D");
    assert_eq!(s.cursor(), (2, 3));
}

#[test]
fn newline_at_bottom_scrolls() {
    // fill 24 rows, then one more newline scrolls "row0" off the top
    let mut s = aterm_conformance::Screen::new(3, 80);
    s.feed(b"A\r\nB\r\nC"); // rows: A / B / C, cursor on last row
    s.feed(b"\r\nD"); // scroll: A off, now B / C / D
    assert_eq!(s.row(0), "B");
    assert_eq!(s.row(1), "C");
    assert_eq!(s.row(2), "D");
}
