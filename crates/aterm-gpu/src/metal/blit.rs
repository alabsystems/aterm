// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The present-path blit on Metal, for the blit differential.
//!
//! [`super::tests::blit_matches_wgpu_byte_for_byte`] pushes the same source
//! pixels and 112 uniform bytes through the wgpu blit and through this one and
//! requires every byte to agree. That is stronger than `blit_invert.rs` and
//! `blit_bands.rs`, which assert properties of one backend's output, because a
//! gamma shift, swapped channel, filtered sample or half-texel offset shows as
//! a byte difference on arbitrary pixels.
//!
//! # Binding map
//!
//! WGSL's `@group @binding` has no Metal equivalent, so this module binds by
//! reading `Pipeline::Blit.spec().binds` ([`crate::pipeline_table::BindSpec`]).
//! The MSL scan in `super::shaders` holds that column equal to `blit.metal`,
//! and the differential pins the bytes, so a swapped slot fails both.
//!
//! Test-only: the macOS backend runs its blit from the same row inside
//! [`super::present::encode_present_sequence`].

use super::ffi::{self, Device, Obj, PixelFormat, SamplerDesc};
use super::pipelines;
use crate::pipeline_table::Pipeline;

/// Metal twin of `renderer.rs::build_blit_resources`: the shader library, the
/// pipeline state for one destination format, the nearest sampler and the
/// 112-byte uniform buffer. wgpu's bind-group and pipeline layouts have no
/// Metal counterpart; Metal binds by slot at encode time.
pub(crate) struct MetalBlit {
    device: Device,
    queue: Obj,
    /// `MTLRenderPipelineState` for `vs_blit` + `fs_blit` at one destination
    /// format (the readable swapchain stand-in's).
    pipeline: Obj,
    /// [`SamplerDesc::NEAREST_CLAMP`], as `build_blit_resources` uses. Bound
    /// because the shader declares it; `fs_blit` fetches with `read()`.
    sampler: Obj,
    /// The shared 112-byte `BlitUniform` buffer, written per present.
    uniform: Obj,
    format: PixelFormat,
}

impl MetalBlit {
    /// Bytes per `BlitUniform`, asserted against the Rust struct where it is
    /// filled, so a field added on one side only fails loudly.
    pub(crate) const UNIFORM_BYTES: usize = 112;

    /// The offscreen frame's format; the upload stride uses its
    /// [`PixelFormat::bytes_per_texel`].
    const SRC_FORMAT: PixelFormat = PixelFormat::Rgba8Unorm;

    /// Compile `shaders/blit.metal` and build every object the pass needs.
    /// Returns `Err` without a Metal device.
    pub(crate) fn new(format: PixelFormat) -> Result<Self, String> {
        // The device the metal arm renders on (the low-power GPU of a dual-GPU
        // Mac) — see `Device::preferred`.
        let device = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
        let queue = device
            .new_command_queue()
            .ok_or_else(|| "MTLCommandQueue allocation failed".to_owned())?;
        // The pipeline state comes from the `Pipeline::Blit` row, the one
        // `renderer.rs::ensure_blit_pipeline` uses: entry points, REPLACE (Metal:
        // blending disabled), the all-channel mask (the blit publishes alpha),
        // and no vertex descriptor (`vs_blit` reads `[[vertex_id]]`).
        let spec = Pipeline::Blit.spec();
        let library = pipelines::compile(&device, spec)?;
        let pipeline = pipelines::build(&device, &library, spec, format)?;

        let sampler = device
            .new_sampler(SamplerDesc::NEAREST_CLAMP)
            .ok_or_else(|| "MTLSamplerState allocation failed".to_owned())?;
        let uniform = device
            .new_buffer(Self::UNIFORM_BYTES)
            .ok_or_else(|| "blit uniform buffer allocation failed".to_owned())?;

        Ok(Self {
            device,
            queue,
            pipeline,
            sampler,
            uniform,
            format,
        })
    }

    /// The GPU this blit runs on, for diagnostics.
    pub(crate) fn device_name(&self) -> String {
        self.device.name()
    }

