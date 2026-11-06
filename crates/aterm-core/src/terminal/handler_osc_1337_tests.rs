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

/// A painted 4096x100 sixel (1.6 MiB RGBA raster) used by the live-budget tests: 16 full bands
/// and a last one of 4 rows.
fn row_sixel() -> Vec<u8> {
    let mut sixel = b"\x1bPq".to_vec();
    sixel.extend_from_slice(&b"!4096~-".repeat(16));
    sixel.extend_from_slice(b"!4096N\x1b\\");
    sixel
}
const ROW_SIXEL_BYTES: usize = 4096 * 100 * 4;
const SIXEL_CELL_PX: u16 = 100;
/// 1-based cell coordinates where the live-budget tests place [`row_sixel`]s.
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

/// A screen's images are bounded as a whole ([`row_sixel`]).
#[test]
fn a_screen_of_sixels_keeps_its_images_within_the_live_budget() {
    let mut term = Terminal::new(24, 80);
    term.set_cell_pixel_size(10, SIXEL_CELL_PX);
    let raster = ROW_SIXEL_BYTES;
    let sixel = row_sixel();
    term.process(b"\x1b[?80h");
    for (row, col) in SIXEL_CELLS {
        term.process(format!("\x1b[{row};{col}H").as_bytes());
        term.process(&sixel);
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
    let sixel = row_sixel();
    term.process(b"\x1b[?80h");
    for (row, col) in SIXEL_CELLS {
        term.process(format!("\x1b[{row};{col}H").as_bytes());
        term.process(&sixel);
        let main = live_image_bytes(&term);
        assert!(
            main <= bound,
            "at row {row}, column {col} the main screen holds {main} bytes of images"
        );
        term.process(b"\x1b[?47h\x1b[1;1H");
        term.process(&sixel);
        let alternate = live_image_bytes(&term);
        assert!(
            alternate <= bound,
            "the alternate screen holds {alternate} bytes of images"
        );
        term.process(b"\x1b[?47l");
    }
}

/// The placed image covering visible cell `(row, col)`, if any.
fn image_at(term: &Terminal, row: u16, col: u16) -> Option<std::sync::Arc<aterm_grid::ImageData>> {
    let extra = term.grid().cell_extra(row, col)?;
    extra
        .image()
        .map(|layer| std::sync::Arc::clone(&layer.image))
}

/// A DECGRA declaration reserves a footprint but holds bytes only for the pixels data paints: a
/// 19-byte declaration of 4096x1024 is placed over its 64 rows as one transparent pixel.
#[test]
fn a_declared_sixel_holds_only_the_pixels_it_paints() {
    let mut term = Terminal::new(10, 20);
    term.set_cell_pixel_size(8, 16);
    term.process(b"\x1bPq\"1;1;4096;1024\x1b\\");
    let image = image_at(&term, 0, 0).expect("the declared footprint is placed");
    assert_eq!(image.rows, 64, "the footprint is the declared raster's");
    assert!(
        image.payload_bytes() <= 4,
        "an unpainted 4096x1024 declaration holds {} bytes",
        image.payload_bytes()
    );
    term.process(b"\x1b[H\x1bPq\"1;1;4096;1024#1~\x1b\\");
    let image = image_at(&term, 0, 0).expect("the painted column is placed");
    assert_eq!(image.bytes.len(), 4 * 6, "one painted 1x6 column");
}

/// A 4096x16 sixel: 256 KiB of RGBA one 16-pixel cell row tall.
const BAND_SIXEL: &[u8] = b"\x1bPq#1!4096~-!4096~-!4096N\x1b\\";
const BAND_SIXEL_BYTES: usize = 4096 * 16 * 4;

/// Payload bytes of the distinct images the primary history references (hot tier, staged rows
/// and ring), each counted whole however few of its rows remain.
fn history_image_payload(term: &Terminal) -> usize {
    let mut seen = std::collections::HashSet::new();
    term.main_grid()
        .history_images()
        .filter(|span| seen.insert(std::sync::Arc::as_ptr(&span.image)))
        .map(|span| span.image.payload_bytes())
        .sum()
}

/// A tab with a 1000-row ring over a tiered store, its scrollback budget set to `budget`.
fn tiered(rows: u16, cols: u16, cell: (u16, u16), budget: usize) -> Terminal {
    let mut term = Terminal::with_scrollback(
        rows,
        cols,
        1000,
        crate::scrollback::Scrollback::with_defaults(),
    );
    term.set_cell_pixel_size(cell.0, cell.1);
    term.set_memory_budget(budget).expect("budget");
    term
}

/// Sixels that scroll into history are held to the scrollback budget, oldest taken off first,
/// however many rows the ring has room for.
#[test]
fn a_sixel_flood_keeps_history_images_within_the_scrollback_budget() {
    let budget = 4 << 20;
    let mut term = tiered(24, 80, (8, 16), budget);
    let first = {
        term.process(BAND_SIXEL);
        image_at(&term, 0, 0).expect("the first band shows")
    };
    // What may scroll in between two measures: the screen's 24 rows of bands and the room's
    // floor, past the budget itself.
    let bound = budget + 24 * BAND_SIXEL_BYTES + (1 << 20) + BAND_SIXEL_BYTES;
    for n in 1..240 {
        term.process(BAND_SIXEL);
        let held = history_image_payload(&term);
        assert!(
            held <= bound,
            "after {n} bands history holds {held} bytes of images (bound {bound})"
        );
    }
    assert!(
        !term
            .main_grid()
            .history_images()
            .any(|span| std::sync::Arc::ptr_eq(&span.image, &first)),
        "the oldest band is taken off first"
    );
    assert!(
        history_image_payload(&term) >= budget / 2,
        "the newest bands stay"
    );
}

/// History counts a picture whole however few of its rows remain: tall sixels erased down to
/// the one row each pushed into history still hold every byte of their rasters there.
#[test]
fn a_picture_cut_down_to_one_history_row_counts_whole() {
    let budget = 1 << 20;
    let mut term = tiered(6, 20, (10, 10), budget);
    // 1000x60: six 10-pixel rows, 240,000 bytes of RGBA.
    let mut tall = b"\x1bPq#1".to_vec();
    tall.extend_from_slice(&b"!1000~-".repeat(10));
    tall.extend_from_slice(b"\x1b\\");
    let raster = 1000 * 60 * 4;
    let bound = budget + (1 << 20) + raster;
    for n in 0..60 {
        term.process(b"\x1b[H");
        term.process(&tall);
        term.process(b"\x1b[H\x1b[J");
        let held = history_image_payload(&term);
        assert!(
            held <= bound,
            "after {n} pictures history holds {held} bytes of images (bound {bound})"
        );
    }
}

/// The pressure shed sees history's images in the reclaimable bytes and takes them off.
#[test]
fn a_pressure_shed_reclaims_history_images() {
    let budget = 8 << 20;
    let mut term = tiered(24, 80, (8, 16), budget);
    for _ in 0..40 {
        term.process(BAND_SIXEL);
    }
    let held = history_image_payload(&term);
    assert!(
        held >= 16 * BAND_SIXEL_BYTES,
        "history holds {held} bytes of images"
    );
    let shed = term.shed_history_scrollback(|_| 0).expect("a tiered store");
    assert!(
        shed.freed() >= held,
        "the shed freed {} of the {held} image bytes",
        shed.freed()
    );
    assert_eq!(history_image_payload(&term), 0);
}

/// A measure of history's images walks every history row, picture or not, and the room it leaves
/// the screen pays for that walk, so a long text history is not walked again per megabyte placed.
#[test]
fn a_history_measure_leaves_room_for_its_walk() {
    let budget = 64 << 10;
    let mut term = Terminal::with_scrollback(
        24,
        80,
        50_000,
        crate::scrollback::Scrollback::with_defaults(),
    );
    term.set_cell_pixel_size(8, 16);
    term.set_memory_budget(budget).expect("budget");
    term.process(&b"\n".repeat(50_000));
    term.process(BAND_SIXEL);
    let rows = term.grid().scrollback_lines();
    assert!(rows > 40_000, "premise: {rows} history rows");
    let found =
        super::measure_history_images(term.grid_mut(), &std::collections::HashSet::new(), budget);
    let walk = rows * std::mem::size_of::<aterm_scrollback::ImageSpan>();
    assert!(
        found.room(budget) >= walk,
        "a measure walking {rows} rows leaves {} bytes of room, under the {walk} its walk is worth",
        found.room(budget)
    );
}

/// A picture whose row has moved past the ring into the store's hot tier, which books a share of
/// it per row, adds its payload to the reclaimable history bytes once, not twice.
#[test]
fn a_picture_in_the_hot_tier_counts_once_in_the_history_bytes() {
    let history_bytes = |picture: &[u8]| {
        let mut term = tiered(4, 80, (8, 16), 64 << 20);
        term.process(picture);
        term.process(&b"\n".repeat(1100));
        let bytes = term.history_scrollback_bytes().expect("a tiered store");
        let in_store = term
            .main_grid()
            .scrollback()
            .into_iter()
            .flat_map(aterm_scrollback::ScrollbackStorage::images)
            .count();
        (bytes, in_store)
    };
    let (with, in_store) = history_bytes(BAND_SIXEL);
    let (without, _) = history_bytes(b"\r\n");
    assert_eq!(in_store, 1, "premise: the picture's row is in the hot tier");
    let added = with - without;
    assert!(
        (BAND_SIXEL_BYTES..BAND_SIXEL_BYTES + 1024).contains(&added),
        "the picture added {added} history bytes for its {BAND_SIXEL_BYTES} payload bytes"
    );
}

/// Pictures scrolled off the top of the screen are held by history alone, so its budget bounds
/// them: rows of 80 small sixels on the top line, each row scrolled off at once, on the primary
/// screen (into history) and on the alternate screen (which keeps none).
#[test]
fn sixels_scrolled_off_the_top_stay_within_the_history_budget() {
    // 64x16: 4 KiB of RGBA, eight 8x16 cells wide; each row's later sixels cover all but the
    // first cell of the one before, so a row holds 80 of them.
    const SMALL_SIXEL: &[u8] = b"\x1bPq#1!64~-!64~-!64N\x1b\\";
    const SMALL_SIXEL_BYTES: usize = 64 * 16 * 4;
    let budget = 64 << 10;
    for (screen, enter, bound) in [
        (
            "primary",
            &b""[..],
            budget + 80 * SMALL_SIXEL_BYTES + (1 << 20) + SMALL_SIXEL_BYTES,
        ),
        ("alternate", &b"\x1b[?1049h"[..], 80 * SMALL_SIXEL_BYTES),
    ] {
        let mut term = tiered(24, 80, (8, 16), budget);
        term.process(enter);
        let mut placed = Vec::new();
        for _ in 0..40 {
            for col in 1..=80u16 {
                term.process(format!("\x1b[1;{col}H").as_bytes());
                term.process(SMALL_SIXEL);
                let image = image_at(&term, 0, col - 1).expect("the sixel shows");
                placed.push(std::sync::Arc::downgrade(&image));
            }
            term.process(b"\x1b[24H\n");
        }
        let alive: usize = placed
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .map(|image| image.payload_bytes())
            .sum();
        assert!(
            alive <= bound,
            "{screen}: {} placed pictures keep {alive} bytes alive (bound {bound})",
            placed.len()
        );
    }
}
