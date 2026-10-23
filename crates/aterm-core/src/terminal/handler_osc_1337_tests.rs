// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the OSC 1337 `File=` inline-image handler.

use super::super::Terminal;

/// Minimal PNG magic prefix so [`detect_format`](super::detect_format) classifies the payload as PNG.
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
    assert_eq!(image_cell_count(&term), 12);
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
fn malformed_or_non_inline_draws_nothing() {
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    let rows: [(&str, Vec<u8>); 6] = [
        (
            "inline=0",
            osc_1337_file("inline=0;width=4;height=3", &payload),
        ),
        (
            "inline missing",
            osc_1337_file("width=4;height=3", &payload),
        ),
        (
            "no payload separator",
            b"\x1b]1337;File=inline=1;width=4;height=3\x1b\\".to_vec(),
        ),
        (
            "bad base64",
            b"\x1b]1337;File=inline=1;width=4;height=3:!!!!\x1b\\".to_vec(),
        ),
        (
            "empty payload",
            b"\x1b]1337;File=inline=1;width=4;height=3:\x1b\\".to_vec(),
        ),
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
    term.process(&osc_1337_file("inline=1;width=100;height=1", &payload));
    let grid = term.grid();
    let row0: usize = (0..grid.cols())
        .filter(|&c| grid.cell_extra(0, c).is_some_and(|e| e.image().is_some()))
        .count();
    assert_eq!(row0, 8, "width clamped to the 8-col grid");
}

#[test]
fn unknown_format_still_places_cells_but_renders_nothing() {
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
    assert_eq!(term.grid().cursor_row(), 3);
    assert_eq!(term.grid().cursor_col(), 0);
}

#[test]
fn whitespace_in_payload_is_tolerated() {
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    let b64 = aterm_codec::base64::encode(&payload).expect("encode");
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
    let mut term = Terminal::new(10, 20);

    let body: Vec<u8> = (0..512 * 1024)
        .map(|i| (i as u8).wrapping_mul(31))
        .collect();
    let payload = [PNG_MAGIC, &body[..]].concat();

    term.process(&osc_1337_file("inline=1;width=4;height=3", &payload));

    assert_eq!(image_cell_count(&term), 12, "large image must be placed");

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
    let mut term = Terminal::new(10, 20);
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    let b64 = aterm_codec::base64::encode(&payload).expect("encode");
    let mut seq = b"\x1b]1337;File=inline=1:".to_vec();
    seq.extend_from_slice(b64.as_bytes());
    seq.extend_from_slice(b"\x1b\\");
    term.process(&seq);
    assert!(image_cell_count(&term) >= 1);
}

#[test]
fn semicolon_after_payload_start_is_rejected() {
    let mut term = Terminal::new(10, 20);
    term.process(b"\x1b]1337;File=inline=1;width=2;height=2:QUJD;RUY=\x1b\\");
    assert_eq!(image_cell_count(&term), 0);
}

/// An image taller than the screen scrolls the grid and leaves the final visible rows matching text flow.
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

/// With a DECSTBM region, footprint rows stamped above the region survive.
#[test]
fn tall_image_above_scroll_region_keeps_leading_rows() {
    let mut term = Terminal::new(10, 20);
    term.process(b"\x1b[5;8r");
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=2;height=40", &payload));
    let grid = term.grid();
    for r in 0..4u16 {
        let image = grid
            .cell_extra(r, 0)
            .and_then(|e| e.image())
            .unwrap_or_else(|| panic!("image ref expected at ({r},0)"));
        assert_eq!(image.cell_row, r, "above-region rows persist untouched");
    }
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

/// Minimal 1x1 PNG header.
fn one_pixel_png() -> Vec<u8> {
    let mut png = PNG_MAGIC.to_vec();
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&1u32.to_be_bytes());
    png.extend_from_slice(&1u32.to_be_bytes());
    png
}

/// Image footprints are clamped to the screen height or `MAX_DECODED_IMAGE_PX`, whichever is larger.
#[test]
fn a_footprint_is_no_taller_than_the_screen_or_the_decodable_height() {
    let cell_h = 20;
    let tallest = 4096_u16.div_ceil(cell_h);
    for (args, want) in [
        ("width=80;height=4096", tallest),
        ("width=80;height=300", tallest),
        ("width=80;height=100", 100),
        ("width=80;height=5000px", tallest),
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

/// A DECGRA-only sixel whose declared dimensions exceed `MAX_IMAGE_BYTES` places no cells.
#[test]
fn decgra_only_sixel_over_image_cap_is_rejected() {
    let mut term = Terminal::new(10, 20);
    term.process(b"\x1bPq\"1;1;2100;2100\x1b\\");
    assert_eq!(
        image_cell_count(&term),
        0,
        "a DECGRA-only sixel exceeding MAX_IMAGE_BYTES must place no cells"
    );
}

/// Total payload bytes of distinct images currently referenced on the visible screen.
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

/// Declared 4096x100 sixel (1.6 MiB RGBA raster) used by the live-budget tests.
const ROW_SIXEL: &[u8] = b"\x1bPq\"1;1;4096;100\x1b\\";
const ROW_SIXEL_BYTES: usize = 4096 * 100 * 4;
const SIXEL_CELL_PX: u16 = 100;
/// 1-based cell coordinates where the live-budget tests place [`ROW_SIXEL`]s.
const SIXEL_CELLS: [(u16, u16); 160] = {
    let mut cells = [(0, 0); 160];
    let mut i = 0;
    while i < cells.len() {
        cells[i] = (1 + i as u16 / 20, 1 + i as u16 % 20);
        i += 1;
    }
    cells
};
const _: () = assert!(SIXEL_CELLS.len() * ROW_SIXEL_BYTES > super::MAX_LIVE_IMAGE_BYTES * 3 / 2);

/// A screen's images are bounded as a whole ([`ROW_SIXEL`]).
#[test]
fn a_screen_of_sixels_keeps_its_images_within_the_live_budget() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, SIXEL_CELL_PX);
    let raster = ROW_SIXEL_BYTES;
    term.process(b"\x1b[?80h");
    for (row, col) in SIXEL_CELLS {
        term.process(format!("\x1b[{row};{col}H").as_bytes());
        term.process(ROW_SIXEL);
        let live = live_image_bytes(&term);
        assert!(
            live <= super::MAX_LIVE_IMAGE_BYTES + raster,
            "at row {row}, column {col} the screen holds {live} bytes of images"
        );
    }
    let newest = term.grid().cell_extra(7, 19).and_then(|e| e.image());
    assert!(
        newest.is_some_and(|image| image.image.bytes.len() == raster),
        "the image just placed is never the one taken off"
    );
}

/// Each screen tracks and enforces its own live image byte budget independently across mode 47 switches.
#[test]
fn each_screen_keeps_its_images_within_the_live_budget() {
    const _: () = assert!((super::MAX_LIVE_IMAGE_BYTES / ROW_SIXEL_BYTES) % 2 == 1);
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, SIXEL_CELL_PX);
    let bound = super::MAX_LIVE_IMAGE_BYTES + ROW_SIXEL_BYTES;
    term.process(b"\x1b[?80h");
    for (row, col) in SIXEL_CELLS {
        term.process(format!("\x1b[{row};{col}H").as_bytes());
        term.process(ROW_SIXEL);
        let main = live_image_bytes(&term);
        assert!(
            main <= bound,
            "at row {row}, column {col} the main screen holds {main} bytes of images"
        );
        term.process(b"\x1b[?47h\x1b[1;1H");
        term.process(ROW_SIXEL);
        let alternate = live_image_bytes(&term);
        assert!(
            alternate <= bound,
            "the alternate screen holds {alternate} bytes of images"
        );
        term.process(b"\x1b[?47l");
    }
}
