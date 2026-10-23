// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only Metal present-blit runner using [`Pipeline::Blit`], compared byte-for-byte against the wgpu blit oracle.

#[cfg(wgpu_arm)]
use super::ffi::{self, Device, Obj, PixelFormat, SamplerDesc};
#[cfg(wgpu_arm)]
use super::pipelines;
#[cfg(wgpu_arm)]
use crate::pipeline_table::Pipeline;

/// Size in bytes of `BlitUniform`.
pub(crate) const UNIFORM_BYTES: usize = 112;

/// Metal present-blit pipeline, nearest sampler, and 112-byte uniform buffer for one destination format.
#[cfg(wgpu_arm)]
pub(crate) struct MetalBlit {
    device: Device,
    queue: Obj,
    /// `MTLRenderPipelineState` for `vs_blit` + `fs_blit` at `self.format`.
    pipeline: Obj,
    /// Nearest-clamp sampler bound for `fs_blit`.
    sampler: Obj,
    /// Shared 112-byte `BlitUniform` buffer.
    uniform: Obj,
    format: PixelFormat,
}

#[cfg(wgpu_arm)]
impl MetalBlit {
    /// [`UNIFORM_BYTES`], re-exported for the differential test.
    pub(crate) const UNIFORM_BYTES: usize = UNIFORM_BYTES;

    /// Offscreen source frame pixel format.
    const SRC_FORMAT: PixelFormat = PixelFormat::Rgba8Unorm;

    /// Compiles `shaders/blit.metal` and allocates the blit pipeline, sampler, and uniform buffer.
    pub(crate) fn new(format: PixelFormat) -> Result<Self, String> {
        let device = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
        let queue = device
            .new_command_queue()
            .ok_or_else(|| "MTLCommandQueue allocation failed".to_owned())?;
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

    /// Name of the Metal device this blit runs on.
    pub(crate) fn device_name(&self) -> String {
        self.device.name()
    }

    /// Runs `vs_blit` + `fs_blit` over `src` (`src_w * src_h` RGBA8 texels) with `uniform` and returns the destination texels.
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
        let binds = Pipeline::Blit.spec().binds;
        ffi::draw_and_read(
            &self.queue,
            &ffi::Pass {
                pso: &self.pipeline,
                dst: &dst_tex,
                dst_w: dw,
                dst_h: dh,
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
