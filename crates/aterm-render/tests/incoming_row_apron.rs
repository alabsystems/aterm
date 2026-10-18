// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incoming-row apron (CPU): during a sub-row up-glide the exposed bottom strip shows the
//! top `frac` px of the row sliding in from below the viewport, raster-exact. The oracle is
//! the engine: the apron at offset `d` is the bottom viewport row at offset `d - 1`.
//!
//! The bundled DejaVu face with runtime font discovery off keeps this machine-independent;
//! the fixture text is ASCII caps and digits, which DejaVu covers.

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme, WindowCpu};

const ROWS: usize = 8;
const COLS: usize = 20;
const BG: u32 = 0x0011_1318; // Theme::default().bg

fn renderer() -> Renderer {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset");
    let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).expect("fixture parses");
    r.set_runtime_font_discovery(false);
    r.debug_block_on_lazy_fallbacks();
    r
}

/// Distinct, descender-free text per line (caps + digits only), so a row's
/// ink never overshoots its own band and the per-row oracle is exact at the
/// row seam. The cursor is hidden so no oracle row carries a caret.
fn seeded_term() -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    t.process(b"\x1b[?25l");
    for i in 0..40u8 {
        let tag = (b'A' + i % 26) as char;
        t.process(format!("LINE {i:02} ABCDEF {tag}{tag}\r\n").as_bytes());
    }
    t
}

/// The partitioned input at the terminal's current offset: row 0 is a pinned
/// (top) chrome row, the band reaches the frame's last row.
fn partitioned(t: &mut Terminal, frac: i32) -> aterm_core::render::RenderInput {
    let mut input = t.cell_frame(ROWS, COLS);
    input.grid_top_row = 1;
    input.grid_bot_row = ROWS;
    input.scroll_frac_px = frac;
    input
}

fn rows_of(f: &Frame, y0: usize, n: usize) -> &[u32] {
    &f.pixels[y0 * f.width..(y0 + n) * f.width]
}

fn rows_of_px(px: &[u32], w: usize, y0: usize, n: usize) -> &[u32] {
    &px[y0 * w..(y0 + n) * w]
}

#[test]
fn up_glide_strip_shows_the_incoming_rows_top_pixels() {
    let mut r = renderer();
    let (_cw, ch) = r.cell_size();
    let grid_top = r.grid_top();
    let frac = (ch / 2).max(1) as i32;
    let n = frac as usize;
    let d = 3;

    // The glide frame at offset d.
    let mut t = seeded_term();
    t.scroll_display(d);
    let glide_in = partitioned(&mut t, frac);
    assert_eq!(glide_in.display_offset, d);
    assert!(
        glide_in.apron_row.present,
        "scrolled back: a row lies below the viewport"
    );
    let glide = r.render_input(&glide_in);
    // The untranslated frame at offset d.
    let flat = r.render_input(&partitioned(&mut t, 0));
    // The whole-row oracle at offset d - 1: its bottom grid row is the apron.
    let mut o = seeded_term();
    o.scroll_display(d - 1);
    let oracle = r.render_input(&partitioned(&mut o, 0));
    assert_eq!((glide.width, glide.height), (oracle.width, oracle.height));

    let w = glide.width;
    let y1 = grid_top + ROWS * ch; // grid_bot_row == ROWS
    let strip = rows_of(&glide, y1 - n, n);
    let expected = rows_of(&oracle, y1 - ch, n);
    assert_eq!(
        strip, expected,
        "the exposed bottom strip must be the incoming row's top {n} px"
    );
    // Control: the oracle strip differs from the bottom row's own bottom px.
    assert_ne!(
        expected,
        rows_of(&flat, y1 - n, n),
        "control: the incoming row's top px differ from the placeholder"
    );
    // Non-vacuity: the strip carries real ink, not just background.
    assert!(
        strip.iter().any(|&p| (p & 0x00ff_ffff) != BG),
        "the incoming row's top half carries glyph ink"
    );
    // The band's moved interior is the plain translate: row y (in-band, above
    // the strip) shows untranslated row y + frac.
    for y in grid_top + ch..y1 - n {
        assert_eq!(
            &glide.pixels[y * w..y * w + w],
            &flat.pixels[(y + n) * w..(y + n) * w + w],
            "interior row {y} is the up-translate"
        );
    }
    // Chrome invariance with the apron painted: the pinned top chrome row and
    // the pad bands are byte-identical to the frac-0 frame.
    for y in (0..grid_top + ch).chain(y1..glide.height) {
        assert_eq!(
            &glide.pixels[y * w..y * w + w],
            &flat.pixels[y * w..y * w + w],
            "chrome row {y} is pinned"
        );
    }
}

