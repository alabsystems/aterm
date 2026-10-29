// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sixel (DEC graphics) DCS decoder producing packed-RGBA [`SixelImage`] rasters bounded by
//! [`SIXEL_MAX_DIMENSION`] and [`MAX_COLOR_REGISTERS`].

#![forbid(unsafe_code)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use core::num::NonZeroUsize;

/// Maximum number of color registers a sixel stream may define/select.
pub const MAX_COLOR_REGISTERS: usize = 1024;

/// Maximum sixel image dimension (pixels) on either axis.
pub const SIXEL_MAX_DIMENSION: usize = 4096;

/// Maximum total pixel count (width × height) of a decoded raster.
pub const SIXEL_MAX_PIXELS: usize = 4 * 1024 * 1024;

const SIXEL_ROW_MASKS: [u8; 6] = [1, 2, 4, 8, 16, 32];

/// Maximum decoded-image byte budget (packed RGBA, 4 bytes/px) for a single sixel sequence.
pub const SIXEL_MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

const SIXEL_MAX_PARAMS: usize = 8;

const TRANSPARENT: u32 = 0;

/// A decoded sixel raster image.
#[derive(Debug, Clone)]
pub struct SixelImage {
    width: usize,
    height: usize,
    /// Row-major packed `0xAARRGGBB`, length == `width * height`.
    pixels: Vec<u32>,
}

impl SixelImage {
    /// Image width in pixels.
    #[inline]
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Image height in pixels.
    #[inline]
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Packed `0xAARRGGBB` pixels, row-major, length `width * height`.
    #[inline]
    #[must_use]
    pub const fn pixels(&self) -> &[u32] {
        self.pixels.as_slice()
    }

    /// Number of grid rows this image spans given a cell height in pixels.
    #[inline]
    #[must_use]
    pub const fn rows_spanned(&self, cell_h: u16) -> usize {
        let ch = cell_h as usize;
        let cell_h = if ch == 0 { 1 } else { ch };
        let rem_bump = if self.height.is_multiple_of(cell_h) {
            0
        } else {
            1
        };
        let ceil = (self.height / cell_h).saturating_add(rem_bump);
        let min_span = if self.height > 0 { 1 } else { 0 };
        if ceil > min_span { ceil } else { min_span }
    }

    /// Number of grid columns this image spans given a cell width in pixels.
    #[inline]
    #[must_use]
    pub const fn cols_spanned(&self, cell_w: u16) -> usize {
        let cw = cell_w as usize;
        let cell_w = if cw == 0 { 1 } else { cw };
        let rem_bump = if self.width.is_multiple_of(cell_w) {
            0
        } else {
            1
        };
        let ceil = (self.width / cell_w).saturating_add(rem_bump);
        let min_span = if self.width > 0 { 1 } else { 0 };
        if ceil > min_span { ceil } else { min_span }
    }
}

/// Incremental sixel DCS decoder.
#[derive(Debug)]
pub struct SixelDecoder {
    /// `true` between `hook` and `unhook`/`abort`.
    active: bool,

    /// Color registers as packed `0x00RRGGBB`.
    palette: Vec<u32>,
    /// Currently selected color register.
    current_color: usize,

    /// Numeric-parameter accumulator for `#`/`!`/`"` introducers.
    params: Vec<u32>,
    /// Which introducer we are collecting parameters for.
    mode: ParamMode,

    /// Packed `0xAARRGGBB` raster, row-major over `alloc_width`.
    raster: Vec<u32>,
    /// Allocated raster width (stride).
    alloc_width: usize,
    /// Allocated raster height.
    alloc_height: usize,

    /// Current write column (x).
    x: usize,
    /// Top pixel row of the current 6-px band (y).
    band_top: usize,
    /// Exclusive max x written (for final width).
    max_x: usize,
    /// Exclusive max y written (for final height).
    max_y: usize,

    /// Declared raster width from `"` DECGRA (0 = none).
    declared_w: usize,
    /// Declared raster height from `"` DECGRA (0 = none).
    declared_h: usize,
    /// Pending DECGRI repeat count for the next data byte (0 = none).
    pending_repeat: u32,
}

