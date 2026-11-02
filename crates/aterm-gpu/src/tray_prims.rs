// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GPU tray prims (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.2): the vector subset of
//! the chrome's prims as instances the GPU draws into a card, each with the CPU raster's coverage
//! formula and composite law (`shaders/tray_prims.metal`), so a card the GPU draws holds the bytes
//! the CPU raster (`aterm-gui`'s `tray_raster`) gives for the same prims, within the parity the
//! tests declare. The painter converts its prims (`tray_raster::gpu_prims`): every place, size,
//! clip and colour is its, resolved to device pixels before the GPU sees it.
//!
//! The renderer draws them on the Metal arm of a device with framebuffer fetch
//! ([`crate::GpuRenderer::tray_prims_supported`]); elsewhere the CPU raster stays the only one.

/// What one [`TrayPrim`] draws, and what its [`TrayPrim::geom`] holds (device px; a `Canvas`
/// method of `tray_raster` names the CPU law each follows).
pub mod kind {
    /// `Canvas::round_rect_under` (a panel, a capsule's track or fill): `x, y, w, h, r`.
    pub const ROUND_RECT: u32 = 1;
    /// `Canvas::arc` (a ring's track or fill): `cx, cy, r_mid, thickness, frac, start`.
    pub const RING_ARC: u32 = 2;
    /// `Canvas::arc_band`: `cx, cy, r, half, from, span`, then both ends' `x, y`.
    pub const ARC_BAND: u32 = 3;
    /// `Canvas::line_segment`: `x1, y1, vx, vy, length_squared, radius`.
    pub const SEGMENT: u32 = 4;
    /// `Canvas::disc`: `cx, cy, r`.
    pub const DISC: u32 = 5;
    /// `Canvas::stroke_rect`'s rounded arm: `cx, cy, hx, hy, r, hw`.
    pub const STROKE: u32 = 6;
    /// A whole-pixel fill (`fill_px_rect`, `fill_device`): `x0, y0, x1, y1`.
    pub const FILL: u32 = 7;
}

/// How many per-frame offsets a draw carries ([`TrayPrim::offset`] selects one; 0 is none).
pub const TRAY_PRIM_OFFSETS: usize = 8;

/// One GPU tray prim: 64 bytes, `shaders/tray_prims.metal`'s `TrayPrim`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrayPrim {
    /// What it draws ([`kind`]).
    pub kind: u32,
    /// The per-frame offset it moves by, as an index into the draw's offsets (0: it stays where
    /// its geometry is). The geometry moves; the clip folded into [`Self::rect`] does not.
    pub offset: u32,
    /// The device pixels it may ink, `x0, y0, x1, y1` half-open: the CPU raster's bounds for the
    /// prim within its clip, widened by any travel its offset may take.
    pub rect: [i16; 4],
    /// Its geometry, device px, laid out as its [`kind`] says; unused entries are 0.
    pub geom: [f32; 10],
    /// Its colour, straight RGBA8 in sRGB.
    pub color: [u8; 4],
    /// Explicit tail padding to 64 bytes.
    pub _pad: u32,
}

const _: () = assert!(
    size_of::<TrayPrim>() == 64,
    "TrayPrim is the shader's 64 bytes"
);

// SAFETY: `TrayPrim` is `#[repr(C)]`, `Copy + 'static`, made only of `Pod` fields, and has no
// padding (its size is the sum of its fields', asserted above); every bit pattern is a value.
unsafe impl aterm_bits::Zeroable for TrayPrim {}
// SAFETY: as above.
unsafe impl aterm_bits::Pod for TrayPrim {}

impl TrayPrim {
    /// A prim of `kind` with `geom` (padded with zeros) in `color`, inking within `rect` (device
    /// px, half-open, clamped to the `i16` range).
    #[must_use]
    pub fn new(kind: u32, rect: [i32; 4], geom: &[f32], color: [u8; 4]) -> Self {
        let mut g = [0.0; 10];
        for (slot, v) in g.iter_mut().zip(geom) {
            *slot = *v;
        }
        let narrow = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        Self {
            kind,
            offset: 0,
            rect: rect.map(narrow),
            geom: g,
            color,
            _pad: 0,
        }
    }

    /// Whether it may ink no pixel.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rect[2] <= self.rect[0] || self.rect[3] <= self.rect[1]
    }
}

/// The CPU raster's composite law as tables (`tray_raster`'s): each byte's linear value, each
/// byte over 255, and the 255 linear values at which an encoded byte steps up (padded with `+∞`).
/// The GPU reads these rather than computing them, so its composite is the CPU's.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrayPrimLaw {
    /// `srgb_to_linear_lut`.
    pub to_linear: [f32; 256],
    /// `f32::from(b) / 255.0` for every byte `b`.
    pub unit: [f32; 256],
    /// `linear_to_srgb_u8`'s thresholds: entry `n` is the least linear value that encodes past
    /// byte `n`; entry 255 is `+∞`.
    pub thresholds: [f32; 256],
}

