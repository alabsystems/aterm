// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MSL twins of aterm's WGSL shaders. The entry-point roster is derived
//! from [`crate::pipeline_table`] (see `libraries`); `crate::shader_twins`
//! scans every twin's entry points and bindings against it.
//!
//! The WGSL lives in `const &str` literals in `renderer.rs`; the MSL lives in
//! `crates/aterm-gpu/shaders/*.metal`, pulled in with `include_str!`, so the
//! compile test, editors and `xcrun metal` can read it.
//!
//! # Colour spaces
//!
//! `renderer.rs::pick_surface_format` chooses a non-sRGB surface format, never
//! an `*_sRGB` one:
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
//! Metal expresses the pairing with `newTextureViewWithPixelFormat:` on
//! Unorm/sRGB view-compatible formats. The formats used (`Bgra8Unorm`,
//! `Rgba8Unorm`, `R8Unorm`, `Rgba16Float`) are declared in
//! [`super::ffi::PixelFormat`] and exercised by [`super::tests`].

//! # WGSL to MSL mapping
//!
//! Structural differences: `@location` params become `[[attribute(n)]]` +
//! `[[stage_in]]` structs; `@interpolate(flat)` becomes `[[flat]]`;
//! `bitcast<i32>` becomes `as_type<int>`; `textureLoad` becomes `read`;
//! `textureSampleLevel(.., 0.0)` becomes `sample(.., level(0.0))`; uniforms,
//! textures and samplers become function arguments. In `cell.metal` the rain
//! weight and fire shading tails are shared `static inline` helpers so the
//! parity kernel calls the same code as the fragments. Integer math is
//! otherwise op for op. `parity_kernel.metal` is verification-only.
//!
//! Every construct maps exactly:
//!
//! * Arithmetic right shift of negative integers (`fire_core`'s
//!   `(body0 - 128) * edge >> 8`): sign-replicating in both WGSL and MSL for
//!   signed types; the fire differential in `super::tests` confirms it.
//! * Wrapping `u32` arithmetic (`fire_hash`): defined in both.
//! * Flat integer interpolation: MSL requires `[[flat]]`; every flat value is
//!   per instance, so the provoking vertex does not matter.
//! * Uniform layout: std140 in WGSL, natural C layout in MSL. Every member here
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

/// The MSL source for one [`ShaderLibrary`]. Which entry points it must export
/// comes from `crate::pipeline_table::entry_points`.
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

/// Every `(library, source, entry points)` triple, derived from the pipeline
/// table, so one test proves every library compiles and every entry point the
/// renderer requests resolves. Derived rather than listed so the roster cannot
/// agree with a rename the renderer does not know about.
#[cfg(test)]
pub(crate) fn libraries() -> Vec<(ShaderLibrary, &'static str, Vec<&'static str>)> {
    ShaderLibrary::ALL
        .into_iter()
        .map(|lib| (lib, source(lib), crate::pipeline_table::entry_points(lib)))
        .collect()
}

/// Verification-only state probes (fullscreen triangle, constant-colour and
/// sampling fragments) that let [`super::tests`] prove the write mask,
/// scissor, viewport and sampler filters in [`super::ffi`] are honoured.
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

#[cfg(test)]
mod tests {
    /// The Metal half of the bind column the twin scan cannot state off
    /// macOS: no row's vertex uniform sits on `INSTANCE_STREAM_SLOT`, where the
    /// vertex descriptor binds the instance stream. The rest of the MSL entry
    /// point and binding scan is `crate::shader_twins`, which reads every
    /// dialect's twins on every host.
    #[test]
    fn no_vertex_uniform_sits_on_the_instance_stream_slot() {
        for p in crate::pipeline_table::ALL_PIPELINES {
            assert_ne!(
                p.spec().binds.vertex_uniform,
                Some(crate::metal::ffi::INSTANCE_STREAM_SLOT as u32),
                "{}: the vertex uniform may not sit on INSTANCE_STREAM_SLOT",
                p.name()
            );
        }
    }
}
