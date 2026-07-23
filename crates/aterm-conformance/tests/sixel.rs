// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Sixel graphics — behavioral locks for what the engine ACTUALLY implements.
//
// HONEST SUPPORT STATEMENT (verified against source, 2026-06):
// aterm's sixel support is DECODED and RENDERED. With the `sixel` feature on
// (which aterm-conformance enables), the DCS dispatcher recognizes
// `DCS Ps q <data> ST` (handler_dcs.rs), feeds the body to the `aterm-sixel`
// decoder, and at ST routes the decoded RGBA raster into the SAME inline-image
// placement/blit path the iTerm2 OSC 1337 `File=` protocol uses
// (handler_osc_1337.rs `place_sixel_image` -> `place_image`). The image lands
// in per-cell image EXTRAS (observable via `Terminal::images_row`), not in
// glyph cells, so `screen()`/`row()` stay empty for it. DA1 advertises sixel
// (code 4). XTSMGRAPHICS reports 1024 color registers / 4096 max geometry from
// the decoder's exported constants.
//
// What IS live, and what this file locks:
//   * Decode + placement: a valid sixel DCS produces an inline image whose
//     footprint, format (RawRgba8), and pixels are observable; the cursor moves
//     per the sixel mode (scrolling: below the image; display/DECSDM: unmoved).
//   * Graceful consumption: payloads (truncated, garbage, huge DECGRI repeats)
//     never leak onto the GLYPH grid, never panic, never corrupt parser state.
//   * DECSDM (DEC private mode 80) set/reset tracking, observable via DECRQM
//     (VT510: CSI ? Ps $ p -> CSI ? Ps ; Pm $ y), and its reset on DECSTR.
//   * XTSMGRAPHICS (CSI ? Pi ; Pa ; Pv S), read-only: 1024 color registers and
//     4096x4096 max sixel geometry.

use aterm_conformance::{Screen, run};

/// A well-formed minimal 4x6 two-color sixel image:
/// raster attrs 1;1;4;6, color 0 = black, color 1 = red (RGB% space),
/// select color 1, four full sixel columns, graphics-CR, graphics-NL.
/// At the default 8x16 cell metric this is a 1x1-cell inline image.
const SIXEL_4X6: &[u8] = b"\x1bP0;0;8q\"1;1;4;6#0;2;0;0;0#1;2;100;0;0#1~~~~$-\x1b\\";

// --- A. graceful consumption + placement ------------------------------------

#[test]
fn sixel_dcs_renders_image_then_text_flows_below() {
    // A valid sixel DCS is decoded into an inline image. No payload byte
    // ('~', '#', '"', digits...) leaks onto the GLYPH grid, and the DCS itself
    // generates no reply. In scrolling mode (DECSDM reset, the default) the
    // image occupies row 0 and the cursor advances to the line below it, so
    // following text lands on row 1.
    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);
    assert_eq!(
        s.screen(),
        "",
        "sixel payload bytes must not leak as glyphs"
    );
    assert_eq!(
        s.take_response(),
        None,
        "sixel DCS must not generate a reply"
    );
    // Image placed on row 0.
    assert_eq!(s.images_row(0).len(), 1, "one inline image cell on row 0");
    // 4x6 px at 8x16 cell -> 1x1 footprint -> cursor on row 1, col 0.
    assert_eq!(
        s.cursor(),
        (1, 0),
        "scrolling mode: cursor moves below image"
    );
    s.feed(b"AB");
    assert_eq!(s.row(1), "AB");
    assert_eq!(s.cursor(), (1, 2));
}

#[test]
fn sixel_dcs_leaves_parser_state_intact_for_subsequent_csi() {
    // Parser must return to Ground after ST: a following CUP and CPR (DSR 6)
    // must work exactly as on a fresh terminal (VT510 CPR: CSI 6 n ->
    // CSI Pr ; Pc R, 1-indexed). Placement does not disturb parser state.
    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);
    s.feed(b"\x1b[5;10H\x1b[6n");
    assert_eq!(s.cursor(), (4, 9));
    assert_eq!(s.response_string(), "\x1b[5;10R");
}

