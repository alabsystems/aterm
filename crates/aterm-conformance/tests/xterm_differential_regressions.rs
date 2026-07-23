// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Regression tests for engine bugs found by the differential oracle
// (aterm-bench tests/differential.rs: aterm vs alacritty_terminal on
// identical byte streams) and adjudicated against xterm ground truth.
//
// Every expectation below is the xterm behavior, with the xterm source
// cited inline (charproc.c / util.c / tabs.c / cursor.c). Each test pins a
// bug class that aterm originally got wrong; the differential harness keeps
// the same inputs pinned on the comparative side.

use aterm_conformance::{Screen, run};

// =========================================================================
// 1. G1 defaults to ASCII at VT100+ level (not DEC Special Graphics)
// =========================================================================

/// xterm resetCharsets() does `initCharset(screen, 1, nrc_ASCII)`: G0-G3
/// all default to ASCII at VT100+ level. (DEC Special Graphics is the
/// default only in VT52 graphics mode.) SO (LS1) with no prior SCS must
/// therefore print plain text, not box-drawing glyphs.
#[test]
fn g1_defaults_to_ascii_so_prints_plain_text() {
    let s = run(b"\x0ex");
    assert_eq!(s.row(0), "x", "SO + 'x' with default G1 must print 'x'");
    assert_eq!(s.cursor(), (0, 1));
}

/// Designating G1 explicitly still works after the default change: the bug
/// was only the power-on default, not SCS designation.
#[test]
fn g1_designated_line_drawing_still_translates() {
    let s = run(b"\x1b)0\x0eq\x0fq");
    assert_eq!(s.row(0), "\u{2500}q", "ESC ) 0 + SO must still map via SCS");
}

// =========================================================================
// 2. HT preserves the wrap-pending (do_wrap) flag
// =========================================================================

/// xterm CASE_TAB -> TabToNextStop() (tabs.c) only calls set_cur_col and
/// never touches screen->do_wrap: a TAB issued while autowrap is pending
/// leaves the cursor AT the right margin with the wrap still pending, so
/// the NEXT printable wraps to the next row instead of overprinting the
/// last column.
#[test]
fn ht_preserves_pending_wrap_next_printable_wraps() {
    // 80 chars fill row 0 and arm pending-wrap; TAB must not disarm it.
    let mut input: Vec<u8> = std::iter::repeat_n(b'a', 80).collect();
    input.extend_from_slice(b"\tZ");
    let s = run(&input);
    assert_eq!(
        s.row(0).chars().last(),
        Some('a'),
        "col 79 must keep its char — TAB must not enable overprinting"
    );
    assert_eq!(s.row(1), "Z", "printable after TAB wraps to the next row");
    assert_eq!(s.cursor(), (1, 1));
}

/// The cursor itself stays at the right margin after such a TAB (xterm
/// leaves it there with do_wrap set; TabToNextStop finds no stop past the
/// last column).
#[test]
fn ht_at_pending_wrap_keeps_cursor_at_margin() {
    let input: Vec<u8> = std::iter::repeat_n(b'a', 80)
        .chain(std::iter::once(b'\t'))
        .collect();
    let s = run(&input);
    assert_eq!(s.cursor(), (0, 79), "cursor stays at the right margin");
}

/// The original differential repro: tab-riding to the last stop then
/// printing must produce xterm's layout (j at col 79; k/l on the next row),
/// not overprint col 79.
#[test]
fn ht_ride_to_last_stop_matches_xterm_layout() {
    let s = run(b"\ta\tb\tc\td\te\tf\tg\th\ti\tj\tk\tl");
    assert_eq!(
        s.row(0),
        "        a       b       c       d       e       f       g       h       i      j",
    );
    assert_eq!(s.row(1), "k       l");
    assert_eq!(s.cursor(), (1, 9));
}

// =========================================================================
// 3. Alt screen 1049 SET does not move the cursor
// =========================================================================

