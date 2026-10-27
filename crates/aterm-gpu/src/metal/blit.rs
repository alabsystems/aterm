// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only standalone runners of the Metal present's rows, [`Pipeline::Blit`] and the crown
//! rows ([`Pipeline::HdrGlow`], [`Pipeline::SdrGlow`]), whose frames the frame record holds to
//! their CPU twins and compares with wgpu's rows (`renderer::frame_record_tests`).

#[cfg(test)]
use super::ffi::{self, Device, Obj, PixelFormat, SamplerDesc};
#[cfg(test)]
use super::pipelines;
#[cfg(test)]
use crate::pipeline_table::Pipeline;

/// Size in bytes of `BlitUniform`.
pub(crate) const UNIFORM_BYTES: usize = 112;

/// Metal present-blit pipeline, nearest sampler, and 112-byte uniform buffer for one destination format.
#[cfg(test)]
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

#[cfg(test)]
impl MetalBlit {
    /// [`UNIFORM_BYTES`], re-exported for the blit's callers.
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

/// Runs a crown row standalone: `HDR_GLOW` (`hdr`, additive into `Rgba16Float`) or `SDR_GLOW`
/// (its screen blend into `Bgra8Unorm`) over a `w` x `h` target seeded with `seed`, drawing
/// `instances` (`[x, y, w, h]` and a colour, packed as `GlowInstance`s) in one scissored `Load`
/// pass with the crown uniform, and returns the target's texels.
#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "the crown fixture states its geometry, boost and headroom explicitly"
)]
pub(crate) fn crown_row_bytes(
    hdr: bool,
    instances: &[([u16; 4], [u8; 4])],
    content_off: [f32; 2],
    boost: f32,
    headroom: f32,
    seed: &[u8],
    scissor: [u32; 4],
    (w, h): (u32, u32),
) -> Result<Vec<u8>, String> {
    use super::encoder::{EncodeSession, RenderPassDesc, StoreAction};
    use super::loss::{CbOutcome, LossLatch};
    use std::sync::Arc;

    /// `hdr_glow.metal`'s uniform block, laid out as `HdrGlowUniform`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CrownU {
        screen: [f32; 2],
        content_off: [f32; 2],
        boost: f32,
        headroom: f32,
        _pad: [f32; 2],
    }

    let _pool = ffi::AutoreleasePool::new();
    let dev = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
    let (row_id, fmt) = if hdr {
        (Pipeline::HdrGlow, PixelFormat::Rgba16Float)
    } else {
        (Pipeline::SdrGlow, PixelFormat::Bgra8Unorm)
    };
    let (w, h) = (w as usize, h as usize);
    let texel = fmt.bytes_per_texel();
    let row = w * texel;
    if seed.len() != row * h {
        return Err(format!(
            "the seed is {} bytes, {w}x{h} {fmt:?} needs {}",
            seed.len(),
            row * h
        ));
    }
    let mut stream_bytes: Vec<u8> = Vec::new();
    for (rect, colour) in instances {
        for v in rect {
            stream_bytes.extend_from_slice(&v.to_le_bytes());
        }
        stream_bytes.extend_from_slice(colour);
        stream_bytes.extend_from_slice(colour);
    }
    if stream_bytes.len() as u64
        != crate::pipeline_table::GLOW_LAYOUT.stride * instances.len() as u64
    {
        return Err("the instances do not pack as tight GlowInstances".to_owned());
    }

    let latch = Arc::new(LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch))?;
    let mint = super::resources::MetalResourceDevice::new(&dev, latch);
    let stream = dev
        .new_buffer(stream_bytes.len())
        .ok_or("the instance stream allocation failed")?;
    // SAFETY: a fresh shared buffer of exactly `stream_bytes.len()` bytes that no command buffer
    // uses yet.
    unsafe { ffi::buffer_write(&stream, &stream_bytes) };
    let spec = row_id.spec();
    let binds = spec.binds;
    let lib = pipelines::compile(&dev, spec)?;
    let pso = pipelines::build(&dev, &lib, spec, PixelFormat::Bgra8Unorm)?;
    let dst = mint.texture_2d(
        fmt,
        w,
        h,
        ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ,
    )?;
    // SAFETY: a fresh non-private `w` x `h` texture of `fmt`; `seed` is `row * h` bytes (checked
    // above) at the tight stride `row`.
    unsafe { ffi::texture_upload(dst.obj(), ffi::MtlRegion::full_2d(w, h), seed, row) };
    #[expect(clippy::cast_precision_loss, reason = "test extents")]
    let cu = CrownU {
        screen: [w as f32, h as f32],
        content_off,
        boost,
        headroom,
        _pad: [0.0; 2],
    };
    let ubuf = dev
        .new_buffer(size_of::<CrownU>())
        .ok_or("the uniform allocation failed")?;
    // SAFETY: `CrownU` is `repr(C)` plain floats, written into a fresh shared buffer of exactly
    // its size that no command buffer uses yet.
    unsafe {
        ffi::buffer_write(
            &ubuf,
            std::slice::from_raw_parts(std::ptr::from_ref(&cu).cast::<u8>(), size_of::<CrownU>()),
        );
    };
    let rb = dev
        .new_buffer(row * h)
        .ok_or("the readback allocation failed")?;
    let mut cb = session.begin()?;
    {
        let pass = cb.render_pass(&RenderPassDesc {
            target: &dst,
            load: ffi::LoadAction::Load,
            store: StoreAction::Store,
            viewport: None,
            scissor: Some(ffi::MtlScissorRect {
                x: scissor[0] as usize,
                y: scissor[1] as usize,
                width: (scissor[2] - scissor[0]) as usize,
                height: (scissor[3] - scissor[1]) as usize,
            }),
        })?;
        pass.set_pipeline(&pso);
        pass.set_vertex_buffer(
            &ubuf,
            binds
                .vertex_uniform
                .ok_or("the crown rows bind a vertex uniform")? as usize,
        )?;
        pass.set_fragment_buffer(&ubuf, binds.fragment_buffers[0] as usize);
        pass.set_instance_stream(&stream);
        pass.draw_instanced(
            pipelines::metal_primitive_type(spec.topology),
            6,
            instances.len(),
        )?;
    }
    cb.copy_texture_to_buffer(&dst, w, h, &rb, row)?;
    if cb.commit().wait_outcome() != CbOutcome::Completed {
        return Err("the crown pass did not complete".to_owned());
    }
    // SAFETY: shared storage of exactly `row * h` bytes whose writing command buffer completed.
    Ok(unsafe { ffi::buffer_bytes(&rb, row * h) })
}
