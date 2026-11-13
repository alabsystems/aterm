// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

fn row_glyphs(term: &Terminal, row: u16) -> String {
    let grid = term.grid();
    (0..grid.cols())
        .map(|col| grid.cell(row, col).map_or(' ', |c| c.char()))
        .collect()
}

fn cursor(term: &Terminal) -> (u16, u16) {
    let c = term.cursor();
    (c.row, c.col)
}

fn glyph(term: &Terminal, row: u16, col: u16) -> char {
    term.grid().cell(row, col).map_or(' ', |c| c.char())
}

fn row_is_blank(term: &Terminal, row: u16) -> bool {
    row_glyphs(term, row).trim().is_empty()
}

#[test]
fn el0_at_pending_wrap_ghostty_golden() {
    let mut term = Terminal::new(1, 5);
    term.process(b"ABCDE\x1b[KB");
    assert_eq!(row_glyphs(&term, 0), "ABCDB");
    assert_eq!(cursor(&term), (0, 4));
    assert!(term.grid().pending_wrap(), "the B re-arms the wrap");
}

#[test]
fn el0_at_pending_wrap_with_decawm_off() {
    let mut term = Terminal::new(1, 5);
    term.process(b"\x1b[?7lABCDE\x1b[KB");
    assert_eq!(row_glyphs(&term, 0), "ABCDB");
    assert_eq!(cursor(&term), (0, 4));
}

#[test]
fn vt52_esc_k_at_pending_wrap_clears_parked_glyph() {
    let mut term = Terminal::new(2, 5);
    term.process(b"\x1b[?2lABCDE");
    assert!(
        term.grid().pending_wrap(),
        "precondition: VT52 print armed the wrap"
    );
    term.process(b"\x1bK");
    assert_eq!(row_glyphs(&term, 0), "ABCD ");
    assert!(!term.grid().pending_wrap(), "ESC K resets the wrap");
}

#[test]
fn vt52_esc_j_at_pending_wrap_clears_parked_glyph() {
    let mut term = Terminal::new(2, 5);
    term.process(b"\x1b[?2l\x1bY! xyz\x1bH");
    term.process(b"ABCDE");
    assert!(
        term.grid().pending_wrap(),
        "precondition: VT52 print armed the wrap"
    );
    term.process(b"\x1bJ");
    assert_eq!(row_glyphs(&term, 0), "ABCD ");
    assert!(row_is_blank(&term, 1), "ESC J clears the rows below");
    assert!(!term.grid().pending_wrap(), "ESC J resets the wrap");
}

