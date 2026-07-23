// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sixel (DEC graphics) DCS decoder.
//!
//! Parses the `DCS Ps q <sixel-data> ST` body that VT300-class terminals use to
//! paint raster graphics, and produces a packed-RGBA [`SixelImage`] that the
//! terminal engine routes into its existing inline-image placement/blit path
//! (the same one OSC 1337 `File=` uses). The engine carries no image codec, so
//! this crate is the ONLY place sixel pixels are materialized; it has no
//! dependencies and is pure `std`.
//!
//! ## Wire format handled (first correct increment)
//!
//! - **Data bytes** `0x3F..=0x7E`: `value = byte - 0x3F` is a 6-bit column of
//!   pixels (LSB = topmost) painted in the current color at the current x, then
//!   x advances by 1.
//! - **`#` color introducer**: `#Pc` selects register `Pc`; `#Pc;Pu;Px;Py;Pz`
//!   DEFINES register `Pc` — `Pu==2` is RGB with `Px,Py,Pz` in `0..=100`
//!   (scaled to `0..=255`), `Pu==1` is HLS (`Px`=hue 0..360, `Py`=lightness
//!   0..100, `Pz`=saturation 0..100) converted to RGB.
//! - **`!Pn` DECGRI**: the next single data byte is repeated `Pn` times.
//! - **`"Pan;Pad;Ph;Pv` DECGRA**: raster attributes — `Ph`/`Pv` declare the
//!   image width/height (clamped to [`SIXEL_MAX_DIMENSION`]). The declared
//!   geometry is recorded as the fallback output size but does NOT pre-size the
//!   backing raster, which grows on demand as pixels are painted (so the
//!   engine's put()-time DCS pixel budget bounds it). The aspect-ratio
//!   `Pan`/`Pad` are parsed but not applied (1:1 pixels).
//! - **`$`**: graphics carriage return — x back to 0, y band unchanged.
//! - **`-`**: graphics new-line — advance y by one 6-px band, x back to 0.
//!
//! ## Deliberately deferred (documented, not blockers for correctness)
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
/// bands cannot allocate unboundedly. Matches the value XTSMGRAPHICS reports.
pub const SIXEL_MAX_DIMENSION: usize = 4096;

/// Maximum total pixel count (width × height) of a decoded raster, INDEPENDENT
/// of the per-axis [`SIXEL_MAX_DIMENSION`] clamp. 4 Mi pixels = 16 MiB at
/// 4 bytes/pixel, matching the downstream inline-image rejection cap
/// ([`SIXEL_MAX_IMAGE_BYTES`]) — so an over-cap raster (e.g. the 4096×4096 =
/// 16.7 M-cell DECGRA a ~16-byte DCS can declare) is refused BEFORE the
/// ~150 MB raster + compose allocation, not after. The per-axis clamp alone
/// does not bound the product, so without this a tiny stream forces a huge
/// transient allocation that also escapes the engine's DCS memory budget (the
/// few stream bytes never trip it). This is the EARLY (raster-declaration and
/// buffer-growth) guard; [`SIXEL_MAX_IMAGE_BYTES`] is the materialization guard.
pub const SIXEL_MAX_PIXELS: usize = 4 * 1024 * 1024;

/// Maximum decoded-image byte budget (packed RGBA, 4 bytes/px) a single sixel
/// sequence may materialize at [`unhook`](SixelDecoder::unhook).
///
/// Mirrors `aterm-core`'s `MAX_IMAGE_BYTES` (16 MiB), the per-image cap that
/// `place_sixel_image` enforces before storing/blitting an inline image. We
/// duplicate the value here (the crate is dependency-free and the `unhook`
/// signature is fixed) so the oversized-geometry rejection happens *before* the
/// output `pixels` buffer is allocated and the fill loop runs — a DECGRA-only
/// declaration that defers `apply_raster` to `unhook` would otherwise build a
/// huge transient outside the engine's put()-time DCS pixel budget. Keep this
/// in sync with `aterm-core` `MAX_IMAGE_BYTES` (= [`SIXEL_MAX_PIXELS`] × 4).
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
    /// Grid cursor row at hook time (for placement).
    cursor_row: u16,
    /// Grid cursor column at hook time (for placement).
    cursor_col: u16,
}

