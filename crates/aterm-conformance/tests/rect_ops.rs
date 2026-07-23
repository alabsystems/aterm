// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// VT420/VT520 rectangular area operations — DECFRA, DECERA, DECSERA, DECCRA,
// DECCARA, DECSACE — plus degenerate/adversarial parameter handling.
//
// Dispatch forms under test (xterm ctlseqs, "Functions using CSI"):
//   CSI Pch ; Pt ; Pl ; Pb ; Pr $ x   DECFRA  — Fill Rectangular Area
//   CSI Pt ; Pl ; Pb ; Pr $ z         DECERA  — Erase Rectangular Area
//   CSI Pt ; Pl ; Pb ; Pr $ {         DECSERA — Selective Erase Rect. Area
//   CSI ... ; Pps ; Ptd ; Pld ; Ppd $ v  DECCRA — Copy Rectangular Area
//   CSI Pt ; Pl ; Pb ; Pr ; Ps.. $ r  DECCARA — Change Attributes in Rect. Area
//   CSI Ps * x                        DECSACE — Select Attribute Change Extent
//
// Coordinates are 1-based and inclusive; a parameter of 0 or omitted takes its
// default (Pt=1, Pl=1, Pb=page bottom, Pr=page right) per the VT420/VT520
// programmer manuals (EK-VT520-RM). Every expectation below is spec-correct
// with the source cited inline; spec-correct tests the engine fails are marked
// `#[ignore = "ENGINE BUG: ..."]` rather than bent to match the engine.

use aterm_conformance::Screen;
use aterm_core::grid::cell_flags::CellFlags;

/// True if the resolved cell at (r, c) carries every flag in `want`.
fn has_flags(s: &Screen, r: u16, c: u16, want: CellFlags) -> bool {
    s.cell_flags_bits(r, c) & want.bits() == want.bits()
}

/// DECALN (ESC # 8): fill the whole screen with 'E' — the standard backdrop
/// for "outside the rectangle is untouched" assertions (VT510: screen
/// alignment pattern).
fn decaln(s: &mut Screen) {
    s.feed(b"\x1b#8");
}

// =========================================================================
// 1. DECFRA — Fill Rectangular Area (CSI Pch;Pt;Pl;Pb;Pr $ x)
// =========================================================================

#[test]
fn decfra_fills_exact_rect_and_leaves_outside_untouched() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    // VT520 EK-VT520-RM (DECFRA): fill the area bounded by (Pt;Pl)=(2;3) and
    // (Pb;Pr)=(4;6), inclusive, with character 42 = '*'.
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
    // VT520 (DECFRA): Pt/Pl default to 1, Pb/Pr default to the page bottom /
    // right — only Pch given means fill the entire page. 65 = 'A'.
    s.feed(b"\x1b[65$x");
    for r in 0..4 {
        assert_eq!(s.row(r), "AAAAAA", "row {r} fully filled by default rect");
    }
}

#[test]
fn decfra_rejects_non_printable_fill_char() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    // VT520 (DECFRA): "Pch can be any value from 32 to 126 or from 160 to
    // 255" — control values (7 = BEL, 27 = ESC code, 127 = DEL) and the C1
    // range (130) are not valid fill characters, so the command is ignored.
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
    // VT420/VT520 (DECFRA): the fill character takes the visual character
    // attributes set by the last SGR command. xterm implements this by
    // filling with the current attribute flags (util.c ScrnFillRectangle).
    // SGR 1;4 = bold + single underline, then fill 'X' (88) rows 2-3 cols 2-4.
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
    // DECALN backdrop materializes the rows so this test pins protection
    // semantics independently of DECFRA row-length tracking (covered by
    // decfra_default_coords_fill_full_screen).
    decaln(&mut s);
    // DEC STD 070 / xterm: DECFRA writes the fill character with the current
    // rendition AND the current DECSCA protection state (xterm keeps
    // PROTECTED in its ATTRIBUTES mask applied by ScrnFillRectangle).
    // Protected fill at cols 1-3, unprotected fill at cols 5-6:
    s.feed(b"\x1b[1\"q\x1b[88;1;1;1;3$x\x1b[0\"q\x1b[88;1;5;1;6$x");
    assert_eq!(s.row(0), "XXXEXXEEEE");
    // DECSERA full screen erases only the unprotected cells (VT520 DECSERA:
    // erases all *erasable* characters; DECSCA defines erasability).
    s.feed(b"\x1b[${");
    assert_eq!(s.row(0), "XXX", "protected DECFRA fill survives DECSERA");
}

