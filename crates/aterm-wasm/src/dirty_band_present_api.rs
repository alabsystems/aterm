// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Dirty-band present export for the CPU wasm path (audit E3, Codex-modified).
//!
//! The old present pipeline paid O(W×H) EVERY frame regardless of damage: the
//! whole u32 framebuffer was re-expanded to RGBA8 and the host `putImageData`d
//! the whole canvas — ~4 MB of conversion + upload for a one-row keystroke.
//! The engine already computes exact per-row damage (`compute_dirty_rows`, one
//! source shared with the GPU scissor path); this module carries that damage
//! ACROSS the wasm boundary the same way the spill band carries its rects:
//!
//! * `render()` now RE-EXPANDS ONLY the dirty pixel bands into the persistent
//!   RGBA buffer (`rgba`/`rgba_ptr` semantics unchanged: the FULL buffer is
//!   always coherent, because undamaged bytes are last frame's — byte-equal).
//! * [`AtermTerminal::present_band_count`] / [`AtermTerminal::present_bands_ptr`]
//!   export the packed `(x, y, w, h)` i32 bands of the JUST-rendered frame
//!   (the `spill_rects_ptr` read discipline: consume synchronously after
//!   `render()`, never cache the JS view). The host calls
//!   `ctx.putImageData(imageData, 0, 0, x, y, w, h)` per band — and SKIPS the
//!   canvas entirely on a zero-band (gate-hit) frame.
//!
//! ## Band semantics (the glue contract)
//!
//! * `Full` damage (first frame, resize, theme/font epoch, scrollback offset
//!   change, DECDHL) → ONE full-frame band.
//! * Gate hit (nothing changed) → ZERO bands; the RGBA buffer is already this
//!   frame, the host skips conversion AND `putImageData`.
//! * Row damage → one band per merged run of dirty grid rows, full-width,
//!   `cell_h`-aligned within the grid box (a padding recolour forces `Full`
//!   upstream). Only at 0/0 chrome — a chromed frame (`set_chrome`) lets
//!   effects paint outside the row lattice, so chromed embedders always get
//!   the full band (their band optimization is the spill export).
//! * FRACTIONAL SCROLL: while a sub-row residual is presented (`scroll_px`
//!   mid-row) — and on the frame that clears it — the M1b translate shifts
//!   the whole grid band, so those frames export one full-frame band. Never a
//!   stale partial under a moving band.
//!
//! ## Overlay damage / second-canvas contract (Codex requirement)
//!
//! ENGINE-drawn overlays (cursor, selection tint, LUMEN aurora / trail /
//! sparkle quads, search-band effects) are already folded into
//! `compute_dirty_rows` — their rows appear in the exported bands, so a
//! partial present can never strand a moving glow. HOST-drawn overlays (IME
//! preedit, find-bar highlight rectangles, DOM-side decorations) are outside
//! the engine's damage model: a host must either (a) draw them on a SEPARATE
//! canvas layered above the terminal canvas — the recommended shape, partial
//! presents then never touch them — or (b) repaint its overlay after EVERY
//! nonzero-band present. Compositing host overlays into the terminal canvas
//! and presenting partial bands WILL ghost them.
//!
//! (Codex's third E3 note — measure how often the CPU fallback is actually
//! taken vs WebGPU — is host-side telemetry: orc's glue owns backend choice;
//! integrator note recorded in the commit.)

use crate::AtermTerminal;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Expand `pixels[start..end]` (0xTTRRGGBB) into `rgba[start*4..end*4]`
/// (straight RGBA8, alpha = 255 − TT) — the one packing rule of the CPU
/// present path, band-scoped.
pub(crate) fn expand_rgba_band(pixels: &[u32], rgba: &mut [u8], start: usize, end: usize) {
    let end = end.min(pixels.len());
    let start = start.min(end);
    for (i, &p) in pixels[start..end].iter().enumerate() {
        let o = (start + i) * 4;
        rgba[o] = (p >> 16) as u8;
        rgba[o + 1] = (p >> 8) as u8;
        rgba[o + 2] = p as u8;
        rgba[o + 3] = 0xff - (p >> 24) as u8;
    }
}

