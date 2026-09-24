// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M1b INCOMING-ROW APRON (CPU): during a sub-row up-glide the exposed bottom
//! strip shows the TOP `frac` px of the row sliding in from below the viewport
//! — raster-exact — instead of the band's own stale pixels (the retired
//! placeholder). The oracle is the engine itself: THE APRON AT OFFSET `d` IS
//! THE BOTTOM VIEWPORT ROW AT OFFSET `d - 1`, so the strip of the frac frame at
//! offset `d` must equal the top `frac` px of the bottom grid row of the
//! whole-row frame at offset `d - 1`. Against today's placeholder the strip
//! holds the bottom row's own BOTTOM `frac` px, so the first assertion FAILS.
//!
//! Gated: no system font ⇒ no-op.

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme, WindowCpu};

const ROWS: usize = 8;
const COLS: usize = 20;
const BG: u32 = 0x0011_1318; // Theme::default().bg

fn renderer() -> Option<Renderer> {
    let mut r = Renderer::from_system(18.0, Theme::default())?;
    r.debug_block_on_lazy_fallbacks();
    Some(r)
}

/// Distinct, DESCENDER-FREE text per line (caps + digits only), so a row's
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
    let Some(mut r) = renderer() else {
        return;
    };
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
    // The untranslated frame at offset d — the retired placeholder's source.
    let flat = r.render_input(&partitioned(&mut t, 0));
    // The whole-row ORACLE at offset d - 1: its bottom grid row IS the apron.
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
    // Teeth: the oracle strip differs from the placeholder (the bottom row's own
    // bottom px), so the assertion above cannot pass on the retired code.
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

/// The PRESENTATION hot path: a frac-only frame over unchanged content is a
/// damage-cache gate hit, and the strip must still be painted from the apron on
/// that zero-raster re-present (the apron raster is a present-step, not a
/// damage-step). Same oracle as above.
#[test]
fn cached_gate_hit_frame_still_paints_the_strip() {
    let Some(mut r) = renderer() else {
        return;
    };
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
    // And landing the row (frac 0) hands back the pristine cache: the strip is
    // then the ordinary bottom row of the offset-d frame, untouched by the apron.
    let landed = r.render_input_cached(&mut wc, &partitioned(&mut t, 0));
    let flat = r.render_input(&partitioned(&mut t, 0));
    assert_eq!(
        landed.pixels(),
        flat.pixels.as_slice(),
        "frac 0 is the identity"
    );
}

/// A DOWN-bounce (negative residual) owes NO apron: the top strip stays the
/// placeholder on this backend exactly as before — byte-identical to a plain
/// band translate of the untranslated frame.
#[test]
fn down_bounce_keeps_the_placeholder() {
    let Some(mut r) = renderer() else {
        return;
    };
    let (_cw, ch) = r.cell_size();
    let grid_top = r.grid_top();
    let frac = -((ch / 2).max(1) as i32);
    let mut t = seeded_term();
    t.scroll_display(3);
    let flat = r.render_input(&partitioned(&mut t, 0));
    let bounce = r.render_input(&partitioned(&mut t, frac));
    let mut expect = flat.pixels.clone();
    aterm_render::scroll_translate::translate_grid_band_in_place(
        &mut expect,
        flat.width,
        grid_top + ch,
        grid_top + ROWS * ch,
        i64::from(frac),
    );
    assert_eq!(bounce.pixels, expect, "a bounce is the bare translate");
}
