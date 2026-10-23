// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Raw payload and placement types for inline images (iTerm2, sixel, and Kitty graphics).

/// Source encoding of an inline image's raw payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG container payload decoded by the renderer.
    Png,
    /// Packed row-major RGBA8 raster (`4 * width * height` bytes).
    RawRgba8 {
        /// Source raster width in pixels.
        width: u16,
        /// Source raster height in pixels.
        height: u16,
    },
    /// Unsupported container format kept verbatim and not drawn.
    Unknown,
}

/// Inline image payload and placement parameters shared across covered cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Raw payload bytes as delivered.
    pub bytes: Vec<u8>,
    /// Encoding format of `bytes`.
    pub format: ImageFormat,
    /// Footprint width in cells.
    pub cols: u16,
    /// Footprint height in cells.
    pub rows: u16,
    /// Stacking order (`< 0` behind cell text, `>= 0` over it).
    pub z_index: i32,
    /// Height in device pixels by which the raster extends above its first row.
    pub band_lift_px: u16,
    /// Sub-cell top-left offset `(x, y)` in device pixels inside the first cell.
    pub cell_offset_px: (u16, u16),
    /// How the source raster maps onto the cell footprint.
    pub scaling: ImageScaling,
    /// Optional sub-rectangle of the source raster to display.
    pub source_rect: Option<SourceRect>,
}

/// Identity and ordering tag for a Kitty graphics placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyPlacementTag {
    /// Client or terminal-assigned Kitty image ID (`i=` or `I=`).
    pub image_id: u32,
    /// Client placement ID (`p=`), or `0` when unnamed.
    pub placement_id: u32,
    /// Per-terminal transmission serial for z-index ordering.
    pub image_serial: core::num::NonZeroU32,
    /// Per-terminal serial unique to this placement.
    pub serial: core::num::NonZeroU32,
}

/// Returns the `(z_index, image_serial, placement_serial)` sort key for an image layer.
#[inline]
#[must_use]
pub const fn layer_order(z_index: i32, kitty: Option<KittyPlacementTag>) -> (i32, u32, u32) {
    let (image, serial) = match kitty {
        Some(tag) => (tag.image_serial.get(), tag.serial.get()),
        None => (0, 0),
    };
    (z_index, image, serial)
}

/// How a placement's source raster maps onto its cell footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageScaling {
    /// Scale preserving aspect ratio to fit within the footprint, centred.
    #[default]
    Fit,
    /// Scale to fill the entire footprint, ignoring aspect ratio.
    Stretch,
    /// Map one source pixel to one device pixel, anchored top-left.
    PixelExact,
}

/// Sub-rectangle of a source raster in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRect {
    /// Left edge in source pixels.
    pub x: u32,
    /// Top edge in source pixels.
    pub y: u32,
    /// Width in source pixels (`0` means to the right edge).
    pub width: u32,
    /// Height in source pixels (`0` means to the bottom edge).
    pub height: u32,
}

impl SourceRect {
    /// Clamps this rectangle to a `src_w × src_h` raster, or returns `None` if empty.
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
    /// Heap capacity in bytes owned by `self.bytes`.
    #[must_use]
    #[inline]
    pub const fn payload_bytes(&self) -> usize {
        self.bytes.capacity()
    }

    /// Per-row share of [`payload_bytes`](Self::payload_bytes) across `self.rows`.
    #[must_use]
    #[inline]
    pub fn per_row_bytes(&self) -> usize {
        self.payload_bytes() / usize::from(self.rows.max(1))
    }
}
