// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The raw payload of an inline image placement (iTerm2 OSC 1337 `File=`, sixel, Kitty
//! graphics).
//!
//! It lives in the vocabulary crate because one placement is referenced from two storage
//! models that cannot see each other, the live grid's `CellExtras` (`aterm-grid`) and the
//! history line model (`aterm-scrollback`), and must be the same `Arc` on both sides: the
//! renderer's decode cache is keyed by pointer identity, and a copy would re-decode and
//! re-charge memory once per history row it covers. The engine keeps the bytes as delivered
//! (it decodes PNG only to compose Kitty animation frames).

/// Source encoding of an inline image's raw payload. Only PNG is decoded; an unknown format
/// draws nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG (`\x89PNG\r\n…`). The one container format the renderer decodes.
    Png,
    /// Already-decoded packed RGBA8, row-major over `width`: sixel rasters (decoded in the
    /// engine by `aterm-sixel`) and composed Kitty animation frames. `ImageData.bytes` holds
    /// exactly `4 * width * height` bytes, resampled to the footprint with no codec.
    RawRgba8 {
        /// Source raster width in pixels.
        width: u16,
        /// Source raster height in pixels.
        height: u16,
    },
    /// Anything else (JPEG, GIF, …): kept verbatim, not drawn.
    Unknown,
}

/// An inline image placed on the grid. The payload is stored once behind an `Arc`; each
/// covered cell holds an `ImageRef` with its own sub-cell coordinates, and the renderer decodes
/// once per `Arc` (keyed by pointer identity) and blits each cell's slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Raw, undecoded payload (e.g. the PNG file bytes) as delivered.
    pub bytes: Vec<u8>,
    /// Encoding hint for the renderer's decoder.
    pub format: ImageFormat,
    /// Footprint width in cells.
    pub cols: u16,
    /// Footprint height in cells.
    pub rows: u16,
    /// Kitty `z=` stacking order: `< 0` draws behind the cell's text, `>= 0` (iTerm2, sixel,
    /// Kitty's default) over it.
    pub z_index: i32,
    /// Chrome-band lift, in device px: how far the raster extends above its first row into the
    /// window's chrome band. Only the host's tab-strip raster (`aterm-gui`) sets it; the engine's
    /// constructors leave `0`, so no application output draws outside its cells. Renderers
    /// decode the footprint `lift` px taller and let the first row's tile also paint
    /// `[y0 − lift, y0)`; later rows read their source `lift` px lower.
    pub band_lift_px: u16,
    /// Where the raster starts inside its first cell, in device px `(x, y)` (Kitty `X=`/`Y=`);
    /// the strip before it stays transparent. `(0, 0)` for every other image.
    pub cell_offset_px: (u16, u16),
    /// How the source raster maps onto the footprint.
    pub scaling: ImageScaling,
    /// The sub-rectangle of the source raster shown (Kitty `x=`/`y=`/`w=`/`h=`), or `None` for
    /// all of it. The renderer crops first, then applies [`scaling`](Self::scaling).
    pub source_rect: Option<SourceRect>,
}

/// The identity of one Kitty graphics placement, stamped into every cell it
/// covers and carried into history with the row, where it keeps ordering the
/// cell's image layers. Kitty deletion addresses the screen, so a placement
/// whose rows are in history is no delete target until they return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyPlacementTag {
    /// The image id (`i=`, or the id the terminal assigned an `I=` number).
    pub image_id: u32,
    /// The client's placement id (`p=`); `0` when the client named none.
    pub placement_id: u32,
    /// When the image was first transmitted, as a per-terminal serial: kitty
    /// draws images of one z-index in that order (its internal image id),
    /// which a later transmit under the same id keeps.
    pub image_serial: core::num::NonZeroU32,
    /// A per-terminal serial, unique to this one placement act, so two
    /// unnamed placements of the same image stay distinct targets.
    pub serial: core::num::NonZeroU32,
}

/// Where an image layer stacks among the images sharing a cell, lowest
/// first, as kitty draws them: by z-index, then by when the image was first
/// transmitted, then by placement order. An image that is not a Kitty
/// placement (`kitty` is `None`) sorts first among its z-index.
#[inline]
#[must_use]
pub const fn layer_order(z_index: i32, kitty: Option<KittyPlacementTag>) -> (i32, u32, u32) {
    let (image, serial) = match kitty {
        Some(tag) => (tag.image_serial.get(), tag.serial.get()),
        None => (0, 0),
    };
    (z_index, image, serial)
}

/// How a placement's source raster maps onto its whole-cell footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageScaling {
    /// Scale, aspect preserved, into the largest box that fits the footprint, centred, the
    /// rest transparent. For a target named in cells (`File=width=40;height=8`, Kitty `c=`
    /// or `r=` alone, the host's chrome rasters).
    #[default]
    Fit,
    /// Scale to the whole footprint, aspect ignored: only on explicit request (iTerm2's
    /// `preserveAspectRatio=0`, Kitty's `c=` with `r=`, a box kitty fills whole from the
    /// cursor cell).
    Stretch,
    /// One source pixel to one device pixel, anchored top-left, the remainder transparent.
    /// For protocols that name pixels: a sixel footprint is the pixel size rounded up to whole
    /// cells, so scaling to it would only magnify rounding noise and blur 1-px features
    /// (xterm, foot, mlterm, wezterm, contour and mintty all draw sixel 1:1). Kitty graphics
    /// sent without `c=`/`r=` are the same case.
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
    #[inline]
    #[must_use]
    pub const fn clamp_to(self, src_w: u32, src_h: u32) -> Option<(u32, u32, u32, u32)> {
        if self.x >= src_w || self.y >= src_h {
            return None;
        }
        let max_w = src_w - self.x;
        let max_h = src_h - self.y;
        let w = if self.width == 0 || self.width > max_w {
            max_w
        } else {
            self.width
        };
        let h = if self.height == 0 || self.height > max_h {
            max_h
        } else {
            self.height
        };
        if w > 0 && h > 0 {
            Some((self.x, self.y, w, h))
        } else {
            None
        }
    }
}

impl ImageData {
    /// Heap bytes this payload owns (the raster, not the struct).
    #[must_use]
    #[inline]
    pub fn payload_bytes(&self) -> usize {
        self.bytes.capacity()
    }

    /// This payload's per-row share of [`payload_bytes`](Self::payload_bytes): summed over the
    /// footprint's rows it recovers the payload once (less at most `rows` bytes of truncation).
    #[must_use]
    #[inline]
    pub fn per_row_bytes(&self) -> usize {
        self.payload_bytes() / usize::from(self.rows.max(1))
    }
}