#[test]
fn sixel_dcs_split_across_feeds_renders_identically() {
    // Chunk-boundary safety: the same payload split mid-sequence across
    // process() calls must behave identically to a single feed — image on
    // row 0, cursor on row 1, "OK" printed there.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6");
    s.feed(b"#0;2;0;0;0#1;2;100;0;0");
    s.feed(b"#1~~");
    s.feed(b"~~$-");
    s.feed(b"\x1b\\");
    s.feed(b"OK");
    assert_eq!(
        s.images_row(0).len(),
        1,
        "image placed across feed boundaries"
    );
    assert_eq!(s.row(1), "OK");
    assert_eq!(s.cursor(), (1, 2));
}

#[test]
fn truncated_sixel_consumes_following_text_until_escape_breaks_out() {
    // Missing ST: per the VT500 parser model the terminal stays in
    // DCS-passthrough, so following printable text is payload, NOT display
    // text. The next ESC (here: starting CSI 6 n) terminates the string via
    // the "anywhere" ESC transition — which is EXACTLY how a real `ESC \` ST
    // terminates DCS — so the DCS unhooks normally and the (declared 4x6)
    // image IS placed, then the CSI executes.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1~~"); // no ST
    s.feed(b"XYZ"); // still inside the DCS: must NOT print as glyphs
    assert_eq!(s.screen(), "", "text after unterminated DCS leaked to grid");
    assert!(
        s.images_row(0).is_empty(),
        "no terminator yet -> not placed yet"
    );
    s.feed(b"\x1b[6n"); // ESC breaks out (= ST), image placed, then CSI runs
    assert_eq!(
        s.images_row(0).len(),
        1,
        "ESC-breakout terminates -> image placed"
    );
    // Image occupied row 0, cursor advanced to row 1 -> CPR reports row 2 (1-based).
    assert_eq!(s.response_string(), "\x1b[2;1R");
    s.feed(b"OK");
    assert_eq!(s.row(1), "OK");
}

#[test]
fn garbage_inside_sixel_payload_is_contained() {
    // C0 controls (BEL, TAB, LF, CR), 8-bit bytes, and non-sixel characters
    // inside the payload are all DCS data per the parser tables
    // (aterm-parser table/dcs_osc.rs): nothing executes, nothing prints as a
    // glyph, and the terminal recovers at ST. The decoder ignores the stray
    // bytes; the image (if any pixels were painted) is placed harmlessly.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1~");
    s.feed(&[0x07, 0x09, 0x0a, 0x0d, 0x80, 0xfe, b'(', b'%']);
    s.feed(b"~~\x1b\\");
    assert_eq!(s.screen(), "", "no garbage byte leaks as a glyph");
    s.feed(b"OK");
    // The image occupies row 0; "OK" flows below it onto row 1.
    assert_eq!(s.row(1), "OK");
}

#[test]
fn can_aborts_sixel_dcs_and_returns_to_ground() {
    // CAN (0x18) cancels a control string from any state (VT500 "anywhere"
    // transition); the partial image is aborted (no placement) and subsequent
    // text must print normally at the unchanged cursor.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1~~");
    s.feed(&[0x18]);
    assert!(s.images_row(0).is_empty(), "CAN aborts -> no image placed");
    s.feed(b"OK");
    assert_eq!(s.row(0), "OK");
    assert_eq!(s.cursor(), (0, 2), "aborted sixel does not move the cursor");
}

#[test]
fn zero_size_sixel_image_is_harmless() {
    // Raster attributes declaring a 0x0 image with no data: the decoder yields
    // no image (degenerate), so nothing is placed and the cursor is unmoved.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;0;0\x1b\\");
    assert_eq!(s.screen(), "");
    assert!(s.images_row(0).is_empty(), "0x0 declaration places nothing");
    assert_eq!(s.take_response(), None);
    assert_eq!(s.cursor(), (0, 0), "degenerate sixel does not move cursor");
    s.feed(b"OK");
    assert_eq!(s.row(0), "OK");
}

