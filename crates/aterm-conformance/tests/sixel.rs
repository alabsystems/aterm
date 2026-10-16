// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Sixel graphics with the `sixel` feature on (this crate enables it): the DCS
// dispatcher (handler_dcs.rs) feeds `DCS Ps q <data> ST` to the `aterm-sixel`
// decoder, and at ST places the RGBA raster through the OSC 1337 inline-image
// path (handler_osc_1337.rs `place_sixel_image`). The image lives in per-cell
// image extras (`Screen::images_row`), not glyph cells, so `screen()`/`row()`
// stay empty for it. DA1 advertises sixel (code 4).

use aterm_conformance::{Screen, run};

/// A well-formed minimal 4x6 two-color sixel image:
/// raster attrs 1;1;4;6, color 0 = black, color 1 = red (RGB% space),
/// select color 1, four full sixel columns, graphics-CR, graphics-NL.
/// At the default 8x16 cell metric this is a 1x1-cell inline image.
const SIXEL_4X6: &[u8] = b"\x1bP0;0;8q\"1;1;4;6#0;2;0;0;0#1;2;100;0;0#1~~~~$-\x1b\\";

// --- consumption and placement ---

#[test]
fn sixel_dcs_renders_image_then_text_flows_below() {
    // The image is placed, no payload byte leaks as a glyph, and no reply is
    // sent. In scrolling mode (DECSDM reset, the default) the cursor moves
    // below the image, so text lands on row 1.
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
    // The parser returns to Ground after ST: CUP and CPR work as on a fresh
    // terminal.
    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);
    s.feed(b"\x1b[5;10H\x1b[6n");
    assert_eq!(s.cursor(), (4, 9));
    assert_eq!(s.response_string(), "\x1b[5;10R");
}

#[test]
fn sixel_dcs_split_across_feeds_renders_identically() {
    // Split mid-sequence across feeds, the payload behaves as one feed.
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
    // Missing ST: the terminal stays in DCS passthrough (VT500 parser model), so
    // following text is payload. The next ESC ends the string as `ESC \` would,
    // so the declared image is placed and then the CSI runs.
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
    // C0 controls, 8-bit bytes and non-sixel characters in the payload are DCS
    // data (aterm-parser table/dcs_osc.rs): nothing executes or prints, and the
    // terminal recovers at ST.
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
    // CAN (0x18) cancels a control string from any state: no image, and text
    // prints at the unchanged cursor.
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
    // A declared 0x0 image with no data decodes to nothing: nothing placed,
    // cursor unmoved.
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
    // DECGRI `! Pn <char>` repeats <char> Pn times. A hostile Pn must not OOM or
    // panic: the decoder clamps a run to SIXEL_MAX_DIMENSION (4096), and
    // MAX_DCS_GLOBAL_BUDGET (10 MiB, callbacks/mod.rs) backstops the allocation.
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
    // 11 MiB exceeds MAX_DCS_GLOBAL_BUDGET (10 MiB): the engine keeps consuming
    // and recovers at ST.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1");
    let chunk = vec![b'~'; 1 << 20]; // 1 MiB of full sixel columns
    for _ in 0..11 {
        s.feed(&chunk);
    }
    s.feed(b"\x1b\\OK");
    // Recovery is the lock; whether the image was placed depends on where the
    // budget tripped.
    assert!(s.row(0).ends_with("OK") || s.row(1) == "OK");
}

