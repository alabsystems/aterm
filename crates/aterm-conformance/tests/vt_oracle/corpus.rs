// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The hand-written corpus: byte streams chosen to sit on the edges of the
//! subset, each fed to both engines one byte at a time and compared after
//! every byte (so agreement holds at every prefix, which is replay at any
//! offset). A case either agrees or is named in
//! [`super::exclusions::EXCLUSIONS`] with its triage.
//!
//! Sources: astream-term's own fixtures (its `tests/screen_fold.rs`,
//! `tests/scroll_region.rs` and `tests/bce.rs`, restated here, since a test
//! file is no API), the fixtures of the cut `astream-oracle` side workspace
//! (fd3700d10), cases written against xterm's source for every category the
//! fold models, and one regression case per aterm bug this oracle found
//! (each also pinned by a unit test beside the fix).

/// One corpus case: a grid size and the bytes to feed.
pub struct Case {
    pub name: &'static str,
    pub cols: u16,
    pub rows: u16,
    pub bytes: &'static [u8],
}

const fn case(name: &'static str, cols: u16, rows: u16, bytes: &'static [u8]) -> Case {
    Case {
        name,
        cols,
        rows,
        bytes,
    }
}

pub const CORPUS: &[Case] = &[
    // --- C0 format effectors: CR, LF, BS, HT -------------------------------
    case("cr_lf_bs", 10, 3, b"abc\rX\nY\x08Z"),
    case("bs_stops_at_column_0", 10, 2, b"\x08\x08a\x08\x08\x08b"),
    // astream-term screen_fold: xterm parks the cursor ON the last column, so
    // BS after a full line lands on the second-to-last.
    case("bs_after_a_full_line", 5, 2, b"abcde\x08X"),
    case("ht_stops_every_8", 20, 2, b"a\tb\tc\td"),
    // xterm TabToNextStop never touches do_wrap: a TAB at a pending wrap
    // keeps it, and the next glyph wraps.
    case("ht_keeps_a_pending_wrap", 8, 2, b"abcdefgh\tZ"),
    case("ht_rides_to_the_last_column", 20, 2, b"\ta\tb\tc\td\te"),
    case("lf_resolves_a_pending_wrap", 4, 3, b"abcd\nZ"),
    case("lf_scrolls_at_the_bottom", 4, 2, b"a\r\nb\r\nc"),
    case("ignored_c0_and_del", 10, 2, b"a\x00b\x07c\x7fd"),
    // xterm CASE_VMOT: VT and FF index like LF.
    case("vt_and_ff_are_line_feeds", 10, 3, b"a\x0bb\x0cc"),
    // --- autowrap ----------------------------------------------------------
    case("deferred_wrap", 3, 2, b"abcd"),
    case("wrap_that_scrolls", 5, 3, b"abcdefghijklmnop"),
    case("one_column_screen", 1, 3, b"abcde"),
    case("one_row_screen", 5, 1, b"hello world"),
    case("cuf_at_a_pending_wrap", 4, 3, b"abcd\x1b[CZ"),
    case("cub_after_a_full_line", 5, 2, b"abcde\x1b[2DX"),
    case("cuu_cud_resolve_a_pending_wrap", 4, 3, b"\x1b[2;1Habcd\x1b[AZ\x1b[BY"),
    // DECSC/DECRC round-trip the wrap (xterm CursorSave2 `wrap_flag`).
    case("decsc_round_trips_a_pending_wrap", 4, 3, b"abcd\x1b7\x1b[H\x1b8Z"),
    // --- CSI cursor motion -------------------------------------------------
    case("cup_clamps_and_defaults", 10, 4, b"\x1b[99;99HX\x1b[0;0HY\x1b[2;3fZ\x1b[;5HW"),
    case("cuu_cud_cuf_cub", 10, 5, b"\x1b[3;4H\x1b[2AA\x1b[3BB\x1b[2CC\x1b[4DD\x1b[0CE"),
    case("cnl_cpl_cha_vpa", 10, 5, b"\x1b[3;4H\x1b[Ex\x1b[2Fy\x1b[7Gz\x1b[4dw"),
    case("huge_counts_saturate", 10, 8, b"\x1b[4;4H\x1b[65535B\x1b[65535C!\x1b[99999999A?"),
    // --- DECSTBM -----------------------------------------------------------
    case("region_scrolls_inside_only", 10, 6, b"\x1b[2;4r\x1b[2;1HA\r\nB\r\nC\r\nD\r\nE"),
    case("region_never_scrolls_rows_outside", 10, 6, b"\x1b[6;1HKEEP\x1b[1;3r\x1b[1;1H1\r\n2\r\n3\r\n4\r\n5"),
    case("bare_decstbm_resets_to_full", 10, 3, b"\x1b[1;2r\x1b[rA\r\nB\r\nC\r\nD"),
    case("bottom_margin_past_the_screen_clamps", 10, 4, b"TOP\x1b[2;99r\x1b[4;1HA\r\nB\r\nC"),
    case("empty_region_is_ignored", 10, 5, b"\x1b[2;3r\x1b[5;5H\x1b[3;3rX"),
    case("cuu_cud_stop_at_the_margins", 10, 5, b"\x1b[5;1HSTATUS\x1b[1;4r\x1b[2;1H\x1b[99BX\x1b[99AY"),
    case("lf_below_the_region_at_the_last_row", 10, 5, b"\x1b[1;3r\x1b[5;1Hx\ny\nz"),
    // The cut astream-oracle's scroll-region fixtures (fd3700d10).
    case("oracle_region_1", 24, 8, b"\x1b[3;6r\x1b[3;1Hone\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix"),
    case("oracle_region_2", 24, 8, b"\x1b[1;3r\x1b[1;1Ha\r\nb\r\nc\r\nd\r\ne"),
    case("oracle_region_3", 24, 8, b"top\r\n\x1b[2;4rin1\r\nin2\r\nin3\r\nin4"),
    case("oracle_region_4", 24, 8, b"\x1b[2;5rL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\x1b[rAFTER"),
    case("oracle_region_5", 24, 8, b"\x1b[4;7r\x1b[7;1Hbottomline\r\nnext\r\nmore"),
    // --- ED / EL, and background-colour erase --------------------------------
    case("ed_0", 6, 3, b"abcdef\r\nghijkl\r\nmnopqr\x1b[2;3H\x1b[J"),
    case("ed_1", 6, 3, b"abcdef\r\nghijkl\r\nmnopqr\x1b[2;3H\x1b[1J"),
    case("ed_2_keeps_the_cursor", 6, 3, b"abcdef\r\nghijkl\x1b[2;3H\x1b[2JX"),
    case("el_0_1_2", 6, 3, b"abcdef\r\nghijkl\r\nmnopqr\x1b[1;3H\x1b[K\x1b[2;3H\x1b[1K\x1b[3;3H\x1b[2K"),
    // DECSED/DECSEL with nothing protected erase exactly like ED/EL.
    case("decsed_decsel", 6, 3, b"abcdef\r\nghijkl\r\nmnopqr\x1b[2;3H\x1b[?1J\x1b[3;3H\x1b[?0K"),
    case("ed_unknown_mode_is_ignored", 6, 2, b"abcdef\x1b[1;3H\x1b[9J\x1b[7K"),
    // astream-term bce / the cut astream-oracle bce fixtures.
    case("ed_with_a_background", 8, 3, b"\x1b[44m\x1b[2J"),
    case("el_with_a_background", 8, 2, b"AB\r\nCD\x1b[1;1H\x1b[41m\x1b[K"),
    case("bce_then_text", 10, 3, b"\x1b[42mhi\x1b[2J\x1b[0mok\x1b[44m\x1b[K"),
    case("oracle_bce_1", 8, 4, b"noise\x1b[41m\x1b[2Jafter"),
    case("oracle_bce_2", 8, 4, b"\x1b[42mgreen\x1b[0m\x1b[2Jdone"),
    // Regression: aterm erased DECSEL/DECSED to the default background
    // (aterm-grid `selective_erases_fill_with_the_bce_background`).
    case("decsel_decsed_with_a_background", 8, 3, b"abcdefgh\r\nijkl\x1b[1;3H\x1b[45m\x1b[?K\x1b[2;2H\x1b[?1J"),
    // Reverse video does not reach the erase (xterm ClearCells writes the
    // colour pair, not the attribute flags).
    case("erase_under_reverse_video", 8, 2, b"\x1b[7;44m\x1b[2J"),
    // xterm ClearRight clears from cur_col (the parked last column) and resets
    // the wrap; ClearLeft reaches it too.
    case("el_0_at_a_pending_wrap", 4, 3, b"abcd\x1b[KZ"),
    case("el_1_at_a_pending_wrap", 4, 3, b"abcd\x1b[1KZ"),
    case("ed_0_at_a_pending_wrap", 4, 3, b"\x1b[2;1Hxyz\x1b[1;1Habcd\x1b[JZ"),
    // xterm do_erase_display case 3 erases saved lines only.
    case("ed_3_spares_the_screen", 6, 2, b"abc\x1b[3J"),
    // A line scrolled in takes the current background (ScrnClearLines).
    case("bce_on_scroll", 8, 3, b"a\r\nb\r\nc\x1b[44m\r\nd"),
    case("bce_on_region_scroll", 8, 4, b"\x1b[2;3r\x1b[3;1H\x1b[42m\r\nx"),
    // --- ICH / DCH / ECH / IL / DL -----------------------------------------
    case("ich", 10, 2, b"abcdef\x1b[1;3H\x1b[2@X"),
    case("ich_clamps_at_the_edge", 4, 1, b"abcd\x1b[1;2H\x1b[99@"),
    case("ich_at_a_pending_wrap", 4, 2, b"abcd\x1b[@Z"),
    case("dch", 10, 2, b"abcdef\x1b[1;3H\x1b[2P"),
    case("dch_zero_means_one", 4, 1, b"abcd\x1b[1;2H\x1b[0P"),
    case("ech_with_a_background", 8, 1, b"abcdef\x1b[1;2H\x1b[44m\x1b[3X"),
    case("il_inside_the_region", 10, 5, b"0\r\n1\r\n2\r\n3\r\n4\x1b[2;4r\x1b[3;5H\x1b[L"),
    case("dl_inside_the_region", 10, 5, b"0\r\n1\r\n2\r\n3\r\n4\x1b[2;4r\x1b[2;3H\x1b[2M"),
    case("il_outside_the_region_does_nothing", 10, 5, b"0\r\n1\x1b[2;4r\x1b[1;3H\x1b[L"),
    case("il_dl_full_screen", 10, 4, b"a\r\nb\r\nc\x1b[2;1H\x1b[L\x1b[4;1H\x1b[M"),
    // --- IND / NEL / RI ----------------------------------------------------
    case("ind_scrolls_at_the_bottom", 10, 2, b"a\r\nb\x1bDx"),
    case("nel", 10, 3, b"ab\x1bEx"),
    case("ri_scrolls_the_region_down", 10, 5, b"T\x1b[2;4r\x1b[2;1HA\r\nB\r\nC\x1b[5;1HZ\x1b[2;1H\x1bMx"),
    case("ri_full_screen_at_row_0", 10, 4, b"a\r\nb\x1b[1;1H\x1bMx"),
    // Regressions: aterm kept the wrap at the top margin and left a one-row
    // screen standing (aterm-grid `reverse_line_feed_at_the_top_margin_*`,
    // `reverse_line_feed_on_a_one_row_screen_*`).
    case("ri_resets_a_pending_wrap", 3, 2, b"bAz\x1bMQ"),
    case("ri_on_a_one_row_screen", 3, 1, b"b\x1bM"),
    // --- SGR ---------------------------------------------------------------
    case("sgr_attributes_and_their_resets", 10, 2, b"\x1b[1;31mA\x1b[4mB\x1b[7mC\x1b[22;24;27mD\x1b[0mE"),
    case("sgr_256_both_forms", 10, 2, b"\x1b[38;5;196mA\x1b[48;5;21mB\x1b[38:5:10mC\x1b[48:5:200mD\x1b[39;49mE"),
    case("sgr_bright", 10, 2, b"\x1b[91mA\x1b[102mB\x1b[1;30mC"),
    case("sgr_underline_styles", 10, 2, b"\x1b[4:3mA\x1b[4:0mB\x1b[4:1mC\x1b[24mD"),
    case("sgr_empty_parameters", 10, 2, b"\x1b[1m\x1b[;4mX\x1b[mY"),
    // Truecolour is consumed with its parameters, never re-dispatched: the
    // `4` after the triple underlines and the colours are reset before
    // anything paints (the colour itself is outside the subset).
    case("sgr_truecolour_is_consumed_whole", 10, 2, b"\x1b[1m\x1b[38;2;255;0;0;4m\x1b[39mX\x1b[48;2;0;0;0m\x1b[49mY"),
    // Regression: aterm re-dispatched a refused colour's tail (`38;5` turned
    // on blink); xterm's parse_extended_colors consumes it (aterm-core
    // `a_refused_extended_colour_consumes_what_xterm_consumes`).
    case("sgr_truncated_extended_colours", 10, 2, b"\x1b[31;44m\x1b[38;5mA\x1b[48;5mB\x1b[38;2;1;2mC\x1b[0;38;7;1mD"),
    case("sgr_private_markers_never_reset_the_pen", 10, 1, b"\x1b[1;31mX\x1b[>4;2mY\x1b[=1mZ"),
    // --- alternate screen, DECSC/DECRC, DECTCEM, RIS -------------------------
    case("alt_1049_round_trip", 20, 3, b"abc\x1b[?1049hXYZ\x1b[?1049lD"),
    case("alt_1047_round_trip", 20, 3, b"abc\x1b[?1047hXYZ\x1b[?1047lD"),
    // astream-term screen_fold: one DECSC slot per buffer, never consumed.
    case("decsc_slot_per_buffer", 20, 12, b"\x1b[3;4H\x1b7\x1b[?1049h\x1b[10;1Hx\x1b7\x1b[?1049l\x1b[1;1H\x1b8"),
    case("rmcup_47_shape", 20, 12, b"\x1b[3;4H\x1b7\x1b[?47h\x1b[10;1Hxyz\x1b7\x1b[2J\x1b[?47l\x1b8"),
    // A stray `?1049l`: xterm restores the main slot (or homes with none
    // saved); aterm moves nothing, deliberately (the exclusions say why).
    case("stray_1049l_with_a_saved_slot", 20, 4, b"\x1b[2;6H\x1b7\x1b[4;9H\x1b[?1049lX"),
    case("stray_1049l_with_nothing_saved", 20, 4, b"\t\x1b[1;31m\x1b[?1049lX"),
    // A `?47h` screen left with `?1049l` is a real exit: the restore of an
    // unsaved slot homes the cursor (aterm-core `stray_1049_reset_tests`).
    case("a_47_screen_left_with_1049l_homes", 20, 4, b"\x1b[3;9H\x1b[?47h\x1b[2;2Hx\x1b[?1049lX"),
    case("alt_47_persists", 10, 2, b"main\x1b[?47h\x1b[HALT\x1b[?47l\x1b[?47h"),
    case("second_1049h_clears_again", 10, 2, b"\x1b[?1049hX\x1b[?1049h"),
    case("alt_switch_resets_a_pending_wrap", 2, 3, b"ab\x1b[?1049hZ"),
    // Regression: a 47 switch carries the current background to the buffer it
    // swaps in (aterm-core `alt_switch_bce_tests`).
    case("background_carries_across_a_47_switch", 10, 3, b"\x1b[44m\x1b[?47h\x1b[3;1H\n"),
    case("a_1049_visit_leaves_no_buffer_behind", 10, 3, b"\x1b[?1049hVIM\x1b[?1049l\x1b[?47h"),
    case("osc133_a_leaves_an_orphaned_alt_screen", 10, 3, b"\x1b[?1049h\x1b]133;A\x07"),
    case("decrc_restores_the_pen", 10, 2, b"\x1b[31m\x1b7\x1b[32m\x1b8X"),
    case("decrc_with_nothing_saved_resets_the_pen", 10, 2, b"\x1b[2;3H\x1b[7;35m\x1b8Z"),
    case("dectcem", 10, 2, b"\x1b[?25lA\x1b[?25hB\x1b[?25l"),
    case("ris_resets_everything", 10, 3, b"\x1b[31mabc\x1b[?25l\x1b[2;3r\x1b[?1049h\x1bcX"),
    // xterm's RIS keeps the alternate buffer and its DECSC slot; aterm's does
    // not (the `ris_keeps_*` exclusions).
    case("ris_keeps_the_alternate_buffer", 10, 3, b"\x1b[?47hX\x1b[?47l\x1bc\x1b[?47h"),
    case("ris_keeps_the_alternate_slot", 10, 3, b"\x1b[?1049h\x1b[2;5H\x1b7\x1b[?1049l\x1bc\x1b[?1049h\x1b8Z"),
    // --- sequences both consume --------------------------------------------
    // astream-term screen_fold: OSC (BEL and ST), OSC 133 marks on the main
    // screen and a DCS payload paint nothing. (Its fixture's DCS is a sixel,
    // which aterm draws: `sixel_dcs_is_an_image`.)
    case(
        "string_sequences_paint_nothing",
        40,
        6,
        b"\x1b]0;user@host: ~\x07$ ls\r\n\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]2;st title\x1b\\a\x1bP$qm\x1b\\b",
    ),
    case("sixel_dcs_is_an_image", 40, 4, b"a\x1bPq#0;2;0;0;0#0~~\x1b\\b"),
    case("charsets_and_keypad_modes", 20, 3, b"\x1b(B\x1b[mok\x1b)0\x1b=\x1b>!"),
    case("esc_restarts_an_unfinished_csi", 20, 3, b"junk\x1b[3\x1b[2Jhi"),
    case("c0_executes_inside_a_csi", 20, 3, b"ab\x1b[2\r\nCZ"),
    case("decscusr_xtmodkeys_decrqss", 20, 2, b"a\x1b[2 qb\x1b[>4;2mc\x1bP$qm\x1b\\d"),
    case("pm_and_sos", 20, 2, b"a\x1b^private\x1b\\b\x1bXsos\x1b\\c"),
    // --- UTF-8 -------------------------------------------------------------
    case("utf8_narrow_glyphs", 12, 2, "caf\u{e9} \u{2500} \u{f1}\u{df}".as_bytes()),
    case("utf8_c1_controls_are_ignored", 10, 2, b"a\xc2\x9b31mb"),
    case("utf8_truncated_sequences", 10, 2, b"\xe2\x82A\xf0\x9f\x98x\xc2y"),
    case("utf8_invalid_leads", 10, 2, b"\x80a\xf5b\xffc"),
    // One U+FFFD per maximal subpart (Unicode 3.9).
    case("utf8_overlong_and_surrogate", 16, 2, b"\xc0\x80|\xe0\x80\x8a|\xed\xa0\x80|\xf4\x90\x80\x80|\xc1\xbf"),
    // --- astream-term's adversarial session, and the cut oracle's fixtures ---
    case(
        "astream_adversarial_session",
        20,
        6,
        b"\x1b[2J\x1b[1;1HHELLO\x1b[31mRED\x1b[0m\r\nline2 \x1b[1mbold\x1b[22m end\x1b[?1049h\x1b[2JALT\x1b[?1049l\x1b[5;3Hmoved",
    ),
    case("oracle_fold_1", 80, 24, b"\x1b[2Jhello world\r\nsecond line"),
    case("oracle_fold_2", 80, 24, b"line1\r\nline2\r\nline3\x1b[1;1Htop"),
    case("oracle_fold_3", 80, 24, b"\x1b[2Jabc\x1b[2;5Hxyz"),
    case("oracle_fold_4", 80, 24, b"a\x1b[31mRED\x1b[0m b\x1b[32mGRN\x1b[0m c"),
    case("oracle_fold_5", 80, 24, b"prompt$ make build\r\nBUILD FAILED: 2 errors\r\n$ "),
    case("oracle_fold_6", 80, 24, b"x\x1b[3Cy\x1b[2Bz"),
    case("oracle_fold_7", 80, 24, b"erase\x1b[1;1H\x1b[Knew"),
];
