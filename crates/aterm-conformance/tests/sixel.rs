// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for Sixel graphics decoding and inline placement.

use aterm_conformance::{Screen, run};

const SIXEL_4X6: &[u8] = b"\x1bP0;0;8q\"1;1;4;6#0;2;0;0;0#1;2;100;0;0#1~~~~$-\x1b\\";

#[test]
fn sixel_dcs_renders_image_then_text_flows_below() {
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
    let mut s = Screen::new(24, 80);
    s.feed(SIXEL_4X6);
    s.feed(b"\x1b[5;10H\x1b[6n");
    assert_eq!(s.cursor(), (4, 9));
    assert_eq!(s.response_string(), "\x1b[5;10R");
}

#[test]
fn sixel_dcs_split_across_feeds_renders_identically() {
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
fn truncated_sixel_consumes_following_text_until_an_escape_breaks_it_off() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1~~"); // no ST
    s.feed(b"XYZ"); // still inside the DCS: must NOT print as glyphs
    assert_eq!(s.screen(), "", "text after unterminated DCS leaked to grid");
    assert!(
        s.images_row(0).is_empty(),
        "no terminator yet -> not placed yet"
    );
    s.feed(b"\x1b[6n"); // ESC breaks the string off, then the CSI runs
    assert!(
        s.images_row(0).is_empty(),
        "a sixel broken off by ESC [ is not placed"
    );
    assert_eq!(s.response_string(), "\x1b[1;1R");
    s.feed(b"OK");
    assert_eq!(s.row(0), "OK");
}

#[test]
fn garbage_inside_sixel_payload_is_contained() {
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1~");
    s.feed(&[0x07, 0x09, 0x0a, 0x0d, 0x80, 0xfe, b'(', b'%']);
    s.feed(b"~~\x1b\\");
    assert_eq!(s.screen(), "", "no garbage byte leaks as a glyph");
    s.feed(b"OK");
    assert_eq!(s.row(1), "OK");
}

#[test]
fn can_aborts_sixel_dcs_and_returns_to_ground() {
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
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1");
    s.feed(b"!4294967295~!999999999~!0~!123456789012345678901234567890~");
    s.feed(b"\x1b\\OK");
    assert_eq!(s.row(1), "OK");
}

#[test]
fn payload_larger_than_global_dcs_budget_is_dropped_without_panic() {
    // Path 1: raw stream bytes exceed `MAX_DCS_GLOBAL_BUDGET` (10 MiB) while the clamped
    // 4096x6 raster stays well within budget (~98 KiB RGBA8); bytes past 10 MiB are dropped,
    // the clamped prefix image is placed on row 0, and subsequent text flows onto row 1.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q\"1;1;4;6#1");
    let chunk = vec![b'~'; 1 << 20]; // 1 MiB of full sixel columns
    for _ in 0..11 {
        s.feed(&chunk);
    }
    s.feed(b"\x1b\\OK");
    let placed = s.images_row(0);
    assert_eq!(
        placed.len(),
        80,
        "clamped 4096x6 prefix raster within budget spans all 80 columns on row 0"
    );
    assert_eq!(
        placed[0].1.image.format,
        aterm_core::grid::extra::ImageFormat::RawRgba8 {
            width: 4096,
            height: 6,
        }
    );
    assert!(
        placed
            .iter()
            .all(|(_, r)| std::sync::Arc::ptr_eq(&r.image, &placed[0].1.image)),
        "every placed cell on row 0 references the single clamped image"
    );
    assert_eq!(s.row(1), "OK");

    // Path 2: decoded raster allocation pushes `total_bytes > MAX_DCS_GLOBAL_BUDGET`
    // (3000 * 1002 * 4 = 12,024,000 bytes > 10 MiB, within SIXEL_MAX_IMAGE_BYTES = 16 MiB);
    // the decoder aborts, no image is placed, and subsequent text prints on row 0.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP0;0;8q#1");
    s.feed(&[b'-'; 166]);
    s.feed(b"!3000~\x1b\\OK");
    assert!(
        s.images_row(0).is_empty(),
        "raster allocation exceeding MAX_DCS_GLOBAL_BUDGET aborts without placing an image"
    );
    assert_eq!(s.row(0), "OK");
}

#[test]
fn decsdm_mode80_set_reset_roundtrip_via_decrqm() {
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
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1b[?80h\x1b[!p\x1b[?80$p");
    assert_eq!(s.response_string(), "\x1b[?80;2$y");
}

#[test]
fn xtsmgraphics_reports_read_only_limits_and_errors() {
    for (name, query, reply) in [
        ("colors read", &b"\x1b[?1;1S"[..], "\x1b[?1;0;1024S"),
        ("colors read max", b"\x1b[?1;4S", "\x1b[?1;0;1024S"),
        ("geometry read", b"\x1b[?2;1S", "\x1b[?2;0;1000;1000S"),
        ("geometry read max", b"\x1b[?2;4S", "\x1b[?2;0;1000;1000S"),
        ("regis", b"\x1b[?3;1S", "\x1b[?3;3;0S"),
        ("colors set", b"\x1b[?1;3;99S", "\x1b[?1;3;0S"),
        ("geometry reset", b"\x1b[?2;2S", "\x1b[?2;3;0S"),
        ("unknown item", b"\x1b[?9;1S", "\x1b[?9;1;0S"),
        ("item zero", b"\x1b[?0;1S", "\x1b[?0;1;0S"),
        ("unknown action", b"\x1b[?1;9S", "\x1b[?1;2;0S"),
        ("omitted action", b"\x1b[?1S", "\x1b[?1;2;0S"),
    ] {
        assert_eq!(run(query).response_string(), reply, "{name}");
    }
}

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
    assert_eq!(iref.cell_row, 0, "image top-left tile row");
    assert_eq!(iref.cell_col, 0, "image top-left tile col");
    assert_eq!(
        s.cursor(),
        (1, 0),
        "scrolling mode: cursor below image at col 0"
    );
}

#[test]
fn sixel_mid_line_does_not_overprint_cells_to_its_left() {
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
    assert_eq!(data.cols, 1, "1-column footprint");
    assert_eq!(data.rows, 1, "1-row footprint");

    match data.format {
        ImageFormat::RawRgba8 { width, height } => {
            assert_eq!(width, 4, "raster width = declared/painted 4px");
            assert_eq!(height, 6, "raster height = one 6px band");
        }
        other => panic!("expected RawRgba8, got {other:?}"),
    }

    assert_eq!(data.bytes.len(), 4 * 6 * 4, "RGBA8 byte count = 4*w*h");

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
