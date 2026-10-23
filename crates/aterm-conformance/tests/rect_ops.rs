// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_conformance::Screen;
use aterm_core::grid::cell_flags::CellFlags;

/// True if the resolved cell at (r, c) carries every flag in `want`.
fn has_flags(s: &Screen, r: u16, c: u16, want: CellFlags) -> bool {
    s.cell_flags_bits(r, c) & want.bits() == want.bits()
}

/// DECALN (ESC # 8): fill the whole screen with 'E'.
fn decaln(s: &mut Screen) {
    s.feed(b"\x1b#8");
}

#[test]
fn decfra_fills_exact_rect_and_leaves_outside_untouched() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[42;2;3;4;6$x");
    assert_eq!(s.row(0), "EEEEEEEEEE", "row above the rect untouched");
    assert_eq!(s.row(1), "EE****EEEE", "top rect row: cols 3-6 filled");
    assert_eq!(s.row(2), "EE****EEEE", "middle rect row filled");
    assert_eq!(s.row(3), "EE****EEEE", "bottom rect row: inclusive Pb");
    assert_eq!(s.row(4), "EEEEEEEEEE", "row below the rect untouched");
    assert_eq!(s.row(5), "EEEEEEEEEE", "last row untouched");
}

#[test]
fn decfra_default_coords_fill_full_screen() {
    let mut s = Screen::new(4, 6);
    s.feed(b"\x1b[65$x");
    for r in 0..4 {
        assert_eq!(s.row(r), "AAAAAA", "row {r} fully filled by default rect");
    }
}

#[test]
fn decfra_rejects_non_printable_fill_char() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[7;1;1;2;2$x");
    s.feed(b"\x1b[27;1;1;2;2$x");
    s.feed(b"\x1b[127;1;1;2;2$x");
    s.feed(b"\x1b[130;1;1;2;2$x");
    for r in 0..6 {
        assert_eq!(
            s.row(r),
            "EEEEEEEEEE",
            "row {r} unchanged after invalid Pch"
        );
    }
}

#[test]
fn decfra_fill_uses_current_sgr_rendition() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[1;4m\x1b[88;2;2;3;4$x");
    assert_eq!(s.row(1), "EXXXEEEEEE");
    assert_eq!(s.row(2), "EXXXEEEEEE");
    let want = CellFlags::BOLD.union(CellFlags::UNDERLINE);
    for r in 1..=2 {
        for c in 1..=3 {
            assert!(
                has_flags(&s, r, c, want),
                "({r},{c}) must be bold+underline"
            );
        }
    }
    assert_eq!(
        s.cell_flags_bits(0, 1),
        0,
        "row above rect keeps plain attrs"
    );
    assert_eq!(
        s.cell_flags_bits(1, 0),
        0,
        "col left of rect keeps plain attrs"
    );
    assert_eq!(
        s.cell_flags_bits(1, 4),
        0,
        "col right of rect keeps plain attrs"
    );
}

#[test]
fn decfra_fill_carries_current_decsca_protection() {
    let mut s = Screen::new(4, 10);
    decaln(&mut s);
    s.feed(b"\x1b[1\"q\x1b[88;1;1;1;3$x\x1b[0\"q\x1b[88;1;5;1;6$x");
    assert_eq!(s.row(0), "XXXEXXEEEE");
    s.feed(b"\x1b[${");
    assert_eq!(s.row(0), "XXX", "protected DECFRA fill survives DECSERA");
}

#[test]
fn decera_erases_rect_to_spaces_outside_untouched() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2;3;4;6$z");
    assert_eq!(s.row(0), "EEEEEEEEEE");
    assert_eq!(s.row(1), "EE    EEEE");
    assert_eq!(s.row(2), "EE    EEEE");
    assert_eq!(s.row(3), "EE    EEEE");
    assert_eq!(s.row(4), "EEEEEEEEEE");
    assert_eq!(s.row(5), "EEEEEEEEEE");
}

#[test]
fn decera_erases_visual_attributes() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1H\x1b[1;4mABCD\x1b[0m"); // bold+underline ABCD
    let want = CellFlags::BOLD.union(CellFlags::UNDERLINE);
    assert!(has_flags(&s, 0, 0, want), "precondition: text is styled");
    s.feed(b"\x1b[1;1;1;4$z");
    assert_eq!(s.row(0), "");
    for c in 0..4 {
        assert_eq!(s.cell_flags_bits(0, c), 0, "(0,{c}) attrs erased by DECERA");
    }
}

#[test]
fn decera_ignores_decsca_protection() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1\"qAB\x1b[0\"qCD");
    assert_eq!(s.row(0), "ABCD");
    s.feed(b"\x1b[$z"); // defaults: full screen
    assert_eq!(
        s.screen(),
        "",
        "DECERA erases protected and unprotected alike"
    );
}

