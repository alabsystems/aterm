// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The raw payload of an inline image placement (iTerm2 OSC 1337 `File=`, the
//! sixel path that reuses the same placement, and Kitty graphics).
//!
//! ## Why this lives in the vocabulary crate
//!
//! One placement is referenced from TWO storage models that cannot see each
//! other: the live grid's `CellExtras` side table (`aterm-grid`) and the history
//! line model (`aterm-scrollback`, which `aterm-grid` is built on top of). The
//! payload has to be the SAME allocation on both sides — a row that scrolls off
//! the top hands its `Arc` to history, and a scrolled-back row hands the very
//! same `Arc` back to the renderer, whose decode cache is keyed by pointer
//! identity. A type defined in either of those crates could only be shared with
//! the other by copying it, which would re-decode (and re-charge memory for) one
//! picture once per row of history it covers.
//!
//! The engine does NOT decode pixels — it carries no image codec. It stores the
//! bytes as delivered plus the [`ImageFormat`] hint, and the renderer decodes
//! once per distinct payload.

/// Source encoding of an inline image's raw payload.
///
/// The engine does NOT decode pixels (it carries no image-codec dependency);
/// it stores the raw bytes plus this hint, and the renderer decodes once. Only
/// PNG is decoded today; an unknown format degrades to drawing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (`\x89PNG\r\n…`). The one container format the renderer decodes.
    Png,
    /// Already-decoded, packed RGBA8 pixels (`[r, g, b, a]` per pixel,
    /// row-major over `width`). Used by the sixel path, which decodes the
    /// raster in the engine (the `aterm-sixel` crate) since sixel has no
    /// container the renderer's PNG decoder could read. `ImageData.bytes` then
    /// holds exactly `4 * width * height` bytes; the renderer resamples them to
    /// the footprint directly (no codec). This keeps the engine codec-free.
    RawRgba8 {
        /// Source raster width in pixels.
        width: u16,
        /// Source raster height in pixels.
        height: u16,
    },
    /// Anything else (JPEG, GIF, …) — kept verbatim but not drawn yet.
    Unknown,
}

/// An inline image placed on the grid (iTerm2 OSC 1337 `File=`).
///
/// Decoupled from the cells: the (possibly large) payload is stored ONCE behind
/// an `Arc` and every covered cell holds a cheap `ImageRef` into it with its own
/// sub-cell coordinates. The engine keeps the RAW (undecoded) bytes — it has no
/// image codec — and the renderer decodes them to RGBA a single time, keyed by
/// the `Arc`'s pointer identity, then blits the cell's slice of the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Raw, undecoded payload (e.g. the PNG file bytes) as delivered.
    pub bytes: Vec<u8>,
    /// Encoding hint for the renderer's decoder.
    pub format: ImageFormat,
    /// Footprint width in CELLS (how many columns the image spans).
    pub cols: u16,
    /// Footprint height in CELLS (how many rows the image spans).
    pub rows: u16,
    /// Kitty `z=` stacking order. `< 0` draws BEHIND the cell's text (the glyph
    /// paints on top); `>= 0` (the default for iTerm2/Sixel and `z=0` Kitty) draws
    /// OVER the cell, which keeps the historical "image owns the cell" behavior.
    pub z_index: i32,
    /// CHROME-BAND LIFT, in device px: how far this image's raster extends ABOVE
    /// its first covered cell row, into the window's chrome band (`pad_top +
    /// head`). `0` for every terminal-content image — the engine's OSC 1337 /
    /// sixel / Kitty constructors never set it, so nothing an application prints
    /// can draw outside its own cells. Non-zero ONLY for the host's tab-strip
    /// band raster (`aterm-gui`'s pixel band), whose design needs the one canvas
    /// a cell-quantised footprint cannot give it: the full optical band from the
    /// window's top edge down. Renderers honour it by (a) decoding the footprint
    /// `lift` px taller than `rows·cell_h` and (b) letting the FIRST footprint
    /// row's tile paint `[y0 − lift, y0)` as well as its own cell band; rows past
    /// the first read their source `lift` px lower. With `0` both clauses are
    /// arithmetic no-ops, byte-identical to the pre-lift renderers.
    pub band_lift_px: u16,
    /// How the source raster maps onto the footprint: [`ImageScaling::Fit`]
    /// (the default), [`ImageScaling::Stretch`] or [`ImageScaling::PixelExact`].
    /// Which one is right depends on what the PROGRAM named — see the variants.
    pub scaling: ImageScaling,
    /// The sub-rectangle of the source raster this placement shows (Kitty
    /// `x=`/`y=`/`w=`/`h=`), or `None` for the whole raster — which every
    /// protocol but Kitty always passes. The renderer crops FIRST, then applies
    /// [`scaling`](Self::scaling) to what is left.
    pub source_rect: Option<SourceRect>,
}

