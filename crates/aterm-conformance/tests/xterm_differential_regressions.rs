// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Differential conformance regressions adjudicated against xterm (`charproc.c`,
// `util.c`, `tabs.c`, `cursor.c`).

use aterm_conformance::{Screen, run};

/// G0-G3 default to ASCII at VT100+ level (`resetCharsets`), so SO (LS1) without a prior SCS prints plain text.
#[test]
fn g1_defaults_to_ascii_so_prints_plain_text() {
    let s = run(b"\x0ex");
    assert_eq!(s.row(0), "x", "SO + 'x' with default G1 must print 'x'");
    assert_eq!(s.cursor(), (0, 1));
}

/// TAB (`TabToNextStop`) preserves pending autowrap (`do_wrap`), so the next printable wraps to the next row.
#[test]
fn ht_preserves_pending_wrap_next_printable_wraps() {
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

/// TAB at pending wrap keeps the cursor at the right margin with `do_wrap` set.
#[test]
fn ht_at_pending_wrap_keeps_cursor_at_margin() {
    let input: Vec<u8> = std::iter::repeat_n(b'a', 80)
        .chain(std::iter::once(b'\t'))
        .collect();
    let s = run(&input);
    assert_eq!(s.cursor(), (0, 79), "cursor stays at the right margin");
}

/// Tab-riding to the last stop then printing wraps after column 79 rather than overprinting it.
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

/// Entering mode 1049 (`srm_OPT_ALTBUF_CURSOR`) saves the cursor and clears the alt screen without homing the cursor.
#[test]
fn mode_1049_enter_keeps_cursor_position() {
    let s = run(b"4\x1b[?1049h");
    assert_eq!(s.cursor(), (0, 1), "cursor stays after '4' on 1049 enter");
    let s2 = run(b"\x1b[10;30Hx\x1b[?1049h");
    assert_eq!(s2.cursor(), (9, 30), "cursor stays at (9,30) on 1049 enter");
    assert_eq!(s2.screen(), "", "alt screen is still cleared on enter");
}

/// DECSTBM ignores empty or inverted regions (`bot <= top`), leaving margins and cursor position unchanged.
#[test]
fn invalid_decstbm_is_fully_ignored() {
    for (name, input) in [
        ("equal margins", &b"x\x1b[10;10r"[..]),
        ("inverted margins", b"x\x1b[20;5r"),
        ("top past the screen", b"x\x1b[28r"),
        ("top far past the screen", b"x\x1b[114r"),
    ] {
        let s = run(input);
        assert_eq!(s.cursor(), (0, 1), "{name}: cursor must not home");
        assert_eq!(s.row(0), "x", "{name}");
    }
}

/// An invalid DECSTBM leaves a previously set scroll region active.
#[test]
fn invalid_decstbm_keeps_previous_region() {
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

/// A valid DECSTBM sets the scroll region and homes the cursor.
#[test]
fn valid_decstbm_still_sets_region_and_homes() {
    let s = run(b"\x1b[10;20Hmid\x1b[4;14r");
    assert_eq!(s.cursor(), (0, 0), "valid DECSTBM homes the cursor");
}

/// Autowrap on the bottom screen row below the scroll region wraps to column 0 of the same row without scrolling (`xtermIndex`).
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

/// LF on the bottom screen row below the scroll region clamps at `max_row` without scrolling.
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

/// REP (`CASE_REP`) replays the raw last character through the GL charset active at repeat time.
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

/// A character printed under DEC Special Graphics repeats as ASCII after switching GL back to `ESC ( B`.
#[test]
fn rep_after_charset_switch_back_repeats_ascii() {
    let s = run(b"\x1b(0q\x1b(B\x1b[3b");
    assert_eq!(
        s.row(0),
        "\u{2500}qqq",
        "raw 'q' repeats as ASCII after ESC ( B"
    );
}

/// REP under default ASCII repeats the previous character verbatim.
#[test]
fn rep_plain_ascii_bulk_path_still_repeats() {
    let s = run(b"X\x1b[9b");
    assert_eq!(s.row(0), "XXXXXXXXXX", "1 print + 9 repeats = 10 X's");
    assert_eq!(s.cursor(), (0, 10));
}

/// Unsaved DECRC does not modify `DECAWM` (`DECSC_FLAGS` excludes `WRAPAROUND`).
#[test]
fn unsaved_decrc_leaves_decawm_unchanged() {
    let mut input = b"\x1b[?7l\x1b8\x1b[1;55H".to_vec();
    input.extend(std::iter::repeat_n(b' ', 27));
    let s = run(&input);
    assert_eq!(
        s.cursor(),
        (0, 79),
        "autowrap stays off across unsaved DECRC"
    );
}

/// Saved DECSC/DECRC does not restore `DECAWM`.
#[test]
fn saved_decrc_does_not_restore_decawm() {
    let mut input = b"\x1b7\x1b[?7l\x1b8\x1b[1;55H".to_vec();
    input.extend(std::iter::repeat_n(b'x', 27));
    let s = run(&input);
    assert_eq!(s.cursor(), (0, 79), "DECRC must not turn autowrap back on");
    assert_eq!(s.row(1), "", "no wrapped output on row 1");
}

/// Unsaved DECRC resets charsets, clears `ORIGIN`, and homes the cursor.
#[test]
fn unsaved_decrc_still_resets_origin_and_homes() {
    let s = run(b"\x1b[5;15r\x1b[?6h\x1b[3;3Hx\x1b8Z");
    assert_eq!(s.cursor(), (0, 1), "unsaved DECRC homes to absolute (0,0)");
    assert_eq!(s.row(0), "Z", "ORIGIN cleared: home is screen-absolute");
}

/// Mode 1049 shares the per-buffer DECSC slot without consuming `sc->saved` on exit, so a subsequent bare DECRC restores that cursor.
#[test]
fn bare_decrc_after_1049_round_trip_restores_saved_cursor() {
    let s = run(b" \x1b[?1049h\x1b[?1049l\x1b8 ");
    assert_eq!(
        s.cursor(),
        (0, 2),
        "DECRC restores the slot 1049 saved (0,1)"
    );
}

/// Entering mode 1049 overwrites a prior DECSC save in the primary buffer's slot.
#[test]
fn mode_1049_save_overwrites_decsc_slot() {
    let s = run(b"main\x1b7\x1b[5;10H\x1b[?1049h\x1b[?1049l\x1b8X");
    assert_eq!(s.cursor(), (4, 10), "ESC 8 lands at the 1049-saved spot");
}

/// A scrolling LF at the bottom row scrolls once and clears pending wrap (`CursorDown` `ResetWrap`), so the next printable overwrites the last column.
#[test]
fn scrolling_lf_clears_pending_wrap() {
    let mut input = b"\x1b[24;1H".to_vec();
    input.extend(std::iter::repeat_n(b'x', 80));
    input.extend_from_slice(b"\n!");
    let s = run(&input);
    assert_eq!(s.row(22), "x".repeat(80), "x row scrolled up exactly once");
    assert_eq!(
        s.row(23),
        format!("{}!", " ".repeat(79)),
        "cleared pending wrap: '!' overwrites the last column, it does not wrap",
    );
    assert_eq!(s.cursor(), (23, 79));
}

/// A non-scrolling LF clears pending wrap, so the next printable overwrites the last column of the new row.
#[test]
fn moving_lf_clears_pending_wrap() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', 80).collect();
    input.extend_from_slice(b"\n!");
    let s = run(&input);
    assert_eq!(s.cursor(), (1, 79), "'!' printed at (1,79), no wrap");
    assert_eq!(s.row(2), "", "nothing wrapped to row 2");
}

/// Under DECSLRM margins, a scrolling LF still clears pending wrap before scrolling.
#[test]
fn margined_scrolling_lf_clears_pending_wrap() {
    let s = run(b"\x1b[?69h\x1b[2;5s\x1b[24;5Ha\nZ");
    assert_eq!(s.row(22), "    a", "the rectangle scrolled up exactly once");
    assert_eq!(s.row(23), "    Z", "Z overwrites the right margin, no wrap");
    assert_eq!(s.cursor(), (23, 4));
}

/// SU (`CSI S`) preserves pending wrap (`xtermScroll` `save_wrap`).
#[test]
fn su_preserves_pending_wrap() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', 80).collect();
    input.extend_from_slice(b"\x1b[1S!");
    let s = run(&input);
    assert_eq!(s.row(1), "!", "wrap still pending after SU: '!' wraps");
    assert_eq!(s.cursor(), (1, 1));
}