#[test]
fn decera_default_params_erase_full_screen() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[$z");
    assert_eq!(s.screen(), "");
}

#[test]
fn decsera_preserves_decsca_protected_cells() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1HA\x1b[1\"qB\x1b[0\"qC");
    assert_eq!(s.row(0), "ABC");
    s.feed(b"\x1b[1;1;1;10${");
    assert_eq!(
        s.row(0),
        " B",
        "protected 'B' survives, A/C erased to spaces"
    );
}

#[test]
fn decsera_erases_only_inside_rect() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2;3;4;6${");
    assert_eq!(s.row(0), "EEEEEEEEEE");
    assert_eq!(s.row(1), "EE    EEEE");
    assert_eq!(s.row(2), "EE    EEEE");
    assert_eq!(s.row(3), "EE    EEEE");
    assert_eq!(s.row(4), "EEEEEEEEEE");
    assert_eq!(s.row(5), "EEEEEEEEEE");
}

#[test]
fn decsera_preserves_visual_attributes_of_erased_positions() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1H\x1b[1mABCD\x1b[0m"); // bold ABCD, unprotected
    assert!(
        has_flags(&s, 0, 0, CellFlags::BOLD),
        "precondition: bold text"
    );
    s.feed(b"\x1b[1;1;1;4${");
    assert_eq!(s.row(0), "", "characters erased to spaces");
    for c in 0..4 {
        assert!(
            has_flags(&s, 0, c, CellFlags::BOLD),
            "(0,{c}) SGR attrs must survive DECSERA"
        );
    }
}

#[test]
fn deccra_copies_rect_and_source_remains() {
    let mut s = Screen::new(6, 10);
    s.feed(b"\x1b[1;1HABCDEF\x1b[2;1HGHIJKL");
    s.feed(b"\x1b[1;1;2;3;1;4;5;1$v");
    assert_eq!(s.row(0), "ABCDEF", "source row 1 unchanged");
    assert_eq!(s.row(1), "GHIJKL", "source row 2 unchanged");
    assert_eq!(s.row(3), "    ABC", "destination row 4 cols 5-7 = copy");
    assert_eq!(s.row(4), "    GHI", "destination row 5 cols 5-7 = copy");
    assert_eq!(s.row(2), "", "rows outside source/destination untouched");
    assert_eq!(s.row(5), "");
}

#[test]
fn deccra_overlapping_copy_behaves_as_if_buffered() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1HABCDEF");
    s.feed(b"\x1b[1;1;1;4;1;1;3;1$v");
    assert_eq!(s.row(0), "ABABCD", "overlap copied from pre-copy source");
}