/// How a placement's source raster maps onto its cell footprint.
///
/// The footprint is always a whole number of cells; what differs is what the
/// program asked the pixels to do inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageScaling {
    /// FIT: scale the raster, aspect PRESERVED, into the largest box that fits
    /// the footprint and centre it there, the rest fully transparent. Right when
    /// the program named the target in CELLS (`File=width=40;height=8`, Kitty
    /// `c=`/`r=`, the host's own chrome rasters): it asked for a cell-sized
    /// picture, so filling the cells it asked for is the spec — without
    /// distorting it, since the cell box is only a whole-cell approximation of
    /// the picture's shape.
    #[default]
    Fit,
    /// STRETCH: scale the raster to the WHOLE footprint, aspect ignored. Only an
    /// explicit request earns it — iTerm2's `preserveAspectRatio=0` ("stretch
    /// to fill, ignore the inherent ratio"), where the program sized both axes
    /// itself and wants exactly that box filled.
    Stretch,
    /// PIXEL-EXACT: draw the source ONE SOURCE PIXEL TO ONE DEVICE PIXEL,
    /// anchored at the footprint's TOP-LEFT, the remainder fully transparent (a
    /// partial cell at the right/bottom edge simply goes unpainted).
    ///
    /// This is for the protocols that name PIXELS. A sixel raster's footprint is
    /// DERIVED from its pixel size by rounding UP to whole cells, so scaling the
    /// raster back out to that rounded box is pure rounding noise: a 40x12 sixel
    /// in a 9x17 cell box would be magnified to 45x14 and a 4x6 sprite to 9x14
    /// (2.25x), with an interpolating resample that destroys exactly the 1-px
    /// features such an image is drawn out of. xterm, foot, mlterm, wezterm,
    /// contour and mintty all draw sixel 1:1, and every sixel-emitting tool
    /// sizes its output to the reported cell geometry on that assumption. Kitty
    /// graphics transmitted WITHOUT `c=`/`r=` are the same case.
    PixelExact,
}

/// A sub-rectangle of a source raster, in raster pixels (Kitty `x=`, `y=`,
/// `w=`, `h=`). A `width`/`height` of `0` means "to the raster's right/bottom
/// edge", as in the Kitty protocol; the renderer clamps the whole rectangle to
/// the raster, and an empty result draws nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRect {
    /// Left edge, in source pixels.
    pub x: u32,
    /// Top edge, in source pixels.
    pub y: u32,
    /// Width in source pixels; `0` = to the right edge.
    pub width: u32,
    /// Height in source pixels; `0` = to the bottom edge.
    pub height: u32,
}

impl SourceRect {
    /// This rectangle clamped to a `src_w × src_h` raster, as
    /// `(x, y, width, height)`, or `None` when nothing of it lies inside.
    #[must_use]
    pub fn clamp_to(self, src_w: u32, src_h: u32) -> Option<(u32, u32, u32, u32)> {
        if self.x >= src_w || self.y >= src_h {
            return None;
        }
        let max_w = src_w - self.x;
        let max_h = src_h - self.y;
        let w = if self.width == 0 {
            max_w
        } else {
            self.width.min(max_w)
        };
        let h = if self.height == 0 {
            max_h
        } else {
            self.height.min(max_h)
        };
        (w > 0 && h > 0).then_some((self.x, self.y, w, h))
    }
}

impl ImageData {
    /// Heap bytes this payload owns — the raster, not the struct.
    ///
    /// Used by the history line model to charge each covered row its SHARE of
    /// one shared placement (`payload_bytes() / rows`), so a footprint that is
    /// retained whole is charged exactly once no matter how many lines carry it.
    #[must_use]
    #[inline]
    pub fn payload_bytes(&self) -> usize {
        self.bytes.capacity()
    }

    /// This payload's per-ROW share of [`payload_bytes`](Self::payload_bytes).
    ///
    /// `rows` is the footprint height, so summing the share over every covered
    /// row recovers the payload exactly once (up to integer truncation, which is
    /// bounded by `rows` bytes in total). Dropping one line therefore drops
    /// exactly that line's share of the picture from the memory budget, which is
    /// the accounting a per-row FULL copy would get wrong by a factor of `rows`.
    #[must_use]
    #[inline]
    pub fn per_row_bytes(&self) -> usize {
        self.payload_bytes() / usize::from(self.rows.max(1))
    }
}
