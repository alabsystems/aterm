// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party Direct3D 11 backend: Windows's renderer
//! (docs/AUDIT-third-party-2026-10-15.md GW.5 + WB.4), beside `metal` on macOS
//! and `gl` on Linux and in the browser. wgpu's DX12 backend stays as the
//! test oracle every D3D11 frame is byte-compared against on the same adapter
//! (`--features wgpu-oracle`), the way the Metal and GL arms' oracles work,
//! and as the frames it drew on WARP before the flip
//! (`oracle/warp-frames.tsv`).
//!
//! Every call goes through aterm-win32's hand-written COM slices
//! (`aterm_win32::d3d11`, `dxgi`, `dcomp`), which its oracle holds slot by
//! slot to `windows` 0.62.2; no `windows` crate is linked.
//!
//! * [`device`]: the adapter (DXGI's by power preference, or the
//!   `ATERM_GPU_ADAPTER` seam), the `ID3D11Device` at feature level 11_0 and
//!   its immediate context behind a lock, the shader compiler
//!   (`d3dcompiler_47.dll`, loaded from System32) and the device-removed
//!   latch.
//! * [`resources`]: textures with their views, buffers and samplers.
//! * [`shaders`]: the HLSL twins of the WGSL, `shaders/<library>.hlsl`.
//! * [`pipelines`]: shaders, input layouts and fixed-function state from the
//!   pipeline table.
//! * [`encoder`]: the frame and pass verbs the device layer forwards to.
//! * [`surface`]: a window's flip-model swap chain, on the window or on a
//!   DirectComposition visual, with the frame-latency waitable and the scRGB
//!   tag.
//!
//! # Coordinates and colour
//!
//! Direct3D's conventions are wgpu's: row 0 of a texture is its top row and
//! clip-space y points up, so the twins are the WGSL's math unchanged.
//!
//! The offscreen is `R8G8B8A8_TYPELESS` with `UNORM` and `UNORM_SRGB` views,
//! exactly the texture wgpu-dx12 makes for an `Rgba8Unorm` texture with an
//! `Rgba8UnormSrgb` view format: the base passes draw through the sRGB view
//! (encode on write, blend in linear light), the additive passes through the
//! Unorm view. Feature level 11_0 guarantees both, so the D3D11 arm always
//! runs the native format plan.

pub(crate) mod device;
pub(crate) mod encoder;
pub(crate) mod pipelines;
pub(crate) mod resources;
pub(crate) mod shaders;
#[cfg_attr(
    all(wgpu_arm, not(test)),
    expect(dead_code, reason = "a build carrying wgpu attaches windows on wgpu")
)]
pub(crate) mod surface;

/// A headless device for a D3D11 test; a machine without a Direct3D 11
/// adapter could not run the test.
#[cfg(test)]
pub(crate) fn test_device() -> device::D3dDevice {
    device::D3dDevice::headless()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no D3D11 device: {e}")))
}
