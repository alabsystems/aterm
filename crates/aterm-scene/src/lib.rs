// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-scene` — shared deterministic animation primitives used by the live effects
//! engine: RGB mixing, easing, vector-path filling, and the [`Tile`] raster surface.

#![forbid(unsafe_code)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub(crate) mod atlas;
pub mod vector;

pub use atlas::Tile;
pub use vector::{PathCmd, PathSeg, PathTransform, fill_path, fill_path_fixed, parse_path};

// =====================================================================================
// Frame-rate-independent math shared by the scenes.
// =====================================================================================

/// Clamp `v` into `[lo, hi]`. `lo <= hi` is the caller's contract; if violated the
/// result is `lo` (we clamp high first, then low), which is still finite and bounded.
#[must_use]
pub(crate) fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    let v = if v > hi { hi } else { v };
    if v < lo { lo } else { v }
}

/// Smoothstep easing of `t ∈ [0,1]` (the classic `3t² − 2t³`).
#[must_use]
pub fn smoothstep(t: f32) -> f32 {
    let t = clampf(t, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Per-channel linear blend of two packed `0x00RRGGBB` colours by `t ∈ [0,1]` (the
/// shared gradient/daylight helper for every scene). `t` is clamped.
#[must_use]
pub fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = clampf(t, 0.0, 1.0);
    // Each call site shifts by a *constant* (16/8/0) and hands the closure the
    // pre-shifted channels, so every shift amount is trivially in range — same
    // math as shifting inside the closure, but provably panic-free.
    let ch = |ca: u32, cb: u32| {
        let ca = (ca & 0xff) as f32;
        let cb = (cb & 0xff) as f32;
        ((ca + (cb - ca) * t) + 0.5) as u32 & 0xff
    };
    (ch(a >> 16, b >> 16) << 16) | (ch(a >> 8, b >> 8) << 8) | ch(a, b)
}

/// Pack `(r, g, b)` channels into `0x00RRGGBB`.
#[must_use]
pub const fn pack_rgb24(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Unpack a `0x00RRGGBB` color into `(r, g, b)`.
#[must_use]
pub const fn unpack_rgb24(rgb: u32) -> (u8, u8, u8) {
    (
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// Integer linear blend of two 8-bit channels by `alpha ∈ 0..=255`.
#[must_use]
pub const fn blend_channel_u8(a: u8, b: u8, alpha: u8) -> u8 {
    let inv = 255u32.saturating_sub(alpha as u32);
    let num = (a as u32)
        .saturating_mul(inv)
        .saturating_add((b as u32).saturating_mul(alpha as u32))
        .saturating_add(127);
    (num / 255) as u8
}

/// Integer linear blend of two `0x00RRGGBB` colors by `alpha ∈ 0..=255`.
#[must_use]
pub const fn blend_rgb24_u8(a: u32, b: u32, alpha: u8) -> u32 {
    let (ar, ag, ab) = unpack_rgb24(a);
    let (br, bg, bb) = unpack_rgb24(b);
    pack_rgb24(
        blend_channel_u8(ar, br, alpha),
        blend_channel_u8(ag, bg, alpha),
        blend_channel_u8(ab, bb, alpha),
    )
}

/// Encode a 4-word tile rectangle `(x, y, w, h)` into 16 little-endian bytes.
#[must_use]
pub const fn encode_tile_rect_le(x: u32, y: u32, w: u32, h: u32) -> [u8; 16] {
    [
        x as u8,
        (x >> 8) as u8,
        (x >> 16) as u8,
        (x >> 24) as u8,
        y as u8,
        (y >> 8) as u8,
        (y >> 16) as u8,
        (y >> 24) as u8,
        w as u8,
        (w >> 8) as u8,
        (w >> 16) as u8,
        (w >> 24) as u8,
        h as u8,
        (h >> 8) as u8,
        (h >> 16) as u8,
        (h >> 24) as u8,
    ]
}

/// Decode 16 little-endian bytes into a 4-word tile rectangle `(x, y, w, h)`.
#[must_use]
pub const fn decode_tile_rect_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let x = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let y = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let w = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let h = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (x, y, w, h)
}

#[cfg(test)]
mod scene_const_tests {
    use super::*;

    #[test]
    fn scene_rgb_and_tile_rect_const_helpers() {
        let c = pack_rgb24(0x12, 0x34, 0x56);
        assert_eq!(unpack_rgb24(c), (0x12, 0x34, 0x56));
        assert_eq!(blend_rgb24_u8(c, 0x00FF_FFFF, 0), c);
        assert_eq!(blend_rgb24_u8(c, 0x00FF_FFFF, 255), 0x00FF_FFFF);
        let r = encode_tile_rect_le(10, 20, 300, 400);
        assert_eq!(decode_tile_rect_le(r), (10, 20, 300, 400));
    }
}