// =========================================================================
// 2. DECERA — Erase Rectangular Area (CSI Pt;Pl;Pb;Pr $ z)
// =========================================================================

#[test]
fn decera_erases_rect_to_spaces_outside_untouched() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    // VT520 (DECERA): replaces all character positions in the rectangle with
    // the space character; rows 2-4, cols 3-6, inclusive bounds.
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
    // VT520 (DECERA): "DECERA erases character values and visual attributes"
    // — erased positions are spaces with no rendition.
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
    // VT510 (DECSCA): protection only guards against the *selective* erase
    // functions (DECSED/DECSEL, and DECSERA on VT420+). DECERA is the
    // non-selective rectangle erase, so protected characters are erased too.
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
    // VT520 (DECERA): all parameters default → rectangle = entire page.
    s.feed(b"\x1b[$z");
    assert_eq!(s.screen(), "");
}

// =========================================================================
// 3. DECSERA — Selective Erase Rectangular Area (CSI Pt;Pl;Pb;Pr $ {)
// =========================================================================

#[test]
fn decsera_preserves_decsca_protected_cells() {
    let mut s = Screen::new(4, 10);
    // VT520 (DECSERA): erases all *erasable* characters in the rectangle;
    // DECSCA (CSI 1 " q) defines subsequent characters as not erasable.
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
    // DECALN pattern characters are erasable (no DECSCA protection set), so
    // DECSERA acts like a rectangle erase here: rows 2-4 cols 3-6 blanked,
    // everything outside untouched (VT520 DECSERA: inclusive bounds).
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
    // VT520 (DECSERA): "DECSERA does not change: visual attributes set by the
    // select graphic rendition (SGR) function; protection attributes set by
    // DECSCA; line attributes." Only the characters become spaces. xterm
    // matches (DECSERA resets the character, not the video attributes).
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

// =========================================================================
// 4. DECCRA — Copy Rectangular Area (CSI Pts;Pls;Pbs;Prs;Pps;Ptd;Pld;Ppd $ v)
// =========================================================================

#[test]
fn deccra_copies_rect_and_source_remains() {
    let mut s = Screen::new(6, 10);
    s.feed(b"\x1b[1;1HABCDEF\x1b[2;1HGHIJKL");
    // VT520 (DECCRA): copy rows 1-2, cols 1-3 of page 1 to destination
    // (4;5) of page 1. The copy does not move the source — it duplicates it.
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
    // DEC STD 070 / xterm: DECCRA reads the source rectangle before writing
    // the destination (xterm copies via a temporary buffer), so an
    // overlapping copy must reproduce the ORIGINAL source content.
    // Copy row 1 cols 1-4 ("ABCD") to destination (1;3): cols 3-6 <- "ABCD".
    s.feed(b"\x1b[1;1;1;4;1;1;3;1$v");
    // A naive in-place forward copy would produce "ABABAB" instead.
    assert_eq!(s.row(0), "ABABCD", "overlap copied from pre-copy source");
}

#[test]
fn deccra_copies_character_attributes() {
    let mut s = Screen::new(4, 10);
    // VT520 (DECCRA): "The copied text takes on the character attributes of
    // the source area" — renditions travel with the characters.
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
    // VT520 (DECCRA): "If the destination area is partially off the page,
    // then DECCRA clips the off-page data." Copy 1x4 to (6;9): only cols
    // 9-10 of row 6 fit; nothing wraps to other rows.
    s.feed(b"\x1b[1;1;1;4;1;6;9;1$v");
    assert_eq!(s.row(5), "        WX", "clipped copy: only 'WX' fits");
    assert_eq!(s.row(0), "WXYZ", "source unchanged");
    for r in 1..5 {
        assert_eq!(s.row(r), "", "no wraparound spill into row {r}");
    }
}

// =========================================================================
// 5. DECCARA — Change Attributes in Rectangular Area (CSI Pt;Pl;Pb;Pr;Ps.. $ r)
//    + DECSACE — Select Attribute Change Extent (CSI Ps * x)
// =========================================================================

#[test]
fn deccara_single_row_applies_attrs_and_keeps_characters() {
    let mut s = Screen::new(6, 10);
    s.feed(b"\x1b[2;1HABCDEFGHIJ");
    // xterm ctlseqs (DECCARA): "Pt;Pl;Pb;Pr denotes the rectangle; Ps denotes
    // the SGR attributes to change: 0, 1, 4, 5, 7." A one-row area is
    // identical under both DECSACE extents, so this pins the attribute
    // application itself. SGR 1;4 = bold + underline on row 2, cols 3-6.
    // VT520 (DECCARA): "DECCARA does not change the characters".
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
    // xterm ctlseqs (DECSACE): "Ps = 2 -> rectangle (exact)." VT520
    // (DECSACE): Ps=2 -> DECCARA affects all character positions in the
    // rectangular area. So bold must land on rows 2-4 x cols 3-6 ONLY.
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
    // xterm ctlseqs (DECSACE): "Ps = 1 -> from start to end position,
    // wrapped." VT520 (DECSACE): Ps=0/1 -> DECCARA affects the stream of
    // character positions from (Pt;Pl) through (Pb;Pr): first row from Pl to
    // line end, intermediate rows full width, last row from line start to Pr.
    s.feed(b"\x1b[1*x\x1b[2;3;4;6;1$r");
    // Row 2 (first): cols 3-10 bold, cols 1-2 not.
    assert!(!has_flags(&s, 1, 0, CellFlags::BOLD));
    assert!(!has_flags(&s, 1, 1, CellFlags::BOLD));
    for c in 2..10 {
        assert!(
            has_flags(&s, 1, c, CellFlags::BOLD),
            "(1,{c}) first stream row"
        );
    }
    // Row 3 (middle): entire row bold.
    for c in 0..10 {
        assert!(
            has_flags(&s, 2, c, CellFlags::BOLD),
            "(2,{c}) middle stream row"
        );
    }
    // Row 4 (last): cols 1-6 bold, cols 7-10 not.
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
    // VT520 (DECSACE): the default extent (Ps=0, i.e. before any DECSACE is
    // sent) is the character STREAM, not the rectangle — xterm likewise
    // defaults to wrapped/stream extent. Middle row must go bold full-width.
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
    // Bold + DECSCA-protected "AB":
    s.feed(b"\x1b[1;1H\x1b[1m\x1b[1\"qAB\x1b[0\"q\x1b[0m");
    assert!(has_flags(&s, 0, 0, CellFlags::BOLD), "precondition: bold");
    // VT520 (DECCARA): Ps = 0 -> "attributes off" (clears bold/underline/
    // blink/reverse); and "DECCARA does not change: ... the protection
    // attribute (DECSCA)". One-row rect, so extent is irrelevant.
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
    // Protection must be intact: DECSERA spares the cells.
    s.feed(b"\x1b[${");
    assert_eq!(s.row(0), "AB", "DECSCA protection survives DECCARA 0");
}

#[test]
fn decrqss_decsace_roundtrips_current_extent() {
    // VT520 (DECRQSS "*x"): reports the current DECSACE extent. After
    // CSI 2 * x the report must carry Ps=2 (rectangle); after CSI 1 * x it
    // must carry Ps=1 (stream). Response format: DCS 1 $ r Ps * x ST
    // (xterm validity convention: 1 = valid request).
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[2*x\x1bP$q*x\x1b\\");
    assert_eq!(s.response_string(), "\x1bP1$r2*x\x1b\\");
    s.feed(b"\x1b[1*x\x1bP$q*x\x1b\\");
    assert_eq!(s.response_string(), "\x1bP1$r1*x\x1b\\");
}

// =========================================================================
// 6. DECOM interaction
// =========================================================================

#[test]
fn rect_coords_are_relative_to_margins_when_decom_set() {
    let mut s = Screen::new(10, 10);
    decaln(&mut s);
    // VT520 (DECERA notes): "The coordinates of the rectangular area are
    // affected by the setting of origin mode (DECOM)." With DECSTBM 3;8 and
    // DECOM set, (Pt;Pl)=(1;1) addresses absolute row 3, and Pb is clamped
    // to the bottom margin (origin-mode addressing cannot leave the region).
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
    // Pb=99 clamps to the bottom margin (absolute row 8), not the page end:
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
    // DECOM reset (default): rectangle coordinates address the page directly;
    // DECSTBM margins do not shift them (VT520: origin mode selects home
    // position / addressing origin; rect ops follow the same origin rule).
    s.feed(b"\x1b[5;8r"); // margins set, but DECOM is reset
    s.feed(b"\x1b[1;1;1;4$z");
    assert_eq!(
        s.row(0),
        "    EEEEEE",
        "row 1 is absolute row 1, not margin top"
    );
    assert_eq!(s.row(4), "EEEEEEEEEE", "margin-top row untouched");
}

// =========================================================================
// 7. Degenerate / adversarial parameters
// =========================================================================

#[test]
fn reversed_coords_are_ignored() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    // DEC STD 070 (rectangular area ops): the area must satisfy Pt <= Pb and
    // Pl <= Pr; otherwise the command is ignored (xterm: no-op). None of
    // these may modify the screen or panic.
    s.feed(b"\x1b[4;3;2;6$z"); // DECERA, Pb < Pt
    s.feed(b"\x1b[2;6;4;3$z"); // DECERA, Pr < Pl
    s.feed(b"\x1b[88;4;3;2;6$x"); // DECFRA, Pb < Pt
    s.feed(b"\x1b[4;3;2;6${"); // DECSERA, Pb < Pt
    s.feed(b"\x1b[2*x\x1b[4;3;2;6;1$r"); // DECCARA (rect extent), Pb < Pt
    s.feed(b"\x1b[4;1;2;3;1;1;1;1$v"); // DECCRA, source Pb < Pt
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
    // VT520 parameter convention: a parameter value of 0 is treated as the
    // default — CSI 0;0;0;0 $ z therefore erases the full page (Pt=1, Pl=1,
    // Pb=bottom, Pr=right).
    s.feed(b"\x1b[0;0;0;0$z");
    assert_eq!(s.screen(), "", "0;0;0;0 = full-page defaults");
}

#[test]
fn rect_clipped_at_screen_bottom() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    // Pb beyond the page is clipped to the page limit (DEC STD 070; xterm
    // clamps the rectangle to the screen): fill rows 5-99 -> rows 5-6 only,
    // interior columns 2-4. Must not panic or wrap.
    s.feed(b"\x1b[42;5;2;99;4$x");
    assert_eq!(s.row(0), "EEEEEEEEEE");
    assert_eq!(s.row(3), "EEEEEEEEEE");
    assert_eq!(s.row(4), "E***EEEEEE", "row 5 filled, Pb clamped");
    assert_eq!(s.row(5), "E***EEEEEE", "row 6 (page bottom) filled");
}

