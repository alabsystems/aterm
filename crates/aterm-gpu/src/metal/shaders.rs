// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MSL twins of aterm's WGSL shaders (`renderer.rs`), loaded from `crates/aterm-gpu/shaders/*.metal`.
//!
//! Base OVER/REPLACE passes render linear light into an sRGB texture view; additive
//! passes bind the Unorm view for byte-exact `add_sat`; the swapchain is never sRGB
//! (`pick_surface_format`) so the blit does not double-encode. Uniforms use std140 in
//! WGSL and natural C layout in MSL (every member is `<= 16` bytes and naturally aligned).

use crate::pipeline_table::ShaderLibrary;

/// Cell shader (WGSL twin: `renderer.rs::SHADER`).
pub(crate) const CELL: &str = include_str!("../../shaders/cell.metal");
/// Offscreen-to-swapchain blit shader (WGSL twin: `renderer.rs::BLIT_SHADER`).
pub(crate) const BLIT: &str = include_str!("../../shaders/blit.metal");
/// Swapchain-side aurora crown shader (WGSL twin: `renderer.rs::HDR_GLOW_SHADER`).
pub(crate) const HDR_GLOW: &str = include_str!("../../shaders/hdr_glow.metal");
/// Tray/overlay texture blit shader (WGSL twin: `renderer.rs::TRAY_SHADER`).
pub(crate) const TRAY: &str = include_str!("../../shaders/tray.metal");
/// 5x5 Gaussian bloom shader (WGSL twin: `renderer.rs::BLOOM_SHADER`).
pub(crate) const BLOOM: &str = include_str!("../../shaders/bloom.metal");
/// EMBERFORGE heat-haze displacement shader (WGSL twin: `renderer.rs::SHIMMER_SHADER`).
pub(crate) const SHIMMER: &str = include_str!("../../shaders/shimmer.metal");

/// MSL source for [`ShaderLibrary`].
#[inline]
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

/// Verification-only state probe shaders for write-mask and sampler-filter tests.
#[cfg(test)]
pub(crate) const STATE_PROBE: &str = include_str!("../../shaders/state_probe.metal");

/// Verification-only compute kernels concatenated onto [`CELL`] for parity tests.
#[cfg(test)]
pub(crate) const PARITY_KERNEL: &str = include_str!("../../shaders/parity_kernel.metal");

/// [`CELL`] plus [`PARITY_KERNEL`] source for parity tests.
#[cfg(test)]
pub(crate) fn cell_with_parity_kernels() -> String {
    format!("{CELL}\n{PARITY_KERNEL}")
}