/// Which numeric-parameter introducer the decoder is mid-collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamMode {
    /// No pending introducer.
    None,
    /// `#` — color select/define.
    Color,
    /// `!` — DECGRI repeat.
    Repeat,
    /// `"` — DECGRA raster attributes.
    Raster,
}

impl Default for SixelDecoder {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl SixelDecoder {
    /// A fresh, inactive decoder with the default VT340 16-color palette.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: false,
            palette: default_palette(),
            current_color: 0,
            params: Vec::new(),
            mode: ParamMode::None,
            raster: Vec::new(),
            alloc_width: 0,
            alloc_height: 0,
            x: 0,
            band_top: 0,
            max_x: 0,
            max_y: 0,
            declared_w: 0,
            declared_h: 0,
            pending_repeat: 0,
        }
    }

    /// Begin a sixel sequence.
    pub fn hook(&mut self) {
        self.palette = default_palette();
        self.current_color = 0;
        self.params.clear();
        self.mode = ParamMode::None;
        self.raster.clear();
        self.alloc_width = 0;
        self.alloc_height = 0;
        self.x = 0;
        self.band_top = 0;
        self.max_x = 0;
        self.max_y = 0;
        self.declared_w = 0;
        self.declared_h = 0;
        self.pending_repeat = 0;
        self.active = true;
    }

    /// Feed one sixel data byte.
    #[inline]
    pub fn put(&mut self, byte: u8) {
        if !self.active {
            return;
        }
        match byte {
            b'#' => {
                self.start_params(ParamMode::Color);
            }
            b'!' => {
                self.start_params(ParamMode::Repeat);
            }
            b'"' => {
                self.start_params(ParamMode::Raster);
            }
            b'0'..=b'9' => {
                let d = u32::from(byte - b'0');
                if let Some(slot) = self.params.last_mut() {
                    *slot = slot.saturating_mul(10).saturating_add(d);
                } else {
                    self.params.push(d);
                }
            }
            b';' if self.params.len() < SIXEL_MAX_PARAMS => {
                self.params.push(0);
            }
            b'$' => {
                self.finish_params();
                self.x = 0;
                self.pending_repeat = 0;
            }
            b'-' => {
                self.finish_params();
                self.x = 0;
                self.band_top = self.band_top.saturating_add(6);
                self.pending_repeat = 0;
            }
            0x3F..=0x7E => {
                let bits = byte - 0x3F;
                self.finish_params();
                self.emit_sixel(bits, self.pending_repeat.max(1));
                self.pending_repeat = 0;
            }
            _ => {
                // Ignored.
            }
        }
    }

    /// Finalize the sequence and return the image, if one was produced.
    #[must_use]
    pub fn unhook(&mut self) -> Option<SixelImage> {
        if !self.active {
            return None;
        }
        self.finish_params();
        self.active = false;

        let width = self.max_x.max(self.declared_w).min(SIXEL_MAX_DIMENSION);
        let height = self.max_y.max(self.declared_h).min(SIXEL_MAX_DIMENSION);
        let total = width.saturating_mul(height);
        if width == 0 || height == 0 || total > SIXEL_MAX_PIXELS {
            self.release();
            return None;
        }
        const _: () = assert!(SIXEL_MAX_PIXELS * 4 <= SIXEL_MAX_IMAGE_BYTES);

        let stride = self.alloc_width;
        let total = total.min(SIXEL_MAX_PIXELS);
        let mut pixels = Vec::with_capacity(total);
        let in_w = width.min(stride);
        let pad = width.saturating_sub(in_w);
        for y in 0..height.min(self.alloc_height) {
            let start = y.saturating_mul(stride);
            if let Some(row) = self.raster.get(start..start.saturating_add(in_w)) {
                pixels.extend_from_slice(row);
            }
            pixels.resize(pixels.len().saturating_add(pad).min(total), TRANSPARENT);
        }
        pixels.resize(total, TRANSPARENT);
        self.release();
        Some(SixelImage {
            width,
            height,
            pixels,
        })
    }

    /// Abort the current sequence, dropping all buffers without allocating a copy.
    #[inline]
    pub fn abort(&mut self) {
        self.active = false;
        self.release();
    }

    /// Live pixel-buffer byte size, for the engine's DCS memory budget.
    #[inline]
    #[must_use]
    pub const fn pixel_alloc_bytes(&self) -> usize {
        self.raster
            .len()
            .saturating_mul(core::mem::size_of::<u32>())
    }

    fn release(&mut self) {
        self.raster = Vec::new();
        self.params = Vec::new();
        self.alloc_width = 0;
        self.alloc_height = 0;
        self.x = 0;
        self.band_top = 0;
        self.declared_w = 0;
        self.declared_h = 0;
        self.pending_repeat = 0;
    }

    /// Begin collecting params for a new introducer, finishing any prior one.
    #[inline]
    fn start_params(&mut self, mode: ParamMode) {
        self.finish_params();
        self.params.clear();
        self.mode = mode;
    }

    /// Apply the just-collected params for the pending introducer. `#` selects/
    /// defines a color, `"` declares raster geometry, and `!` stashes the repeat
    /// count in `pending_repeat` for the upcoming data byte.
    #[inline]
    fn finish_params(&mut self) {
        if self.mode == ParamMode::None && self.params.is_empty() {
            return;
        }
        match self.mode {
            ParamMode::Color => self.apply_color(),
            ParamMode::Raster => self.apply_raster(),
            ParamMode::Repeat => self.apply_repeat(),
            ParamMode::None => {}
        }
        self.mode = ParamMode::None;
        self.params.clear();
    }

    fn apply_color(&mut self) {
        let Some(&pc) = self.params.first() else {
            return;
        };
        let reg = (pc as usize).min(MAX_COLOR_REGISTERS.saturating_sub(1));
        if let (Some(&pu), Some(&px), Some(&py), Some(&pz)) = (
            self.params.get(1),
            self.params.get(2),
            self.params.get(3),
            self.params.get(4),
        ) {
            let rgb = match pu {
                1 => hls_to_rgb(px, py, pz),
                _ => rgb_percent(px, py, pz),
            };
            if let Some(slot) = self.palette.get_mut(reg) {
                *slot = rgb;
            }
        }
        self.current_color = reg;
    }

    /// `"Pan;Pad;Ph;Pv` — record declared raster geometry.
    #[inline]
    fn apply_raster(&mut self) {
        let ph = self.params.get(2).copied().unwrap_or(0) as usize;
        let pv = self.params.get(3).copied().unwrap_or(0) as usize;
        self.declared_w = ph.min(SIXEL_MAX_DIMENSION);
        self.declared_h = pv.min(SIXEL_MAX_DIMENSION);
    }

    /// `!Pn` — stash the repeat count for the next data byte.
    #[inline]
    fn apply_repeat(&mut self) {
        self.pending_repeat = self.params.first().copied().unwrap_or(0);
    }

    fn emit_sixel(&mut self, bits: u8, count: u32) {
        let max_run = SIXEL_MAX_DIMENSION.saturating_sub(self.x);
        let count = (count as usize).min(max_run);
        if count == 0 {
            return;
        }
        let bits = bits & 0x3F;
        if bits == 0 {
            self.x = self.x.saturating_add(count).min(SIXEL_MAX_DIMENSION);
            self.max_x = self.max_x.max(self.x.min(SIXEL_MAX_DIMENSION));
            return;
        }
        let color = self.palette.get(self.current_color).copied().unwrap_or(0) & 0x00FF_FFFF;
        let highest_row = (u8::BITS - 1 - bits.leading_zeros()) as usize;
        let want_y = self.band_top.saturating_add(highest_row);
        self.ensure_capacity(self.x.saturating_add(count).saturating_sub(1), want_y);
        let stride = self.alloc_width;
        if stride == 0 {
            return;
        }
        if self.raster.len() == stride.saturating_mul(self.alloc_height) {
            let run_end = self.x.saturating_add(count).min(self.alloc_width);
            if run_end > self.x {
                let pixel = 0xFF00_0000 | color;
                let mut painted_max_y = 0usize;
                if run_end.saturating_sub(self.x) == 1 {
                    for (row, mask) in SIXEL_ROW_MASKS.into_iter().enumerate() {
                        if bits & mask == 0 {
                            continue;
                        }
                        let py = self.band_top.saturating_add(row);
                        if py >= self.alloc_height {
                            break;
                        }
                        let idx = py.saturating_mul(stride).saturating_add(self.x);
                        if let Some(slot) = self.raster.get_mut(idx) {
                            *slot = pixel;
                            painted_max_y = painted_max_y.max(py.saturating_add(1));
                        }
                    }
                } else {
                    for (row, mask) in SIXEL_ROW_MASKS.into_iter().enumerate() {
                        if bits & mask == 0 {
                            continue;
                        }
                        let py = self.band_top.saturating_add(row);
                        if py >= self.alloc_height {
                            break;
                        }
                        let base = py.saturating_mul(stride);
                        let s = base.saturating_add(self.x);
                        let e = base.saturating_add(run_end);
                        if let Some(slice) = self.raster.get_mut(s..e) {
                            slice.fill(pixel);
                            painted_max_y = painted_max_y.max(py.saturating_add(1));
                        }
                    }
                }
                if painted_max_y > 0 {
                    self.max_x = self.max_x.max(run_end);
                    self.max_y = self.max_y.max(painted_max_y);
                }
            }
        } else {
            self.paint_pixelwise(bits, count, color, stride);
        }
        self.x = self.x.saturating_add(count).min(SIXEL_MAX_DIMENSION);
        self.max_x = self.max_x.max(self.x.min(SIXEL_MAX_DIMENSION));
    }

    /// Cold fallback per-pixel paint when `raster.len() != alloc_width * alloc_height`.
    #[cold]
    #[inline(never)]
    fn paint_pixelwise(&mut self, bits: u8, count: usize, color: u32, stride: usize) {
        for col in 0..count {
            let px = self.x.saturating_add(col);
            if px >= self.alloc_width {
                break;
            }
            for (row, mask) in SIXEL_ROW_MASKS.into_iter().enumerate() {
                if bits & mask == 0 {
                    continue;
                }
                let py = self.band_top.saturating_add(row);
                if py >= self.alloc_height {
                    break;
                }
                let idx = py.saturating_mul(stride).saturating_add(px);
                if let Some(slot) = self.raster.get_mut(idx) {
                    *slot = 0xFF00_0000 | color;
                    self.max_x = self.max_x.max(px.saturating_add(1));
                    self.max_y = self.max_y.max(py.saturating_add(1));
                }
            }
        }
    }

    /// Grow the raster so coordinates up to `(want_x, want_y)` inclusive
    /// are addressable, clamped to [`SIXEL_MAX_DIMENSION`].
    #[cfg_attr(trust_verify, trust::skip)]
    fn ensure_capacity(&mut self, want_x: usize, want_y: usize) {
        let need_w = want_x.saturating_add(1).min(SIXEL_MAX_DIMENSION);
        let need_h = want_y.saturating_add(1).min(SIXEL_MAX_DIMENSION);
        if need_w <= self.alloc_width && need_h <= self.alloc_height {
            return;
        }
        let new_h = need_h.max(self.alloc_height);
        let new_w = if need_w > self.alloc_width {
            let geo = self
                .alloc_width
                .saturating_mul(2)
                .max(need_w)
                .min(SIXEL_MAX_DIMENSION);
            if geo
                .checked_mul(new_h)
                .is_some_and(|n| n <= SIXEL_MAX_PIXELS)
            {
                geo
            } else {
                let per_h = if let Some(h) = NonZeroUsize::new(new_h) {
                    SIXEL_MAX_PIXELS / h
                } else {
                    SIXEL_MAX_PIXELS
                };
                per_h
                    .min(SIXEL_MAX_DIMENSION)
                    .max(need_w)
                    .max(self.alloc_width)
            }
        } else {
            self.alloc_width
        };
        let new_len = new_w.checked_mul(new_h).unwrap_or(0);
        if new_len == 0 || new_len > SIXEL_MAX_PIXELS {
            return;
        }
        if new_w == self.alloc_width {
            self.raster.resize(new_len, 0);
            self.alloc_height = new_h;
            return;
        }
        let mut new_raster = vec![0u32; new_len];
        let old_stride = self.alloc_width;
        if old_stride != 0 && new_w != 0 {
            let dst_rows = new_raster.chunks_mut(new_w);
            let src_rows = self.raster.chunks(old_stride);
            for (dst_row, src_row) in dst_rows.zip(src_rows) {
                for (d, s) in dst_row.iter_mut().zip(src_row.iter()) {
                    *d = *s;
                }
            }
        }
        self.raster = new_raster;
        self.alloc_width = new_w;
        self.alloc_height = new_h;
    }
}