/// xterm srm_OPT_ALTBUF_CURSOR SET (charproc.c) = CursorSave + ToAlternate +
/// ClearScreen — none of which moves the cursor. The single cursor is
/// shared by both buffers; entering the 1049 alt screen must NOT home it.
#[test]
fn mode_1049_enter_keeps_cursor_position() {
    let s = run(b"4\x1b[?1049h");
    assert_eq!(s.cursor(), (0, 1), "cursor stays after '4' on 1049 enter");
    let s2 = run(b"\x1b[10;30Hx\x1b[?1049h");
    assert_eq!(s2.cursor(), (9, 30), "cursor stays at (9,30) on 1049 enter");
    assert_eq!(s2.screen(), "", "alt screen is still cleared on enter");
}

/// 1049 exit still restores the cursor saved on enter (CursorRestore leg of
/// srm_OPT_ALTBUF_CURSOR RESET) — the enter-side fix must not break it.
#[test]
fn mode_1049_round_trip_still_restores_cursor() {
    let mut s = Screen::new(24, 80);
    s.feed(b"main\x1b[3;5H"); // saved cursor (2,4)
    s.feed(b"\x1b[?1049h\x1b[12;40H\x1b[?1049l");
    assert_eq!(
        s.cursor(),
        (2, 4),
        "exit restores the cursor saved on enter"
    );
}

// =========================================================================
// 4. Invalid DECSTBM is ignored entirely (no margins, no home)
// =========================================================================

/// xterm CASE_DECSTBM (charproc.c) guards BOTH set_tb_margins and
/// CursorSet(0,0) behind `if (bot > top)`: a region that is empty or
/// inverted after defaulting (e.g. CSI 10;10r) must change nothing — not
/// the margins, not the cursor.
#[test]
fn invalid_decstbm_equal_margins_is_fully_ignored() {
    let s = run(b"x\x1b[10;10r");
    assert_eq!(
        s.cursor(),
        (0, 1),
        "cursor must not home on invalid DECSTBM"
    );
    assert_eq!(s.row(0), "x");
}

/// Inverted margins (bottom < top) likewise change nothing.
#[test]
fn invalid_decstbm_inverted_margins_is_fully_ignored() {
    let s = run(b"x\x1b[20;5r");
    assert_eq!(
        s.cursor(),
        (0, 1),
        "cursor must not home on inverted DECSTBM"
    );
}

/// A top at/past the screen bottom is invalid after the bottom defaults to
/// the last row (xterm: bot defaults to MaxRows, then `bot > top` fails for
/// CSI 28r or CSI 114r on 24 rows).
#[test]
fn invalid_decstbm_top_past_screen_is_fully_ignored() {
    let s = run(b"x\x1b[28r");
    assert_eq!(s.cursor(), (0, 1), "CSI 28r on 24 rows must be ignored");
    let s2 = run(b"x\x1b[114r");
    assert_eq!(s2.cursor(), (0, 1), "CSI 114r on 24 rows must be ignored");
}

/// An invalid DECSTBM must also leave a previously set region in force —
/// xterm does not reset to full screen on the invalid request.
#[test]
fn invalid_decstbm_keeps_previous_region() {
    // Valid region rows 5..10 (1-based), then an invalid request, then LF
    // churn from the region bottom: scrolling stays confined to the region.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[5;10r"); // valid: homes cursor
    s.feed(b"\x1b[20;3r"); // invalid: ignored entirely
    s.feed(b"\x1b[10;1Hbottom\n\n"); // two LFs at region bottom scroll the region
    assert_eq!(
        s.row(7),
        "bottom",
        "region 5..10 still active: content scrolled within it"
    );
    assert_eq!(s.cursor(), (9, 6), "cursor pinned at region bottom row");
}

/// Valid DECSTBM still sets margins and homes (xterm: CursorSet(0,0) inside
/// the `bot > top` arm) — the guard must not break the valid path.
#[test]
fn valid_decstbm_still_sets_region_and_homes() {
    let s = run(b"\x1b[10;20Hmid\x1b[4;14r");
    assert_eq!(s.cursor(), (0, 0), "valid DECSTBM homes the cursor");
}

// =========================================================================
// 5. Autowrap below the scroll region at the last screen row never scrolls
// =========================================================================