/// Shift the pixel rows of the RGBA grid band `[y0, y1)` of `rgba` (a `w`-wide,
/// 4-byte/px, row-major buffer) by `frac_px` device pixels IN PLACE — the u8
/// twin of `scroll_translate::translate_grid_band_in_place`. The E7 scroll path
/// applies it to the persistent present buffer so its retained rows track the
/// engine's cache blit WITHOUT re-expanding them from the u32 cache. Same
/// direction convention (positive = up, `dst[y] ← src[y + |frac|]`) and the same
/// direction-matched in-place walk (a source row is never read after overwrite),
/// so the shifted band is byte-identical to copying from a pristine snapshot.
pub(crate) fn shift_rgba_band(rgba: &mut [u8], w: usize, y0: usize, y1: usize, frac_px: i64) {
    if w == 0 || y0 >= y1 || frac_px == 0 {
        return;
    }
    let stride = w * 4;
    let band_h = y1 - y0;
    let mag = frac_px.unsigned_abs() as usize;
    let moved = band_h.saturating_sub(mag);
    if frac_px > 0 {
        // Shift UP: dst pulls from below (dst + mag). Walk top-down.
        for i in 0..moved {
            let dst = y0 + i;
            let src = dst + mag;
            rgba.copy_within(src * stride..src * stride + stride, dst * stride);
        }
    } else {
        // Shift DOWN: dst pulls from above (dst − mag). Walk bottom-up.
        for i in 0..moved {
            let dst = y1 - 1 - i;
            let src = dst - mag;
            rgba.copy_within(src * stride..src * stride + stride, dst * stride);
        }
    }
}

/// Full-frame expansion + the single full band. Also (re)sizes the RGBA
/// buffer, so every non-partial path funnels through one place.
pub(crate) fn expand_full(
    pixels: &[u32],
    rgba: &mut Vec<u8>,
    bands: &mut Vec<i32>,
    w: usize,
    h: usize,
) {
    rgba.resize(pixels.len() * 4, 0);
    expand_rgba_band(pixels, rgba, 0, pixels.len());
    bands.clear();
    bands.extend_from_slice(&[0, 0, w as i32, h as i32]);
}

/// Merge consecutive dirty GRID rows into full-width pixel bands
/// (`x, y, w, h` quads appended to `out`), mapping row `r` to
/// `grid_top + r*cell_h .. +cell_h`, clamped to the frame height.
pub(crate) fn dirty_rows_to_bands(
    dirty: &[bool],
    grid_top: usize,
    cell_h: usize,
    frame_w: usize,
    frame_h: usize,
    out: &mut Vec<i32>,
) {
    out.clear();
    let mut r = 0usize;
    while r < dirty.len() {
        if !dirty[r] {
            r += 1;
            continue;
        }
        let run_start = r;
        while r < dirty.len() && dirty[r] {
            r += 1;
        }
        let y0 = (grid_top + run_start * cell_h).min(frame_h);
        let y1 = (grid_top + r * cell_h).min(frame_h);
        if y1 > y0 {
            out.extend_from_slice(&[0, y0 as i32, frame_w as i32, (y1 - y0) as i32]);
        }
    }
}