/// Scale an `0..=100` percent component to `0..=255`.
#[inline]
#[must_use]
pub const fn scale_pct(v: u32) -> u32 {
    let v = if v > 100 { 100 } else { v };
    v.saturating_mul(255).saturating_add(50) / 100
}

/// Pack an RGB-percent triple (`0..=100` each) into `0x00RRGGBB`.
#[inline]
#[must_use]
pub const fn rgb_percent(r: u32, g: u32, b: u32) -> u32 {
    (scale_pct(r) << 16) | (scale_pct(g) << 8) | scale_pct(b)
}

/// Convert DEC HLS (`h`=0..360, `l`=0..100, `s`=0..100) to packed `0x00RRGGBB`.
#[allow(
    clippy::manual_range_contains,
    reason = "manual comparisons keep the float range-pin transparent to the Trust verifier"
)]
fn hls_to_rgb(h: u32, l: u32, s: u32) -> u32 {
    let li = if l > 100 { 100 } else { l };
    let si = if s > 100 { 100 } else { s };
    let l = (li as f64) / 100.0;
    let s = (si as f64) / 100.0;
    let h = (h % 360) as f64;
    let hue = (h + 240.0) % 360.0;
    let chroma_scale = 1.0 - (2.0 * l - 1.0).abs();
    let c = if chroma_scale > 0.0 && chroma_scale <= 1.0 && s > 0.0 && s <= 1.0 {
        chroma_scale.mul_add(s, 0.0)
    } else {
        0.0
    };
    let h6 = hue / 60.0;
    let xfac = 1.0 - ((h6 % 2.0) - 1.0).abs();
    let x = if c > 0.0 && c <= 1.0 && xfac > 0.0 && xfac <= 1.0 {
        c.mul_add(xfac, 0.0)
    } else {
        0.0
    };
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_u8 = |v: f64| {
        let v = if v >= 0.0 && v <= 1.0 { v } else { 0.0 };
        let mb = if m >= -0.5 && m <= 1.0 { m } else { 0.0 };
        let chan = v.mul_add(1.0, mb);
        if chan >= 1.0 {
            255
        } else if chan > 0.0 {
            chan.mul_add(255.0, 0.0).round() as u32
        } else {
            0
        }
    };
    (to_u8(r1) << 16) | (to_u8(g1) << 8) | to_u8(b1)
}

fn default_palette() -> Vec<u32> {
    const DEC: [(u32, u32, u32); 16] = [
        (0, 0, 0),
        (20, 20, 80),
        (80, 13, 13),
        (20, 80, 20),
        (80, 20, 80),
        (20, 80, 80),
        (80, 80, 20),
        (53, 53, 53),
        (26, 26, 26),
        (33, 33, 60),
        (60, 26, 26),
        (33, 60, 33),
        (60, 33, 60),
        (33, 60, 60),
        (60, 60, 33),
        (80, 80, 80),
    ];
    let mut pal = vec![0u32; MAX_COLOR_REGISTERS];
    for (i, &(r, g, b)) in DEC.iter().enumerate() {
        // `i < 16 <= MAX_COLOR_REGISTERS == pal.len()`, so this is always
        // `Some`; the verifier reloads `pal.len()` and can't carry the
        // `vec![_; 1024]` length, so index via get_mut (behavior-identical to
        // `pal[i] = …`).
        if let Some(slot) = pal.get_mut(i) {
            *slot = rgb_percent(r, g, b);
        }
    }
    pal
}

#[cfg(test)]
mod tests;