/// xterm xtermIndex (util.c): `cur_row > bot_marg` means CursorDown, not a
/// scroll — and CursorDown clamps at max_row. So autowrap on the BOTTOM
/// SCREEN row while the cursor sits BELOW the scroll region neither scrolls
/// the display nor moves down: output wraps to column 0 of the same last
/// row and overwrites it in place.
#[test]
fn autowrap_below_scroll_region_at_bottom_row_does_not_scroll() {
    let s = run(b"\x1b[4;12r\x1b[24;75Habcdefghij");
    assert_eq!(s.row(22), "", "row above must not receive scrolled content");
    assert_eq!(
        s.row(23),
        "ghij                                                                      abcdef",
        "last row is overwritten in place from col 0 after the wrap"
    );
    assert_eq!(s.cursor(), (23, 4));
}

/// LF below the region at the last screen row is the same xterm logic
/// (CursorDown clamped at max_row): no scroll, no move.
#[test]
fn lf_below_scroll_region_at_bottom_row_does_not_scroll() {
    let s = run(b"\x1b[4;12r\x1b[24;1Hlast\ntail");
    assert_eq!(
        s.row(23),
        "lasttail",
        "LF at clamped bottom stays on the row"
    );
    assert_eq!(s.cursor(), (23, 8));
}

// =========================================================================
// 6. REP re-translates the raw char through the CURRENT GL charset
// =========================================================================

/// xterm CASE_REP (charproc.c) does `dotext(xw, screen->gsets[curgl],
/// lastchar)`: the RAW last received character is replayed through the GL
/// charset in effect AT REPEAT TIME. Designating DEC Special Graphics
/// between the print and the REP therefore changes the repeated glyph.
#[test]
fn rep_translates_raw_char_through_current_charset() {
    let s = run(b"x\x1b(0\x1b[3b");
    assert_eq!(
        s.row(0),
        "x\u{2502}\u{2502}\u{2502}",
        "REP after ESC ( 0 must repeat 'x' as DEC-graphics U+2502"
    );
    assert_eq!(s.cursor(), (0, 4));
}

/// The converse direction: a glyph printed UNDER DEC graphics repeats as
/// plain ASCII once the charset is switched back (raw char re-translated,
/// not the produced glyph).
#[test]
fn rep_after_charset_switch_back_repeats_ascii() {
    let s = run(b"\x1b(0q\x1b(B\x1b[3b");
    assert_eq!(
        s.row(0),
        "\u{2500}qqq",
        "raw 'q' repeats as ASCII after ESC ( B"
    );
}

/// REP with no charset games still repeats the char verbatim (the fast
/// ASCII fill path must stay correct under the passthrough gate).
#[test]
fn rep_plain_ascii_bulk_path_still_repeats() {
    let s = run(b"X\x1b[9b");
    assert_eq!(s.row(0), "XXXXXXXXXX", "1 print + 9 repeats = 10 X's");
    assert_eq!(s.cursor(), (0, 10));
}

// =========================================================================
// 7. DECSC/DECRC never touch DECAWM
// =========================================================================

/// xterm DECSC_FLAGS = (ATTRIBUTES|ORIGIN|PROTECTED) (cursor.c) — WRAPAROUND
/// is NOT among them, so DECRC neither restores a saved autowrap state nor
/// resets it when nothing was saved. (The VT510 "wrap flag" DECSC saves is
/// the PENDING-wrap state: xterm `sc->wrap_flag = screen->do_wrap`.)
#[test]
fn unsaved_decrc_leaves_decawm_unchanged() {
    // DECAWM off, then unsaved DECRC, then text past the margin: autowrap
    // must STILL be off (cursor pinned at the right margin, no wrap).
    let mut input = b"\x1b[?7l\x1b8\x1b[1;55H".to_vec();
    input.extend(std::iter::repeat_n(b' ', 27));
    let s = run(&input);
    assert_eq!(
        s.cursor(),
        (0, 79),
        "autowrap stays off across unsaved DECRC"
    );
}