#[test]
fn enormous_decgri_repeat_count_no_panic_no_oom() {
    // DECGRI: `! Pn <char>` repeats the sixel <char> Pn times. A hostile
    // Pn (here u32::MAX, plus several more) must not OOM or panic: the
    // decoder clamps every run to SIXEL_MAX_DIMENSION (4096) and the 10 MiB
    // global DCS budget (callbacks/mod.rs MAX_DCS_GLOBAL_BUDGET) backstops the
    // pixel allocation.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1");
    s.feed(b"!4294967295~!999999999~!0~!123456789012345678901234567890~");
    s.feed(b"\x1b\\OK");
    // The image (clamped to 4096px wide -> grid-width-clamped footprint) is on
    // row 0; "OK" flows below.
    assert_eq!(s.row(1), "OK");
}

#[test]
fn payload_larger_than_global_dcs_budget_is_dropped_without_panic() {
    // 11 MiB of sixel data exceeds MAX_DCS_GLOBAL_BUDGET (10 MiB): the
    // engine must keep consuming (dropping/aborting) bytes and recover cleanly
    // at ST without panicking.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1");
    let chunk = vec![b'~'; 1 << 20]; // 1 MiB of full sixel columns
    for _ in 0..11 {
        s.feed(&chunk);
    }
    s.feed(b"\x1b\\OK");
    // Recovery is the lock: "OK" prints and no panic occurred. (The image may
    // or may not have been placed depending on where the budget tripped; the
    // contract is graceful recovery, not a specific footprint.)
    assert!(s.row(0).ends_with("OK") || s.row(1) == "OK");
}

// --- B. DECSDM (DEC private mode 80) -----------------------------------------

#[test]
fn decsdm_mode80_set_reset_roundtrip_via_decrqm() {
    // VT510 DECRQM: CSI ? 80 $ p -> CSI ? 80 ; Pm $ y (1=set, 2=reset).
    // Power-on default is reset; CSI ? 80 h sets, CSI ? 80 l resets.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80$p");
    assert_eq!(
        s.response_string(),
        "\x1b[?80;2$y",
        "DECSDM default must be reset"
    );
    s.feed(b"\x1b[?80h\x1b[?80$p");
    assert_eq!(s.response_string(), "\x1b[?80;1$y");
    s.feed(b"\x1b[?80l\x1b[?80$p");
    assert_eq!(s.response_string(), "\x1b[?80;2$y");
}

#[test]
fn decsdm_is_reset_by_decstr_soft_reset() {
    // Engine behavior (handler_report.rs, #7496), matching xterm: DECSTR
    // (CSI ! p) resets sixel display mode.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80h\x1b[!p\x1b[?80$p");
    assert_eq!(s.response_string(), "\x1b[?80;2$y");
}

// --- C. XTSMGRAPHICS (CSI ? Pi ; Pa ; Pv S) ----------------------------------
// xterm ctlseqs: response is CSI ? Pi ; Ps ; Pv S with Ps: 0=success,
// 1=error in Pi, 2=error in Pa, 3=failure. The engine is read-only and
// reports its documented limits (handler_xtsmgraphics.rs): 1024 color
// registers, 4096 max sixel dimension — sourced from aterm-sixel's exported
// constants when the feature is on, so the values stay consistent.

#[test]
fn xtsmgraphics_color_registers_read_and_read_max() {
    // Pi=1 Pa=1 (read) and Pa=4 (read max) both report 1024.
    assert_eq!(run(b"\x1b[?1;1S").response_string(), "\x1b[?1;0;1024S");
    assert_eq!(run(b"\x1b[?1;4S").response_string(), "\x1b[?1;0;1024S");
}

#[test]
fn xtsmgraphics_sixel_geometry_read_falls_back_to_max_without_window_callback() {
    // Pi=2 Pa=1 reads the current text-area pixel size via the host window
    // callback (#7470); the conformance harness registers none, so the
    // engine's documented fallback is the max dimension on both axes.
    assert_eq!(run(b"\x1b[?2;1S").response_string(), "\x1b[?2;0;4096;4096S");
}

#[test]
fn xtsmgraphics_sixel_geometry_read_max() {
    // Pi=2 Pa=4: maximum sixel geometry, width;height.
    assert_eq!(run(b"\x1b[?2;4S").response_string(), "\x1b[?2;0;4096;4096S");
}

