// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M1b INCOMING-ROW APRON — the CPU raster of the row a sub-row up-glide is
//! sliding in at the bottom of the grid band (see the `scroll_translate`
//! module doc, "Exposed strip: the incoming-row apron").
//!
//! The row arrives on [`RenderInput::apron_row`] (the engine's one-past-the-
//! viewport row) and is rastered here into a RESIDENT one-row scratch through
//! the SAME phase runner every viewport row takes (`composite_free`, then
//! `draw_cursor`), as row 0 of a one-row `RenderInput` — so its glyphs,
//! decorations, DEC line size and caret are raster-identical to what the row
//! gets the moment it lands as an ordinary viewport row. The scratch is
//! `w × (grid_top + cell_h)` px: the renderer's row→px law places row 0 at
//! `grid_top` (see `row_ctx`), and the pad rows above it exist only so an
//! upward glyph overshoot has somewhere to land; only the `cell_h`-row band is
//! ever read back ([`ApronScratch::band`]).
//!
//! Cost: ONE row raster per sub-row frame that owes a strip (`frac > 0`, a
//! present apron) — nothing on whole-row frames, nothing on a bounce, nothing
//! once the glide lands. No per-frame allocation once warm: the one-row input
//! refills in place and the pixel buffer keeps its capacity.

use aterm_core::render::RenderInput;

use crate::{ImageCache, Renderer};

/// Resident scratch for the incoming-row raster — one per presenter
/// (`WindowCpu` holds one; a GPU window holds one too and uploads
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
    /// Everything the last raster depended on — so a sub-row frame whose apron
    /// and frame facts are unchanged (a glide INSIDE one engine row: ~119 of
    /// every 120 trackpad frames) rasters nothing and keeps the band.
    key: Option<ApronKey>,
    /// The apron row the band was rastered from, compared IN PLACE so a key
    /// hit allocates nothing; refilled field-wise (capacity kept) on a raster.
    /// It lived inside [`ApronKey`] until 2026-09-24, which cloned the row on
    /// every sub-row frame only to compare and drop it.
    key_row: aterm_core::render::ApronRow,
    /// Bumped on every REAL raster; the GPU arms key their strip pack on it.
    generation: u64,
}

/// The inputs a rastered apron band is a pure function of, besides the row
/// itself ([`ApronScratch::key_row`], compared in place): the frame facts the
/// one-row input copies, the renderer geometry the row→px law reads, and the
/// RENDERER state the raster reads that no input field carries. Appearance
/// changes no term here sees (text blending, minimum contrast, font thicken,
/// stem gamma, hinting, the theme's cursor/fg fallbacks) reach it through
/// [`ApronScratch::invalidate`], which the hosts' own cache invalidation calls.
#[derive(Clone, Copy, PartialEq)]
struct ApronKey {
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
    /// Forget the rastered row, so the next sub-row frame re-rasters it. Called
    /// by the hosts' appearance invalidation (`WindowCpu::invalidate`,
    /// `WindowGpu::invalidate_present`) for the changes no key term can see.
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    /// The rastered incoming row: `rows()` rows of `width()` packed pixels,
    /// its TOP row first — the source [`crate::scroll_translate::paint_incoming_strip`]
    /// reads. Empty until [`Renderer::rasterize_apron_row`] has run.
    #[must_use]
    pub fn band(&self) -> &[u32] {
        &self.pixels[self.band.clone()]
    }

    /// Width in px of the rastered row (the frame width).
    #[must_use]
    pub fn width(&self) -> usize {
        self.w
    }

    /// Rows in [`band`](Self::band) (`cell_h` after a raster, else `0`).
    #[must_use]
    pub fn rows(&self) -> usize {
        self.band.len().checked_div(self.w).unwrap_or(0)
    }

    /// The raster generation: advances exactly when [`Renderer::rasterize_apron_row`]
    /// really rastered (a key miss); unchanged on a hit.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Renderer {
    /// Raster `input.apron_row` into `scratch` as row 0 of a one-row frame
    /// (see the module doc). Returns `false` — and leaves `scratch` empty —
    /// when the apron is absent, so a caller can gate the strip on the result.
    ///
    /// Everything the row needs travels on the one-row input: its cells,
    /// clusters, combining marks and line size; the frame's live default bg,
    /// default fg and cursor colour (the base fill must be the frame's); and
    /// the cursor style family (the caret rides in only when
    /// [`ApronRow::cursor_col`] says the projected cursor is on this row).
    /// Nothing else: no selection, no effect stream, no image, no wallpaper
    /// (`incoming_row_applies` refuses a wallpaper frame before this runs).
    ///
    /// [`ApronRow::cursor_col`]: aterm_core::render::ApronRow::cursor_col
    pub fn rasterize_apron_row(&mut self, input: &RenderInput, scratch: &mut ApronScratch) -> bool {
        let ap = &input.apron_row;
        if !ap.present || input.cols == 0 {
            scratch.w = 0;
            scratch.band = 0..0;
            return false;
        }
        let cols = input.cols;
        let (w, _) = self.frame_size(1, cols);
        let grid_top = self.grid_top();
        let h = grid_top + self.cell_h;
        let band_bg = self.frame_bg(input) | (self.bg_transmittance() << 24);
        // THE KEY: a hit keeps the band and rasters nothing (see `ApronKey`).
        let key = ApronKey {
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
        if scratch.w != 0 && scratch.key == Some(key) && scratch.key_row == *ap {
            return true;
        }
        // Refill the one-row input IN PLACE (the per-row Vecs keep capacity).
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
        // The band's base, exactly as `full_render`'s clear lays it (the frame
        // default bg carrying the background-opacity transmittance byte), so the
        // row's bg pass elides the same runs it elides on a viewport row.
        scratch.pixels.clear();
        scratch.pixels.resize(w.saturating_mul(h), band_bg);
        // Frame-scope the cursor-row plan on BOTH sides of the one-row raster:
        // the resident plan describes the caller's frame, never this row, and
        // this row's plan must not outlive it either (`glyph_plan_row`).
        self.glyph_plan_row = None;
        // The row is rastered as ROW 0 of a one-row frame, but it is NOT a
        // chrome row: `composite_free` would hand row 0 the host's `ChromeBleed`
        // (gutter fills, the `[0, grid_top)` lip, the seam) and the strip would
        // carry tab-strip colour the landed row never has. Suspend the bleed for
        // the raster and restore it after — it belongs to the caller's frame.
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
        // Field-wise: the derived `ApronRow::clone_from` is `*self = clone()`
        // and would reallocate on every miss.
        let kr = &mut scratch.key_row;
        kr.present = ap.present;
        kr.cells.clone_from(&ap.cells);
        kr.clusters.clone_from(&ap.clusters);
        kr.combining.clone_from(&ap.combining);
        kr.line_size = ap.line_size;
        kr.cursor_col = ap.cursor_col;
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

    /// THE KEY SEES WHAT THE RASTER READS (audit, 2026-09-24). The resident
    /// raster keyed only the row, its geometry and the frame colours, so a
    /// landed fallback face (a `font_epoch` bump) and every appearance change
    /// the hosts answer with a cache invalidation (blending, contrast, font
    /// knobs) left the strip on a stale raster. A hit still rasters nothing;
    /// an epoch bump, a direct invalidation and the CPU window's own
    /// `invalidate` each force exactly one re-raster; and the caret-only terms
    /// (blink phase) cannot churn a row that carries no caret.
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
