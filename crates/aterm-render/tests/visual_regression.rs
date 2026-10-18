// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The CPU render pipeline, end to end, in ONE rendered frame: render a
// controlled grid to real pixels and assert SEMANTIC properties of each cell
// (a colour is present, a glyph was drawn), not a brittle golden PNG. This is
// aterm's `read_image` oracle (ATERM_DESIGN §8) turned into an automated gate.
// It locks in:
//   - foreground colour (red text renders red pixels)
//   - background colour (blue-bg cell fills blue)
//   - inverse video (cell background becomes the light fg colour)
//   - Unicode font fallback (CJK draws glyph pixels instead of going blank)
//   - a blank cell stays background (the control that keeps the rows above
//     from passing on an "everything is non-bg" frame)
//   - `read_image`: the frame encodes as a valid PNG that decodes back to the
//     exact rendered size and the exact 0xTTRRGGBB → RGBA projection
// against the theme's dark background.

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme};

const BG: u32 = 0x0011_1318; // Theme::default().bg

fn r(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn g(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn b(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// Manhattan distance between two packed RGB colours.
fn dist(a: u32, c: u32) -> i32 {
    (r(a) - r(c)).abs() + (g(a) - g(c)).abs() + (b(a) - b(c)).abs()
}

/// All pixels inside cell (row, col), given cell size.
fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

/// How many pixels in the cell differ meaningfully from the theme background.
fn non_bg_count(px: &[u32]) -> usize {
    px.iter().filter(|&&p| dist(p, BG) > 24).count()
}

#[test]
fn the_render_pipeline_draws_and_reads_back_what_it_was_fed() {
    if std::env::var("ATERM_NO_FONT").is_ok() {
        aterm_spec::host::could_not_run("ATERM_NO_FONT is set: this run has no font to render");
    }
    let Some(mut rend) = Renderer::from_system(18.0, Theme::default()) else {
        aterm_spec::host::could_not_run("no system monospace font");
    };
    // Deterministic pixels: block on the lazy fallback parses so CJK/emoji
    // probes assert against real glyphs, not a provisional `.notdef`, and so a
    // lazy parse landing between the live and replay renders cannot make the
    // two frames legitimately differ.
    rend.debug_block_on_lazy_fallbacks();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // row0: red "RR"        row1: blue-bg "  "     row2: CJK "日本"
    // row3: inverse "XX"    row4: plain "ab"       (row5 left blank)
    term.process(
        b"\x1b[31mRR\x1b[0m\r\n\
\x1b[44m  \x1b[0m\r\n\
\xe6\x97\xa5\xe6\x9c\xac\r\n\
\x1b[7mXX\x1b[0m\r\n\
ab\r\n",
    );
    let (cw, ch) = rend.cell_size();
    let f = rend.render_input(&term.cell_frame(rows, cols));

    // Foreground colour: the first 'R' draws red glyph pixels.
    let px = cell_pixels(&f, cw, ch, 0, 0);
    let red = px.iter().any(|&p| r(p) > 140 && g(p) < 90 && b(p) < 90);
    assert!(red, "expected red glyph pixels in cell (0,0)");

    // Background colour: a space on blue bg fills most of its cell blue.
    let px = cell_pixels(&f, cw, ch, 1, 0);
    let blue = px.iter().filter(|&&p| b(p) > 110 && r(p) < 90).count();
    assert!(
        blue > px.len() / 2,
        "expected most of cell (1,0) to be blue background ({}/{} blue)",
        blue,
        px.len()
    );

    // Inverse video: the cell background becomes the light fg (~0xD0D0D0).
    let px = cell_pixels(&f, cw, ch, 3, 0);
    let light = px
        .iter()
        .filter(|&&p| r(p) > 150 && g(p) > 150 && b(p) > 150)
        .count();
    assert!(
        light > px.len() / 3,
        "expected inverse cell (3,0) to have a light background ({}/{} light)",
        light,
        px.len()
    );

    // Font fallback: the primary monospace face has no CJK glyph; the Unicode
    // fallback must draw 日 so the cell is NOT blank. A blank cell here means
    // fallback is broken.
    let drawn = non_bg_count(&cell_pixels(&f, cw, ch, 2, 0));
    assert!(
        drawn > 12,
        "CJK cell (2,0) is blank ({drawn} non-bg pixels) — font fallback regressed"
    );

    // Control: an untouched cell (row 5) must be (near-)pure theme background.
    let px = cell_pixels(&f, cw, ch, 5, 8);
    let drawn = non_bg_count(&px);
    assert!(
        drawn < px.len() / 20,
        "blank cell (5,8) should be background ({drawn} non-bg)"
    );

    // read_image: the rendered screen encodes as a valid PNG that round-trips.
    let bytes = f.to_png();
    assert_eq!(
        &bytes[..8],
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
        "PNG magic"
    );
    let decoder = aterm_png::Decoder::new(std::io::Cursor::new(&bytes));
    let mut reader = decoder.read_info().expect("decode header");
    let info = reader.info();
    assert_eq!(
        (info.width as usize, info.height as usize),
        (f.width, f.height)
    );
    assert_eq!(
        info.color_type,
        aterm_png::ColorType::Rgba,
        "read_image retains framebuffer alpha"
    );
    let mut decoded = vec![0; reader.output_buffer_size()];
    let output = reader.next_frame(&mut decoded).expect("decode pixels");
    assert_eq!(
        &decoded[..output.buffer_size()],
        f.rgba_bytes().as_slice(),
        "PNG pixels are the exact 0xTTRRGGBB → RGBA projection"
    );
}