/// Saved DECSC/DECRC likewise must not restore DECAWM: turn autowrap off
/// between the save and the restore — it stays off.
#[test]
fn saved_decrc_does_not_restore_decawm() {
    let mut input = b"\x1b7\x1b[?7l\x1b8\x1b[1;55H".to_vec();
    input.extend(std::iter::repeat_n(b'x', 27));
    let s = run(&input);
    assert_eq!(s.cursor(), (0, 79), "DECRC must not turn autowrap back on");
    assert_eq!(s.row(1), "", "no wrapped output on row 1");
}

/// The unsaved-DECRC defaults (xterm CursorRestoreFlags with sc->saved ==
/// False) still apply: reset charsets, clear ORIGIN, home the cursor.
#[test]
fn unsaved_decrc_still_resets_origin_and_homes() {
    let s = run(b"\x1b[5;15r\x1b[?6h\x1b[3;3Hx\x1b8Z");
    assert_eq!(s.cursor(), (0, 1), "unsaved DECRC homes to absolute (0,0)");
    assert_eq!(s.row(0), "Z", "ORIGIN cleared: home is screen-absolute");
}

// =========================================================================
// 8. Mode 1049 shares the DECSC saved-cursor slot
// =========================================================================

/// xterm srm_OPT_ALTBUF_CURSOR SET does CursorSave(xw) into the SAME
/// per-buffer slot DECSC uses (screen->sc[whichBuf]), and the RESET-side
/// CursorRestore reads it WITHOUT consuming (sc->saved stays True). A bare
/// DECRC after a 1049 round trip therefore restores the 1049-saved cursor —
/// it does not reset to defaults.
#[test]
fn bare_decrc_after_1049_round_trip_restores_saved_cursor() {
    let s = run(b" \x1b[?1049h\x1b[?1049l\x1b8 ");
    assert_eq!(
        s.cursor(),
        (0, 2),
        "DECRC restores the slot 1049 saved (0,1)"
    );
}

/// And a DECSC before 1049 is overwritten by the 1049 enter-save (same
/// slot), exactly as in xterm.
#[test]
fn mode_1049_save_overwrites_decsc_slot() {
    // DECSC at (0,4); move to (4,9); 1049 enter saves (4,9) over the slot.
    let s = run(b"main\x1b7\x1b[5;10H\x1b[?1049h\x1b[?1049l\x1b8X");
    assert_eq!(s.cursor(), (4, 10), "ESC 8 lands at the 1049-saved spot");
}

// =========================================================================
// 9. Explicit SU/SD scrolling preserves the wrap-pending flag; LF/RI clear it
//    (LF is a cursor-down — it clears the flag even when it scrolls)
// =========================================================================

/// aterm decision (commit f69ae58, verified against xterm.js): LF is a
/// cursor-down and ALWAYS resets the deferred wrap (xterm CursorDown
/// ResetWrap) — including the at-bottom LF that scrolls. So an LF after a full
/// bottom row scrolls EXACTLY ONCE and clears the pending wrap; the next
/// printable then OVERWRITES the last column instead of wrapping (which would
/// have triggered a second scroll). Only the explicit SU/SD CSI ops preserve
/// the flag (via xtermScroll `save_wrap`); those do not go through `line_feed`.
/// Mirrors the aterm-grid owner test `line_feed_always_clears_pending_wrap`.
#[test]
fn scrolling_lf_clears_pending_wrap() {
    // Fill the bottom row to arm pending-wrap, LF scrolls ONCE + clears the flag,
    // '!' overwrites the last column (no wrap, no second scroll).
    let mut input = b"\x1b[24;1H".to_vec();
    input.extend(std::iter::repeat_n(b'x', 80));
    input.extend_from_slice(b"\n!");
    let s = run(&input);
    // The x-row scrolled up exactly ONCE (to row 22); the cleared wrap means '!'
    // overwrites the last column of the bottom row rather than wrapping to col 0.
    assert_eq!(s.row(22), "x".repeat(80), "x row scrolled up exactly once");
    assert_eq!(
        s.row(23),
        format!("{}!", " ".repeat(79)),
        "cleared pending wrap: '!' overwrites the last column, it does not wrap",
    );
    assert_eq!(s.cursor(), (23, 79));
}