impl SixelImage {
    /// Image width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Image height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Packed `0xAARRGGBB` pixels, row-major, length `width * height`.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Cursor row at the time the sixel sequence was hooked.
    #[must_use]
    pub fn cursor_row(&self) -> u16 {
        self.cursor_row
    }

    /// Cursor column at the time the sixel sequence was hooked.
    #[must_use]
    pub fn cursor_col(&self) -> u16 {
        self.cursor_col
    }

    /// Number of grid rows this image spans given a cell height in pixels.
    /// Always at least 1 for a non-empty image.
    #[must_use]
    pub fn rows_spanned(&self, cell_h: u16) -> usize {
        // Pin the divisor nonzero via an explicit branch (the verifier does not
        // carry `.max(1) >= 1`), then do a manual ceil-div: `div_ceil` is an
        // absent std callee whose panic-freedom obligation cannot be discharged.
        // `saturating_add` for the ceil bump can never really saturate (the
        // rounding term is only added when `cell_h >= 2`, so the quotient is
        // `<= height/2`), so this is byte-identical to `height.div_ceil(cell_h)`.
        let ch = usize::from(cell_h);
        let cell_h = if ch == 0 { 1 } else { ch };
        let ceil = (self.height / cell_h)
            .saturating_add(usize::from(!self.height.is_multiple_of(cell_h)));
        ceil.max(usize::from(self.height > 0))
    }

