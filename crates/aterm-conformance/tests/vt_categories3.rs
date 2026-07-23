// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// VT conformance, batch 4 — behavioral depth: the alt-screen mode trio
// (47/1047/1049), DECCOLM gating + DECNCSM, DECSCNM reverse video, left/right
// margins (DECLRMM/DECSLRM), tab ops (CHT/CBT/TBC), charset shifts (SO/SI/SS2),
// DECALN, DECDWL/DECSWL, DECSC charset save, REP, and DECSCA selective erase.
//
// Every expectation is spec-correct xterm/VT510 behavior with the source cited
// inline. The engine bugs this batch originally flushed out (47/1047 cursor
// sharing, alt-buffer persistence, DECSCA text read-back) are fixed; all pass.

use aterm_conformance::{Screen, run};
use aterm_core::terminal::Terminal;

/// Local helper (Screen lacks color access): feed bytes to a raw engine
/// `Terminal` so tests can read render-resolved colors / responses.
fn term_24x80(input: &[u8]) -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(input);
    t
}

// =========================================================================
// 1. Alt-screen mode trio (47 / 1047 / 1049)
// =========================================================================

#[test]
fn mode_1049_saves_cursor_switches_and_clears_alt() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[3;5H"); // primary content, cursor at (2,4)
    s.feed(b"\x1b[?1049h");
    // xterm ctlseqs: 1049h = save cursor as in DECSC, switch to alt screen, clearing it first
    assert_eq!(s.screen(), "", "alt screen must be cleared on 1049 enter");
    s.feed(b"\x1b[1;1HALT");
    assert_eq!(s.row(0), "ALT");
    s.feed(b"\x1b[?1049l");
    // xterm ctlseqs: 1049l = use normal screen buffer and restore cursor as in DECRC
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
    // xterm ctlseqs: 1047l = use normal screen buffer, clearing the alt screen
    // first if we were in it
    s.feed(b"\x1b[?1047l");
    assert_eq!(
        s.row(0),
        "main",
        "primary content intact after 1047 round trip"
    );
    // Re-enter via mode 47 (which never clears): the 1047 exit must have
    // cleared the alt buffer, so it reads back blank.
    s.feed(b"\x1b[?47h");
    assert_eq!(s.screen(), "", "1047 exit cleared the alt screen");
}

#[test]
fn mode_1047_does_not_save_or_restore_cursor() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[3;3H"); // primary cursor at (2,2)
    s.feed(b"\x1b[?1047h\x1b[11;7H"); // in alt, move to (10,6)
    s.feed(b"\x1b[?1047l");
    // xterm ctlseqs: only 1048/1049 save+restore the cursor; 1047 is a buffer
    // switch (plus alt clear on exit) and leaves the cursor where it is.
    assert_eq!(
        s.cursor(),
        (10, 6),
        "1047 exit must not restore the saved cursor"
    );
}

#[test]
fn mode_47_round_trip_keeps_primary_content() {
    let mut s = Screen::new(24, 80);
    // xterm ctlseqs: 47h/47l = plain buffer swap, no save, no clear on either edge
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
    // xterm: mode 47 is a buffer swap only; the (single) cursor does not move
    assert_eq!(s.cursor(), (4, 8), "47 enter must not move the cursor");
}

#[test]
fn mode_47_alt_content_persists_across_reentry() {
    let mut s = Screen::new(24, 80);
    // xterm: mode 47 never clears the alt buffer, so content written to it
    // survives exit + re-enter (this stale-content behavior is exactly why
    // 1047/1049 added clearing).
    s.feed(b"\x1b[?47h\x1b[1;1HSTALE\x1b[?47l\x1b[?47h");
    assert_eq!(
        s.row(0),
        "STALE",
        "alt buffer content survives a 47 round trip"
    );
}

// =========================================================================
// 2. DECCOLM gating (modes 3 / 40 / 95)
// =========================================================================

#[test]
fn deccolm_ignored_when_mode_40_is_reset() {
    // xterm: CSI ?3h is honored only when 80<->132 switching is enabled
    // (mode 40 / c132 resource); otherwise it is ignored entirely.
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
    // Host-side 132-column grid; mode 40 enables DECCOLM, then CSI ?3h switches
    // to 132-column mode.
    let mut s = Screen::new(24, 132);
    s.feed(b"abc\x1b[7;11H");
    s.feed(b"\x1b[?40h\x1b[?3h");
    // xterm/VT420: honoring DECCOLM clears the screen and homes the cursor
    assert_eq!(s.screen(), "", "DECCOLM switch clears the screen");
    assert_eq!(s.cursor(), (0, 0), "DECCOLM switch homes the cursor");

    // VT510 DECRQM: mode 3 must now report "set" (132-column mode active)
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
    // VT510 DECNCSM: when set, changing the column mode does not clear the screen
    assert_eq!(s.row(0), "keep", "DECNCSM suppresses the DECCOLM clear");
}

