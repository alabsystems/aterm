// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Dirty-band present for the CPU wasm path. `render()` re-expands only the damaged
//! pixel bands into the persistent RGBA buffer, which stays fully coherent, and
//! [`AtermTerminal::present_band_count`] / [`AtermTerminal::present_bands_ptr`]
//! export them as `(x, y, w, h)` quads for the host to `putImageData` one by one.
//!
//! Band semantics:
//!
//! * `Full` damage (first frame, resize, theme/font epoch, DECDHL) → one full-frame
//!   band.
//! * Gate hit → zero bands; the host skips conversion and `putImageData`.
//! * Row damage → one full-width band per run of dirty grid rows, only without chrome:
//!   a chromed frame (`set_chrome`) lets effects paint outside the row lattice, so it
//!   always gets the full band.
//! * A whole-row scroll blit, a presented sub-row residual and the frame that clears
//!   it move the whole grid band → one full-frame band.
//!
//! Engine-drawn overlays (cursor, selection, effects) are in the engine's damage.
//! Host-drawn overlays (IME preedit, find highlights, DOM decorations) are not: draw
//! them on a separate canvas above this one, or repaint them after every nonzero-band
//! present, or partial presents will ghost them.

use crate::AtermTerminal;
#[cfg(target_arch = "wasm32")]
use aterm_wasm_abi::export;

/// Expand `pixels[start..end]` (0xTTRRGGBB) into `rgba[start*4..end*4]` (straight
/// RGBA8, alpha = 255 − TT).
///
/// The innermost loop of the CPU present path: zipping `as_chunks_mut::<4>()` proves
/// the bound once per band, so each pixel is one 4-byte store rather than four
/// bounds-checked byte writes that block LLVM's store merging.
pub(crate) fn expand_rgba_band(pixels: &[u32], rgba: &mut [u8], start: usize, end: usize) {
    let end = end.min(pixels.len());
    let start = start.min(end);
    for (&p, out) in pixels[start..end]
        .iter()
        .zip(rgba[start * 4..end * 4].as_chunks_mut::<4>().0)
    {
        *out = [
            (p >> 16) as u8,
            (p >> 8) as u8,
            p as u8,
            0xff - (p >> 24) as u8,
        ];
    }
}

/// Shift rows `[y0, y1)` of a `w`-wide RGBA buffer by `frac_px` device px in place:
/// the u8 twin of `scroll_translate::translate_grid_band_in_place`, with the same
/// convention (positive = up) and a direction-matched walk so no source row is read
/// after it is overwritten.
pub(crate) fn shift_rgba_band(rgba: &mut [u8], w: usize, y0: usize, y1: usize, frac_px: i64) {
    if w == 0 || y0 >= y1 || frac_px == 0 {
        return;
    }
    let stride = w * 4;
    let band_h = y1 - y0;
    let mag = frac_px.unsigned_abs() as usize;
    let moved = band_h.saturating_sub(mag);
    if frac_px > 0 {
        // Shift up: walk top-down.
        for i in 0..moved {
            let dst = y0 + i;
            let src = dst + mag;
            rgba.copy_within(src * stride..src * stride + stride, dst * stride);
        }
    } else {
        // Shift down: walk bottom-up.
        for i in 0..moved {
            let dst = y1 - 1 - i;
            let src = dst - mag;
            rgba.copy_within(src * stride..src * stride + stride, dst * stride);
        }
    }
}

/// Full-frame expansion plus the single full band; also sizes the RGBA buffer.
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

/// Merge runs of dirty grid rows into full-width `x, y, w, h` bands in `out`: row `r`
/// spans `grid_top + r*cell_h ..+cell_h`, clamped to the frame height.
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
    /// Apply the just-rendered frame's damage to the persistent RGBA buffer and
    /// publish the band list. Not called on a frame carrying or releasing a sub-row
    /// translate: the caller full-expands the translated view instead.
    pub(crate) fn expand_damage_to_rgba(&mut self) {
        use aterm_render::DamageOutcome;
        let expected = self.width * self.height * 4;
        let stale = self.rgba.len() != expected;
        let (cw, ch) = self.renderer.cell_size();
        // Chrome lets effects paint outside the grid-row lattice (the spill band's
        // job), so only a chromeless frame takes row bands.
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
            // Scroll blit: the engine shifted its cache rows by `delta_rows·ch` and
            // re-rasterized only the exposed strip and cursor rows. Mirror the shift
            // with a row memmove, re-expand the flagged bands, then export one full
            // band: every retained row moved on the host canvas.
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
                self.present_bands.clear();
                self.present_bands.extend_from_slice(&[
                    0,
                    0,
                    self.width as i32,
                    self.height as i32,
                ]);
            }
            // Full damage, or a stale buffer (first frame, resize race).
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

#[cfg_attr(target_arch = "wasm32", export)]
impl AtermTerminal {
    /// Number of dirty present bands from the last `render()`; `0` means the frame is
    /// unchanged, so skip the RGBA read and `putImageData`.
    pub fn present_band_count(&self) -> u32 {
        (self.present_bands.len() / 4) as u32
    }