impl AtermTerminal {
    /// Apply the recorded damage of the frame just rendered into the
    /// persistent RGBA buffer, band-scoped, and publish the band list.
    /// NOT called on a frame carrying (or just releasing) a sub-row
    /// translate: the caller already full-expanded from the TRANSLATED view
    /// (`expand_full` under its live borrow), which this untranslated-cache
    /// path could not reproduce.
    pub(crate) fn expand_damage_to_rgba(&mut self) {
        use aterm_render::DamageOutcome;
        let expected = self.width * self.height * 4;
        let stale = self.rgba.len() != expected;
        let (cw, ch) = self.renderer.cell_size();
        // Window chrome ([head][pad][grid][pad]) lets effects paint OUTSIDE
        // the grid-row lattice (that is what the spill band exports), so row
        // bands cannot cover a chromed frame's damage — chromed embedders
        // take the full band. 0/0 chrome (the shipped pane shape) is the
        // optimized path.
        let chromeless = self.renderer.grid_top() == 0
            && self.height == self.rows * ch
            && self.width == self.cols * cw;
        match self.win.last_damage() {
            DamageOutcome::GateHit if !stale => self.present_bands.clear(),
            DamageOutcome::Rows if !stale && chromeless => {
                dirty_rows_to_bands(
                    self.win.dirty_rows(),
                    self.renderer.grid_top(),
                    ch,
                    self.width,
                    self.height,
                    &mut self.present_bands,
                );
                let pixels = self.win.frame_pixels();
                let (quads, _) = self.present_bands.as_chunks::<4>();
                for quad in quads {
                    let (y, h) = (quad[1] as usize, quad[3] as usize);
                    expand_rgba_band(pixels, &mut self.rgba, y * self.width, (y + h) * self.width);
                }
            }
            // E7 SCROLL BLIT: the engine shifted the retained CACHE rows by
            // `delta_rows·ch` and re-rasterized only the exposed strip + cursor
            // rows. Mirror that in the persistent RGBA buffer with a cheap row
            // memmove (instead of an O(W·H) re-expansion of the retained rows),
            // then re-expand ONLY the flagged bands from the shifted+patched
            // cache — leaving the whole buffer coherent. Every retained row MOVED
            // on the host canvas, so a partial band list would strand them: export
            // ONE FULL band (the host re-uploads the whole coherent buffer, exactly
            // as it does for `Full`). The rendered buffer is byte-identical to a
            // from-scratch full expansion (the E7 frame-identity oracle).
            DamageOutcome::Scroll { delta_rows } if !stale && chromeless => {
                let grid_top = self.renderer.grid_top();
                let y1 = (grid_top + self.rows * ch).min(self.height);
                shift_rgba_band(
                    &mut self.rgba,
                    self.width,
                    grid_top,
                    y1,
                    i64::from(delta_rows) * ch as i64,
                );
                // The flagged (exposed + cursor) rows, as pixel bands.
                dirty_rows_to_bands(
                    self.win.dirty_rows(),
                    grid_top,
                    ch,
                    self.width,
                    self.height,
                    &mut self.present_bands,
                );
                let pixels = self.win.frame_pixels();
                let (quads, _) = self.present_bands.as_chunks::<4>();
                for quad in quads {
                    let (y, h) = (quad[1] as usize, quad[3] as usize);
                    expand_rgba_band(pixels, &mut self.rgba, y * self.width, (y + h) * self.width);
                }
                // Retained rows moved on-canvas → export one full band.
                self.present_bands.clear();
                self.present_bands
                    .extend_from_slice(&[0, 0, self.width as i32, self.height as i32]);
            }
            // Full damage — and any stale-buffer surprise on the other arms
            // (first frame, resize race): one full expansion, one full band.
            _ => {
                let (w, h) = (self.width, self.height);
                // Split borrows: pixels live in win, the buffers in self.
                let AtermTerminal {
                    win,
                    rgba,
                    present_bands,
                    ..
                } = self;
                expand_full(win.frame_pixels(), rgba, present_bands, w, h);
            }
        }
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermTerminal {
    /// Number of dirty present bands from the LAST `render()`. `0` = the
    /// frame is byte-identical to the previous one — skip RGBA reads and
    /// `putImageData` entirely. Read together with
    /// [`present_bands_ptr`](Self::present_bands_ptr).
    pub fn present_band_count(&self) -> u32 {
        (self.present_bands.len() / 4) as u32
    }

    /// Byte offset (in wasm linear memory) of the packed dirty-band array:
    /// `present_band_count()` bands of 4 `i32`s — `x, y, w, h`,
    /// FRAME-ABSOLUTE device px, non-overlapping, top-to-bottom. Same read
    /// discipline as `rgba_ptr`: consume synchronously after `render()`,
    /// never cache the JS view across engine calls. The host presents each
    /// band with `putImageData(imageData, 0, 0, x, y, w, h)` over the SAME
    /// full-frame ImageData `rgba_ptr` backs. See the module docs for the
    /// overlay/second-canvas contract.
    pub fn present_bands_ptr(&self) -> usize {
        self.present_bands.as_ptr() as usize
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn term() -> Option<AtermTerminal> {
        AtermTerminal::new_from_system(8, 40, 14.0)
    }

    /// Reference oracle: what a from-scratch full expansion of the current
    /// frame would produce.
    fn full_expansion(t: &AtermTerminal) -> Vec<u8> {
        let mut rgba = Vec::new();
        let mut bands = Vec::new();
        expand_full(
            t.win.frame_pixels(),
            &mut rgba,
            &mut bands,
            t.width,
            t.height,
        );
        rgba
    }

    #[test]
    fn band_merge_maps_rows_to_pixel_runs() {
        let mut out = Vec::new();
        // rows 1-2 dirty (merged), row 4 dirty, grid_top 3, cell_h 10, h 100.
        dirty_rows_to_bands(&[false, true, true, false, true], 3, 10, 80, 100, &mut out);
        assert_eq!(out, vec![0, 13, 80, 20, 0, 43, 80, 10]);
        // Clamped at the frame edge; degenerate bands dropped.
        dirty_rows_to_bands(&[true], 95, 10, 80, 100, &mut out);
        assert_eq!(out, vec![0, 95, 80, 5]);
        dirty_rows_to_bands(&[true], 100, 10, 80, 100, &mut out);
        assert!(out.is_empty(), "fully out-of-frame band drops");
    }

    #[test]
    fn first_frame_is_one_full_band_and_gate_hit_is_zero() {
        let Some(mut t) = term() else { return };
        t.process(b"hello band world\r\n");
        t.render();
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "first frame presents one full band"
        );
        let full_rgba = t.rgba.clone();
        t.render();
        assert_eq!(t.present_band_count(), 0, "no change → zero bands");
        assert_eq!(t.rgba, full_rgba, "gate-hit leaves the buffer intact");
    }

    #[test]
    fn keystroke_damage_exports_a_sub_frame_band_and_stays_byte_exact() {
        let Some(mut t) = term() else { return };
        t.process(b"line one\r\nline two\r\nline three");
        t.render();
        t.process_str("x"); // one-row edit on the cursor row
        t.render();
        assert!(
            t.present_band_count() >= 1,
            "damage must surface as at least one band"
        );
        let banded_px: i32 = t
            .present_bands
            .as_chunks::<4>()
            .0
            .iter()
            .map(|q| q[2] * q[3])
            .sum();
        assert!(
            (banded_px as usize) < t.width * t.height,
            "a keystroke never pays a full-frame band"
        );
        // THE oracle: the partially-updated persistent buffer is byte-equal
        // to a from-scratch expansion of the same frame.
        assert_eq!(t.rgba, full_expansion(&t), "partial update is byte-exact");
    }

    #[test]
    fn scroll_and_resize_take_the_full_band_path() {
        let Some(mut t) = term() else { return };
        for i in 0..50 {
            t.process(format!("scroll line {i}\r\n").as_bytes());
        }
        t.render();
        // A display-offset change is a FullRepaint upstream → one full band.
        t.scroll_lines(3);
        t.render();
        assert_eq!(t.present_bands[..2], [0, 0]);
        assert_eq!(t.present_bands[2..4], [t.width as i32, t.height as i32]);
        assert_eq!(t.rgba, full_expansion(&t));
        // Resize: geometry change → full band, buffer resized coherently.
        t.resize(10, 44);
        t.render();
        assert_eq!(t.present_band_count(), 1);
        assert_eq!(t.rgba.len(), t.width * t.height * 4);
        assert_eq!(t.rgba, full_expansion(&t));
    }

    #[test]
    fn fractional_scroll_frames_present_the_full_band() {
        let Some(mut t) = term() else { return };
        for i in 0..50 {
            t.process(format!("frac line {i}\r\n").as_bytes());
        }
        t.render();
        let half_cell = t.cell_height() as f64 / 2.0;
        t.scroll_px(half_cell); // sub-row residual banks, no whole-row flip
        t.render();
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "a translated band frame must not export partial bands"
        );
        // The frame that RELEASES the residual is full too (the band snaps
        // back), and the buffer matches the untranslated cache again.
        t.scroll_px(-half_cell);
        t.render();
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "the residual-clearing frame re-presents the whole band"
        );
        assert_eq!(t.rgba, full_expansion(&t));
    }