/// xterm CursorDown ends with ResetWrap: an LF that only MOVES the cursor
/// clears the pending wrap, so the next printable overwrites the last
/// column of the new row instead of wrapping.
#[test]
fn moving_lf_clears_pending_wrap() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', 80).collect();
    input.extend_from_slice(b"\n!");
    let s = run(&input);
    assert_eq!(s.cursor(), (1, 79), "'!' printed at (1,79), no wrap");
    assert_eq!(s.row(2), "", "nothing wrapped to row 2");
}

/// SU (CSI S) also goes through xtermScroll and preserves the flag.
#[test]
fn su_preserves_pending_wrap() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', 80).collect();
    input.extend_from_slice(b"\x1b[1S!");
    let s = run(&input);
    assert_eq!(s.row(1), "!", "wrap still pending after SU: '!' wraps");
    assert_eq!(s.cursor(), (1, 1));
}

// =========================================================================
// 10. The wrap-pending flag is independent of DECAWM
// =========================================================================

/// xterm arms `do_wrap` whenever a print fills to the margin REGARDLESS of
/// WRAPAROUND (charproc.c dotext: `screen->do_wrap = need_wrap`), and
/// srm_DECAWM only flips the mode bit. Filling the line with autowrap OFF
/// and then re-enabling it makes the next printable wrap.
#[test]
fn margin_fill_with_decawm_off_arms_wrap_for_later() {
    let mut input = b"\x1b[?7l".to_vec();
    input.extend(std::iter::repeat_n(b'x', 80));
    input.extend_from_slice(b"\x1b[?7h!");
    let s = run(&input);
    assert_eq!(s.row(1), "!", "armed wrap consumed after DECAWM re-enable");
    assert_eq!(s.cursor(), (1, 1));
}

/// With autowrap STILL off, the armed flag is consumed flag-only at the
/// next print (xterm: `do_wrap = False;` without WrapLine) — output keeps
/// overstriking the last column.
#[test]
fn margin_fill_with_decawm_off_overstrikes_without_wrap() {
    let mut input = b"\x1b[?7l".to_vec();
    input.extend(std::iter::repeat_n(b'x', 80));
    input.extend_from_slice(b"AB");
    let s = run(&input);
    assert_eq!(s.row(0).chars().last(), Some('B'), "B overstrikes col 79");
    assert_eq!(s.row(1), "", "no wrap while DECAWM is off");
    assert_eq!(s.cursor(), (0, 79));
}

/// Disabling DECAWM does not discard an armed wrap either: the flag
/// survives the mode toggle and is consumed flag-only by the next print.
#[test]
fn decawm_reset_does_not_clear_armed_wrap() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', 80).collect();
    input.extend_from_slice(b"\x1b[?7l\x1b[?7h!");
    let s = run(&input);
    assert_eq!(s.row(1), "!", "flag survived the DECAWM off/on round trip");
    assert_eq!(s.cursor(), (1, 1));
}

// =========================================================================
// 11. DECSTBM/DECSLRM margins persist across alt-screen switches
// =========================================================================

/// xterm keeps the margins in the shared TScreen (top_marg/bot_marg):
/// ToAlternate/FromAlternate (modes 47/1047/1049) do not reset them. RI at
/// row 0 with a region starting at row 1 must NOT scroll — the cursor is
/// above the region — even right after entering the alt screen.
#[test]
fn scroll_region_persists_into_alt_screen() {
    let s = run(b"\x1b[2;3r\x1b[?1049h!\x1bM");
    assert_eq!(
        s.row(0),
        "!",
        "RI above the inherited region must not scroll"
    );
    assert_eq!(s.cursor(), (0, 1));
}

/// The reverse direction: a region set while IN the alt screen stays in
/// force after exiting (xterm shared-TScreen margins).
#[test]
fn scroll_region_set_in_alt_screen_persists_after_exit() {
    let s = run(b"\x1b[?47h\x1b[2;3r\x1b[?47l!\x1bM");
    assert_eq!(s.row(0), "!", "region set in alt survives the exit");
    assert_eq!(s.cursor(), (0, 1));
}