#[test]
fn ed_at_pending_wrap_then_print_overwrites_last_column() {
    for ed in [b"\x1b[J".as_slice(), b"\x1b[1J", b"\x1b[2J"] {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[1;80H!");
        term.process(ed);
        term.process(b"?");
        let label = String::from_utf8_lossy(ed);
        assert_eq!(glyph(&term, 0, 79), '?', "{label}");
        assert!(row_is_blank(&term, 1), "{label}: nothing wrapped to row 1");
        assert_eq!(cursor(&term), (0, 79), "{label}");
    }
}

#[test]
fn ed0_without_a_print_clears_the_parked_glyph() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[1;80H!\x1b[0J");
    assert_eq!(glyph(&term, 0, 79), ' ');
    assert_eq!(cursor(&term), (0, 79));
    assert!(!term.grid().pending_wrap());
}

#[test]
fn ed1_at_bottom_right_pending_wrap_reduces_to_full_clear() {
    let mut term = Terminal::new(24, 80);
    term.process(b"top\x1b[24;80H!\x1b[1J?");
    for row in 0..23 {
        assert!(row_is_blank(&term, row), "row {row} cleared");
    }
    assert_eq!(row_glyphs(&term, 23).trim(), "?");
    assert_eq!(glyph(&term, 23, 79), '?');
    assert_eq!(cursor(&term), (23, 79));
}

#[test]
fn el1_and_el2_at_pending_wrap_then_print_overwrite_the_last_column() {
    for el in ["1K", "2K"] {
        let mut term = Terminal::new(2, 5);
        term.process(format!("ABCDE\x1b[{el}B").as_bytes());
        assert_eq!(row_glyphs(&term, 0), "    B", "EL {el}");
        assert_eq!(cursor(&term), (0, 4), "EL {el}");
        assert!(row_is_blank(&term, 1), "EL {el}");
    }
}

#[test]
fn el1_at_pending_wrap_four_columns_is_xterms_result_not_xterm_js() {
    let mut term = Terminal::new(2, 4);
    term.process(b"ABCD\x1b[1KZ");
    assert_eq!(row_glyphs(&term, 0), "   Z");
    assert!(row_is_blank(&term, 1));
}

#[test]
fn decsel0_protected_parked_glyph_survives_but_wrap_resets() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[1\"q\x1b[1;80H!\x1b[0\"q\x1b[?K");
    assert_eq!(
        glyph(&term, 0, 79),
        '!',
        "DECSCA-protected glyph survives DECSEL"
    );
    assert!(
        !term.grid().pending_wrap(),
        "ClearRight's ResetWrap still runs"
    );
    term.process(b"?");
    assert_eq!(
        glyph(&term, 0, 79),
        '?',
        "the print overwrites the parked column"
    );
    assert!(row_is_blank(&term, 1), "nothing wrapped to row 1");
}

#[test]
fn alt_1047_exit_clears_the_wrap_it_hands_back() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?1047h\x1b[1;80H!\x1b[?1047l?");
    assert!(!term.is_alternate_screen(), "back on the main screen");
    assert_eq!(glyph(&term, 0, 79), '?');
    assert!(row_is_blank(&term, 1), "nothing wrapped to row 1");
}

#[test]
fn alt_1049_enter_resets_wrap_and_exit_restores_it() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[1;80H!\x1b[?1049h?");
    assert!(term.is_alternate_screen());
    assert_eq!(
        glyph(&term, 0, 79),
        '?',
        "alt screen: the print overwrites col 79"
    );
    assert_eq!(cursor(&term), (0, 79));
    assert!(row_is_blank(&term, 1), "alt screen: nothing wrapped");
    term.process(b"\x1b[?1049lZ");
    assert!(!term.is_alternate_screen());
    assert_eq!(
        glyph(&term, 0, 79),
        '!',
        "main screen keeps its parked glyph"
    );
    assert_eq!(
        glyph(&term, 1, 0),
        'Z',
        "the restored wrap takes the next glyph"
    );
}

/// CHA counts from the left margin under DECOM and DECLRMM and stops at the
/// right one, as HPA does (xterm parses both to `CASE_HPA`).
#[test]
fn cha_counts_from_the_left_margin_under_decom_like_hpa() {
    for final_byte in ["G", "`"] {
        let mut term = Terminal::new(3, 20);
        // Margins at columns 5..=10 (1-based), origin mode on: home is (0,4).
        term.process(b"\x1b[?69h\x1b[5;10s\x1b[?6h");
        term.process(format!("\x1b[3{final_byte}").as_bytes());
        assert_eq!(
            cursor(&term),
            (0, 6),
            "CSI 3 {final_byte}: the margin's third column"
        );
        term.process(format!("\x1b[99{final_byte}").as_bytes());
        assert_eq!(
            cursor(&term),
            (0, 9),
            "CSI 99 {final_byte}: stopped at the right margin"
        );
        // Without origin mode both are absolute.
        term.process(format!("\x1b[?6l\x1b[3{final_byte}").as_bytes());
        assert_eq!(cursor(&term), (0, 2), "CSI 3 {final_byte} without DECOM");
    }
}

/// xterm's `CursorRestoreFlags` ends `do_wrap = sc->wrap_flag` and Ghostty's `restoreCursor`
/// restores `pending_wrap`: a saved wrap comes back wherever the column lands.
#[test]
fn decrc_puts_a_saved_wrap_back_wherever_the_column_lands() {
    for (bytes, row1, what) in [
        (
            &b"\x1b[?69h\x1b[1;4sabcd\x1b7\x1b[?69l\x1b8X"[..],
            "X         ",
            "saved on a right margin a DECLRMM reset moved aside",
        ),
        (
            b"\x1b[?69h\x1b[1;4sabcd\x1b[?69l\x1b7\x1b8X",
            "X         ",
            "left mid-line by a DECLRMM reset, then saved",
        ),
        (
            b"\x1b[?69h\x1b[;2sab\x1b7\x1b[2;0s\x1b8X",
            " X        ",
            "saved before a DECSLRM moved the margins",
        ),
    ] {
        let mut term = Terminal::new(3, 10);
        term.process(bytes);
        assert_eq!(row_glyphs(&term, 1), row1, "{what}");
    }
}

/// A resize changes neither the slot's pending wrap nor, across a width round trip, its cell
/// (SPECS `saved-cursor-follows-content`): back at the width it was saved at, DECRC puts the
/// wrap back and the next glyph continues the line on the next row. A reflow that leaves the
/// wrap mid-line sends the next glyph to the cell after the saved one, on the same row
/// (Ghostty `Screen.resize`), not over the saved glyph and not onto the next row.
#[test]
fn a_saved_wrap_survives_a_width_round_trip_and_a_widening_takes_the_next_column() {
    let mut term = Terminal::new(3, 10);
    term.process(b"abcdefghij\x1b7");
    term.resize(3, 6);
    term.resize(3, 10);
    term.process(b"\x1b[3;1H\x1b8X");
    assert_eq!(row_glyphs(&term, 0), "abcdefghij");
    assert_eq!(row_glyphs(&term, 1), "X         ");
    assert_eq!(
        term.grid().row_is_wrapped(1),
        Some(true),
        "X continues the line"
    );
    let mut term = Terminal::new(3, 5);
    term.process(b"abcde\x1b7");
    term.resize(3, 10);
    term.process(b"\x1b[3;1H\x1b8X");
    assert_eq!(
        row_glyphs(&term, 0),
        "abcdeX    ",
        "the e stays and X follows it"
    );
    assert!(row_is_blank(&term, 1), "nothing wrapped to row 1");
    assert_eq!(term.grid().row_is_wrapped(1), Some(false));
}

/// Any change of size marks the slot, a rows-only one included, since it resets the DECSLRM
/// margins (Ghostty's `Screen.resize` checks a saved wrap whatever changed). A same-size
/// resize marks nothing, so DECRC still puts back, as xterm does, a wrap that a DECLRMM reset
/// left mid-line.
#[test]
fn only_a_resize_that_changes_the_size_moves_a_mid_line_saved_wrap() {
    for (rows, row0, row1, what) in [
        (
            3,
            "abcd      ",
            "X         ",
            "same size: the wrap comes back",
        ),
        (4, "abcdX     ", "          ", "rows only: the next column"),
    ] {
        let mut term = Terminal::new(3, 10);
        term.process(b"\x1b[?69h\x1b[1;4sabcd\x1b7\x1b[?69l");
        term.resize(rows, 10);
        term.process(b"\x1b8X");
        assert_eq!(row_glyphs(&term, 0), row0, "{what}");
        assert_eq!(row_glyphs(&term, 1), row1, "{what}");
    }
}

/// `?1049l` restores the main slot as DECRC does: after a widening under the alternate
/// screen left the saved wrap mid-line, the shell's prompt follows the output on its row.
#[test]
fn a_1049l_after_a_widening_puts_the_prompt_after_the_output() {
    let mut term = Terminal::new(4, 5);
    term.process(b"abcde\x1b[?1049h");
    term.resize(4, 10);
    term.process(b"\x1b[?1049l$ ");
    assert_eq!(row_glyphs(&term, 0), "abcde$    ");
    assert_eq!(cursor(&term), (0, 7));
    assert!(!term.grid().pending_wrap());
}

#[test]
fn decrc_restores_the_deferred_wrap_decsc_saved() {
    let mut term = Terminal::new(2, 5);
    term.process(b"ABCDE\x1b7\rx\x1b8Z");
    assert_eq!(row_glyphs(&term, 0), "xBCDE", "the parked glyph stays");
    assert_eq!(row_glyphs(&term, 1), "Z    ", "the restored wrap takes Z");
    assert_eq!(cursor(&term), (1, 1));
}

#[test]
fn a_second_1049_enter_clears_the_alt_screen_it_finds() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[5;10H\x1b[?1049h");
    assert!(term.is_alternate_screen());
    term.process(b"\x1b[1;1HFIRST APP");
    assert_eq!(
        glyph(&term, 0, 0),
        'F',
        "the first app painted the alt screen"
    );

    term.process(b"\x1b[?1049h");
    assert!(term.is_alternate_screen(), "still on the alt screen");
    assert!(
        row_is_blank(&term, 0),
        "a second 1049 SET runs ClearScreen — the previous app's row must be gone"
    );

    term.process(b"\x1b[?1049l");
    assert!(!term.is_alternate_screen(), "back on the main screen");
    assert_eq!(
        cursor(&term),
        (4, 9),
        "the main cursor came from the FIRST save, not the second"
    );
}

#[test]
fn alt_47_enter_carries_the_wrap_over() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[1;80H!\x1b[?47hZ");
    assert!(term.is_alternate_screen());
    assert_eq!(glyph(&term, 1, 0), 'Z', "the carried wrap moves Z to row 1");
}