/// Filling a row to the margin with `DECAWM` off still arms `do_wrap`, so re-enabling `DECAWM` wraps the next printable.
#[test]
fn margin_fill_with_decawm_off_arms_wrap_for_later() {
    let mut input = b"\x1b[?7l".to_vec();
    input.extend(std::iter::repeat_n(b'x', 80));
    input.extend_from_slice(b"\x1b[?7h!");
    let s = run(&input);
    assert_eq!(s.row(1), "!", "armed wrap consumed after DECAWM re-enable");
    assert_eq!(s.cursor(), (1, 1));
}

/// With `DECAWM` off, an armed `do_wrap` is cleared without wrapping on the next print, overstriking the last column.
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

/// Toggling `DECAWM` off and back on preserves an armed `do_wrap`.
#[test]
fn decawm_reset_does_not_clear_armed_wrap() {
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', 80).collect();
    input.extend_from_slice(b"\x1b[?7l\x1b[?7h!");
    let s = run(&input);
    assert_eq!(s.row(1), "!", "flag survived the DECAWM off/on round trip");
    assert_eq!(s.cursor(), (1, 1));
}

/// Scroll margins (`top_marg`/`bot_marg`) persist across alternate-screen switches (modes 47/1047/1049).
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

/// A scroll region set in the alternate screen remains active after exiting to the primary screen.
#[test]
fn scroll_region_set_in_alt_screen_persists_after_exit() {
    let s = run(b"\x1b[?47h\x1b[2;3r\x1b[?47l!\x1bM");
    assert_eq!(s.row(0), "!", "region set in alt survives the exit");
    assert_eq!(s.cursor(), (0, 1));
}