// SAFETY: `#[repr(C)]` arrays of `f32`, no padding; every bit pattern is a value.
unsafe impl aterm_bits::Zeroable for TrayPrimLaw {}
// SAFETY: as above.
unsafe impl aterm_bits::Pod for TrayPrimLaw {}

/// `shaders/tray_prims.metal`'s `TrayPrimsU`: the target's size and its top-left in the prims'
/// device px, and the draw's offsets.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrayPrimsUniform {
    pub(crate) target: [f32; 2],
    pub(crate) origin: [i32; 2],
    pub(crate) offsets: [[f32; 2]; TRAY_PRIM_OFFSETS],
}

// SAFETY: `#[repr(C)]` of `Pod` arrays with no padding (8 + 8 + 64 bytes).
#[cfg(target_os = "macos")]
unsafe impl aterm_bits::Zeroable for TrayPrimsUniform {}
// SAFETY: as above.
#[cfg(target_os = "macos")]
unsafe impl aterm_bits::Pod for TrayPrimsUniform {}

#[cfg(target_os = "macos")]
const _: () = assert!(size_of::<TrayPrimsUniform>() == 80);

#[cfg(target_os = "macos")]
impl TrayPrimsUniform {
    /// The uniform for a `size` target whose top-left is device px `origin`, with `offsets`
    /// (index 0 forced to none; at most [`TRAY_PRIM_OFFSETS`]).
    pub(crate) fn new(size: (u32, u32), origin: (i32, i32), offsets: &[[f32; 2]]) -> Self {
        let mut o = [[0.0; 2]; TRAY_PRIM_OFFSETS];
        for (slot, v) in o.iter_mut().zip(offsets).skip(1) {
            *slot = *v;
        }
        Self {
            target: [size.0 as f32, size.1 as f32],
            origin: [origin.0, origin.1],
            offsets: o,
        }
    }
}

/// `shaders/tray_prims.metal`'s `TrayLayerU`: one card of a tray stack folded into the stack's
/// card, at its place there and its opacity.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrayLayerUniform {
    pub(crate) target: [f32; 2],
    pub(crate) origin: [i32; 2],
    pub(crate) size: [i32; 2],
    pub(crate) opacity: f32,
    pub(crate) _pad: f32,
}

// SAFETY: `#[repr(C)]` of `Pod` fields with no padding (8 + 8 + 8 + 4 + 4 bytes).
#[cfg(target_os = "macos")]
unsafe impl aterm_bits::Zeroable for TrayLayerUniform {}
// SAFETY: as above.
#[cfg(target_os = "macos")]
unsafe impl aterm_bits::Pod for TrayLayerUniform {}

#[cfg(target_os = "macos")]
const _: () = assert!(size_of::<TrayLayerUniform>() == 32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prim_is_the_shaders_64_bytes_at_the_shaders_offsets() {
        let p = TrayPrim::new(kind::DISC, [1, 2, 3, 4], &[5.0, 6.0, 7.0], [8, 9, 10, 11]);
        let bytes = aterm_bits::bytes_of(&p);
        assert_eq!(bytes.len(), 64);
        assert_eq!(&bytes[0..4], &kind::DISC.to_ne_bytes());
        assert_eq!(&bytes[8..10], &1i16.to_ne_bytes(), "rect at 8");
        assert_eq!(&bytes[16..20], &5.0f32.to_ne_bytes(), "geometry at 16");
        assert_eq!(&bytes[56..60], &[8, 9, 10, 11], "colour at 56");
        assert_eq!(p.geom[3..], [0.0; 7]);
    }

    #[test]
    fn a_rect_past_i16_is_clamped_and_an_inverted_one_is_empty() {
        let p = TrayPrim::new(kind::FILL, [-40_000, 0, 40_000, 9], &[], [0; 4]);
        assert_eq!(p.rect, [i16::MIN, 0, i16::MAX, 9]);
        assert!(!p.is_empty());
        assert!(TrayPrim::new(kind::FILL, [5, 0, 5, 9], &[], [0; 4]).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn offset_zero_is_none_whatever_the_caller_passes() {
        let u = TrayPrimsUniform::new((10, 20), (3, 4), &[[9.0, 9.0], [1.5, -2.0]]);
        assert_eq!(u.offsets[0], [0.0, 0.0]);
        assert_eq!(u.offsets[1], [1.5, -2.0]);
        assert_eq!((u.target, u.origin), ([10.0, 20.0], [3, 4]));
    }
}