    /// Run `vs_blit` + `fs_blit` over `src` and read the destination back as
    /// tightly packed RGBA8. `src` is `src_w * src_h` RGBA8 texels; `uniform` is
    /// the 112 bytes of a `BlitUniform` from `present_blit_uniform`, as the
    /// wgpu path uses.
    pub(crate) fn run(
        &self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        uniform: &[u8],
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Vec<u8>, String> {
        let _pool = ffi::AutoreleasePool::new();
        let (sw, sh) = (src_w as usize, src_h as usize);
        let (dw, dh) = (dst_w as usize, dst_h as usize);
        let src_row = sw
            .checked_mul(Self::SRC_FORMAT.bytes_per_texel())
            .ok_or_else(|| format!("source row {sw} texels overflows usize"))?;
        let src_len = src_row
            .checked_mul(sh)
            .ok_or_else(|| format!("source size {src_row} x {sh} overflows usize"))?;
        if src.len() != src_len {
            return Err(format!(
                "source is {} bytes, expected {} for {src_w}x{src_h} {:?}",
                src.len(),
                src_len,
                Self::SRC_FORMAT
            ));
        }
        if uniform.len() != Self::UNIFORM_BYTES {
            return Err(format!(
                "uniform is {} bytes, expected {}",
                uniform.len(),
                Self::UNIFORM_BYTES
            ));
        }

        let src_tex = self
            .device
            .new_texture_2d(Self::SRC_FORMAT, sw, sh, ffi::TEXTURE_USAGE_SHADER_READ)
            .ok_or_else(|| "blit source texture allocation failed".to_owned())?;
        // SAFETY: `src_tex` is 2-D `SRC_FORMAT`, `sw` x `sh`, non-Private;
        // `src_row` is that format's texel size times `sw`, and `src` is exactly
        // `src_row * sh` bytes.
        unsafe {
            ffi::texture_upload(&src_tex, ffi::MtlRegion::full_2d(sw, sh), src, src_row);
        }

        let dst_tex = self
            .device
            .new_texture_2d(
                self.format,
                dw,
                dh,
                ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ,
            )
            .ok_or_else(|| "blit destination texture allocation failed".to_owned())?;

        // SAFETY: `self.uniform` is shared storage of exactly `UNIFORM_BYTES`,
        // and no command buffer uses it: the previous call waited, and `run` is
        // `&self` on a non-`Sync` type.
        unsafe { ffi::buffer_write(&self.uniform, uniform) };

        // The destination format's own texel size (8 for `Rgba16Float`); a wrong
        // stride copies silently with validation off.
        let row = dw
            .checked_mul(self.format.bytes_per_texel())
            .ok_or_else(|| format!("readback row {dw} texels overflows usize"))?;
        let readback_len = row
            .checked_mul(dh)
            .ok_or_else(|| format!("readback size {row} x {dh} overflows usize"))?;
        let readback = self
            .device
            .new_buffer(readback_len)
            .ok_or_else(|| "blit readback buffer allocation failed".to_owned())?;
        // Binding indices come from the row's `BindSpec`.
        let binds = Pipeline::Blit.spec().binds;
        ffi::draw_and_read(
            &self.queue,
            &ffi::Pass {
                pso: &self.pipeline,
                dst: &dst_tex,
                dst_w: dw,
                dst_h: dh,
                // Opaque black; the pass writes every pixel, so this only
                // matters for an aborted pass.
                load: ffi::LoadAction::Clear(ffi::ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                viewport: None,
                scissor: None,
                src_tex: Some((&src_tex, binds.fragment_textures[0] as usize)),
                sampler: Some((&self.sampler, binds.fragment_samplers[0] as usize)),
                uniform: Some((&self.uniform, binds.fragment_buffers[0] as usize)),
                vertex_uniform: None,
                draw: None,
            },
            &readback,
            row,
        )?;
        // SAFETY: `readback` is shared storage of exactly `row * dh` bytes, and
        // `draw_and_read` returned after `waitUntilCompleted`.
        Ok(unsafe { ffi::buffer_bytes(&readback, readback_len) })
    }
}
