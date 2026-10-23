// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU raster of the incoming apron row (`RenderInput::apron_row`) exposed at the bottom of the
//! grid band during a sub-row up-glide.

use aterm_core::render::RenderInput;

use crate::{ImageCache, Renderer};

/// Resident scratch for the incoming-row raster, one per presenter.
#[derive(Default)]
pub struct ApronScratch {
    input: RenderInput,
    ic: ImageCache,
    pixels: Vec<u32>,
    w: usize,
    band: std::ops::Range<usize>,
    key: Option<ApronKey>,
    generation: u64,
}

/// What a rastered apron band depends on besides the row itself (which is compared in
/// place against the last raster's input): the frame facts the one-row input copies, the
/// geometry the row→px law reads, and renderer state no input field carries. Appearance
/// changes outside the key (blending, contrast, font knobs, the theme's fallbacks) reach
/// the band through [`ApronScratch::invalidate`].
#[derive(Clone, Copy, PartialEq)]
struct ApronKey {
    present: bool,
    cols: usize,
    w: usize,
    grid_top: usize,
    cell_h: usize,
    band_bg: u32,
    default_fg: u32,
    cursor_color: u32,
    cursor_style: aterm_core::terminal::CursorStyle,
    cursor_effect_style_override: Option<aterm_core::terminal::CursorStyle>,
    cursor_fill_override: Option<u32>,
    font_epoch: u64,
    cursor_blink_phase: bool,
    renderer_cursor_style_override: Option<aterm_core::terminal::CursorStyle>,
}

impl ApronScratch {
    /// Forget the rastered row so the next sub-row frame re-rasters it.
    #[inline]
    pub const fn invalidate(&mut self) {
        self.key = None;
    }

    /// The rastered incoming row as `rows() * width()` packed pixels, or empty before rastering.
    #[inline]
    #[must_use]
    pub fn band(&self) -> &[u32] {
        &self.pixels[self.band.clone()]
    }

    /// Width in px of the rastered row (the frame width).
    #[inline]
    #[must_use]
    pub const fn width(&self) -> usize {
        self.w
    }

    /// Rows in [`band`](Self::band) (`cell_h` once written, else `0`).
    #[inline]
    #[must_use]
    pub const fn rows(&self) -> usize {
        let len = self.band.end.saturating_sub(self.band.start);
        match len.checked_div(self.w) {
            Some(r) => r,
            None => 0,
        }
    }

