// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MSL twins of aterm's WGSL shaders. The WGSL lives in `renderer.rs`; the
//! MSL in `crates/aterm-gpu/shaders/*.metal`, pulled in with `include_str!` so
//! editors and `xcrun metal` can read it. Which entry points each library
//! exports is the business of [`crate::pipeline_table`].
//!
//! # Colour spaces
//!
//! The surface format is never `*_sRGB` (`pick_surface_format`):
//!
//! * Base OVER/REPLACE passes render into an sRGB view of the offscreen; the
//!   fragments emit linear light (`s2l`) and the view re-encodes on store, so
//!   blending composites in linear light like the CPU `blend`.
//! * Additive passes (`fs_glow`, `fs_rain_glow`, `fs_fire_add`, `fs_deco_add`)
//!   bind the Unorm view of the same texture and emit raw values, so One/One
//!   is byte-exact with the CPU `add_sat`.
//! * The blit writes already-encoded bytes; an sRGB swapchain would encode
//!   twice.
//!
//! The view pairs come from `newTextureViewWithPixelFormat:` on
//! view-compatible Unorm/sRGB formats.
//!
//! # WGSL to MSL mapping
//!
//! `@location` params become `[[attribute(n)]]` + `[[stage_in]]` structs;
//! `@interpolate(flat)` becomes `[[flat]]`; `bitcast<i32>` becomes
//! `as_type<int>`; `textureLoad` becomes `read`; `textureSampleLevel(.., 0.0)`
//! becomes `sample(.., level(0.0))`; uniforms, textures and samplers become
//! function arguments. Integer math is otherwise op for op. In `cell.metal` the
//! rain weight and fire shading are shared `static inline` helpers so the
//! verification-only `parity_kernel.metal` calls the same code as the
//! fragments.
//!
//! * Arithmetic right shift of a negative signed integer (`fire_core`'s
//!   `(body0 - 128) * edge >> 8`) sign-replicates in both languages.
//! * Wrapping `u32` arithmetic (`fire_hash`) is defined in both.
//! * Every flat value is per instance, so the provoking vertex does not matter.
//! * Uniforms are std140 in WGSL and natural C layout in MSL. Every member here
//!   is at most 16 bytes and naturally aligned, so they agree; recheck
//!   `BlitUniform` (112 bytes) and `ShimmerU`'s `float4[16]` when adding
//!   fields.

use crate::pipeline_table::ShaderLibrary;

/// The cell shader: backgrounds, aurora, rain halo, fire field, glyphs,
/// decorations and sprites. WGSL twin: `renderer.rs::SHADER`.
pub(crate) const CELL: &str = include_str!("../../shaders/cell.metal");
/// The offscreen-to-swapchain blit, with remainder bands, bell invert, drop
/// overlay and HDR encode. WGSL twin: `renderer.rs::BLIT_SHADER`.
pub(crate) const BLIT: &str = include_str!("../../shaders/blit.metal");
/// The swapchain-side aurora crown, HDR and SDR arms. WGSL twin:
/// `renderer.rs::HDR_GLOW_SHADER`.
pub(crate) const HDR_GLOW: &str = include_str!("../../shaders/hdr_glow.metal");
/// The tray/overlay texture blit. WGSL twin: `renderer.rs::TRAY_SHADER`.
pub(crate) const TRAY: &str = include_str!("../../shaders/tray.metal");
/// The 5x5 gaussian bloom tap. WGSL twin: `renderer.rs::BLOOM_SHADER`.
pub(crate) const BLOOM: &str = include_str!("../../shaders/bloom.metal");
/// The EMBERFORGE heat-haze displacement. WGSL twin:
/// `renderer.rs::SHIMMER_SHADER`.
pub(crate) const SHIMMER: &str = include_str!("../../shaders/shimmer.metal");

/// The MSL source for one [`ShaderLibrary`].
pub(crate) const fn source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => CELL,
        ShaderLibrary::Blit => BLIT,
        ShaderLibrary::HdrGlow => HDR_GLOW,
        ShaderLibrary::Tray => TRAY,
        ShaderLibrary::Bloom => BLOOM,
        ShaderLibrary::Shimmer => SHIMMER,
    }
}

/// Verification-only state probes (fullscreen triangle, constant-colour and
/// sampling fragments) that let [`super::tests`] prove the write mask and
/// sampler filters in [`super::ffi`] are honoured.
#[cfg(test)]
pub(crate) const STATE_PROBE: &str = include_str!("../../shaders/state_probe.metal");

/// Verification-only compute kernels, concatenated onto [`CELL`] by the parity
/// test so the math under test is the shipped math.
#[cfg(test)]
pub(crate) const PARITY_KERNEL: &str = include_str!("../../shaders/parity_kernel.metal");

/// [`CELL`] plus [`PARITY_KERNEL`], the source the parity test compiles.
#[cfg(test)]
pub(crate) fn cell_with_parity_kernels() -> String {
    format!("{CELL}\n{PARITY_KERNEL}")
}
