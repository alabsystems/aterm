// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MSL shaders, loaded from `crates/aterm-gpu/shaders/*.metal`: twins of the WGSL wgpu drew
//! with until GW.6 (docs/AUDIT-third-party-2026-10-15.md), beside the GLSL and HLSL ones.
//!
//! Base OVER/REPLACE passes render linear light into an sRGB texture view; additive
//! passes bind the Unorm view for byte-exact `add_sat`; the swapchain is never sRGB
//! (`Bgra8Unorm` or `Rgba16Float`) so the blit does not double-encode. Uniforms use natural
//! C layout in MSL, which equals the std140 the twins share (every member is `<= 16` bytes and
//! naturally aligned).

use crate::pipeline_table::ShaderLibrary;

/// Cell shader.
pub(crate) const CELL: &str = include_str!("../../shaders/cell.metal");
/// Offscreen-to-swapchain blit shader.
pub(crate) const BLIT: &str = include_str!("../../shaders/blit.metal");
/// Swapchain-side aurora crown shader.
pub(crate) const HDR_GLOW: &str = include_str!("../../shaders/hdr_glow.metal");
/// Tray/overlay texture blit shader.
pub(crate) const TRAY: &str = include_str!("../../shaders/tray.metal");
/// 5x5 Gaussian bloom shader.
pub(crate) const BLOOM: &str = include_str!("../../shaders/bloom.metal");
/// EMBERFORGE heat-haze displacement shader.
pub(crate) const SHIMMER: &str = include_str!("../../shaders/shimmer.metal");

/// The line [`source_for`] puts before [`CELL`] for a device with framebuffer fetch: it compiles `cell.metal`'s fetch twins and probe (`#if ATERM_FETCH`).
pub(crate) const FETCH_DEFINE: &str = "#define ATERM_FETCH 1\n";

/// The MSL a device compiles for `library`: [`source`], with [`FETCH_DEFINE`] before `cell.metal` when `fetch` (the device has framebuffer fetch, [`super::fetch::capable`]).
pub(crate) fn source_for(library: ShaderLibrary, fetch: bool) -> std::borrow::Cow<'static, str> {
    match library {
        ShaderLibrary::Cell if fetch => format!("{FETCH_DEFINE}{CELL}").into(),
        _ => source(library).into(),
    }
}

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