    /// E7 GUARD: the whole-row scroll BLIT path (the `scroll_lines` bench shape)
    /// is actually TAKEN, and every scrolled frame's persistent RGBA buffer is
    /// byte-identical to a from-scratch full expansion — the blit is faster, never
    /// different. Walks many notches (the bench does 200) so the inductive
    /// coherence of the shift+partial-expand buffer is exercised repeatedly.
    #[test]
    fn scroll_blit_is_taken_and_stays_byte_exact() {
        use aterm_render::DamageOutcome;
        let Some(mut t) = term() else { return };
        t.set_scrollback_limit(4000);
        for i in 0..2000 {
            t.process(format!("scroll blit line {i}\r\n").as_bytes());
        }
        t.scroll_to_bottom();
        t.render();
        let mut blit_frames = 0usize;
        for _ in 0..60 {
            t.scroll_lines(3);
            t.render();
            // The rescued whole-row scroll reports a `Scroll` damage outcome.
            if matches!(t.win.last_damage(), DamageOutcome::Scroll { .. }) {
                blit_frames += 1;
            }
            // THE oracle: the shifted + partially re-expanded persistent buffer
            // equals a from-scratch full expansion of the same frame, every notch.
            assert_eq!(
                t.rgba,
                full_expansion(&t),
                "scroll-blit present must be byte-identical to a full repaint"
            );
            // A retained row moved on-canvas → the host gets one full band.
            assert_eq!(
                t.present_bands,
                vec![0, 0, t.width as i32, t.height as i32],
                "a scroll-blit frame exports one full band"
            );
        }
        assert!(
            blit_frames >= 50,
            "the scroll-blit fast path must carry the scroll (took it {blit_frames}/60 frames)"
        );
    }