/// A frac-only frame over unchanged content is a damage-cache gate hit, and the strip is
/// still painted from the apron on that re-present (the apron is a present step, not a
/// damage step). Same oracle as above.
#[test]
fn cached_gate_hit_frame_still_paints_the_strip() {
    let mut r = renderer();
    let (_cw, ch) = r.cell_size();
    let grid_top = r.grid_top();
    let frac = (ch / 2).max(1) as i32;
    let n = frac as usize;
    let d = 2;

    let mut o = seeded_term();
    o.scroll_display(d - 1);
    let oracle = r.render_input(&partitioned(&mut o, 0));

    let mut t = seeded_term();
    t.scroll_display(d);
    let mut wc = WindowCpu::new();
    // Prime the cache with the whole-row frame, then re-present with a residual.
    let _ = r.render_input_cached(&mut wc, &partitioned(&mut t, 0));
    let glide_in = partitioned(&mut t, frac);
    let view = r.render_input_cached(&mut wc, &glide_in);
    let (w, px) = (view.width(), view.pixels().to_vec());
    let y1 = grid_top + ROWS * ch;
    assert_eq!(
        rows_of_px(&px, w, y1 - n, n),
        rows_of(&oracle, y1 - ch, n),
        "the gate-hit re-present paints the incoming row's top {n} px"
    );
    // The presenter reads what the render handed back: re-borrowing the untranslated cache
    // would drop the band shift and this strip.
    assert!(
        wc.last_translated(),
        "a sub-row frame is a translated frame"
    );
    assert_eq!(
        wc.presented_pixels(),
        px.as_slice(),
        "the presenter's pixels ARE the view's"
    );
    assert_ne!(
        wc.presented_pixels(),
        wc.frame_pixels(),
        "control: the untranslated cache is a different frame"
    );
    // Landing the row (frac 0) hands back the pristine cache.
    let landed = r.render_input_cached(&mut wc, &partitioned(&mut t, 0));
    let flat = r.render_input(&partitioned(&mut t, 0));
    assert_eq!(
        landed.pixels(),
        flat.pixels.as_slice(),
        "frac 0 is the identity"
    );
    assert!(
        !wc.last_translated(),
        "a whole-row frame is the cache itself"
    );
    assert_eq!(wc.presented_pixels(), wc.frame_pixels());
}

/// Scrolling past the bottom of the live screen (`display_offset == 0`,
/// `scroll_frac_px > 0`, `apron_row.present == false`) must paint a blank
/// frame-background apron into the bottom `frac` scanlines rather than leaving
/// the untranslated bottom row's glyphs duplicated at the bottom of the band.
#[test]
fn live_bottom_up_overscroll_paints_blank_apron_instead_of_duplicating_bottom_row() {
    let mut r = renderer();
    let (_cw, ch) = r.cell_size();
    let grid_top = r.grid_top();
    let frac = (ch / 2).max(1) as i32;
    let n = frac as usize;
    let mut t = seeded_term();
    t.process(b"BOTTOM ROW GLYPHS");
    assert_eq!(t.grid().display_offset(), 0);
    let flat = r.render_input(&partitioned(&mut t, 0));
    let input = partitioned(&mut t, frac);
    assert!(
        !input.apron_row.present,
        "at display_offset 0 there is no incoming row below the live screen"
    );
    let overscrolled = r.render_input(&input);
    let y1 = grid_top + ROWS * ch;
    assert!(
        rows_of(&flat, y1 - n, n)
            .iter()
            .any(|&px| (px & 0x00ff_ffff) != BG),
        "control: the untranslated bottom strip carries glyph ink"
    );
    assert!(
        rows_of(&overscrolled, y1 - n, n)
            .iter()
            .all(|&px| (px & 0x00ff_ffff) == BG),
        "bottom overscroll strip must be blank background, not duplicated bottom-row glyphs"
    );
}

#[test]
fn a_reordered_bidi_incoming_row_keeps_the_scroll_placeholder() {
    let mut r = renderer();
    let (_cw, ch) = r.cell_size();
    let grid_top = r.grid_top();
    let frac = (ch / 2).max(1) as i32;
    let n = frac as usize;
    let d = 3;
    let mut t = seeded_term();
    // At d=3, screen row ROWS-d is just below the viewport. Its Hebrew
    // run has a nonidentity visual order, so the engine withholds its apron
    // rather than exposing a strip rastered in logical order.
    let incoming_row = ROWS - d as usize;
    t.process(format!("\x1b[{};1H\x1b[2K", incoming_row + 1).as_bytes());
    t.process("\u{05d0}\u{05d1}\u{05d2}".as_bytes());
    let (_, order) = t.row_display_order(incoming_row as u16).unwrap();
    assert!(
        order
            .iter()
            .enumerate()
            .any(|(visual, &logical)| visual != logical),
        "control: the actual incoming row requires BiDi reordering"
    );
    t.scroll_display(d);
    let flat = r.render_input(&partitioned(&mut t, 0));
    let input = partitioned(&mut t, frac);
    assert_eq!(input.display_offset, d);
    assert!(
        !input.apron_row.present,
        "a reordered incoming row withholds its apron despite being above the live bottom"
    );
    let y1 = grid_top + ROWS * ch;
    assert!(
        rows_of(&flat, y1 - n, n)
            .iter()
            .any(|&px| (px & 0x00ff_ffff) != BG),
        "control: blanking the placeholder would erase visible ink"
    );
    let mut expected = flat.pixels.clone();
    aterm_render::scroll_translate::translate_grid_band_in_place(
        &mut expected,
        flat.width,
        grid_top + ch,
        y1,
        i64::from(frac),
    );
    let glide = r.render_input(&input);
    assert_eq!(
        glide.pixels, expected,
        "an unavailable reordered apron preserves the scroll fallback instead of painting a blank row"
    );
}
