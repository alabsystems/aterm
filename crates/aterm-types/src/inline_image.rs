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
