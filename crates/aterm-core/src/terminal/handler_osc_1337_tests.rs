// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the OSC 1337 `File=` inline-image handler.
//!
//! Parsing and placement only: the engine stores an iTerm2 image undecoded, so
//! the payload is arbitrary bytes with a PNG magic prefix, and cell-unit sizes
//! (`width=N`) make placement deterministic.

use super::super::Terminal;

/// A minimal "PNG" prefix so [`detect_format`](super::detect_format) classifies
/// the payload as PNG. The bytes after it are never decoded by the engine.
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Build an OSC 1337 `File=` sequence: `ESC ] 1337 ; File=<args> : <b64> ST`.
fn osc_1337_file(args: &str, payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\"); // ST
    out
}

/// Count cells in the grid carrying an inline-image ref.
fn image_cell_count(term: &Terminal) -> usize {
    let grid = term.grid();
    let mut n = 0;
    for r in 0..grid.rows() {
        for c in 0..grid.cols() {
            if grid.cell_extra(r, c).is_some_and(|e| e.image().is_some()) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn file_with_cell_dimensions_covers_exact_footprint() {
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=4;height=3", &payload));
    // 4 cols × 3 rows = 12 covered cells.
    assert_eq!(image_cell_count(&term), 12);
    // The footprint is anchored at the top-left (column 0).
    let grid = term.grid();
    let extra = grid.cell_extra(0, 0).expect("top-left covered");
    let image = extra.image().expect("image ref at (0,0)");
    assert_eq!(image.cell_row, 0);
    assert_eq!(image.cell_col, 0);
    assert_eq!(image.image.cols, 4);
    assert_eq!(image.image.rows, 3);
}

/// `preserveAspectRatio=0` (stretch to fill) is carried to the renderer; the
/// default and an explicit `=1` stay fit.
#[test]
fn preserve_aspect_ratio_zero_carries_stretch_and_the_default_fits() {
    use aterm_grid::ImageScaling;
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    for (args, want) in [
        ("inline=1;width=4;height=3", ImageScaling::Fit),
        (
            "inline=1;width=4;height=3;preserveAspectRatio=1",
            ImageScaling::Fit,
        ),
        (
            "inline=1;width=4;height=3;preserveAspectRatio=0",
            ImageScaling::Stretch,
        ),
    ] {
        let mut term = Terminal::new(10, 20);
        term.process(&osc_1337_file(args, &payload));
        let grid = term.grid();
        let image = grid
            .cell_extra(0, 0)
            .and_then(|extra| extra.image())
            .unwrap_or_else(|| panic!("{args}: placed"));
        assert_eq!(image.image.scaling, want, "{args}");
        assert_eq!(image.image.source_rect, None, "{args}: iTerm2 never crops");
        assert_eq!(image.kitty, None, "{args}: not a Kitty placement");
    }
}

#[test]
fn tile_coordinates_increase_across_the_footprint() {
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=3;height=2", &payload));
    let grid = term.grid();
    // Cell (row=1, col=2) is the bottom-right tile.
    let extra = grid.cell_extra(1, 2).expect("bottom-right covered");
    let image = extra.image().expect("image ref");
    assert_eq!(image.cell_row, 1);
    assert_eq!(image.cell_col, 2);
    // All tiles share the same underlying payload (one Arc).
    let a = grid.cell_extra(0, 0).unwrap().image().unwrap();
    let b = grid.cell_extra(1, 2).unwrap().image().unwrap();
    assert!(std::sync::Arc::ptr_eq(&a.image, &b.image));
}

#[test]
fn malformed_or_non_inline_draws_nothing() {
    // None of these may place an image, or panic.
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    let rows: [(&str, Vec<u8>); 6] = [
        // inline=0 means "download to disk": no on-screen effect.
        (
            "inline=0",
            osc_1337_file("inline=0;width=4;height=3", &payload),
        ),
        // No inline= argument at all: the default is "not inline".
        (
            "inline missing",
            osc_1337_file("width=4;height=3", &payload),
        ),
        // No ':' means no payload.
        (
            "no payload separator",
            b"\x1b]1337;File=inline=1;width=4;height=3\x1b\\".to_vec(),
        ),
        // '!' is not base64, so decoding fails.
        (
            "bad base64",
            b"\x1b]1337;File=inline=1;width=4;height=3:!!!!\x1b\\".to_vec(),
        ),
        (
            "empty payload",
            b"\x1b]1337;File=inline=1;width=4;height=3:\x1b\\".to_vec(),
        ),
        // A different OSC 1337 sub-command is not an image.
        (
            "SetUserVar",
            b"\x1b]1337;SetUserVar=foo=YmFy\x1b\\".to_vec(),
        ),
    ];
    for (label, bytes) in rows {
        let mut term = Terminal::new(10, 20);
        term.process(&bytes);
        assert_eq!(image_cell_count(&term), 0, "{label}");
    }
}

#[test]
fn footprint_clamps_to_grid_width() {
    let mut term = Terminal::new(10, 8);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    // Request 100 cols on an 8-col grid → clamps to 8.
    term.process(&osc_1337_file("inline=1;width=100;height=1", &payload));
    let grid = term.grid();
    let row0: usize = (0..grid.cols())
        .filter(|&c| grid.cell_extra(0, c).is_some_and(|e| e.image().is_some()))
        .count();
    assert_eq!(row0, 8, "width clamped to the 8-col grid");
}

#[test]
fn unknown_format_still_places_cells_but_renders_nothing() {
    // A non-PNG payload is stored (so the cursor advances consistently) and
    // classified Unknown; the renderer draws nothing for it.
    let mut term = Terminal::new(10, 20);
    let payload = b"GIF89a not really a gif".to_vec();
    term.process(&osc_1337_file("inline=1;width=2;height=2", &payload));
    let grid = term.grid();
    let image = grid.cell_extra(0, 0).unwrap().image().unwrap();
    assert_eq!(image.image.format, aterm_grid::ImageFormat::Unknown);
    assert_eq!(image_cell_count(&term), 4);
}

#[test]
fn cursor_advances_below_the_image() {
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=4;height=3", &payload));
    // After a 3-row image placed at row 0, the cursor sits below it (row 3),
    // column 0, so following text does not overprint the image.
    assert_eq!(term.grid().cursor_row(), 3);
    assert_eq!(term.grid().cursor_col(), 0);
}

#[test]
fn whitespace_in_payload_is_tolerated() {
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    let b64 = aterm_codec::base64::encode(&payload).expect("encode");
    // Insert newlines mid-payload (line-wrapped transfer) — must still decode.
    let mut seq = Vec::new();
    seq.extend_from_slice(b"\x1b]1337;File=inline=1;width=2;height=1:");
    for (i, ch) in b64.bytes().enumerate() {
        if i > 0 && i % 4 == 0 {
            seq.push(b'\n');
        }
        seq.push(ch);
    }
    seq.extend_from_slice(b"\x1b\\");
    term.process(&seq);
    assert_eq!(image_cell_count(&term), 2);
}

#[test]
fn large_inline_image_payload_decodes_intact() {
    // A ~512 KiB image (~683 KiB of base64) decodes intact: the parser's OSC
    // cap (MAX_OSC_DATA) is large enough for real inline images.
    let mut term = Terminal::new(10, 20);

    // Deterministic, non-trivial body so a truncated decode could not coincide.
    let body: Vec<u8> = (0..512 * 1024)
        .map(|i| (i as u8).wrapping_mul(31))
        .collect();
    let payload = [PNG_MAGIC, &body[..]].concat();

    term.process(&osc_1337_file("inline=1;width=4;height=3", &payload));

    // The image must have been placed (4×3 footprint).
    assert_eq!(image_cell_count(&term), 12, "large image must be placed");

    // The stored bytes equal the payload: the whole base64 body arrived.
    let grid = term.grid();
    let image = grid
        .cell_extra(0, 0)
        .and_then(|e| e.image())
        .expect("image ref at (0,0)");
    assert_eq!(
        image.image.bytes.len(),
        payload.len(),
        "decoded image must be the full payload, not a truncated prefix"
    );
    assert_eq!(
        &image.image.bytes[..],
        &payload[..],
        "decoded image bytes must match the original payload exactly"
    );
    assert_eq!(image.image.format, aterm_grid::ImageFormat::Png);
}

#[test]
fn single_arg_body_places_via_borrowed_payload() {
    // No ';' in the body → the parser yields exactly two OSC params and the
    // handler borrows both args and payload in place (zero-copy fast path).
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    let b64 = aterm_codec::base64::encode(&payload).expect("encode");
    let mut seq = b"\x1b]1337;File=inline=1:".to_vec();
    seq.extend_from_slice(b64.as_bytes());
    seq.extend_from_slice(b"\x1b\\");
    term.process(&seq);
    // width/height default to Auto; the header is unreadable pixel-wise beyond
    // the magic, so the footprint degrades to 1x1 — but it must be PLACED.
    assert!(image_cell_count(&term) >= 1);
}

#[test]
fn semicolon_after_payload_start_is_rejected() {
    // A ';' after the first ':' is malformed (base64 has no ';'): the rejoin
    // fallback reconstructs the body and the decode fails — nothing drawn.
    let mut term = Terminal::new(10, 20);
    term.process(b"\x1b]1337;File=inline=1;width=2;height=2:QUJD;RUY=\x1b\\");
    assert_eq!(image_cell_count(&term), 0);
}

/// An image far taller than the screen ends in the same state text flow
/// produces (the middle-band stamp skip in `place_image` is an optimization
/// only). On a 10-row grid a 40-row footprint scrolls 31 times: visible row
/// `r` (0..=8) holds footprint row `r + 31`, row 9 is blank, cursor at (9, 0).
#[test]
fn tall_image_final_state_matches_text_flow() {
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=3;height=40", &payload));
    let grid = term.grid();
    for r in 0..9u16 {
        for c in 0..3u16 {
            let image = grid
                .cell_extra(r, c)
                .and_then(|e| e.image())
                .unwrap_or_else(|| panic!("image ref expected at ({r},{c})"));
            assert_eq!(image.cell_row, r + 31, "visible tile row at ({r},{c})");
            assert_eq!(image.cell_col, c);
        }
    }
    assert_eq!(image_cell_count(&term), 27, "9 visible rows x 3 cols");
    assert_eq!(term.grid().cursor_row(), 9);
    assert_eq!(term.grid().cursor_col(), 0);
}

/// With a DECSTBM region, footprint rows stamped above the region survive,
/// even for a footprint tall enough to trigger the middle-band stamp skip.
#[test]
fn tall_image_above_scroll_region_keeps_leading_rows() {
    let mut term = Terminal::new(10, 20);
    // Scroll region rows 5..8 (1-based) = 4..=7 (0-based); cursor homes to (0,0).
    term.process(b"\x1b[5;8r");
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=2;height=40", &payload));
    let grid = term.grid();
    // Rows 0..=3 (above the region) keep the first four footprint rows.
    for r in 0..4u16 {
        let image = grid
            .cell_extra(r, 0)
            .and_then(|e| e.image())
            .unwrap_or_else(|| panic!("image ref expected at ({r},0)"));
        assert_eq!(image.cell_row, r, "above-region rows persist untouched");
    }
    // The region (rows 4..=7) scrolled: rows 4..=6 hold the last footprint
    // rows 37..=39; row 7 is the freshly scrolled-in blank line.
    for r in 4..7u16 {
        let image = grid
            .cell_extra(r, 0)
            .and_then(|e| e.image())
            .unwrap_or_else(|| panic!("image ref expected at ({r},0)"));
        assert_eq!(image.cell_row, r + 33, "region shows the footprint tail");
    }
    for r in 7..10u16 {
        assert!(
            grid.cell_extra(r, 0).and_then(|e| e.image()).is_none(),
            "row {r} must carry no image"
        );
    }
    assert_eq!(
        term.grid().cursor_row(),
        7,
        "cursor rests at the region bottom"
    );
    assert_eq!(term.grid().cursor_col(), 0);
}

#[test]
fn does_not_panic_on_truncated_sequences() {
    // A battery of malformed/truncated File= sequences must never panic.
    let cases: &[&[u8]] = &[
        b"\x1b]1337;File=\x1b\\",
        b"\x1b]1337;File=:\x1b\\",
        b"\x1b]1337;File=inline=1\x1b\\",
        b"\x1b]1337;File=width=:abc\x1b\\",
        b"\x1b]1337;\x1b\\",
        b"\x1b]1337\x1b\\",
        b"\x1b]1337;File=inline=1;width=999999999;height=999999999:QQ==\x1b\\",
    ];
    for case in cases {
        let mut term = Terminal::new(5, 10);
        term.process(case);
    }
}

/// A one-pixel PNG: the signature and an IHDR naming 1x1 (the engine reads no
/// further).
fn one_pixel_png() -> Vec<u8> {
    let mut png = PNG_MAGIC.to_vec();
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&1u32.to_be_bytes());
    png.extend_from_slice(&1u32.to_be_bytes());
    png
}

/// A footprint is never taller than the screen or the tallest raster that
/// decodes (4096 px), whichever is more, because the renderers decode a
/// placement into one buffer of the footprint's pixel size: `height=4096`
/// (cells) on a one-pixel PNG would ask each draw for 800 x 81,920 px, 262 MB, at
/// this grid. A request within those bounds keeps its height.
#[test]
fn a_footprint_is_no_taller_than_the_screen_or_the_decodable_height() {
    let cell_h = 20;
    // 4096 px is 205 rows of 20 px, more than the screen's 24.
    let tallest = 4096_u16.div_ceil(cell_h);
    for (args, want) in [
        ("width=80;height=4096", tallest),
        ("width=80;height=300", tallest),
        ("width=80;height=100", 100),
        ("width=80;height=5000px", tallest),
        // Height from the width and the picture's aspect: 800 px of a square.
        ("width=80", 40),
        ("width=4096", tallest),
    ] {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, cell_h);
        term.process(&osc_1337_file(
            &format!("inline=1;{args}"),
            &one_pixel_png(),
        ));
        let image = term
            .grid()
            .cell_extra(0, 0)
            .and_then(|e| e.image().cloned())
            .expect("placed");
        assert_eq!(image.image.rows, want, "{args}");
    }
    // The screen itself is always allowed: a grid taller than 4096 px.
    let mut term = Terminal::new(300, 80);
    term.set_cell_pixel_size(10, cell_h);
    term.process(&osc_1337_file(
        "inline=1;width=80;height=4096",
        &one_pixel_png(),
    ));
    let image = term
        .grid()
        .cell_extra(0, 0)
        .and_then(|e| e.image().cloned());
    assert_eq!(image.map(|i| i.image.rows), Some(300));
}

/// A DECGRA-only sixel defers the raster allocation to `unhook`, bypassing the
/// DCS stream budget. A declared 2100x2100 raster is ~17.6 MB of RGBA, over the
/// 16 MiB `MAX_IMAGE_BYTES` cap, so `place_sixel_image` rejects it and stamps
/// no cells.
#[cfg(feature = "sixel")]
#[test]
fn decgra_only_sixel_over_image_cap_is_rejected() {
    let mut term = Terminal::new(10, 20);
    // DCS q  " Pan;Pad;Ph;Pv  ST  — declares a 2100x2100 raster, paints nothing.
    term.process(b"\x1bPq\"1;1;2100;2100\x1b\\");
    assert_eq!(
        image_cell_count(&term),
        0,
        "a DECGRA-only sixel exceeding MAX_IMAGE_BYTES must place no cells"
    );
}

/// The payload bytes of the distinct images the visible screen shows, every
/// layer of every cell counted once per image.
#[cfg(feature = "sixel")]
fn live_image_bytes(term: &Terminal) -> usize {
    let grid = term.grid();
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0;
    for r in 0..grid.rows() {
        for c in 0..grid.cols() {
            for layer in grid.cell_extra(r, c).map_or(&[][..], |e| e.images()) {
                if seen.insert(std::sync::Arc::as_ptr(&layer.image)) {
                    bytes += layer.image.payload_bytes();
                }
            }
        }
    }
    bytes
}

/// The images a screen shows are bounded as a whole, whatever each costs to
/// send. A sixel declaring a 4096x20 raster (`"1;1;4096;20`, no pixel data)
/// takes the rest of its row, so placing one a column further right each time
/// leaves every one of them a cell, and its whole 320 KiB raster, on screen:
/// 1,840 of them, 65 KB of output, would hold 603 MB without a screen budget.
#[cfg(feature = "sixel")]
#[test]
fn a_screen_of_sixels_keeps_its_images_within_the_live_budget() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let raster = 4096 * 20 * 4;
    // DECSDM: each sixel is drawn at the cursor, which stays put. The last row
    // is left alone: a placement there scrolls the screen.
    term.process(b"\x1b[?80h");
    for row in 1..=23u16 {
        for col in 1..=80u16 {
            term.process(format!("\x1b[{row};{col}H\x1bPq\"1;1;4096;20\x1b\\").as_bytes());
            let live = live_image_bytes(&term);
            assert!(
                live <= super::MAX_LIVE_IMAGE_BYTES + raster,
                "at row {row}, column {col} the screen holds {live} bytes of images"
            );
        }
    }
    let newest = term.grid().cell_extra(22, 79).and_then(|e| e.image());
    assert!(
        newest.is_some_and(|image| image.image.bytes.len() == raster),
        "the image just placed is never the one taken off"
    );
}