#[test]
fn deccra_copies_character_attributes() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1H\x1b[1mAB\x1b[0m"); // bold "AB"
    s.feed(b"\x1b[1;1;1;2;1;3;1;1$v"); // copy to (3;1)
    assert_eq!(s.row(2), "AB");
    assert!(
        has_flags(&s, 2, 0, CellFlags::BOLD),
        "copied cell keeps bold"
    );
    assert!(
        has_flags(&s, 2, 1, CellFlags::BOLD),
        "copied cell keeps bold"
    );
}

#[test]
fn deccra_destination_clipped_at_page_edge() {
    let mut s = Screen::new(6, 10);
    s.feed(b"\x1b[1;1HWXYZ");
    s.feed(b"\x1b[1;1;1;4;1;6;9;1$v");
    assert_eq!(s.row(5), "        WX", "clipped copy: only 'WX' fits");
    assert_eq!(s.row(0), "WXYZ", "source unchanged");
    for r in 1..5 {
        assert_eq!(s.row(r), "", "no wraparound spill into row {r}");
    }
}

#[test]
fn deccara_single_row_applies_attrs_and_keeps_characters() {
    let mut s = Screen::new(6, 10);
    s.feed(b"\x1b[2;1HABCDEFGHIJ");
    s.feed(b"\x1b[2;3;2;6;1;4$r");
    assert_eq!(s.row(1), "ABCDEFGHIJ", "characters unchanged");
    let want = CellFlags::BOLD.union(CellFlags::UNDERLINE);
    assert!(!has_flags(&s, 1, 1, want), "col 2 (left of Pl) unchanged");
    for c in 2..=5 {
        assert!(has_flags(&s, 1, c, want), "(1,{c}) inside rect styled");
    }
    assert!(!has_flags(&s, 1, 6, want), "col 7 (right of Pr) unchanged");
    assert!(!has_flags(&s, 0, 3, want), "row above unchanged");
    assert!(!has_flags(&s, 2, 3, want), "row below unchanged");
}

#[test]
fn deccara_rect_extent_decsace2_changes_exact_rectangle() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2*x\x1b[2;3;4;6;1$r");
    for r in 1..=3 {
        assert!(
            !has_flags(&s, r, 0, CellFlags::BOLD),
            "({r},0) outside rect"
        );
        assert!(
            !has_flags(&s, r, 1, CellFlags::BOLD),
            "({r},1) outside rect"
        );
        for c in 2..=5 {
            assert!(
                has_flags(&s, r, c, CellFlags::BOLD),
                "({r},{c}) inside rect"
            );
        }
        assert!(
            !has_flags(&s, r, 6, CellFlags::BOLD),
            "({r},6) outside rect"
        );
    }
    for c in 0..10 {
        assert!(!has_flags(&s, 0, c, CellFlags::BOLD), "row 1 above rect");
        assert!(!has_flags(&s, 4, c, CellFlags::BOLD), "row 5 below rect");
    }
}

#[test]
fn deccara_stream_extent_decsace1_runs_streamwise() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[1*x\x1b[2;3;4;6;1$r");
    assert!(!has_flags(&s, 1, 0, CellFlags::BOLD));
    assert!(!has_flags(&s, 1, 1, CellFlags::BOLD));
    for c in 2..10 {
        assert!(
            has_flags(&s, 1, c, CellFlags::BOLD),
            "(1,{c}) first stream row"
        );
    }
    for c in 0..10 {
        assert!(
            has_flags(&s, 2, c, CellFlags::BOLD),
            "(2,{c}) middle stream row"
        );
    }
    for c in 0..=5 {
        assert!(
            has_flags(&s, 3, c, CellFlags::BOLD),
            "(3,{c}) last stream row"
        );
    }
    for c in 6..10 {
        assert!(!has_flags(&s, 3, c, CellFlags::BOLD), "(3,{c}) past Pr");
    }
}

#[test]
fn deccara_default_extent_is_stream() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2;3;4;6;1$r");
    assert!(
        has_flags(&s, 2, 0, CellFlags::BOLD),
        "(3;1) middle stream row col 1 must be bold with default extent"
    );
    assert!(
        has_flags(&s, 1, 8, CellFlags::BOLD),
        "(2;9) first stream row runs to line end with default extent"
    );
}

#[test]
fn deccara_sgr0_clears_attrs_but_not_protection() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1H\x1b[1m\x1b[1\"qAB\x1b[0\"q\x1b[0m");
    assert!(has_flags(&s, 0, 0, CellFlags::BOLD), "precondition: bold");
    s.feed(b"\x1b[1;1;1;2;0$r");
    assert!(
        !has_flags(&s, 0, 0, CellFlags::BOLD),
        "bold cleared by Ps=0"
    );
    assert!(
        !has_flags(&s, 0, 1, CellFlags::BOLD),
        "bold cleared by Ps=0"
    );
    assert_eq!(s.row(0), "AB", "DECCARA does not change characters");
    s.feed(b"\x1b[${");
    assert_eq!(s.row(0), "AB", "DECSCA protection survives DECCARA 0");
}

#[test]
fn decrqss_decsace_roundtrips_current_extent() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[2*x\x1bP$q*x\x1b\\");
    assert_eq!(s.response_string(), "\x1bP1$r2*x\x1b\\");
    s.feed(b"\x1b[1*x\x1bP$q*x\x1b\\");
    assert_eq!(s.response_string(), "\x1bP1$r1*x\x1b\\");
}

/// Write `text` at row `r` (1-based), column 1, wrapped in `sgr` … SGR 0.
fn styled_row(s: &mut Screen, r: u16, sgr: &str, text: &str) {
    s.feed(format!("\x1b[{r};1H\x1b[{sgr}m{text}\x1b[0m").as_bytes());
}

#[test]
fn decrara_turns_a_named_attribute_on_where_absent_and_off_where_present() {
    let mut s = Screen::new(4, 6);
    styled_row(&mut s, 1, "7", "AAA"); // (0,0..2) already INVERSE
    s.feed(b"\x1b[2;1HBBB"); // (1,0..2) plain
    s.feed(b"\x1b[1;1;2;3;7$t");
    for c in 0..3 {
        assert!(
            !has_flags(&s, 0, c, CellFlags::INVERSE),
            "(0,{c}) had SGR 7 and must lose it"
        );
        assert!(
            has_flags(&s, 1, c, CellFlags::INVERSE),
            "(1,{c}) lacked SGR 7 and must gain it"
        );
    }
    assert_eq!(s.row(0), "AAA", "DECRARA does not change the characters");
    assert_eq!(s.row(1), "BBB", "DECRARA does not change the characters");
}

#[test]
fn decrara_applied_twice_restores_the_original_attributes() {
    let mut s = Screen::new(4, 6);
    styled_row(&mut s, 1, "1", "AAAAAA"); // BOLD across the row
    s.feed(b"\x1b[1;2;1;4;1$t");
    assert!(has_flags(&s, 0, 0, CellFlags::BOLD), "outside the rect");
    assert!(!has_flags(&s, 0, 1, CellFlags::BOLD), "reversed once: off");
    s.feed(b"\x1b[1;2;1;4;1$t");
    for c in 0..6 {
        assert!(
            has_flags(&s, 0, c, CellFlags::BOLD),
            "(0,{c}) restored by the second reversal"
        );
    }
}

#[test]
fn decrara_leaves_every_cell_outside_the_rectangle_alone() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2*x\x1b[2;3;4;6;7$t"); // rect extent, rows 2-4 x cols 3-6
    for r in 1..=3 {
        for c in 0..2 {
            assert!(!has_flags(&s, r, c, CellFlags::INVERSE), "({r},{c}) left");
        }
        for c in 2..=5 {
            assert!(has_flags(&s, r, c, CellFlags::INVERSE), "({r},{c}) inside");
        }
        for c in 6..10 {
            assert!(!has_flags(&s, r, c, CellFlags::INVERSE), "({r},{c}) right");
        }
    }
    for c in 0..10 {
        assert!(!has_flags(&s, 0, c, CellFlags::INVERSE), "row above");
        assert!(!has_flags(&s, 4, c, CellFlags::INVERSE), "row below");
    }
    assert_eq!(s.row(2), "EEEEEEEEEE", "characters survive the reversal");
}

#[test]
fn decrara_default_pm_reverses_modes_1_4_5_7() {
    let mut s = Screen::new(3, 6);
    styled_row(&mut s, 1, "8", "AAAAAA"); // HIDDEN, nothing else
    s.feed(b"\x1b[1;1;1;6$t");
    let want = CellFlags::BOLD
        .union(CellFlags::UNDERLINE)
        .union(CellFlags::BLINK)
        .union(CellFlags::INVERSE);
    for c in 0..6 {
        assert!(has_flags(&s, 0, c, want), "(0,{c}) gains 1, 4, 5 and 7");
        assert!(
            has_flags(&s, 0, c, CellFlags::HIDDEN),
            "(0,{c}) keeps SGR 8: reversing 0 does not name it"
        );
    }
}

#[test]
fn decrara_reversing_mode_8_is_the_documented_xterm_extension() {
    let mut s = Screen::new(3, 6);
    styled_row(&mut s, 1, "8", "AAA"); // HIDDEN
    s.feed(b"\x1b[2;1HBBB"); // plain
    s.feed(b"\x1b[1;1;2;3;8$t");
    for c in 0..3 {
        assert!(!has_flags(&s, 0, c, CellFlags::HIDDEN), "(0,{c}) loses it");
        assert!(has_flags(&s, 1, c, CellFlags::HIDDEN), "(1,{c}) gains it");
    }
}

#[test]
fn decrara_reversing_mode_4_never_produces_an_underline_style_sgr_4_cannot() {
    let mut s = Screen::new(4, 6);
    styled_row(&mut s, 1, "4:3", "AAA"); // curly
    s.feed(b"\x1b[2;1HBBB"); // no underline
    s.feed(b"\x1b[1;1;2;3;4$t");
    for c in 0..3 {
        assert!(
            !has_flags(&s, 0, c, CellFlags::CURLY_UNDERLINE),
            "(0,{c}) curly underline removed, not restyled"
        );
        assert!(
            !has_flags(&s, 0, c, CellFlags::UNDERLINE),
            "(0,{c}) must not become the dotted (UNDERLINE|CURLY) encoding"
        );
        assert!(
            has_flags(&s, 1, c, CellFlags::UNDERLINE),
            "(1,{c}) bare cell gains a single underline"
        );
        assert!(
            !has_flags(&s, 1, c, CellFlags::CURLY_UNDERLINE),
            "(1,{c}) gains SGR 4, not a styled underline"
        );
    }
}

#[test]
fn decrara_ignores_sgr_codes_the_pm_list_does_not_name() {
    let mut s = Screen::new(3, 6);
    styled_row(&mut s, 1, "1", "AAAAAA");
    s.feed(b"\x1b[1;1;1;6;22$t");
    for c in 0..6 {
        assert!(
            has_flags(&s, 0, c, CellFlags::BOLD),
            "(0,{c}) unnamed code 22 must not clear bold"
        );
    }
    assert_eq!(s.row(0), "AAAAAA");
}

#[test]
fn decrara_rect_extent_decsace2_reverses_the_exact_rectangle() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2*x\x1b[2;3;4;6;1$t");
    for r in 1..=3 {
        assert!(!has_flags(&s, r, 1, CellFlags::BOLD), "({r},1) outside");
        for c in 2..=5 {
            assert!(has_flags(&s, r, c, CellFlags::BOLD), "({r},{c}) inside");
        }
        assert!(!has_flags(&s, r, 6, CellFlags::BOLD), "({r},6) outside");
    }
}

#[test]
fn decrara_stream_extent_decsace1_runs_streamwise() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[1*x\x1b[2;3;4;6;1$t");
    assert!(!has_flags(&s, 1, 1, CellFlags::BOLD), "(1,1) before Pl");
    for c in 2..10 {
        assert!(has_flags(&s, 1, c, CellFlags::BOLD), "(1,{c}) first row");
    }
    for c in 0..10 {
        assert!(has_flags(&s, 2, c, CellFlags::BOLD), "(2,{c}) middle row");
    }
    for c in 0..=5 {
        assert!(has_flags(&s, 3, c, CellFlags::BOLD), "(3,{c}) last row");
    }
    for c in 6..10 {
        assert!(!has_flags(&s, 3, c, CellFlags::BOLD), "(3,{c}) past Pr");
    }
}

#[test]
fn decrara_default_extent_is_stream_like_deccara() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[2;3;4;6;1$t");
    assert!(
        has_flags(&s, 2, 0, CellFlags::BOLD),
        "middle stream row runs full width by default"
    );
    assert!(
        has_flags(&s, 1, 8, CellFlags::BOLD),
        "first stream row runs to line end by default"
    );
}

#[test]
fn decrara_does_not_change_dec_sca_protection() {
    let mut s = Screen::new(4, 10);
    s.feed(b"\x1b[1;1H\x1b[1\"qAB\x1b[0\"q");
    s.feed(b"\x1b[1;1;1;2;0$t");
    s.feed(b"\x1b[${");
    assert_eq!(s.row(0), "AB", "DECSCA protection survives DECRARA");
}

#[test]
fn rect_coords_are_relative_to_margins_when_decom_set() {
    let mut s = Screen::new(10, 10);
    decaln(&mut s);
    s.feed(b"\x1b[3;8r\x1b[?6h");
    s.feed(b"\x1b[1;1;1;5$z");
    assert_eq!(
        s.row(1),
        "EEEEEEEEEE",
        "absolute row 2 (above margin) intact"
    );
    assert_eq!(
        s.row(2),
        "     EEEEE",
        "region row 1 = absolute row 3 erased"
    );
    assert_eq!(s.row(3), "EEEEEEEEEE", "Pb=1 inclusive: next row intact");
    s.feed(b"\x1b[1;1;99;2$z");
    assert_eq!(
        s.row(7),
        "  EEEEEEEE",
        "absolute row 8 (bottom margin) erased"
    );
    assert_eq!(s.row(8), "EEEEEEEEEE", "absolute row 9 below margin intact");
}

#[test]
fn rect_coords_are_page_absolute_when_decom_reset() {
    let mut s = Screen::new(10, 10);
    decaln(&mut s);
    s.feed(b"\x1b[5;8r"); // margins set, but DECOM is reset
    s.feed(b"\x1b[1;1;1;4$z");
    assert_eq!(
        s.row(0),
        "    EEEEEE",
        "row 1 is absolute row 1, not margin top"
    );
    assert_eq!(s.row(4), "EEEEEEEEEE", "margin-top row untouched");
}

#[test]
fn reversed_coords_are_ignored() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[4;3;2;6$z"); // DECERA, Pb < Pt
    s.feed(b"\x1b[2;6;4;3$z"); // DECERA, Pr < Pl
    s.feed(b"\x1b[88;4;3;2;6$x"); // DECFRA, Pb < Pt
    s.feed(b"\x1b[4;3;2;6${"); // DECSERA, Pb < Pt
    s.feed(b"\x1b[2*x\x1b[4;3;2;6;1$r"); // DECCARA (rect extent), Pb < Pt
    s.feed(b"\x1b[4;1;2;3;1;1;1;1$v"); // DECCRA, source Pb < Pt
    s.feed(b"\x1b[2*x\x1b[4;3;2;6;1$t"); // DECRARA (rect extent), Pb < Pt
    s.feed(b"\x1b[2*x\x1b[2;6;4;3;1$t"); // DECRARA (rect extent), Pr < Pl
    for r in 0..6 {
        assert_eq!(
            s.row(r),
            "EEEEEEEEEE",
            "row {r} untouched by inverted rects"
        );
    }
    assert!(
        !has_flags(&s, 2, 3, CellFlags::BOLD),
        "no attrs from inverted rect"
    );
}

#[test]
fn zero_params_take_defaults() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[0;0;0;0$z");
    assert_eq!(s.screen(), "", "0;0;0;0 = full-page defaults");
}

#[test]
fn rect_clipped_at_screen_right_edge() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[42;5;8;99;99$x");
    assert_eq!(s.row(0), "EEEEEEEEEE");
    assert_eq!(s.row(3), "EEEEEEEEEE");
    assert_eq!(s.row(4), "EEEEEEE***", "clipped fill reaches the edge");
    assert_eq!(s.row(5), "EEEEEEE***", "clipped fill reaches the edge");
}

#[test]
fn one_by_one_rect_affects_single_cell() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    s.feed(b"\x1b[42;3;5;3;5$x"); // fill '*' at exactly (3;5)
    assert_eq!(s.row(2), "EEEE*EEEEE");
    assert_eq!(s.row(1), "EEEEEEEEEE");
    assert_eq!(s.row(3), "EEEEEEEEEE");
    s.feed(b"\x1b[3;5;3;5$z"); // erase exactly (3;5)
    assert_eq!(s.row(2), "EEEE EEEEE");
}

#[test]
fn rect_ops_do_not_move_cursor() {
    let mut s = Screen::new(10, 20);
    decaln(&mut s);
    s.feed(b"\x1b[5;5H");
    assert_eq!(s.cursor(), (4, 4));
    s.feed(b"\x1b[42;1;1;2;2$x");
    s.feed(b"\x1b[1;1;2;2$z");
    s.feed(b"\x1b[1;1;2;2${");
    s.feed(b"\x1b[1;1;2;2;1$r");
    s.feed(b"\x1b[1;1;2;2;1;3;3;1$v");
    s.feed(b"\x1b[1;1;2;2;7$t");
    assert_eq!(s.cursor(), (4, 4), "cursor pinned through all six rect ops");
}

#[test]
fn adversarial_rect_sequences_never_panic() {
    for (rows, cols) in [(1u16, 1u16), (2, 2), (24, 80)] {
        let mut s = Screen::new(rows, cols);
        s.feed(b"\x1b[65535;65535;65535;65535;65535$x");
        s.feed(b"\x1b[65535;65535;65535;65535$z");
        s.feed(b"\x1b[65535;65535;65535;65535${");
        s.feed(b"\x1b[65535;65535;65535;65535;65535;65535;65535;65535$v");
        s.feed(b"\x1b[65535;65535;65535;65535;1;4;5;7;0;1;4$r");
        s.feed(b"\x1b[65535;65535;65535;65535;1;4;5;7;8;0$t");
        s.feed(b"\x1b[$t"); // DECRARA with no parameters at all
        s.feed(b"\x1b[$v"); // full-screen self-copy (identity)
        s.feed(b"\x1b[$x"); // DECFRA with no Pch at all
        s.feed(b"\x1b[0$x"); // DECFRA with Pch=0
        s.feed(b"\x1b[1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1$v"); // param overflow
        s.feed(b"\x1b[65535*x\x1b[1;1;1;1;7$r\x1b[0*x"); // DECSACE garbage
        s.feed(b"\x1b[1;1H!");
        assert_eq!(
            s.row(0).chars().next(),
            Some('!'),
            "still parsing after barrage"
        );
    }
}