#[test]
fn xtsmgraphics_regis_geometry_reports_failure() {
    // Pi=3 (ReGIS) is unsupported: status 3 (failure).
    assert_eq!(run(b"\x1b[?3;1S").response_string(), "\x1b[?3;3;0S");
}

#[test]
fn xtsmgraphics_set_and_reset_rejected_read_only() {
    // Pa=3 (set) and Pa=2 (reset) are rejected with status 3: the engine's
    // graphics limits are read-only.
    assert_eq!(run(b"\x1b[?1;3;99S").response_string(), "\x1b[?1;3;0S");
    assert_eq!(run(b"\x1b[?2;2S").response_string(), "\x1b[?2;3;0S");
}

#[test]
fn xtsmgraphics_invalid_item_and_action_report_errors() {
    // Unknown Pi -> status 1 (error in Pi); unknown/omitted Pa -> status 2
    // (error in Pa). The raw Pi is echoed back in both cases.
    assert_eq!(run(b"\x1b[?9;1S").response_string(), "\x1b[?9;1;0S");
    assert_eq!(run(b"\x1b[?0;1S").response_string(), "\x1b[?0;1;0S");
    assert_eq!(run(b"\x1b[?1;9S").response_string(), "\x1b[?1;2;0S");
    assert_eq!(run(b"\x1b[?1S").response_string(), "\x1b[?1;2;0S");
}

// --- D. cursor position after sixel, both DECSDM states ----------------------
// Spec (VT340 / xterm): in sixel SCROLLING mode (DECSDM reset) the cursor
// moves to the line after the image; in DISPLAY mode (DECSDM set) the image is
// painted and the cursor does NOT move. aterm now renders the image and matches
// both behaviors via `place_sixel_image`.

#[test]
fn cursor_moves_after_sixel_with_decsdm_reset() {
    // Scrolling mode: cursor advances to the line below the 1-row image.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80l\x1b[5;1H");
    s.feed(SIXEL_4X6);
    assert_eq!(
        s.images_row(4).len(),
        1,
        "image painted on the cursor row (4)"
    );
    assert_eq!(
        s.cursor(),
        (5, 0),
        "scrolling mode: cursor moves to the line after the image"
    );
}

#[test]
fn cursor_unmoved_after_sixel_with_decsdm_set() {
    // Display mode (DECSDM set): the image is painted at the cursor and the
    // cursor is restored, so it does not move.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80h\x1b[5;1H");
    s.feed(SIXEL_4X6);
    assert_eq!(
        s.images_row(4).len(),
        1,
        "image painted on the cursor row (4)"
    );
    assert_eq!(
        s.cursor(),
        (4, 0),
        "display mode (DECSDM set): cursor must not move"
    );
}

// --- D2. mid-line column anchoring -------------------------------------------
// Spec (VT340 / xterm): a sixel image anchors at the CURRENT cursor column, NOT
// at the left margin like iTerm2's OSC 1337 inline images. A sixel emitted after
// some text must paint starting at the cursor's column and must not overprint the
// cells to its left.

#[test]
fn sixel_anchors_at_cursor_column_mid_line() {
    // Scrolling mode (default). Move the cursor to row 0, column 5 (1-based
    // CUP column 6) and emit the 1x1-footprint sixel: it must land at column 5,
    // and there must be NO image cell at columns 0..5.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80l\x1b[1;6H");
    s.feed(SIXEL_4X6);

    let row = s.images_row(0);
    assert_eq!(row.len(), 1, "exactly one image cell on row 0");
    let (col, ref iref) = row[0];
    assert_eq!(
        col, 5,
        "sixel anchors at the cursor column (5), not column 0"
    );
    // The image's own tile coordinate is still (0,0): anchoring shifts where the
    // footprint lands on the grid, not the image's internal tiling.
    assert_eq!(iref.cell_row, 0, "image top-left tile row");
    assert_eq!(iref.cell_col, 0, "image top-left tile col");
    // Cursor moved to the line below (scrolling mode), back at column 0.
    assert_eq!(
        s.cursor(),
        (1, 0),
        "scrolling mode: cursor below image at col 0"
    );
}