/// Each screen's images are bounded, however a program moves between them. A
/// count sees only the active grid, so one charge shared by both screens would
/// be reset by a sixel on the near-empty alternate screen (mode 47 keeps both
/// buffers), and the parked main screen never counted again: hopping over after
/// every placement, it would hold all 1,840 rasters, 603 MB.
#[cfg(feature = "sixel")]
#[test]
fn each_screen_keeps_its_images_within_the_live_budget() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, 20);
    let raster = 4096 * 20 * 4;
    let bound = super::MAX_LIVE_IMAGE_BYTES + raster;
    term.process(b"\x1b[?80h");
    for row in 1..=23u16 {
        for col in 1..=80u16 {
            term.process(format!("\x1b[{row};{col}H\x1bPq\"1;1;4096;20\x1b\\").as_bytes());
            let main = live_image_bytes(&term);
            assert!(
                main <= bound,
                "at row {row}, column {col} the main screen holds {main} bytes of images"
            );
            // Over to the alternate screen for one sixel, always on its first
            // row, and back.
            term.process(b"\x1b[?47h\x1b[1;1H\x1bPq\"1;1;4096;20\x1b\\");
            let alternate = live_image_bytes(&term);
            assert!(
                alternate <= bound,
                "the alternate screen holds {alternate} bytes of images"
            );
            term.process(b"\x1b[?47l");
        }
    }
}
