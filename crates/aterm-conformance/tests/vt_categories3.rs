// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! VT conformance tests for alternate screen, margins, tabs, charsets, and selective erase.

use aterm_conformance::{Screen, run};
use aterm_core::terminal::Terminal;

fn term_24x80(input: &[u8]) -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(input);
    t
}

#[test]
fn mode_1049_saves_cursor_switches_and_clears_alt() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[3;5H"); // primary content, cursor at (2,4)
    s.feed(b"\x1b[?1049h");
    assert_eq!(s.screen(), "", "alt screen must be cleared on 1049 enter");
    s.feed(b"\x1b[1;1HALT");
    assert_eq!(s.row(0), "ALT");
    s.feed(b"\x1b[?1049l");
    assert_eq!(s.row(0), "main", "primary content restored on 1049 exit");
    assert!(
        !s.screen().contains("ALT"),
        "alt content must not leak to primary"
    );
    assert_eq!(s.cursor(), (2, 4), "cursor restored to pre-1049 position");
}

#[test]
fn mode_1047_exit_clears_the_alt_screen() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main");
    s.feed(b"\x1b[?1047h"); // xterm ctlseqs: 1047h = use alt screen buffer (no clear on enter)
    s.feed(b"\x1b[1;1HALT");
    assert_eq!(s.row(0), "ALT");
    s.feed(b"\x1b[?1047l");
    assert_eq!(
        s.row(0),
        "main",
        "primary content intact after 1047 round trip"
    );
    s.feed(b"\x1b[?47h");
    assert_eq!(s.screen(), "", "1047 exit cleared the alt screen");
}

#[test]
fn mode_1047_does_not_save_or_restore_cursor() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[3;3H"); // primary cursor at (2,2)
    s.feed(b"\x1b[?1047h\x1b[11;7H"); // in alt, move to (10,6)
    s.feed(b"\x1b[?1047l");
    assert_eq!(
        s.cursor(),
        (10, 6),
        "1047 exit must not restore the saved cursor"
    );
}

