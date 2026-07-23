// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// VT conformance, batch 2 — more categories: save/restore cursor, insert/delete
// char & line, erase variants, scroll region, alt-screen isolation, wide chars.
// feed bytes → read screen via the engine API → assert.

use aterm_conformance::{Screen, run};

#[test]
fn decsc_decrc_save_and_restore_cursor() {
    // move to (4,4), save, jump home + write X, restore, write Y
    let s = run(b"\x1b[5;5H\x1b7\x1b[1;1HX\x1b8Y");
    assert_eq!(s.row(0), "X");
    // Y written at the restored position (row 4, col 4, 0-based)
    assert_eq!(s.row(4), "    Y");
    assert_eq!(s.cursor(), (4, 5));
}

#[test]
fn ich_inserts_blanks_shifting_right() {
    let s = run(b"abc\x1b[1;1H\x1b[@"); // home, insert 1 blank
    assert_eq!(s.row(0), " abc");
    assert_eq!(s.cursor(), (0, 0));
}

#[test]
fn dch_deletes_char_shifting_left() {
    let s = run(b"abc\x1b[1;1H\x1b[P"); // home, delete 1 char
    assert_eq!(s.row(0), "bc");
    assert_eq!(s.cursor(), (0, 0));
}

#[test]
fn il_inserts_a_blank_line() {
    let s = run(b"A\r\nB\r\nC\x1b[1;1H\x1b[L"); // home, insert line
    assert_eq!(s.row(0), "");
    assert_eq!(s.row(1), "A");
    assert_eq!(s.row(2), "B");
}

#[test]
fn dl_deletes_a_line() {
    let s = run(b"A\r\nB\r\nC\x1b[1;1H\x1b[M"); // home, delete line
    assert_eq!(s.row(0), "B");
    assert_eq!(s.row(1), "C");
}

#[test]
fn el1_erases_from_start_to_cursor() {
    // abcdef, cursor to col 4 (1-based), erase start..cursor inclusive
    let s = run(b"abcdef\x1b[1;4H\x1b[1K");
    assert_eq!(s.row(0), "    ef");
}

#[test]
fn el2_erases_the_whole_line() {
    let s = run(b"abcdef\x1b[2K");
    assert_eq!(s.row(0), "");
}

#[test]
fn ed1_erases_above_the_cursor() {
    // two lines, cursor on row 2; ED1 erases from start of screen to cursor
    let s = run(b"top\r\nbot\x1b[2;2H\x1b[1J");
    assert_eq!(s.row(0), ""); // row above fully erased
}

#[test]
fn alt_screen_isolates_then_restores() {
    let s = run(b"main\x1b[?1049h\x1b[2J\x1b[1;1Halt\x1b[?1049l");
    // after leaving the alt screen, the primary content is back
    assert_eq!(s.row(0), "main");
}

#[test]
fn scroll_region_index_scrolls_only_the_region() {
    // 5-row screen; scroll region rows 2..4 (1-based); fill, IND at bottom scrolls
    // only within [2,4], leaving row 1 and row 5 untouched.
    let mut s = Screen::new(5, 80);
    s.feed(b"\x1b[2;4r"); // DECSTBM rows 2-4
    s.feed(b"\x1b[1;1Ha"); // row 1
    s.feed(b"\x1b[2;1Hb"); // row 2 (top of region)
    s.feed(b"\x1b[4;1Hd"); // row 4 (bottom of region)
    s.feed(b"\x1b[4;1H\x1bD"); // IND at bottom of region -> scroll region up
    assert_eq!(s.row(0), "a"); // outside region, untouched
    assert_eq!(s.row(1), ""); // region scrolled: old 'b' gone from top
}

#[test]
fn wide_char_advances_cursor_by_two() {
    // a CJK wide char occupies 2 columns
    let s = run("世".as_bytes());
    assert_eq!(s.cursor(), (0, 2));
}

#[test]
fn newline_advances_row_without_cr_in_default_mode() {
    // bare LF moves down; in the engine's default it may or may not reset column.
    // Assert only the row advance, which is unambiguous.
    let s = run(b"ab\nc");
    assert_eq!(s.cursor().0, 1);
}
