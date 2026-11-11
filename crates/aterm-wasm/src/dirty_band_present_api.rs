// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Dirty-band present for the CPU wasm path. `render()` re-expands only the damaged
//! pixel bands into the persistent RGBA buffer, which stays fully coherent, and
//! [`AtermTerminal::present_band_count`] / [`AtermTerminal::present_bands_ptr`]
//! export them as `(x, y, w, h)` quads for the host to `putImageData` one by one.

use crate::AtermTerminal;
#[cfg(target_arch = "wasm32")]
use aterm_wasm_abi::export;

/// Expand `pixels[start..end]` (`0xTTRRGGBB`) into `rgba[start*4..end*4]` (straight
/// RGBA8, alpha = `255 - TT`).
#[inline]
pub(crate) fn expand_rgba_band(pixels: &[u32], rgba: &mut [u8], start: usize, end: usize) {
    let end = end.min(pixels.len()).min(rgba.len() / 4);
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

/// Shift rows `[y0, y1)` of a `w`-wide RGBA buffer by `frac_px` device pixels in place.
#[inline]
pub(crate) fn shift_rgba_band(rgba: &mut [u8], w: usize, y0: usize, y1: usize, frac_px: i64) {
    if w == 0 || y0 >= y1 || frac_px == 0 {
        return;
    }
    let Some(stride) = w.checked_mul(4) else {
        return;
    };
    if y1.checked_mul(stride).is_none_or(|end| end > rgba.len()) {
        return;
    }
    let band_h = y1 - y0;
    let mag = usize::try_from(frac_px.unsigned_abs()).unwrap_or(usize::MAX);
    let moved = band_h.saturating_sub(mag);
    if frac_px > 0 {
        for i in 0..moved {
            let dst = y0 + i;
            let src = dst + mag;
            rgba.copy_within(src * stride..src * stride + stride, dst * stride);
        }
    } else {
        for i in 0..moved {
            let dst = y1 - 1 - i;
            let src = dst - mag;
            rgba.copy_within(src * stride..src * stride + stride, dst * stride);
        }
    }
}

/// Full-frame expansion plus the single full band; also sizes the RGBA buffer.
#[inline]
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
#[inline]
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
        let y0 = grid_top
            .saturating_add(run_start.saturating_mul(cell_h))
            .min(frame_h);
        let y1 = grid_top
            .saturating_add(r.saturating_mul(cell_h))
            .min(frame_h);
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
            DamageOutcome::Scroll { delta_rows } if !stale && chromeless => {
                let grid_top = self.renderer.grid_top();
                let y1 = (grid_top + self.rows * ch).min(self.height);
                shift_rgba_band(
                    &mut self.rgba,
                    self.width,
                    grid_top,
                    y1,
                    i64::from(delta_rows).saturating_mul(i64::try_from(ch).unwrap_or(i64::MAX)),
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
            _ => {
                let (w, h) = (self.width, self.height);
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
    pub const fn present_band_count(&self) -> u32 {
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
#[path = "dirty_band_present_api_tests.rs"]
mod tests;
