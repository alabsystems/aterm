// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The incoming-row apron: the CPU raster of the row a sub-row up-glide slides in at the
//! bottom of the grid band (see the `scroll_translate` module doc on the exposed strip).
//!
//! The row arrives on [`RenderInput::apron_row`] (the row one past the viewport) and is
//! rastered as row 0 of a one-row `RenderInput` through the same phase runner every
//! viewport row takes (`composite_free`, then `draw_cursor`), so it is raster-identical
//! to the row once it lands. The scratch is `w × (grid_top + cell_h)` px because the
//! row→px law places row 0 at `grid_top` (see `row_ctx`); the pad above catches upward
//! glyph overshoot, and only the `cell_h`-row band is read back ([`ApronScratch::band`]).
//!
//! Cost: at most one row raster per sub-row frame that owes a strip, none on whole-row
//! frames, and no per-frame allocation once warm.

use aterm_core::render::RenderInput;

use crate::{ImageCache, Renderer};

/// Resident scratch for the incoming-row raster, one per presenter (a GPU window uploads
/// [`band`](Self::band) as the strip's texels).
#[derive(Default)]
pub struct ApronScratch {
    /// The one-row `RenderInput` the raster runs over, refilled in place.
    input: RenderInput,
    /// The row's (always empty) image cache: the phase runner takes one.
    ic: ImageCache,
    /// `w × (grid_top + cell_h)` packed `0xTTRRGGBB` pixels.
    pixels: Vec<u32>,
    /// Framebuffer width the last raster ran at (`0` == nothing rastered).
    w: usize,
    /// The `cell_h`-row band inside `pixels` holding the rastered row.
    band: std::ops::Range<usize>,
    /// What the last raster depended on: a glide inside one engine row (~119 of every
    /// 120 trackpad frames) hits it and rasters nothing.
    key: Option<ApronKey>,
    /// Bumped whenever the band is rewritten; the GPU arms key their strip pack on it.
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
    /// A landed fallback face (or a retired one) re-routes glyphs under
    /// unchanged cells: the main damage cache repaints on it, so must the strip.
    font_epoch: u64,
    /// `draw_cursor` gates the caret on the blink phase — on a caret row only.
    cursor_blink_phase: bool,
    /// …and shapes it by the renderer's unfocused override — a caret row only.
    renderer_cursor_style_override: Option<aterm_core::terminal::CursorStyle>,
}

impl ApronScratch {
    /// Forget the rastered row, so the next sub-row frame re-rasters it. The hosts'
    /// appearance invalidation (`WindowCpu::invalidate`, `WindowGpu::invalidate_present`)
    /// calls it for changes the key cannot see.
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    /// The rastered incoming row: `rows()` rows of `width()` packed pixels, top row first,
    /// as [`crate::scroll_translate::paint_incoming_strip`] reads it. Empty until
    /// [`Renderer::rasterize_apron_row`] has run.
    #[must_use]
    pub fn band(&self) -> &[u32] {
        &self.pixels[self.band.clone()]
    }

    /// Width in px of the rastered row (the frame width).
    #[must_use]
    pub fn width(&self) -> usize {
        self.w
    }

    /// Rows in [`band`](Self::band) (`cell_h` once written, else `0`).
    #[must_use]
    pub fn rows(&self) -> usize {
        self.band.len().checked_div(self.w).unwrap_or(0)
    }

    /// The raster generation: advances exactly when [`Renderer::rasterize_apron_row`]
    /// rewrites the band (a key miss); unchanged on a hit.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Renderer {
    /// Raster `input.apron_row` into `scratch` as row 0 of a one-row frame (see the module
    /// doc). An absent apron (bottom overscroll at `display_offset == 0`) fills the band
    /// with the frame's base bg instead, so the upward translate leaves no duplicate of the
    /// bottom row. Returns `false`, leaving `scratch` empty, only when the frame has no
    /// columns.
    ///
    /// The one-row input carries the row's cells, clusters, combining marks and line size,
    /// the frame's default bg, default fg and cursor colour, and the cursor style (the
    /// caret only when [`ApronRow::cursor_col`] puts the cursor on this row). No
    /// selection, effect stream, image or wallpaper (`incoming_row_applies` refuses a
    /// wallpaper frame first).
    ///
    /// [`ApronRow::cursor_col`]: aterm_core::render::ApronRow::cursor_col
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
        // A key hit keeps the band and rasters nothing.
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
        // The one-row input still holds what the last raster ran over, so compare the
        // row in place.
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
        // Refill the one-row input in place (the per-row Vecs keep capacity).
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
        // The band's base as `full_render`'s clear lays it (default bg plus the
        // transmittance byte), so the bg pass elides the same runs as on a viewport row.
        scratch.pixels.clear();
        scratch.pixels.resize(w.saturating_mul(h), band_bg);
        // Clear the cursor-row plan on both sides: the resident plan describes the
        // caller's frame, and this row's must not outlive it (`glyph_plan_row`).
        self.glyph_plan_row = None;
        // Row 0 of this one-row frame is not a chrome row: suspend the host's
        // `ChromeBleed` (gutter fills, the `[0, grid_top)` lip, the seam) for the
        // raster, or the strip carries tab-strip colour the landed row never has.
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

    /// Machine-independent: the bundled face, runtime discovery off.
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

    /// Scrolled back two rows with the cursor hidden: an apron with no caret.
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

    /// The key sees what the raster reads: a hit rasters nothing, a `font_epoch` bump, a
    /// direct invalidation and the CPU window's `invalidate` each force one re-raster, and
    /// the blink phase cannot churn a row with no caret.
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