#[test]
fn rect_clipped_at_screen_right_edge() {
    let mut s = Screen::new(6, 10);
    decaln(&mut s);
    // Pb/Pr beyond the page are clipped to the page limits (DEC STD 070;
    // xterm clamps the rectangle to the screen): fill rows 5-6, cols 8-10.
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
    // Inclusive coordinates: Pt=Pb, Pl=Pr is a legal 1x1 rectangle.
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
    // VT520: DECFRA/DECERA/DECSERA/DECCARA/DECCRA "do not change the cursor
    // position".
    s.feed(b"\x1b[42;1;1;2;2$x");
    s.feed(b"\x1b[1;1;2;2$z");
    s.feed(b"\x1b[1;1;2;2${");
    s.feed(b"\x1b[1;1;2;2;1$r");
    s.feed(b"\x1b[1;1;2;2;1;3;3;1$v");
    assert_eq!(
        s.cursor(),
        (4, 4),
        "cursor pinned through all five rect ops"
    );
}

#[test]
fn adversarial_rect_sequences_never_panic() {
    // Max/garbage parameters, tiny screens, degenerate rects: the engine must
    // clamp/ignore per DEC STD 070 and stay alive (never panic), and keep
    // parsing afterwards.
    for (rows, cols) in [(1u16, 1u16), (2, 2), (24, 80)] {
        let mut s = Screen::new(rows, cols);
        s.feed(b"\x1b[65535;65535;65535;65535;65535$x");
        s.feed(b"\x1b[65535;65535;65535;65535$z");
        s.feed(b"\x1b[65535;65535;65535;65535${");
        s.feed(b"\x1b[65535;65535;65535;65535;65535;65535;65535;65535$v");
        s.feed(b"\x1b[65535;65535;65535;65535;1;4;5;7;0;1;4$r");
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