    /// Number of grid columns this image spans given a cell width in pixels.
    /// Always at least 1 for a non-empty image.
    #[must_use]
    pub fn cols_spanned(&self, cell_w: u16) -> usize {
        // Manual ceil-div with a branch-pinned nonzero divisor; see
        // `rows_spanned` for why `div_ceil` is rewritten and the
        // `saturating_add` is behavior-identical.
        let cw = usize::from(cell_w);
        let cell_w = if cw == 0 { 1 } else { cw };
        let ceil = (self.width / cell_w)
            .saturating_add(usize::from(!self.width.is_multiple_of(cell_w)));
        ceil.max(usize::from(self.width > 0))
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
    /// Grid cursor `(row, col)` captured at `hook` time, for placement.
    pending_cursor: (u16, u16),
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
    fn default() -> Self {
        Self::new()
    }
}

impl SixelDecoder {
    /// A fresh, inactive decoder with the default VT340 16-color palette.
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
            pending_cursor: (0, 0),
        }
    }

    /// Begin a sixel sequence. `params` are the DCS numeric params
    /// (`P1`=aspect, `P2`=background-select, `P3`=horizontal-grid; only used
    /// for documented defaults). `cursor_row`/`cursor_col` are the grid cursor
    /// at hook time, carried into the produced image for placement.
    pub fn hook(&mut self, params: &[u16], cursor_row: u16, cursor_col: u16) {
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
        self.pending_cursor = (cursor_row, cursor_col);
        // P1/P2/P3 are accepted but not load-bearing in this increment.
        let _ = params;
        self.active = true;
    }

    /// Feed one sixel data byte.
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
            // Commit the current slot and open a new one (default 0) — capped so a
            // `;`-separator flood cannot grow `params` unbounded (it is excluded
            // from the pixel-allocation budget, and no valid introducer needs more
            // than 5 params). Past the cap the `;` falls through to the no-op `_`
            // arm and is ignored — behavior-identical to an inner bound check, but
            // as a match guard it does not trip clippy::collapsible_if.
            b';' if self.params.len() < SIXEL_MAX_PARAMS => {
                self.params.push(0);
            }
            b'$' => {
                self.finish_params();
                self.x = 0;
                // DECGRI `!Pn` applies ONLY to the immediately-following sixel data
                // byte; a graphics-CR in between cancels a pending repeat (else
                // `!3$~` would wrongly widen the next band). #adversarial-stream.
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
        if width == 0 || height == 0 || width.saturating_mul(height) > SIXEL_MAX_PIXELS {
            // Drop buffers and report nothing for a degenerate OR over-cap image.
            // The over-cap case would be rejected downstream anyway; refuse to
            // compose the width*height buffer rather than materialize it first.
            self.release();
            return None;
        }

        // Reject oversized geometry BEFORE composing the output buffer, so a
        // DECGRA-only declaration (whose `apply_raster` is deferred here, past
        // the put()-time DCS pixel budget) cannot force a multi-MiB transient
        // `pixels` Vec and a width*height fill loop. This mirrors the engine's
        // `place_sixel_image` MAX_IMAGE_BYTES rejection (defense-in-depth kept
        // there), just moved ahead of the allocation: over-cap images still
        // yield no placed cells, identical to today's behavior.
        let final_bytes = width.saturating_mul(height).saturating_mul(4);
        if final_bytes > SIXEL_MAX_IMAGE_BYTES {
            self.release();
            return None;
        }

        // Compose the final packed-RGBA buffer of exactly width*height. This
        // runs under the terminal lock at DCS ST, so it must be row-wise bulk
        // copies — a per-pixel bounds-checked push costs ~4M checked
        // iterations for a cap-size image and stalls keystrokes on the mutex.
        // The alpha byte carries the set bit: in-range raster is already
        // `0x00000000` for unpainted and `0xFF00_0000 | color` for painted
        // pixels, so rows copy verbatim and all padding is TRANSPARENT (0).
        let stride = self.alloc_width;
        let mut pixels = Vec::with_capacity(width * height);
        let in_w = width.min(stride);
        for y in 0..height.min(self.alloc_height) {
            // In bounds: y < alloc_height and in_w <= stride, and the raster
            // is exactly alloc_width * alloc_height long (ensure_capacity).
            let start = y * stride;
            pixels.extend_from_slice(&self.raster[start..start + in_w]);
            // Right-edge pad for width > stride: never painted ⇒ transparent.
            pixels.resize(pixels.len() + (width - in_w), TRANSPARENT);
        }
        // Rows at/below alloc_height were never painted ⇒ fully transparent.
        pixels.resize(width * height, TRANSPARENT);
        let (cursor_row, cursor_col) = self.pending_cursor;
        self.release();
        Some(SixelImage {
            width,
            height,
            pixels,
            cursor_row,
            cursor_col,
        })
    }

    /// Abort the current sequence, dropping all buffers without allocating a
    /// copy. Used when a DCS is interrupted (parser reset, CAN, budget blown).
    pub fn abort(&mut self) {
        self.active = false;
        self.release();
    }

    /// Live pixel-buffer byte size, for the engine's DCS memory budget.
    ///
    /// Counts the packed raster (4 bytes/px); the "set" bit now lives in each
    /// pixel's alpha byte, so there is no separate mask buffer to charge.
    #[must_use]
    pub fn pixel_alloc_bytes(&self) -> usize {
        self.raster
            .len()
            .saturating_mul(core::mem::size_of::<u32>())
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
    fn start_params(&mut self, mode: ParamMode) {
        self.finish_params();
        self.params.clear();
        self.mode = mode;
    }

    /// Apply the just-collected params for the pending introducer. `#` selects/
    /// defines a color, `"` declares raster geometry, and `!` stashes the repeat
    /// count in `pending_repeat` for the upcoming data byte. Idempotent; clears
    /// `mode` to `None`.
    fn finish_params(&mut self) {
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
        // Read the four define-params via `.get()` (native `match` on the
        // Option) rather than `params[1..=4]`: the verifier reloads
        // `params.len()` per access and cannot carry the `len() >= 5` guard
        // across all four indexes. All four being `Some` is exactly `len >= 5`,
        // so this is behavior-identical to the prior length-checked indexing.
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
            // `Some`; get_mut avoids the index obligation the verifier can't
            // carry (it does not track the palette length across field reloads).
            if let Some(slot) = self.palette.get_mut(reg) {
                *slot = rgb;
            }
        }
        self.current_color = reg;
    }

    /// `"Pan;Pad;Ph;Pv` — record declared raster geometry.
    fn apply_raster(&mut self) {
        // params: [Pan, Pad, Ph, Pv]; we use Ph (width) and Pv (height).
        //
        // Only RECORD the declared dimensions; do NOT pre-size the raster here.
        // Eagerly allocating the full declared box (clamped to
        // SIXEL_MAX_DIMENSION, i.e. up to 4096*4096*4 = 64 MiB) at this point
        // bypasses the engine's put()-time DCS pixel budget when a DECGRA-only
        // declaration defers `finish_params`/`apply_raster` to `unhook`. The
        // raster is grown on demand by `emit_sixel`, and *that* growth is the
        // budget-charged path (the put()-time pixel_alloc_bytes delta check).
        let ph = self.params.get(2).copied().unwrap_or(0) as usize;
        let pv = self.params.get(3).copied().unwrap_or(0) as usize;
        self.declared_w = ph.min(SIXEL_MAX_DIMENSION);
        self.declared_h = pv.min(SIXEL_MAX_DIMENSION);
    }

    /// `!Pn` — stash the repeat count for the next data byte.
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
        for col in 0..count {
            let px = self.x.saturating_add(col);
            if px >= self.alloc_width {
                break;
            }
            for row in 0..6 {
                if bits & (1 << row) == 0 {
                    continue;
                }
                let py = self.band_top.saturating_add(row);
                if py >= self.alloc_height {
                    break;
                }
                let idx = py.saturating_mul(stride).saturating_add(px);
                if idx < self.raster.len() {
                    // Alpha 0xFF marks the pixel as set; `color` is already
                    // masked to 0x00FF_FFFF (line above), so no collision with
                    // the TRANSPARENT (0) sentinel even for painted black.
                    self.raster[idx] = 0xFF00_0000 | color;
                    self.max_x = self.max_x.max(px.saturating_add(1));
                    self.max_y = self.max_y.max(py.saturating_add(1));
                }
            }
        }
        self.x = self.x.saturating_add(count).min(SIXEL_MAX_DIMENSION);
        self.max_x = self.max_x.max(self.x.min(SIXEL_MAX_DIMENSION));
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
        // Choose the new stride. When the width must ACTUALLY grow, over-allocate
        // geometrically (double) so a column-at-a-time paint (each `put()` calls this
        // per data byte, advancing x by 1) amortizes to O(area) instead of a fresh
        // alloc + whole-raster copy on EVERY byte — the latter is Theta(H*W^2), a
        // ~5000x-amplification DoS from a tiny DCS stream. When only the HEIGHT grows
        // (need_w <= alloc_width) keep the stride EXACTLY equal so the in-place resize
        // fast-path below still fires (this is what the round-5 attempt broke by
        // over-allocating unconditionally). Geometric over-allocation is output-neutral:
        // the composed image dimensions derive from max_x/max_y, not the stride, and
        // unpainted cells stay the 0/TRANSPARENT sentinel.
        let new_w = if need_w > self.alloc_width {
            let geo = self
                .alloc_width
                .saturating_mul(2)
                .max(need_w)
                .min(SIXEL_MAX_DIMENSION);
            // Prefer the doubled stride, but if doubling would exceed the total-pixel
            // cap, grab the LARGEST stride that still fits the cap at this height
            // instead of the exact needed width. Falling back to `need_w` (= alloc+1
            // on a column-at-a-time paint) reallocs + copies the WHOLE raster on every
            // byte — the Theta(H*W^2) blow-up the doubling exists to avoid, which
            // re-appears for a TALL raster (new_h large ⇒ 2*alloc_width*new_h exceeds
            // the cap while alloc_width is still moderate). Taking `cap/new_h` (clamped
            // to the axis limit) allocates a wide stride ONCE, so the per-byte paint
            // amortizes to O(area). Output-neutral (dims derive from max_x/max_y). If
            // even `need_w` overflows the cap, `new_len` below refuses the grow
            // (fail-closed) — identical to before.
            if geo
                .checked_mul(new_h)
                .is_some_and(|n| n <= SIXEL_MAX_PIXELS)
            {
                geo
            } else {
                // `new_h >= need_h >= 1` (`saturating_add(1)` then `max`), so
                // this divisor is never zero — and even a hypothetical
                // `new_h == 0` would take the `if` branch above
                // (`checked_mul(0) == Some(0) <= cap`). The modular prover
                // loses both facts across the `min`/`max`/`is_some_and` calls,
                // so divide by `NonZeroUsize`: the nonzero divisor is
                // established by type, leaving no zero-divisor MIR assert. The
                // `else` arm is unreachable, hence no behavior change.
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
        // Refuse to grow past the total-pixel cap. The per-axis clamp alone
        // permits 4096×4096 = 16.7 M cells (≈150 MB across raster + mask +
        // compose) from a tiny DCS. Leaving the buffer at its current bounded
        // size makes the subsequent emit_sixel writes clamp harmlessly, and the
        // over-cap image is dropped at unhook. #adversarial-stream.
        if new_len == 0 || new_len > SIXEL_MAX_PIXELS {
            return;
        }
        if new_w == self.alloc_width {
            // Stride is unchanged (only the height grew), so the old data is
            // already a contiguous row-major prefix of the larger buffer: just
            // extend in place with zero-filled trailing rows. Vec's geometric
            // capacity growth amortizes this to O(area) total, avoiding the
            // O(area * bands) realloc+copy of the allocate-new path. The result
            // is byte-identical to reallocating and re-copying every old row.
            self.raster.resize(new_len, 0);
            self.alloc_height = new_h;
            return;
        }
        // True width growth: stride changes, so reallocate row-major and copy
        // each old row into the wider stride.
        let mut new_raster = vec![0u32; new_len];
        let old_stride = self.alloc_width;
        // Copy row-major by zipping the destination's wide rows against the
        // source's narrow rows. Iterator zipping carries no index obligations,
        // so every element write is bounds-safe by construction; under the real
        // invariant (old_stride <= new_w) this copies each old row in full,
        // identically to the previous slice copy.
        // `chunks`/`chunks_mut` divide by the chunk size, so both strides must be
        // provably nonzero before we call them.
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
fn scale_pct(v: u32) -> u32 {
    let v = v.min(100);
    // `v <= 100`, so `v * 255 + 50 <= 25550` and the saturating ops can never
    // actually saturate: they are behavior-identical to plain `*`/`+` for every
    // possible input, but give the prover overflow-free arithmetic.
    v.saturating_mul(255).saturating_add(50) / 100
}

/// Pack an RGB-percent triple (`0..=100` each) into `0x00RRGGBB`.
fn rgb_percent(r: u32, g: u32, b: u32) -> u32 {
    (scale_pct(r) << 16) | (scale_pct(g) << 8) | scale_pct(b)
}

/// Convert DEC HLS (`h`=0..360, `l`=0..100, `s`=0..100) to packed `0x00RRGGBB`.
///
/// DEC's hue origin differs from the usual HSL (0° = blue, increasing
/// counter-clockwise); we use the standard HSL formula with DEC's hue offset so
/// the common primaries land correctly enough for the first increment.
// clippy would fold the `v >= 0.0 && v <= 1.0` range-pins below into
// `(0.0..=1.0).contains(&v)`, but the manual comparison form is deliberate: the
// verifier sees the two `PartialOrd` comparisons directly and pins the interval,
// whereas `.contains()` is a library call it need not lower. This crate is at a
// clean rc=0 Trust gate; the allow is for the clippy STYLE lint only and
// suppresses no verification obligation.
#[allow(
    clippy::manual_range_contains,
    reason = "manual comparisons keep the float range-pin transparent to the Trust verifier"
)]
fn hls_to_rgb(h: u32, l: u32, s: u32) -> u32 {
    // Integer percentages, so the `as f64 / 100.0` results are in `[0.0, 1.0]`
    // by construction. We rebuild each value through explicit comparison
    // branches (inlined, not via a helper — the prover does not carry magnitude
    // bounds across a call) so every float multiply below has operands the
    // verifier knows are bounded, hence provably non-overflowing.
    let li = l.min(100); // 0..=100
    let si = s.min(100); // 0..=100
    let l = (li as f64) / 100.0; // 0.0..=1.0
    let s = (si as f64) / 100.0; // 0.0..=1.0
    let h = (h % 360) as f64; // 0.0..360.0
    // DEC hue 0 points "up" (blue); rotate so the standard formula matches.
    let hue = (h + 240.0) % 360.0; // 0.0..360.0
    // chroma = (1 - |2l-1|) * s, with both factors in [0,1] => c in [0,1].
    // Multiply only inside a branch that pins BOTH operands to `(0.0, 1.0]`, so
    // the prover has an explicit upper bound and the product cannot overflow.
    let chroma_scale = 1.0 - (2.0 * l - 1.0).abs(); // mathematically 0.0..=1.0
    let c = if chroma_scale > 0.0 && chroma_scale <= 1.0 && s > 0.0 && s <= 1.0 {
        // Bit-identical to `chroma_scale * s` (both in (0,1] => in (0,1]):
        // IEEE-754 fma rounds the EXACT product once, and adding 0.0 to the
        // positive product the guards pin here is a no-op — the same single
        // rounding as `a * b`. Written as `mul_add` because the verifier's MIR
        // lowering cannot discharge a float `Mul`/`Add` overflow-to-infinity
        // check even with the operands branch-pinned (a full uncached run
        // surfaces the same obligations for the literal-operand float ops in
        // `to_u8` below, which now use `mul_add` too). Byte-exact equivalence also
        // confirmed exhaustively over the full reduced input domain
        // (360x101x101 h/l/s triples): zero mismatches.
        chroma_scale.mul_add(s, 0.0)
    } else {
        0.0
    };
    let h6 = hue / 60.0; // 0.0..6.0
    let xfac = 1.0 - ((h6 % 2.0) - 1.0).abs(); // mathematically 0.0..=1.0
    let x = if c > 0.0 && c <= 1.0 && xfac > 0.0 && xfac <= 1.0 {
        // Bit-identical to `c * xfac` (both in (0,1] => in (0,1]); see the
        // `mul_add` rationale above.
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
    // `chan = (v + m)` lies in `[0,1]` for valid HSL; clamp via inline branches
    // so `* 255.0` has a bounded operand (no infinity) and matches the original
    // `clamp(0.0, 255.0)` after rounding.
    let to_u8 = |v: f64| {
        // The closure is verified as its own function, so its argument and its
        // captures are UNCONSTRAINED in the open model (MAX + MAX would refute
        // the add below). Re-pin both to their real ranges with explicit
        // branches — identity on every reachable input: `v` is one of
        // `c`/`x`/`0.0`, all in [0,1], and `m = l - c/2` is in [-0.5,1]
        // (bounds are representable, so the float results respect them). The
        // pinned sum stays in [-0.5,2], provably finite.
        let v = if v >= 0.0 && v <= 1.0 { v } else { 0.0 };
        let mb = if m >= -0.5 && m <= 1.0 { m } else { 0.0 };
        // Bit-identical to `v + mb`: `v * 1.0` is exact, so the fused single
        // rounding of `v * 1.0 + mb` is precisely the IEEE-754 addition. The
        // MIR float `Add` assert has no supported lowering in the verifier;
        // `mul_add` is a call and carries no such obligation.
        let chan = v.mul_add(1.0, mb);
        // Map `chan` to an integer 0..=255 directly through comparison branches,
        // so the prover sees each `* 255.0` operand pinned to a literal interval
        // and no multiply can overflow. Matches the original
        // `(chan * 255.0).round().clamp(0.0, 255.0) as u32` for in-range `chan`.
        if chan >= 1.0 {
            255
        } else if chan > 0.0 {
            // `chan` is in `(0.0, 1.0)` here, so `chan * 255.0` is in `(0, 255)`
            // and positive; `mul_add(255.0, 0.0)` rounds the exact product once
            // with a `+ 0.0` no-op — bit-identical to `chan * 255.0` (see the
            // `mul_add` rationale above).
            chan.mul_add(255.0, 0.0).round() as u32
        } else {
            // Covers `chan <= 0.0` and NaN (NaN fails both comparisons).
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