    /// E7 GUARD (shade + overshoot). The scroll-blit present must stay byte-exact
    /// even when the retained rows carry absolute-Y-parity shade dithers
    /// (U+2591–2593) and upward-overshooting accented capitals — the two cases the
    /// blit's frame-identity gate refuted. The engine re-rasters the shade-parity
    /// rows and the overshoot aprons into its cache; this checks the wasm present
    /// buffer (a row memmove + partial re-expansion) matches a from-scratch
    /// expansion of that corrected cache, every notch.
    #[test]
    fn shade_scroll_blit_present_stays_byte_exact() {
        use aterm_render::DamageOutcome;
        let Some(mut t) = term() else { return };
        t.set_scrollback_limit(4000);
        for i in 0..1500 {
            // ▓▓▒░ shade + accented capitals (upward overshoot) + box glyphs.
            let line = format!("\u{2593}\u{2593}\u{2592}\u{2591} \u{c9}\u{c1}\u{d1} r{i} \u{2588}\r\n");
            t.process(line.as_bytes());
        }
        t.scroll_to_bottom();
        t.render();
        let mut blit_frames = 0usize;
        for _ in 0..40 {
            t.scroll_lines(1); // single notch → odd pixel shift when cell_h is odd
            t.render();
            if matches!(t.win.last_damage(), DamageOutcome::Scroll { .. }) {
                blit_frames += 1;
            }
            assert_eq!(
                t.rgba,
                full_expansion(&t),
                "shade scroll-blit present must equal a full repaint"
            );
        }
        assert!(
            blit_frames >= 30,
            "the shade scroll must still ride the blit ({blit_frames}/40 frames)"
        );
    }

    #[test]
    fn selection_change_bands_only_the_selection_rows() {
        let Some(mut t) = term() else { return };
        t.process(b"alpha beta gamma\r\ndelta epsilon\r\nzeta eta");
        t.render();
        t.selection_start(1, 0);
        t.selection_extend(1, 5);
        t.render();
        assert!(t.present_band_count() >= 1);
        let banded_px: i32 = t
            .present_bands
            .as_chunks::<4>()
            .0
            .iter()
            .map(|q| q[2] * q[3])
            .sum();
        assert!(
            (banded_px as usize) < t.width * t.height,
            "a one-row selection drag repaints a sub-frame band"
        );
        assert_eq!(t.rgba, full_expansion(&t));
    }
}