    /// Raster generation counter, incremented whenever the band is rewritten.
    #[inline]
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl Renderer {
    /// Raster `input.apron_row` into `scratch` as row 0 of a one-row frame, or fill with the base
    /// background when absent. Returns `false` only when `input.cols == 0`.
    pub fn rasterize_apron_row(&mut self, input: &RenderInput, scratch: &mut ApronScratch) -> bool {
        let ap = &input.apron_row;
        if input.cols == 0 {
            scratch.w = 0;
            scratch.band = 0..0;
            return false;
        }
        let cols = input.cols;
        let (w, _) = self.frame_size(1, cols);
        let grid_top = self.grid_top();
        let h = grid_top + self.cell_h;
        let band_bg = self.frame_bg(input) | (self.bg_transmittance() << 24);
        let key = ApronKey {
            present: ap.present,
            cols,
            w,
            grid_top,
            cell_h: self.cell_h,
            band_bg,
            default_fg: input.default_fg,
            cursor_color: input.cursor_color,
            cursor_style: input.cursor_style,
            cursor_effect_style_override: input.cursor_effect_style_override,
            cursor_fill_override: input.cursor_fill_override,
            font_epoch: self.font_epoch,
            cursor_blink_phase: ap.cursor_col.is_some() && self.cursor_blink_phase,
            renderer_cursor_style_override: ap.cursor_col.and(self.cursor_style_override),
        };
        if !ap.present {
            if scratch.w != 0 && scratch.key == Some(key) {
                return true;
            }
            scratch.pixels.clear();
            scratch.pixels.resize(w.saturating_mul(h), band_bg);
            scratch.w = w;
            scratch.band = grid_top * w..h * w;
            scratch.key = Some(key);
            scratch.generation = scratch.generation.wrapping_add(1);
            return true;
        }
        let mi = &scratch.input;
        if scratch.w != 0
            && scratch.key == Some(key)
            && mi.cells[0] == ap.cells
            && mi.clusters[0] == ap.clusters
            && mi.combining[0] == ap.combining
            && mi.line_sizes[0] == ap.line_size
            && mi.cursor_visible.then_some(mi.cursor_col) == ap.cursor_col
        {
            return true;
        }
        let mi = &mut scratch.input;
        mi.rows = 1;
        mi.cols = cols;
        mi.cells.resize_with(1, Vec::new);
        mi.cells[0].clone_from(&ap.cells);
        mi.clusters.resize_with(1, Vec::new);
        mi.clusters[0].clone_from(&ap.clusters);
        mi.combining.resize_with(1, Vec::new);
        mi.combining[0].clone_from(&ap.combining);
        mi.images.resize_with(1, Vec::new);
        mi.images[0].clear();
        mi.line_sizes.clear();
        mi.line_sizes.push(ap.line_size);
        mi.line_size_spans.resize_with(1, Vec::new);
        mi.line_size_spans[0].clear();
        mi.default_bg_spans.resize_with(1, Vec::new);
        mi.default_bg_spans[0].clear();
        mi.default_bg = input.default_bg;
        mi.default_fg = input.default_fg;
        mi.cursor_color = input.cursor_color;
        mi.cursor_style = input.cursor_style;
        mi.cursor_effect_style_override = input.cursor_effect_style_override;
        mi.cursor_fill_override = input.cursor_fill_override;
        mi.cursor_row = 0;
        mi.cursor_col = ap.cursor_col.unwrap_or(0);
        mi.cursor_visible = ap.cursor_col.is_some();
        scratch.pixels.clear();
        scratch.pixels.resize(w.saturating_mul(h), band_bg);
        self.glyph_plan_row = None;
        let bleed = self.chrome_bleed.take();
        let ApronScratch {
            input: mi,
            ic,
            pixels,
            ..
        } = scratch;
        self.composite_free(ic, pixels, w, h, mi, 0..1, None, None, None);
        self.draw_cursor(pixels, w, h, mi);
        self.chrome_bleed = bleed;
        self.glyph_plan_row = None;
        scratch.w = w;
        scratch.band = grid_top * w..h * w;
        scratch.key = Some(key);
        scratch.generation = scratch.generation.wrapping_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Theme, WindowCpu};
    use aterm_core::terminal::Terminal;

    fn renderer() -> Renderer {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/DejaVuSansMono.ttf"
        ))
        .expect("bundled DejaVu asset");
        let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).expect("fixture parses");
        r.set_runtime_font_discovery(false);
        r
    }

    fn scrolled_input() -> RenderInput {
        let mut t = Terminal::new(8, 20);
        t.process(b"\x1b[?25l");
        for i in 0..40 {
            t.process(format!("LINE {i:02}\r\n").as_bytes());
        }
        t.scroll_display(2);
        let input = t.cell_frame(8, 20);
        assert!(input.apron_row.present && input.apron_row.cursor_col.is_none());
        input
    }

    #[test]
    fn the_apron_raster_follows_the_font_epoch_and_every_invalidation() {
        let mut r = renderer();
        let input = scrolled_input();
        let mut sc = ApronScratch::default();
        assert!(r.rasterize_apron_row(&input, &mut sc));
        let g0 = sc.generation();
        assert!(r.rasterize_apron_row(&input, &mut sc));
        assert_eq!(
            sc.generation(),
            g0,
            "an unchanged frame is a hit: nothing rastered"
        );
        r.cursor_blink_phase = !r.cursor_blink_phase;
        assert!(r.rasterize_apron_row(&input, &mut sc));
        assert_eq!(
            sc.generation(),
            g0,
            "no caret on the row: the blink phase is not its input"
        );
        r.font_epoch += 1;
        assert!(r.rasterize_apron_row(&input, &mut sc));
        assert_eq!(
            sc.generation(),
            g0 + 1,
            "a landed face re-rasters the strip"
        );
        sc.invalidate();
        assert!(r.rasterize_apron_row(&input, &mut sc));
        assert_eq!(
            sc.generation(),
            g0 + 2,
            "an invalidation re-rasters the strip"
        );
        let mut wc = WindowCpu::default();
        assert!(r.rasterize_apron_row(&input, &mut wc.apron));
        let w0 = wc.apron.generation();
        wc.invalidate();
        assert!(r.rasterize_apron_row(&input, &mut wc.apron));
        assert_eq!(
            wc.apron.generation(),
            w0 + 1,
            "the window's invalidation reaches the strip"
        );
    }
}