    /// Byte offset in wasm linear memory of the band array: `present_band_count()`
    /// quads of `i32` `x, y, w, h`, frame-absolute device px, non-overlapping,
    /// top-to-bottom. As with `rgba_ptr`, consume it synchronously after `render()` and
    /// never cache the JS view across engine calls. Present each band with
    /// `putImageData(imageData, 0, 0, x, y, w, h)` over the ImageData `rgba_ptr` backs.
    pub fn present_bands_ptr(&self) -> usize {
        self.present_bands.as_ptr() as usize
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn term() -> AtermTerminal {
        crate::system_font_terminal(8, 40, 14.0)
    }

    /// What a from-scratch full expansion of the current frame produces.
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
        let mut t = term();
        t.process(b"hello band world\r\n");
        t.render().expect("renderable frame");
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "first frame presents one full band"
        );
        let full_rgba = t.rgba.clone();
        t.render().expect("renderable frame");
        assert_eq!(t.present_band_count(), 0, "no change → zero bands");
        assert_eq!(t.rgba, full_rgba, "gate-hit leaves the buffer intact");
    }

    #[test]
    fn keystroke_damage_exports_a_sub_frame_band_and_stays_byte_exact() {
        let mut t = term();
        t.process(b"line one\r\nline two\r\nline three");
        t.render().expect("renderable frame");
        t.process_str("x"); // one-row edit on the cursor row
        t.render().expect("renderable frame");
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
        assert_eq!(t.rgba, full_expansion(&t), "partial update is byte-exact");
    }

    #[test]
    fn scroll_and_resize_take_the_full_band_path() {
        let mut t = term();
        for i in 0..50 {
            t.process(format!("scroll line {i}\r\n").as_bytes());
        }
        t.render().expect("renderable frame");
        // A display-offset change presents one full band.
        t.scroll_lines(3);
        t.render().expect("renderable frame");
        assert_eq!(t.present_bands[..2], [0, 0]);
        assert_eq!(t.present_bands[2..4], [t.width as i32, t.height as i32]);
        assert_eq!(t.rgba, full_expansion(&t));
        t.resize(10, 44).expect("admitted geometry");
        t.render().expect("renderable frame");
        assert_eq!(t.present_band_count(), 1);
        assert_eq!(t.rgba.len(), t.width * t.height * 4);
        assert_eq!(t.rgba, full_expansion(&t));
    }

    #[test]
    fn fractional_scroll_frames_present_the_full_band() {
        let mut t = term();
        for i in 0..50 {
            t.process(format!("frac line {i}\r\n").as_bytes());
        }
        t.render().expect("renderable frame");
        let half_cell = t.cell_height() as f64 / 2.0;
        t.scroll_px(half_cell); // sub-row residual banks, no whole-row flip
        t.render().expect("renderable frame");
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "a translated band frame must not export partial bands"
        );
        // The frame that releases the residual is full too.
        t.scroll_px(-half_cell);
        t.render().expect("renderable frame");
        assert_eq!(
            t.present_bands,
            vec![0, 0, t.width as i32, t.height as i32],
            "the residual-clearing frame re-presents the whole band"
        );
        assert_eq!(t.rgba, full_expansion(&t));
    }

    /// The whole-row scroll blit is taken, and every scrolled frame's buffer equals a
    /// from-scratch full expansion.
    #[test]
    fn scroll_blit_is_taken_and_stays_byte_exact() {
        use aterm_render::DamageOutcome;
        let mut t = term();
        t.set_scrollback_limit(4000);
        for i in 0..2000 {
            t.process(format!("scroll blit line {i}\r\n").as_bytes());
        }
        t.scroll_to_bottom();
        t.render().expect("renderable frame");
        let mut blit_frames = 0usize;
        for _ in 0..60 {
            t.scroll_lines(3);
            t.render().expect("renderable frame");
            if matches!(t.win.last_damage(), DamageOutcome::Scroll { .. }) {
                blit_frames += 1;
            }
            assert_eq!(
                t.rgba,
                full_expansion(&t),
                "scroll-blit present must be byte-identical to a full repaint"
            );
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

    /// The scroll-blit present stays byte-exact when retained rows carry
    /// absolute-Y-parity shade dithers (U+2591–2593) and upward-overshooting capitals,
    /// which the engine re-rasters into its cache.
    #[test]
    fn shade_scroll_blit_present_stays_byte_exact() {
        use aterm_render::DamageOutcome;
        let mut t = term();
        t.set_scrollback_limit(4000);
        for i in 0..1500 {
            // ▓▓▒░ shade + accented capitals (upward overshoot) + box glyphs.
            let line =
                format!("\u{2593}\u{2593}\u{2592}\u{2591} \u{c9}\u{c1}\u{d1} r{i} \u{2588}\r\n");
            t.process(line.as_bytes());
        }
        t.scroll_to_bottom();
        t.render().expect("renderable frame");
        let mut blit_frames = 0usize;
        for _ in 0..40 {
            t.scroll_lines(1); // an odd pixel shift when cell_h is odd
            t.render().expect("renderable frame");
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
        let mut t = term();
        t.process(b"alpha beta gamma\r\ndelta epsilon\r\nzeta eta");
        t.render().expect("renderable frame");
        t.selection_start(1, 0);
        t.selection_extend(1, 5);
        t.render().expect("renderable frame");
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