// =========================================================================
// 3. DECSCNM (mode 5) reverse video
// =========================================================================

#[test]
fn decscnm_reverse_video_flips_existing_and_new_cells() {
    let mut t = term_24x80(b"AB");
    let fg = t.default_foreground();
    let bg = t.default_background();
    let (fg, bg) = ([fg.r, fg.g, fg.b], [bg.r, bg.g, bg.b]);
    // sanity: default-styled cell renders default fg on default bg
    let cells = t.render_row(0);
    assert_eq!((cells[0].fg, cells[0].bg), (fg, bg));

    t.process(b"\x1b[?5h");
    // VT510 DECSCNM set: the whole screen displays reversed — default fg/bg
    // swap for already-written cells, not just new ones
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
    // VT510 DECSCNM reset: normal display restored for every cell
    let cells = t.render_row(0);
    assert_eq!(
        (cells[0].fg, cells[0].bg),
        (fg, bg),
        "reset restores normal video"
    );
    assert_eq!((cells[2].fg, cells[2].bg), (fg, bg));
}

// =========================================================================
// 4. Left/right margins (DECLRMM mode 69 + DECSLRM)
// =========================================================================

#[test]
fn declrmm_autowrap_wraps_to_left_margin() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?69h\x1b[5;20s"); // DECLRMM on; DECSLRM margins cols 5..20 (1-based)
    // VT510 DECSLRM: like DECSTBM, setting margins homes the cursor
    assert_eq!(s.cursor(), (0, 0), "DECSLRM homes the cursor");
    s.feed(b"\x1b[2;19HXYZ"); // X at (1,18), Y at (1,19)=right margin, Z wraps
    // VT420/VT510: with DECLRMM, autowrap at the right margin moves the cursor
    // to the LEFT MARGIN (col 5, 0-based 4) of the next line, not column 0
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
    // VT510: CUF for a cursor inside the margins stops at the right margin
    assert_eq!(s.cursor(), (0, 19), "CUF clamps at the right margin");
    s.feed(b"\x1b[99D");
    // VT510: CUB for a cursor inside the margins stops at the left margin
    assert_eq!(s.cursor(), (0, 4), "CUB clamps at the left margin");
    s.feed(b"\x1b[?69l"); // xterm: DECLRMM reset disables margins (full width)
    s.feed(b"\x1b[1;1H\x1b[999C");
    assert_eq!(s.cursor(), (0, 79), "full width restored after CSI ?69l");
}

// =========================================================================
// 5. Tab ops: CHT, CBT, TBC
// =========================================================================

