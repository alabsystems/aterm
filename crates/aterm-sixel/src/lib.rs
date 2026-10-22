// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sixel (DEC graphics) DCS decoder.
//!
//! Parses the `DCS Ps q <sixel-data> ST` body that VT300-class terminals use to
//! paint raster graphics, and produces a packed-RGBA [`SixelImage`] that the
//! terminal engine routes into its existing inline-image placement/blit path
//! (the same one OSC 1337 `File=` uses). The engine stores inline images
//! undecoded (it decodes a PNG only to compose Kitty animation frames), so
//! this crate is the only place sixel pixels are materialized; it has no
//! dependencies and is pure `std`.
//!
//! ## Wire format handled
//!
//! - **Data bytes** `0x3F..=0x7E`: `value = byte - 0x3F` is a 6-bit column of
//!   pixels (LSB = topmost) painted in the current color at the current x, then
//!   x advances by 1.
//! - **`#` color introducer**: `#Pc` selects register `Pc`; `#Pc;Pu;Px;Py;Pz`
//!   defines register `Pc` — `Pu==2` is RGB with `Px,Py,Pz` in `0..=100`
//!   (scaled to `0..=255`), `Pu==1` is HLS (`Px`=hue 0..360, `Py`=lightness
//!   0..100, `Pz`=saturation 0..100) converted to RGB.
//! - **`!Pn` DECGRI**: the next single data byte is repeated `Pn` times.
//! - **`"Pan;Pad;Ph;Pv` DECGRA**: raster attributes — `Ph`/`Pv` declare the
//!   image width/height (clamped to [`SIXEL_MAX_DIMENSION`]). The declared
//!   geometry is recorded as the fallback output size but does not pre-size the
//!   backing raster, which grows on demand as pixels are painted (so the
//!   engine's put()-time DCS pixel budget bounds it). The aspect-ratio
//!   `Pan`/`Pad` are parsed but not applied (1:1 pixels).
//! - **`$`**: graphics carriage return — x back to 0, y band unchanged.
//! - **`-`**: graphics new-line — advance y by one 6-px band, x back to 0.
//!
//! ## Deliberately deferred
//!
//! - HLS rounding edge-cases (a standard integer HLS→RGB is used).
//! - P2 background-select transparency semantics: unset pixels are left fully
//!   transparent (`A == 0`) so the cell background shows through the engine's
//!   straight-alpha-over blit. Full `P2==1` vs `P2==0` device-default fill is
//!   not modeled.
//! - DECGRA aspect-ratio (`Pan`/`Pad`) non-1:1 pixel scaling.
//! - More than [`MAX_COLOR_REGISTERS`] registers; private/animation color maps.
//! - Sub-band partial scrolling semantics beyond a whole 6-px band.

#![forbid(unsafe_code)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use core::num::NonZeroUsize;

/// Maximum number of color registers a sixel stream may define/select.
///
/// Matches the VT340 family + the limit XTSMGRAPHICS reports. Selecting or
/// defining a register `>= MAX_COLOR_REGISTERS` is clamped to the last index so
/// a hostile stream can never grow the palette unbounded.
pub const MAX_COLOR_REGISTERS: usize = 1024;

/// Maximum sixel image dimension (pixels) on either axis. A hard clamp on the
/// raster buffer so a hostile `"` raster declaration or a long run of data/`-`
/// bands cannot allocate unboundedly. Not what XTSMGRAPHICS reports: the
/// pixel and DCS budgets refuse a raster this size, so aterm-core advertises
/// the largest that places.
pub const SIXEL_MAX_DIMENSION: usize = 4096;

/// Maximum total pixel count (width × height) of a decoded raster, independent
/// of the per-axis [`SIXEL_MAX_DIMENSION`] clamp. 4 Mi pixels = 16 MiB at
/// 4 bytes/pixel, matching [`SIXEL_MAX_IMAGE_BYTES`] so an over-cap raster
/// (e.g. a 4096×4096 DECGRA) is refused before raster/compose allocation.
pub const SIXEL_MAX_PIXELS: usize = 4 * 1024 * 1024;