// --- DECSDM (DEC private mode 80) ---

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
    // As in xterm, DECSTR (CSI ! p) resets sixel display mode (handler_report.rs).
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80h\x1b[!p\x1b[?80$p");
    assert_eq!(s.response_string(), "\x1b[?80;2$y");
}

// --- XTSMGRAPHICS (CSI ? Pi ; Pa ; Pv S) ---
// Reply CSI ? Pi ; Ps ; Pv S, Ps 0 = success, 1 = error in Pi, 2 = error in Pa,
// 3 = failure. Read-only: 1024 color registers and 4096 max sixel dimension,
// aterm-sixel's constants when the feature is on (handler_xtsmgraphics.rs).

#[test]
fn xtsmgraphics_color_registers_read_and_read_max() {
    // Pi=1 Pa=1 (read) and Pa=4 (read max) both report 1024.
    assert_eq!(run(b"\x1b[?1;1S").response_string(), "\x1b[?1;0;1024S");
    assert_eq!(run(b"\x1b[?1;4S").response_string(), "\x1b[?1;0;1024S");
}

#[test]
fn xtsmgraphics_sixel_geometry_read_falls_back_to_max_without_window_callback() {
    // Pi=2 Pa=1 reads the current text-area pixel size behind the same
    // window-ops capability as CSI 14 t; the harness grants no window ops and
    // reports no cell box, so the fallback is the max dimension on both axes.
    // The granted case is pinned in aterm-core's handler_xtsmgraphics tests.
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

// --- cursor after sixel, both DECSDM states ---
// VT340 / xterm: in scrolling mode (DECSDM reset) the cursor moves to the line
// after the image; in display mode (DECSDM set) it does not move.

#[test]
fn cursor_moves_after_sixel_with_decsdm_reset() {
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

// --- mid-line column anchoring ---
// VT340 / xterm: a sixel anchors at the cursor column, not the left margin
// (unlike OSC 1337 images), and does not overprint the cells to its left.

#[test]
fn sixel_anchors_at_cursor_column_mid_line() {
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
    // Image cells live in extras, not glyph cells, so "ABCDE" left of the
    // sixel survives.
    let mut s = Screen::new(24, 80);
    s.feed(b"ABCDE");
    assert_eq!(s.cursor(), (0, 5), "cursor sits just past the text");
    s.feed(SIXEL_4X6);

    let row = s.images_row(0);
    assert_eq!(row.len(), 1, "one image cell placed");
    assert_eq!(row[0].0, 5, "anchored at the cursor column (5), not 0");
    assert_eq!(
        &s.row(0)[..5],
        "ABCDE",
        "glyphs left of the sixel are intact"
    );
}

#[test]
fn sixel_mid_line_anchors_at_cursor_in_display_mode() {
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

// --- image observability ---

#[test]
fn sixel_image_decodes_to_expected_raster_and_is_placed() {
    use aterm_core::grid::extra::ImageFormat;

    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);

    let row = s.images_row(0);
    assert_eq!(row.len(), 1, "one image cell on row 0");
    let (col, ref iref) = row[0];
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

    // The payload is the decoded raster, RawRgba8 at the post-clamp pixel size
    // (4x6, not the footprint).
    match data.format {
        ImageFormat::RawRgba8 { width, height } => {
            assert_eq!(width, 4, "raster width = declared/painted 4px");
            assert_eq!(height, 6, "raster height = one 6px band");
        }
        other => panic!("expected RawRgba8, got {other:?}"),
    }

    assert_eq!(data.bytes.len(), 4 * 6 * 4, "RGBA8 byte count = 4*w*h");

    // Color 1 is RGB% 100;0;0, so every painted pixel is opaque red, laid out
    // [R, G, B, A] (the resample_rgba/blit contract).
    assert_eq!(
        &data.bytes[0..4],
        &[0xFF, 0x00, 0x00, 0xFF],
        "painted pixel must be opaque red in [R,G,B,A] order"
    );
    for px in data.bytes.as_chunks::<4>().0 {
        assert_eq!(*px, [0xFF, 0x00, 0x00, 0xFF], "all painted pixels are red");
    }
}

#[test]
fn sixel_placement_is_pixel_exact_not_scaled_to_its_footprint() {
    // Sixel names pixels: the footprint rounds the raster up to whole cells,
    // and placement is `PixelExact` (1:1 from the top-left, the remainder
    // unpainted), not OSC 1337's scale-to-fill.
    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);
    let row = s.images_row(0);
    assert_eq!(row.len(), 1, "one image cell on row 0");
    assert_eq!(
        row[0].1.image.scaling,
        aterm_core::grid::extra::ImageScaling::PixelExact,
        "a sixel placement must be pixel-exact (1:1, top-left anchored), not \
         scaled out to fill its rounded-up cell footprint"
    );
}
