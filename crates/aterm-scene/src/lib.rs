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

// Frame-rate-independent math shared by the scenes.

/// Clamp `v` into `[lo, hi]`. `lo <= hi` is the caller's contract; for non-NaN inputs
/// with `lo > hi` the result is `lo` (high is clamped first, then low), while `NaN` `v`
/// propagates unchanged.
#[inline]
#[must_use]
pub(crate) const fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    let v = if v > hi { hi } else { v };
    if v < lo { lo } else { v }
}

/// Smoothstep easing of `t ∈ [0,1]` (the classic `3t² − 2t³`).
#[inline]
#[must_use]
pub const fn smoothstep(t: f32) -> f32 {
    let t = clampf(t, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Per-channel linear blend of two packed `0x00RRGGBB` colours by `t ∈ [0,1]` (the
/// shared gradient/daylight helper for every scene). `t` is clamped.
#[inline]
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
