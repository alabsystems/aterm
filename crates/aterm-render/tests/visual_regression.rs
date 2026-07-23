// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Visual regression: render a controlled grid to real pixels and assert
// SEMANTIC properties of each cell (a colour is present, a glyph was drawn),
// not a brittle golden PNG. This is aterm's `read_image` oracle turned into an
// automated gate — the AI-visibility loop, codified. It locks in:
//   - foreground colour (red text renders red pixels)
//   - background colour (blue-bg cell fills blue)
//   - inverse video (cell background becomes the light fg colour)
//   - Unicode font fallback (CJK draws glyph pixels instead of going blank)
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

fn render_demo() -> (Frame, usize, usize) {
    let Some(mut rend) = Renderer::from_system(18.0, Theme::default()) else {
        panic!("SKIP-VIA-PANIC: no system monospace font");
    };
    // Deterministic pixels: block on the lazy fallback parses so CJK/emoji
    // probes assert against real glyphs, not a provisional `.notdef`.
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
    let frame = rend.render_input(&term.cell_frame(rows, cols));
    (frame, cw, ch)
}

#[test]
fn red_text_renders_red_pixels() {
    if std::env::var("ATERM_NO_FONT").is_ok() {
        return;
    }
    let (f, cw, ch) = render_demo();
    let px = cell_pixels(&f, cw, ch, 0, 0); // the first 'R'
    let red = px.iter().any(|&p| r(p) > 140 && g(p) < 90 && b(p) < 90);
    assert!(red, "expected red glyph pixels in cell (0,0)");
}

#[test]
fn blue_background_fills_cell() {
    if std::env::var("ATERM_NO_FONT").is_ok() {
        return;
    }
    let (f, cw, ch) = render_demo();
    let px = cell_pixels(&f, cw, ch, 1, 0); // blue-bg space
    // A space on blue bg: the whole cell should be blue-ish, far from theme bg.
    let blue = px.iter().filter(|&&p| b(p) > 110 && r(p) < 90).count();
    assert!(
        blue > px.len() / 2,
        "expected most of cell (1,0) to be blue background ({}/{} blue)",
        blue,
        px.len()
    );
}

#[test]
fn inverse_video_lightens_background() {
    if std::env::var("ATERM_NO_FONT").is_ok() {
        return;
    }
    let (f, cw, ch) = render_demo();
    let px = cell_pixels(&f, cw, ch, 3, 0); // inverse 'X'
    // Inverse swaps fg/bg: the cell background becomes the light fg (~0xD0D0D0).
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
}

#[test]
fn cjk_glyph_renders_via_font_fallback() {
    if std::env::var("ATERM_NO_FONT").is_ok() {
        return;
    }
    let (f, cw, ch) = render_demo();
    // The primary monospace face has no CJK glyph; the Unicode fallback must
    // draw 日 so the cell is NOT blank. This is the regression lock for the
    // font-fallback fix — a blank cell here means fallback is broken.
    let px = cell_pixels(&f, cw, ch, 2, 0); // 日 (lead cell of the wide char)
    let drawn = non_bg_count(&px);
    assert!(
        drawn > 12,
        "CJK cell (2,0) is blank ({drawn} non-bg pixels) — font fallback regressed"
    );
}

#[test]
fn blank_cell_stays_background() {
    if std::env::var("ATERM_NO_FONT").is_ok() {
        return;
    }
    let (f, cw, ch) = render_demo();
    // Control: an untouched cell (row 5) must be (near-)pure theme background.
    // Guards the other tests against a "everything is non-bg" false pass.
    let px = cell_pixels(&f, cw, ch, 5, 8);
    let drawn = non_bg_count(&px);
    assert!(
        drawn < px.len() / 20,
        "blank cell (5,8) should be background ({drawn} non-bg)"
    );
}