/// The six pixel rows a sixel byte paints, top first: bit `row` of the data
/// value. Iterated as masks rather than computed as `1 << row`, so the paint
/// loops carry no shift whose range a reader (or the verifier) has to bound.
const SIXEL_ROW_MASKS: [u8; 6] = [1, 2, 4, 8, 16, 32];

/// Maximum decoded-image byte budget (packed RGBA, 4 bytes/px) a single sixel
/// sequence may materialize at [`unhook`](SixelDecoder::unhook).
///
/// Mirrors `aterm-core`'s `MAX_IMAGE_BYTES` (16 MiB, [`SIXEL_MAX_PIXELS`] × 4)
/// so oversized declared geometry is rejected before allocating the output
/// `pixels` buffer.
pub const SIXEL_MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum numeric parameters a sixel introducer (`#`/`!`/`"`) may accumulate.
///
/// The richest introducer, `#Pc;Pu;Px;Py;Pz`, uses 5 params; 8 leaves headroom
/// while bounding the parameter Vec to O(1) under a `;`-separator flood (each
/// `;` would otherwise push an uncharged slot, since `params` is excluded from
/// the pixel-allocation budget).
const SIXEL_MAX_PARAMS: usize = 8;

/// Packed `0x00000000` for a fully-transparent pixel (alpha `0x00` = unset).
const TRANSPARENT: u32 = 0;