#[test]
fn cht_forward_and_cbt_backward_tab_stops() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[1;5H\x1b[2I");
    // ECMA-48 CHT / xterm: CSI 2 I advances 2 tab stops (default stops every 8:
    // from col 4 -> 8 -> 16)
    assert_eq!(s.cursor(), (0, 16), "CHT moves forward two tab stops");
    s.feed(b"\x1b[Z");
    // ECMA-48 CBT / xterm: CSI Z moves back one tab stop (16 -> 8)
    assert_eq!(s.cursor(), (0, 8), "CBT moves back one tab stop");
}

#[test]
fn tbc_clears_one_stop_then_all_stops() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[1;9H\x1b[g"); // TBC 0 (default): clear the stop at the cursor (col 8)
    s.feed(b"\x1b[1;1H\t");
    // VT510 TBC 0: only the stop at the cursor column is cleared, so TAB from
    // col 0 now skips 8 and lands on the next stop at 16
    assert_eq!(s.cursor(), (0, 16), "TAB skips the cleared stop at col 8");
    s.feed(b"\x1b[3g"); // TBC 3: clear all tab stops
    s.feed(b"\x1b[1;1H\t");
    // xterm/VT510: with no tab stops, TAB moves to the right margin
    assert_eq!(
        s.cursor(),
        (0, 79),
        "TAB with no stops goes to the right margin"
    );
}

// =========================================================================
// 6. Charsets: SO/SI locking shifts, SS2 single shift
// =========================================================================

#[test]
fn so_si_locking_shift_g1_line_drawing() {
    // ESC ) 0 designates DEC line drawing into G1; SO (LS1) invokes G1 into GL
    // so 'q' renders as U+2500; SI (LS0) re-invokes G0 (ASCII) so 'q' is 'q'.
    // (xterm ctlseqs: SCS + SO/SI shift semantics)
    let s = run(b"\x1b)0\x0eq\x0fq");
    assert_eq!(s.row(0), "\u{2500}q");
}

#[test]
fn ss2_single_shifts_exactly_one_char_from_g2() {
    // ESC * 0 designates DEC line drawing into G2; SS2 (ESC N) shifts G2 into
    // GL for the NEXT CHARACTER ONLY (xterm ctlseqs / ECMA-35 single shift),
    // so the first 'q' is U+2500 and the second is plain ASCII 'q'.
    let s = run(b"\x1b*0\x1bNqq");
    assert_eq!(s.row(0), "\u{2500}q");
}

// =========================================================================
// 7. DECALN
// =========================================================================

#[test]
fn decaln_fills_screen_with_e_and_homes_cursor() {
    let s = run(b"\x1b[5;10H\x1b#8");
    // VT100/xterm DECALN: fill the entire screen with 'E'
    assert_eq!(s.row(0), "E".repeat(80));
    assert_eq!(s.row(12), "E".repeat(80));
    assert_eq!(s.row(23), "E".repeat(80));
    // xterm ctlseqs: DECALN also moves the cursor to home
    assert_eq!(s.cursor(), (0, 0), "DECALN homes the cursor");
}

// =========================================================================
// 8. DECDWL / DECSWL
// =========================================================================

#[test]
fn decdwl_truncates_right_half_and_decswl_keeps_text() {
    let mut s = Screen::new(24, 80);
    s.feed("a".repeat(50).as_bytes()); // cols 0..49 on row 0
    s.feed(b"\x1b[1;1H\x1b#6"); // DECDWL on row 0
    // VT100 DECDWL: switching a single-width line to double width loses all
    // characters to the right of screen center (col >= 40 on an 80-col grid)
    assert_eq!(s.row(0), "a".repeat(40), "right-half characters are lost");
    s.feed(b"ZZ");
    // chars on a double-width line occupy one storage column each (rendered
    // 2 cells wide); writing overwrites cols 0..1 and advances 1 col per char
    assert_eq!(s.row(0), format!("ZZ{}", "a".repeat(38)));
    assert_eq!(s.cursor(), (0, 2));
    s.feed(b"\x1b[1;40HPQ"); // col 39 is the last cell of a double-width line
    // VT100: a double-width line holds cols/2 chars, so autowrap fires at
    // col 39 — Q wraps to the next (single-width) line
    assert_eq!(s.row(1), "Q", "autowrap fires at the double-width boundary");
    assert_eq!(s.cursor(), (1, 1));
    s.feed(b"\x1b[1;1H\x1b#5"); // DECSWL back to single width
    // VT100 DECSWL: the line renders single-width again; its text is kept
    assert_eq!(s.row(0), format!("ZZ{}P", "a".repeat(37)));
}

// =========================================================================
// 9. DECSC/DECRC saves charset state
// =========================================================================

#[test]
fn decsc_decrc_saves_and_restores_charset() {
    // xterm ctlseqs: DECSC saves the character sets (G0..G3) and shift state;
    // DECRC restores them. So: G0=line-drawing, DECSC, G0=ASCII writes 'q',
    // DECRC restores G0=line-drawing AND the cursor, so 'q' overwrites as U+2500.
    let s = run(b"\x1b(0\x1b7\x1b(Bq\x1b8q");
    assert_eq!(s.row(0), "\u{2500}");
    assert_eq!(s.cursor(), (0, 1));
}

// =========================================================================
// 10. REP
// =========================================================================

#[test]
fn rep_repeats_preceding_graphic_char() {
    // ECMA-48 / xterm REP: CSI Ps b repeats the preceding graphic character
    // Ps times — 'A' then CSI 3 b yields "AAAA"
    let s = run(b"A\x1b[3b");
    assert_eq!(s.row(0), "AAAA");
    assert_eq!(s.cursor(), (0, 4));
}

// =========================================================================
// 11. DECSCA + DECSED selective erase
// =========================================================================

#[test]
fn decsca_protects_from_decsed_but_not_from_ed() {
    let mut s = Screen::new(24, 80);
    // "ab" unprotected, then DECSCA 1 protects "CD", then DECSCA 0 turns
    // protection off for later writes (VT510 DECSCA)
    s.feed(b"ab\x1b[1\"qCD\x1b[0\"q");
    s.feed(b"\x1b[?2J");
    // VT510 DECSED 2: erases only cells NOT protected by DECSCA
    assert_eq!(s.row(0), "  CD", "DECSED erases unprotected cells only");
    s.feed(b"\x1b[2J");
    // VT510/xterm: ED is NOT selective — it erases regardless of DECSCA
    assert_eq!(s.screen(), "", "plain ED erases protected cells too");
}