#[test]
fn mode_47_round_trip_keeps_primary_content() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[?47h\x1b[1;1HALT\x1b[?47l");
    assert_eq!(
        s.row(0),
        "main",
        "primary content intact after mode 47 round trip"
    );
    assert!(
        !s.screen().contains("ALT"),
        "alt content must not leak to primary"
    );
}

#[test]
fn mode_47_swap_does_not_move_cursor() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[5;9H"); // cursor at (4,8)
    s.feed(b"\x1b[?47h");
    assert_eq!(s.cursor(), (4, 8), "47 enter must not move the cursor");
}

#[test]
fn mode_47_alt_content_persists_across_reentry() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?47h\x1b[1;1HSTALE\x1b[?47l\x1b[?47h");
    assert_eq!(
        s.row(0),
        "STALE",
        "alt buffer content survives a 47 round trip"
    );
}

#[test]
fn deccolm_ignored_when_mode_40_is_reset() {
    let s = run(b"abc\x1b[?3h");
    assert_eq!(s.row(0), "abc", "no screen clear when DECCOLM is gated off");
    assert_eq!(
        s.cursor(),
        (0, 3),
        "no cursor home when DECCOLM is gated off"
    );
    let s2 = run(b"\x1b[?3h\x1b[1;999H");
    assert_eq!(
        s2.cursor(),
        (0, 79),
        "still 80 columns when DECCOLM is gated off"
    );
}

#[test]
fn deccolm_with_mode_40_clears_screen_and_homes_cursor() {
    let mut s = Screen::new(24, 132);
    s.feed(b"abc\x1b[7;11H");
    s.feed(b"\x1b[?40h\x1b[?3h");
    assert_eq!(s.screen(), "", "DECCOLM switch clears the screen");
    assert_eq!(s.cursor(), (0, 0), "DECCOLM switch homes the cursor");

    let mut t = Terminal::new(24, 132);
    t.process(b"\x1b[?40h\x1b[?3h");
    let _ = t.take_response();
    t.process(b"\x1b[?3$p");
    assert_eq!(
        t.take_response().as_deref(),
        Some(b"\x1b[?3;1$y".as_slice()),
        "DECRQM reports 132-column mode set after ?40h + ?3h"
    );
}

#[test]
fn decncsm_suppresses_clear_on_deccolm_switch() {
    let mut s = Screen::new(24, 132);
    s.feed(b"\x1b[?40h\x1b[?95h"); // enable column switching + DECNCSM
    s.feed(b"keep");
    s.feed(b"\x1b[?3h");
    assert_eq!(s.row(0), "keep", "DECNCSM suppresses the DECCOLM clear");
}

#[test]
fn decscnm_reverse_video_flips_existing_and_new_cells() {
    let mut t = term_24x80(b"AB");
    let fg = t.default_foreground();
    let bg = t.default_background();
    let (fg, bg) = ([fg.r, fg.g, fg.b], [bg.r, bg.g, bg.b]);
    let cells = t.render_row(0);
    assert_eq!((cells[0].fg, cells[0].bg), (fg, bg));

    t.process(b"\x1b[?5h");
    let cells = t.render_row(0);
    assert_eq!(
        (cells[0].fg, cells[0].bg),
        (bg, fg),
        "existing cell flips on DECSCNM set"
    );
    t.process(b"C");
    let cells = t.render_row(0);
    assert_eq!(cells[2].ch, 'C');
    assert_eq!(
        (cells[2].fg, cells[2].bg),
        (bg, fg),
        "cells written while set also flip"
    );

    t.process(b"\x1b[?5l");
    let cells = t.render_row(0);
    assert_eq!(
        (cells[0].fg, cells[0].bg),
        (fg, bg),
        "reset restores normal video"
    );
    assert_eq!((cells[2].fg, cells[2].bg), (fg, bg));
}

#[test]
fn declrmm_autowrap_wraps_to_left_margin() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?69h\x1b[5;20s"); // DECLRMM on; DECSLRM margins cols 5..20 (1-based)
    assert_eq!(s.cursor(), (0, 0), "DECSLRM homes the cursor");
    s.feed(b"\x1b[2;19HXYZ"); // X at (1,18), Y at (1,19)=right margin, Z wraps
    assert_eq!(
        s.row(2),
        "    Z",
        "wrap lands at the left margin, not column 0"
    );
    assert_eq!(s.cursor(), (2, 5));
}

#[test]
fn declrmm_cursor_clamps_and_reset_restores_full_width() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?69h\x1b[5;20s");
    s.feed(b"\x1b[1;10H\x1b[99C");
    assert_eq!(s.cursor(), (0, 19), "CUF clamps at the right margin");
    s.feed(b"\x1b[99D");
    assert_eq!(s.cursor(), (0, 4), "CUB clamps at the left margin");
    s.feed(b"\x1b[?69l"); // xterm: DECLRMM reset disables margins (full width)
    s.feed(b"\x1b[1;1H\x1b[999C");
    assert_eq!(s.cursor(), (0, 79), "full width restored after CSI ?69l");
}

#[test]
fn cht_forward_and_cbt_backward_tab_stops() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[1;5H\x1b[2I");
    assert_eq!(s.cursor(), (0, 16), "CHT moves forward two tab stops");
    s.feed(b"\x1b[Z");
    assert_eq!(s.cursor(), (0, 8), "CBT moves back one tab stop");
}

#[test]
fn cbt_stops_at_the_left_margin_only_under_decom() {
    let armed = b"\x1b[?69h\x1b[11;31s";

    let mut s = Screen::new(3, 40);
    s.feed(armed);
    s.feed(b"\x1b[?6l\x1b[1;13H\x1b[Z");
    assert_eq!(
        s.cursor(),
        (0, 8),
        "DECOM reset: CBT is unclamped and reaches the tab stop at column 8"
    );

    let mut s = Screen::new(3, 40);
    s.feed(armed);
    s.feed(b"\x1b[?6h\x1b[1;3H\x1b[Z");
    assert_eq!(
        s.cursor(),
        (0, 10),
        "DECOM set: CBT stops at the left margin instead of the stop at column 8"
    );
}

#[test]
fn tbc_clears_one_stop_then_all_stops() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[1;9H\x1b[g"); // TBC 0 (default): clear the stop at the cursor (col 8)
    s.feed(b"\x1b[1;1H\t");
    assert_eq!(s.cursor(), (0, 16), "TAB skips the cleared stop at col 8");
    s.feed(b"\x1b[3g"); // TBC 3: clear all tab stops
    s.feed(b"\x1b[1;1H\t");
    assert_eq!(
        s.cursor(),
        (0, 79),
        "TAB with no stops goes to the right margin"
    );
}

#[test]
fn so_si_locking_shift_g1_line_drawing() {
    let s = run(b"\x1b)0\x0eq\x0fq");
    assert_eq!(s.row(0), "\u{2500}q");
}

#[test]
fn ss2_single_shifts_exactly_one_char_from_g2() {
    let s = run(b"\x1b*0\x1bNqq");
    assert_eq!(s.row(0), "\u{2500}q");
}

#[test]
fn so_shift_and_truecolor_survive_a_scroll_region_scroll() {
    const FG: [u8; 3] = [10, 200, 30];
    fn row(t: &Terminal, r: usize) -> String {
        t.row_text(r).unwrap_or_default().trim_end().to_string()
    }
    fn scenario(shift: &[u8], before_new_output: &[u8]) -> Terminal {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x1b[1;1Htop\x1b[5;1Hbelow");
        bytes.extend_from_slice(b"\x1b)0\x1b[2;4r\x1b[38;2;10;200;30m");
        bytes.extend_from_slice(shift);
        bytes.extend_from_slice(b"\x1b[4;1Hqqq");
        bytes.extend_from_slice(b"\n\n");
        bytes.extend_from_slice(before_new_output);
        bytes.extend_from_slice(b"qq");
        term_24x80(&bytes)
    }

    let t = scenario(b"\x0e", b"");
    assert_eq!(row(&t, 1), "\u{2500}\u{2500}\u{2500}");
    let cells = t.render_row(1);
    for (i, cell) in cells.iter().take(3).enumerate() {
        assert_eq!(cell.fg, FG, "moved cell {i} keeps its truecolor foreground");
    }
    assert_eq!(row(&t, 2), "", "the row scrolled in between is blank");
    assert_eq!(row(&t, 0), "top");
    assert_eq!(row(&t, 4), "below");
    assert_eq!(row(&t, 3), "   \u{2500}\u{2500}");
    assert_eq!(t.render_row(3)[3].fg, FG);

    assert_eq!(
        row(&scenario(b"", b""), 1),
        "qqq",
        "without SO the glyphs are ASCII"
    );
    assert_eq!(
        row(&scenario(b"\x0e", b"\x0f"), 3),
        "   qq",
        "SI before the new output re-invokes G0"
    );
}

#[test]
fn decaln_fills_screen_with_e_and_homes_cursor() {
    let s = run(b"\x1b[5;10H\x1b#8");
    assert_eq!(s.row(0), "E".repeat(80));
    assert_eq!(s.row(12), "E".repeat(80));
    assert_eq!(s.row(23), "E".repeat(80));
    assert_eq!(s.cursor(), (0, 0), "DECALN homes the cursor");
}

#[test]
fn decdwl_truncates_right_half_and_decswl_keeps_text() {
    let mut s = Screen::new(24, 80);
    s.feed("a".repeat(50).as_bytes()); // cols 0..49 on row 0
    s.feed(b"\x1b[1;1H\x1b#6"); // DECDWL on row 0
    assert_eq!(s.row(0), "a".repeat(40), "right-half characters are lost");
    s.feed(b"ZZ");
    assert_eq!(s.row(0), format!("ZZ{}", "a".repeat(38)));
    assert_eq!(s.cursor(), (0, 2));
    s.feed(b"\x1b[1;40HPQ"); // col 39 is the last cell of a double-width line
    assert_eq!(s.row(1), "Q", "autowrap fires at the double-width boundary");
    assert_eq!(s.cursor(), (1, 1));
    s.feed(b"\x1b[1;1H\x1b#5"); // DECSWL back to single width
    assert_eq!(s.row(0), format!("ZZ{}P", "a".repeat(37)));
}

#[test]
fn ed2_erases_complete_lines_so_a_decdwl_row_comes_back_single_width() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[1;1H\x1b#6"); // DECDWL poisons row 0
    s.feed(b"\x1b[2J\x1b[H"); // ED 2 + home — the `clear` a user reaches for
    s.feed("a".repeat(50).as_bytes());
    assert_eq!(
        s.row(0),
        "a".repeat(50),
        "the erased row is single-width again"
    );
    assert_eq!(s.row(1), "", "nothing wrapped at the halfway mark");

    let t = term_24x80(b"\x1b[1;1H\x1b#3\x1b[2;1H\x1b#4\x1b[2J");
    for row in 0..2 {
        assert_eq!(
            t.grid().row(row).expect("visible row").line_size(),
            aterm_core::grid::LineSize::SingleWidth,
            "ED 2 erased row {row} as a complete line"
        );
    }
}

#[test]
fn el2_leaves_a_decdwl_row_double_because_xterm_does() {
    let t = term_24x80(b"\x1b[1;1H\x1b#6\x1b[2K");
    assert_eq!(
        t.grid().row(0).expect("visible row").line_size(),
        aterm_core::grid::LineSize::DoubleWidth,
        "EL 2 clears the characters, not the line attribute"
    );
}

#[test]
fn decsc_decrc_saves_and_restores_charset() {
    let s = run(b"\x1b(0\x1b7\x1b(Bq\x1b8q");
    assert_eq!(s.row(0), "\u{2500}");
    assert_eq!(s.cursor(), (0, 1));
}

#[test]
fn rep_repeats_preceding_graphic_char() {
    let s = run(b"A\x1b[3b");
    assert_eq!(s.row(0), "AAAA");
    assert_eq!(s.cursor(), (0, 4));
}

#[test]
fn decsca_protects_from_decsed_but_not_from_ed() {
    let mut s = Screen::new(24, 80);
    s.feed(b"ab\x1b[1\"qCD\x1b[0\"q");
    s.feed(b"\x1b[?2J");
    assert_eq!(s.row(0), "  CD", "DECSED erases unprotected cells only");
    s.feed(b"\x1b[2J");
    assert_eq!(s.screen(), "", "plain ED erases protected cells too");
}