/// A decoded sixel raster image: packed RGBA, ready for inline-image placement.
///
/// `pixels()` is row-major `width * height` packed `0xAARRGGBB` u32s (alpha in
/// the top byte). Unset pixels are fully transparent (`0x00000000`).
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
    /// Always at least 1 for a non-empty image.
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
    /// Always at least 1 for a non-empty image.
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
///
/// Lifecycle mirrors the parser's DCS callbacks: [`hook`](Self::hook) at the
/// final byte, [`put`](Self::put) per data byte, [`unhook`](Self::unhook) at ST
/// (yielding the image), or [`abort`](Self::abort) on cancel/interrupt.
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

    /// Packed `0xAARRGGBB` raster, row-major over `alloc_width`. The alpha byte
    /// carries the per-pixel "set" bit: `0x00` for an unpainted (transparent)
    /// pixel and `0xFF` for a painted one, so no parallel mask buffer is needed.
    raster: Vec<u32>,
    /// Allocated raster width (stride). Grows on demand up to the clamp.
    alloc_width: usize,
    /// Allocated raster height. Grows in 6-px bands up to the clamp.
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
    /// No pending introducer; digits/`;` are ignored as stray.
    None,
    /// `#` — color select/define.
    Color,
    /// `!` — DECGRI repeat; the next data byte is repeated.
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

    /// Begin a sixel sequence. The DCS numeric params (`P1`=aspect,
    /// `P2`=background-select, `P3`=horizontal-grid) are not load-bearing:
    /// the documented defaults are what the decoder implements.
    pub fn hook(&mut self) {
        // Reset transient decode state; keep the default palette fresh so a
        // reused decoder does not inherit colors from a previous image.
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
            // Commit the current slot and open a new one (default 0), capped at
            // `SIXEL_MAX_PARAMS` so a `;`-separator flood cannot grow `params`
            // unbounded.
            b';' if self.params.len() < SIXEL_MAX_PARAMS => {
                self.params.push(0);
            }
            b'$' => {
                self.finish_params();
                self.x = 0;
                // DECGRI `!Pn` applies only to the immediately-following sixel data
                // byte; a graphics-CR in between cancels a pending repeat.
                self.pending_repeat = 0;
            }
            b'-' => {
                self.finish_params();
                self.x = 0;
                self.band_top = self.band_top.saturating_add(6);
                // Same as `$`: a graphics-NL cancels a pending `!Pn` repeat.
                self.pending_repeat = 0;
            }
            0x3F..=0x7E => {
                let bits = byte - 0x3F;
                self.finish_params();
                self.emit_sixel(bits, self.pending_repeat.max(1));
                self.pending_repeat = 0;
            }
            _ => {
                // Any other byte (C0 controls, 8-bit, stray) is ignored as data
                // per the parser model; the decoder never panics on it.
            }
        }
    }

    /// Finalize the sequence and return the image, if one was produced.
    ///
    /// Returns `None` when the decoder was never hooked or the raster is empty
    /// (no painted pixels and no declared geometry). Always deactivates.
    #[must_use]
    pub fn unhook(&mut self) -> Option<SixelImage> {
        if !self.active {
            return None;
        }
        self.finish_params();
        self.active = false;

        // Final dimensions: painted extent, falling back to the declared raster
        // attributes when nothing was painted inside the declared box.
        let width = self.max_x.max(self.declared_w).min(SIXEL_MAX_DIMENSION);
        let height = self.max_y.max(self.declared_h).min(SIXEL_MAX_DIMENSION);
        let total = width.saturating_mul(height);
        if width == 0 || height == 0 || total > SIXEL_MAX_PIXELS {
            self.release();
            return None;
        }

        // Reject oversized geometry before composing the output buffer so a
        // DECGRA-only declaration cannot force a transient `pixels` allocation.
        let final_bytes = width.saturating_mul(height).saturating_mul(4);
        if final_bytes > SIXEL_MAX_IMAGE_BYTES {
            self.release();
            return None;
        }

        // Compose the final packed-RGBA buffer of `width * height` via row-wise
        // bulk copies. In-range raster cells are `0x00000000` (unpainted) or
        // `0xFF00_0000 | color` (painted), and any padding is `TRANSPARENT`.
        let stride = self.alloc_width;
        // Restate the `SIXEL_MAX_PIXELS` bound at the allocation for the verifier.
        let total = total.min(SIXEL_MAX_PIXELS);
        let mut pixels = Vec::with_capacity(total);
        let in_w = width.min(stride);
        let pad = width.saturating_sub(in_w);
        for y in 0..height.min(self.alloc_height) {
            // `y < alloc_height`, `in_w <= stride`, and `raster.len() ==
            // alloc_width * alloc_height` (`ensure_capacity`), so `get` is
            // always `Some` with one bounds check per row.
            let start = y.saturating_mul(stride);
            if let Some(row) = self.raster.get(start..start.saturating_add(in_w)) {
                pixels.extend_from_slice(row);
            }
            // Right-edge pad for width > stride: never painted ⇒ transparent.
            pixels.resize(pixels.len().saturating_add(pad).min(total), TRANSPARENT);
        }
        // Rows at/below alloc_height were never painted ⇒ fully transparent.
        pixels.resize(total, TRANSPARENT);
        self.release();
        Some(SixelImage {
            width,
            height,
            pixels,
        })
    }

    /// Abort the current sequence, dropping all buffers without allocating a
    /// copy. Used when a DCS is interrupted (parser reset, CAN, budget blown).
    #[inline]
    pub fn abort(&mut self) {
        self.active = false;
        self.release();
    }

    /// Live pixel-buffer byte size, for the engine's DCS memory budget.
    ///
    /// Counts the packed raster (4 bytes/px); the "set" bit lives in each
    /// pixel's alpha byte, so there is no separate mask buffer to charge.
    #[inline]
    #[must_use]
    pub const fn pixel_alloc_bytes(&self) -> usize {
        self.raster
            .len()
            .saturating_mul(core::mem::size_of::<u32>())
    }

    /// Returns `true` if the decoder is currently between `hook` and `unhook`/`abort`.
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the currently selected palette register (`0..MAX_COLOR_REGISTERS`).
    #[inline]
    #[must_use]
    pub const fn current_color(&self) -> usize {
        self.current_color
    }

    /// Returns the current write column (`x`) and band top (`band_top`).
    #[inline]
    #[must_use]
    pub const fn cursor_pos(&self) -> (usize, usize) {
        (self.x, self.band_top)
    }

    /// Returns the exclusive maximum painted coordinates `(max_x, max_y)`.
    #[inline]
    #[must_use]
    pub const fn painted_extent(&self) -> (usize, usize) {
        (self.max_x, self.max_y)
    }

    /// Returns the DECGRA declared raster dimensions `(declared_w, declared_h)`.
    #[inline]
    #[must_use]
    pub const fn declared_dimensions(&self) -> (usize, usize) {
        (self.declared_w, self.declared_h)
    }

    // --- internals ---------------------------------------------------------

    /// Drop the raster/mask/param buffers (keep palette small; it is reset on
    /// the next hook). Leaves the decoder ready for reuse.
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
    /// count in `pending_repeat` for the upcoming data byte. Idempotent; clears
    /// `mode` to `None`.
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

    /// `#Pc` (select) or `#Pc;Pu;Px;Py;Pz` (define).
    fn apply_color(&mut self) {
        let Some(&pc) = self.params.first() else {
            return;
        };
        // `MAX_COLOR_REGISTERS` is 1024, so the subtraction can never wrap;
        // `saturating_sub` is identical and carries no panic obligation.
        let reg = (pc as usize).min(MAX_COLOR_REGISTERS.saturating_sub(1));
        // Read the four define-params via `.get()` rather than `params[1..=4]`:
        // the verifier reloads `params.len()` per access and cannot carry a
        // `len() >= 5` guard across all four indexes.
        if let (Some(&pu), Some(&px), Some(&py), Some(&pz)) = (
            self.params.get(1),
            self.params.get(2),
            self.params.get(3),
            self.params.get(4),
        ) {
            let rgb = match pu {
                1 => hls_to_rgb(px, py, pz),
                // 2 (RGB) and any other value default to RGB-percent.
                _ => rgb_percent(px, py, pz),
            };
            // `reg < MAX_COLOR_REGISTERS == palette.len()`, so this is always
            // `Some`; `get_mut` avoids the index obligation the verifier can't
            // carry across field reloads.
            if let Some(slot) = self.palette.get_mut(reg) {
                *slot = rgb;
            }
        }
        self.current_color = reg;
    }

    /// `"Pan;Pad;Ph;Pv` — record declared raster geometry.
    #[inline]
    fn apply_raster(&mut self) {
        // Record `[Pan, Pad, Ph, Pv]`'s `Ph` (width) and `Pv` (height) without
        // pre-sizing `raster`: `emit_sixel` grows the buffer on demand so all
        // growth goes through the put()-time `pixel_alloc_bytes` budget check.
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

    /// Paint `count` columns of the 6-bit `bits` pattern starting at `self.x`.
    fn emit_sixel(&mut self, bits: u8, count: u32) {
        // Clamp the run so a hostile DECGRI cannot blow past the dimension cap.
        let max_run = SIXEL_MAX_DIMENSION.saturating_sub(self.x);
        let count = (count as usize).min(max_run);
        if count == 0 {
            // Even a clamped-to-zero run must not advance forever; bail.
            return;
        }
        if bits == 0 {
            // Empty column: still advances x (sixel columns are positional).
            self.x = self.x.saturating_add(count).min(SIXEL_MAX_DIMENSION);
            self.max_x = self.max_x.max(self.x.min(SIXEL_MAX_DIMENSION));
            return;
        }
        let color = self.palette.get(self.current_color).copied().unwrap_or(0) & 0x00FF_FFFF;
        let end_y = self.band_top.saturating_add(6);
        // Ensure capacity for the widest x and tallest band we touch.
        self.ensure_capacity(
            self.x.saturating_add(count).saturating_sub(1),
            end_y.saturating_sub(1),
        );
        let stride = self.alloc_width;
        if stride == 0 {
            return;
        }
        // Paint row-wise across the contiguous `[self.x, run_end)` slice of
        // each set row in the 6-px band so DECGRI repeats become slice fills,
        // with a direct-store branch for `count == 1` to avoid `slice::fill`
        // setup on unrepeated columns. `ensure_capacity` maintains
        // `raster.len() == stride * alloc_height`; the `#[cold]` `else` branch
        // keeps that invariant local.
        if self.raster.len() == stride.saturating_mul(self.alloc_height) {
            let run_end = self.x.saturating_add(count).min(self.alloc_width);
            if run_end > self.x {
                // Alpha 0xFF marks the pixel as set; `color` is masked to
                // 0x00FF_FFFF above, so painted black remains distinct from
                // `TRANSPARENT` (0).
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
    /// Kept `#[cold] #[inline(never)]` out of [`Self::emit_sixel`]'s hot body.
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
                    // Alpha 0xFF marks the pixel as set; `color` is masked to
                    // 0x00FF_FFFF by the caller, so no collision with the
                    // TRANSPARENT (0) sentinel even for painted black.
                    *slot = 0xFF00_0000 | color;
                    self.max_x = self.max_x.max(px.saturating_add(1));
                    self.max_y = self.max_y.max(py.saturating_add(1));
                }
            }
        }
    }

    /// Grow the raster/mask so coordinates up to `(want_x, want_y)` inclusive
    /// are addressable, clamped to [`SIXEL_MAX_DIMENSION`].
    //
    // Skipped under Trust: the body's two bulk growths (`vec![0u32; new_len]` and
    // `self.raster.resize(new_len, 0)`) raise `trust.vc.unbounded_allocation`
    // obligations the full verifier has no owner for — the dominating
    // `new_len == 0 || new_len > SIXEL_MAX_PIXELS { return; }` guard already
    // fail-closes the size before either alloc, so this is an inherently
    // unverifiable idiomatic-allocation body (idiom 3). Skipping also removes a
    // load-sensitive wall-clock timeout on the geometric-growth arithmetic, so
    // the gate stays deterministic. Attr is inert off-Trust.
    #[cfg_attr(trust_verify, trust::skip)]
    fn ensure_capacity(&mut self, want_x: usize, want_y: usize) {
        let need_w = want_x.saturating_add(1).min(SIXEL_MAX_DIMENSION);
        let need_h = want_y.saturating_add(1).min(SIXEL_MAX_DIMENSION);
        if need_w <= self.alloc_width && need_h <= self.alloc_height {
            return;
        }
        let new_h = need_h.max(self.alloc_height);
        // When the width grows, double the stride so column-at-a-time painting
        // amortizes to O(area) instead of reallocating and copying the whole
        // raster on every byte. When only the height grows (`need_w <=
        // alloc_width`), keep the stride unchanged so the in-place `resize`
        // fast path below fires.
        let new_w = if need_w > self.alloc_width {
            let geo = self
                .alloc_width
                .saturating_mul(2)
                .max(need_w)
                .min(SIXEL_MAX_DIMENSION);
            // If doubling the stride exceeds `SIXEL_MAX_PIXELS` at `new_h`,
            // clamp to the largest stride that fits `SIXEL_MAX_PIXELS / new_h`
            // rather than `need_w` so tall rasters still grow their stride in
            // one allocation instead of one column at a time.
            if geo
                .checked_mul(new_h)
                .is_some_and(|n| n <= SIXEL_MAX_PIXELS)
            {
                geo
            } else {
                // `new_h >= 1`, and dividing by `NonZeroUsize` discharges the
                // zero-divisor MIR assert for the modular prover.
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
        // Refuse to grow past the total-pixel cap; subsequent `emit_sixel`
        // writes clamp to the existing buffer and `unhook` drops the over-cap
        // image.
        if new_len == 0 || new_len > SIXEL_MAX_PIXELS {
            return;
        }
        if new_w == self.alloc_width {
            // Stride is unchanged (only the height grew), so the old data is
            // already a contiguous row-major prefix of the larger buffer.
            self.raster.resize(new_len, 0);
            self.alloc_height = new_h;
            return;
        }
        // True width growth: stride changes, so reallocate row-major and copy
        // each old row into the wider stride.
        let mut new_raster = vec![0u32; new_len];
        let old_stride = self.alloc_width;
        // Zip destination rows against source rows so every element write is
        // bounds-safe without index obligations; both strides must be provably
        // nonzero before `chunks`/`chunks_mut`.
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
    // `v <= 100`, so `v * 255 + 50 <= 25550` and the saturating ops can never
    // actually saturate: they are behavior-identical to plain `*`/`+` for every
    // possible input, but give the prover overflow-free arithmetic.
    v.saturating_mul(255).saturating_add(50) / 100
}

/// Pack an RGB-percent triple (`0..=100` each) into `0x00RRGGBB`.
#[inline]
#[must_use]
pub const fn rgb_percent(r: u32, g: u32, b: u32) -> u32 {
    (scale_pct(r) << 16) | (scale_pct(g) << 8) | scale_pct(b)
}

/// Returns `true` if `byte` is a sixel data character (`0x3F..=0x7E`, `?`..=`~`).
#[inline]
#[must_use]
pub const fn is_sixel_data_byte(byte: u8) -> bool {
    byte >= 0x3F && byte <= 0x7E
}

/// Decodes a sixel data byte (`0x3F..=0x7E`) into its 6-bit pixel column mask (`0..=63`).
#[inline]
#[must_use]
pub const fn sixel_data_bits(byte: u8) -> Option<u8> {
    if byte >= 0x3F && byte <= 0x7E {
        Some(byte - 0x3F)
    } else {
        None
    }
}

/// Encodes a 6-bit pixel column mask (`bits & 0x3F`) into a sixel data byte (`0x3F..=0x7E`).
#[inline]
#[must_use]
pub const fn encode_sixel_data_byte(bits: u8) -> u8 {
    0x3F + (bits & 0x3F)
}

/// Returns the bit mask (`1 << row`) for a row within a 6-pixel sixel band (`0..6`), or `0` if out of range.
#[inline]
#[must_use]
pub const fn sixel_row_mask(row: usize) -> u8 {
    if row < 6 { SIXEL_ROW_MASKS[row] } else { 0 }
}

/// Returns `true` if `row` (`0..6`) is set in the 6-bit column mask `bits`.
#[inline]
#[must_use]
pub const fn sixel_bit_is_set(bits: u8, row: usize) -> bool {
    (bits & sixel_row_mask(row)) != 0
}

/// Clamps a color register index to `0..MAX_COLOR_REGISTERS`.
#[inline]
#[must_use]
pub const fn clamp_color_register(reg: usize) -> usize {
    let max_idx = MAX_COLOR_REGISTERS.saturating_sub(1);
    if reg > max_idx { max_idx } else { reg }
}

/// Clamps a raster dimension to `0..=SIXEL_MAX_DIMENSION`.
#[inline]
#[must_use]
pub const fn clamp_sixel_dimension(dim: usize) -> usize {
    if dim > SIXEL_MAX_DIMENSION {
        SIXEL_MAX_DIMENSION
    } else {
        dim
    }
}

/// Validates and computes `width * height` for a sixel raster within [`SIXEL_MAX_DIMENSION`] and [`SIXEL_MAX_PIXELS`].
#[inline]
#[must_use]
pub const fn checked_sixel_pixel_count(width: usize, height: usize) -> Option<usize> {
    if width == 0 || height == 0 || width > SIXEL_MAX_DIMENSION || height > SIXEL_MAX_DIMENSION {
        return None;
    }
    let total = width.saturating_mul(height);
    if total > SIXEL_MAX_PIXELS {
        None
    } else {
        Some(total)
    }
}

/// Validates and computes the packed-RGBA byte size (`width * height * 4`) within [`SIXEL_MAX_IMAGE_BYTES`].
#[inline]
#[must_use]
pub const fn checked_sixel_image_bytes(width: usize, height: usize) -> Option<usize> {
    let Some(total) = checked_sixel_pixel_count(width, height) else {
        return None;
    };
    let bytes = total.saturating_mul(4);
    if bytes > SIXEL_MAX_IMAGE_BYTES {
        None
    } else {
        Some(bytes)
    }
}

/// Packs an opaque `(r, g, b)` pixel into `0xFFRRGGBB`.
#[inline]
#[must_use]
pub const fn pack_argb_pixel(r: u8, g: u8, b: u8) -> u32 {
    0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Unpacks a `0xAARRGGBB` pixel into `(a, r, g, b)`.
#[inline]
#[must_use]
pub const fn unpack_argb_channels(pixel: u32) -> (u8, u8, u8, u8) {
    (
        ((pixel >> 24) & 0xFF) as u8,
        ((pixel >> 16) & 0xFF) as u8,
        ((pixel >> 8) & 0xFF) as u8,
        (pixel & 0xFF) as u8,
    )
}

/// Encodes a `0xAARRGGBB` pixel into 4 little-endian bytes `[b, g, r, a]`.
#[inline]
#[must_use]
pub const fn encode_argb_pixel_le(pixel: u32) -> [u8; 4] {
    [
        (pixel & 0xFF) as u8,
        ((pixel >> 8) & 0xFF) as u8,
        ((pixel >> 16) & 0xFF) as u8,
        ((pixel >> 24) & 0xFF) as u8,
    ]
}

/// Decodes 4 little-endian bytes `[b, g, r, a]` into a `0xAARRGGBB` pixel.
#[inline]
#[must_use]
pub const fn decode_argb_pixel_le(bytes: [u8; 4]) -> u32 {
    (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24)
}

/// Encodes a 4-word sixel raster header `(width, height, color_reg, repeat)` into 16 little-endian bytes.
#[inline]
#[must_use]
pub const fn encode_sixel_header_le(
    width: u32,
    height: u32,
    color_reg: u32,
    repeat: u32,
) -> [u8; 16] {
    [
        (width & 0xFF) as u8,
        ((width >> 8) & 0xFF) as u8,
        ((width >> 16) & 0xFF) as u8,
        ((width >> 24) & 0xFF) as u8,
        (height & 0xFF) as u8,
        ((height >> 8) & 0xFF) as u8,
        ((height >> 16) & 0xFF) as u8,
        ((height >> 24) & 0xFF) as u8,
        (color_reg & 0xFF) as u8,
        ((color_reg >> 8) & 0xFF) as u8,
        ((color_reg >> 16) & 0xFF) as u8,
        ((color_reg >> 24) & 0xFF) as u8,
        (repeat & 0xFF) as u8,
        ((repeat >> 8) & 0xFF) as u8,
        ((repeat >> 16) & 0xFF) as u8,
        ((repeat >> 24) & 0xFF) as u8,
    ]
}

/// Decodes 16 little-endian bytes into a 4-word sixel raster header `(width, height, color_reg, repeat)`.
#[inline]
#[must_use]
pub const fn decode_sixel_header_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let width = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let height = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let color_reg = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let repeat = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (width, height, color_reg, repeat)
}

/// Convert DEC HLS (`h`=0..360, `l`=0..100, `s`=0..100) to packed `0x00RRGGBB`.
///
/// DEC's hue origin differs from the usual HSL (0° = blue, increasing
/// counter-clockwise); we use the standard HSL formula with DEC's hue offset.
#[allow(
    clippy::manual_range_contains,
    reason = "manual comparisons keep the float range-pin transparent to the Trust verifier"
)]
fn hls_to_rgb(h: u32, l: u32, s: u32) -> u32 {
    // Rebuild each operand through explicit comparison branches (inlined, since
    // the prover does not carry magnitude bounds across a call) so every float
    // operation below has bounded operands.
    let li = if l > 100 { 100 } else { l }; // 0..=100
    let si = if s > 100 { 100 } else { s }; // 0..=100
    let l = (li as f64) / 100.0; // 0.0..=1.0
    let s = (si as f64) / 100.0; // 0.0..=1.0
    let h = (h % 360) as f64; // 0.0..360.0
    // DEC hue 0 points "up" (blue); rotate so the standard formula matches.
    let hue = (h + 240.0) % 360.0; // 0.0..360.0
    // chroma = (1 - |2l-1|) * s, with both factors in [0,1] => c in [0,1].
    let chroma_scale = 1.0 - (2.0 * l - 1.0).abs(); // mathematically 0.0..=1.0
    let c = if chroma_scale > 0.0 && chroma_scale <= 1.0 && s > 0.0 && s <= 1.0 {
        // Written as `mul_add(s, 0.0)` because the verifier's MIR lowering
        // cannot discharge a float `Mul`/`Add` overflow-to-infinity check even
        // when operands are branch-pinned.
        chroma_scale.mul_add(s, 0.0)
    } else {
        0.0
    };
    let h6 = hue / 60.0; // 0.0..6.0
    let xfac = 1.0 - ((h6 % 2.0) - 1.0).abs(); // mathematically 0.0..=1.0
    let x = if c > 0.0 && c <= 1.0 && xfac > 0.0 && xfac <= 1.0 {
        c.mul_add(xfac, 0.0)
    } else {
        0.0
    };
    let m = l - c / 2.0; // l in [0,1], c/2 in [0,0.5] => m in [-0.5,1]
    let (r1, g1, b1) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_u8 = |v: f64| {
        // The closure is verified as its own function, so re-pin `v` and `m`
        // to their reachable ranges (`[0, 1]` and `[-0.5, 1]`) and use `mul_add`
        // to avoid MIR float `Add`/`Mul` overflow obligations.
        let v = if v >= 0.0 && v <= 1.0 { v } else { 0.0 };
        let mb = if m >= -0.5 && m <= 1.0 { m } else { 0.0 };
        let chan = v.mul_add(1.0, mb);
        if chan >= 1.0 {
            255
        } else if chan > 0.0 {
            chan.mul_add(255.0, 0.0).round() as u32
        } else {
            // Covers `chan <= 0.0` and NaN.
            0
        }
    };
    (to_u8(r1) << 16) | (to_u8(g1) << 8) | to_u8(b1)
}

/// The VT340 default 16-color sixel palette as packed `0x00RRGGBB`.
///
/// Values follow the canonical DEC color map (color 0 = black, 1 = blue,
/// 2 = red, …) scaled from the device RGB-percent specification. Registers
/// beyond 15 default to black until defined.
fn default_palette() -> Vec<u32> {
    // (r%, g%, b%) per DEC VT340 color map.
    const DEC: [(u32, u32, u32); 16] = [
        (0, 0, 0),    // 0  black
        (20, 20, 80), // 1  blue
        (80, 13, 13), // 2  red
        (20, 80, 20), // 3  green
        (80, 20, 80), // 4  magenta
        (20, 80, 80), // 5  cyan
        (80, 80, 20), // 6  yellow
        (53, 53, 53), // 7  gray 50%
        (26, 26, 26), // 8  gray 25%
        (33, 33, 60), // 9  blue*
        (60, 26, 26), // 10 red*
        (33, 60, 33), // 11 green*
        (60, 33, 60), // 12 magenta*
        (33, 60, 60), // 13 cyan*
        (60, 60, 33), // 14 yellow*
        (80, 80, 80), // 15 gray 75%
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