#[test]
fn sixel_mid_line_does_not_overprint_cells_to_its_left() {
    // Text "ABCDE" occupies columns 0..5; the cursor is then at column 5. A sixel
    // emitted there must anchor at column 5 and leave the five glyph cells intact
    // (image cells live in EXTRAS, not glyph cells, so the text also survives).
    let mut s = Screen::new(24, 80);
    s.feed(b"ABCDE");
    assert_eq!(s.cursor(), (0, 5), "cursor sits just past the text");
    s.feed(SIXEL_4X6);

    let row = s.images_row(0);
    assert_eq!(row.len(), 1, "one image cell placed");
    assert_eq!(row[0].0, 5, "anchored at the cursor column (5), not 0");
    // The text to the left is untouched — the image did not snap to column 0.
    assert_eq!(
        &s.row(0)[..5],
        "ABCDE",
        "glyphs left of the sixel are intact"
    );
}

#[test]
fn sixel_mid_line_anchors_at_cursor_in_display_mode() {
    // Display mode (DECSDM set): the image anchors at the cursor column AND the
    // cursor is restored to where it was. Position at row 2, column 7.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80h\x1b[3;8H");
    s.feed(SIXEL_4X6);

    let row = s.images_row(2);
    assert_eq!(row.len(), 1, "image on the cursor row (2)");
    assert_eq!(
        row[0].0, 7,
        "display mode also anchors at the cursor column (7)"
    );
    assert_eq!(
        s.cursor(),
        (2, 7),
        "display mode (DECSDM set): cursor unmoved, still at (row 2, col 7)"
    );
}

// --- E. image observability ---------------------------------------------------
// The decoder is compiled in (conformance enables `sixel`), so the placed image
// is observable: footprint, RawRgba8 format with the post-clamp raster size, and
// the painted-pixel colors are all assertable through `images_row`.

#[test]
fn sixel_image_decodes_to_expected_raster_and_is_placed() {
    use aterm_core::grid::extra::ImageFormat;

    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);

    // Exactly one inline-image cell is placed on the cursor's start row (0).
    let row = s.images_row(0);
    assert_eq!(row.len(), 1, "one image cell on row 0");
    let (col, ref iref) = row[0];
    // The cursor was at column 0 (fresh screen) and sixel anchors at the CURRENT
    // cursor column, so the image lands at column 0 here. The mid-line case (a
    // nonzero cursor column) is locked by `sixel_anchors_at_cursor_column_mid_line`.
    assert_eq!(
        col, 0,
        "anchored at the cursor column (0 on a fresh screen)"
    );
    assert_eq!(iref.cell_row, 0);
    assert_eq!(iref.cell_col, 0);

    let data = &*iref.image;
    // 4x6 px at 8x16 cell -> 1x1 footprint.
    assert_eq!(data.cols, 1, "1-column footprint");
    assert_eq!(data.rows, 1, "1-row footprint");

    // The stored payload is the DECODED raster, tagged RawRgba8 with the
    // post-clamp pixel dimensions (4x6 — the sixel raster, not the footprint).
    match data.format {
        ImageFormat::RawRgba8 { width, height } => {
            assert_eq!(width, 4, "raster width = declared/painted 4px");
            assert_eq!(height, 6, "raster height = one 6px band");
        }
        other => panic!("expected RawRgba8, got {other:?}"),
    }

    // 4*6 pixels * 4 bytes (RGBA) = 96 bytes.
    assert_eq!(data.bytes.len(), 4 * 6 * 4, "RGBA8 byte count = 4*w*h");

    // Color 1 was defined as RGB% 100;0;0 = pure red and selected before the
    // four full `~` columns, so every painted pixel is opaque red. Byte layout
    // is [R, G, B, A] (the resample_rgba/blit contract). Check the top-left.
    assert_eq!(
        &data.bytes[0..4],
        &[0xFF, 0x00, 0x00, 0xFF],
        "painted pixel must be opaque red in [R,G,B,A] order"
    );
    // Every painted pixel (the whole 4x6 raster is full red) is the same.
    for px in data.bytes.as_chunks::<4>().0 {
        assert_eq!(*px, [0xFF, 0x00, 0x00, 0xFF], "all painted pixels are red");
    }
}
