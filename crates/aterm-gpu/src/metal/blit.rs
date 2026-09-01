// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROW's falsifiable gate: the present-path blit, first-party on Metal.
//!
//! `wgpu-metal-decision-2026-08-30.md` §8 names ONE experiment that decides
//! whether the macOS cell may drop `wgpu`:
//!
//! > Port `build_blit_resources` (`renderer.rs:4833`) plus `BLIT_SHADER`
//! > (`renderer.rs:759`) to Metal behind a new internal seam, and run the
//! > byte-identical blit assertions against it. If the byte-identical
//! > assertions cannot be held green, REFUSE.
//!
//! This module is the Metal half of that experiment, and
//! [`super::tests::blit_matches_wgpu_byte_for_byte`] is the differential that
//! judges it: the SAME source pixels and the SAME 96 uniform bytes are pushed
//! through the shipped `wgpu` blit and through this one, and every byte of both
//! outputs must agree. It is a strictly stronger gate than `blit_invert.rs` /
//! `blit_bands.rs` on their own, because those two assert properties of one
//! backend's output (`out == src`, `out == 255 - src`) whereas this one pins
//! this backend to the shipped one on ARBITRARY pixels, where a gamma shift, a
//! swapped channel, a filtered sample or an off-by-half-texel has nowhere to
//! hide.
//!
//! # The binding map, which is the whole risk
//!
//! WGSL's `@group(0) @binding(n)` has no Metal equivalent. Under `wgpu`,
//! `wgpu-core` derives the per-stage `[[texture(n)]]/[[sampler(n)]]/[[buffer(n)]]`
//! map from the `BindGroupLayout` and naga's MSL writer honours it. A
//! first-party backend has no such deriver — so the map is DECLARED once, as
//! the `BindSpec` column of THE PIPELINE TABLE
//! ([`crate::pipeline_table::BindSpec`]), and this module binds by READING
//! `Pipeline::Blit.spec().binds` rather than spelling indices of its own.
//! The prose table that used to sit here (texture 0 / sampler 0 / buffer 2,
//! maintained by hand against `blit.metal`) is retired: the MSL scan in
//! `super::shaders` now holds the column equal to the `.metal` declarations
//! in both directions, and the differential test still pins the bytes end to
//! end — a swapped slot is a scan failure AND a byte diff instead of a silent
//! wrong sample.
//!
//! # What this is NOT
//!
//! It is not a renderer. It builds no swapchain, owns no `CAMetalLayer`, and
//! nothing in `renderer.rs` calls it. `GpuRenderer` is still `wgpu` on every
//! cell, and `crates/aterm-gpu/Cargo.toml` still names `wgpu` for macOS.

use super::ffi::{self, Device, Obj, PixelFormat, SamplerDesc};
use super::pipelines;
use crate::pipeline_table::Pipeline;

/// The first-party Metal twin of `renderer.rs::build_blit_resources` — the
/// shader library, the pipeline state for one destination format, the NEAREST
/// sampler and the 96-byte uniform buffer.
///
/// `build_blit_resources` returns a bind-group layout and a pipeline layout as
/// well. Both are `wgpu` bookkeeping with no Metal counterpart: Metal binds
/// resources positionally at encode time, so the layout objects collapse into
/// the binding table in this module's header.
pub(crate) struct MetalBlit {
    device: Device,
    queue: Obj,
    /// `MTLRenderPipelineState` for `vs_blit` + `fs_blit` at one destination
    /// format. `renderer.rs` caches one pipeline per format in
    /// `blit_pipelines`; this gate needs the single format the readable
    /// swapchain stand-in uses.
    pipeline: Obj,
    /// NEAREST/NEAREST/clamp — [`SamplerDesc::NEAREST_CLAMP`], which is exactly
    /// `build_blit_resources`'s `FilterMode::Nearest` triple. Bound because the
    /// shader declares it; `fs_blit` fetches with `read()`, so it is never
    /// actually sampled through (the same is true under `wgpu`).
    sampler: Obj,
    /// The shared 96-byte `BlitUniform` buffer, written per present.
    uniform: Obj,
    format: PixelFormat,
}

impl MetalBlit {
    /// Bytes per `BlitUniform`. Asserted against the Rust struct at the one
    /// call site that fills it, so a field added to `BlitUniform` without a
    /// matching `Blit` member in `blit.metal` fails loudly instead of reading
    /// past the end of the buffer.
    pub(crate) const UNIFORM_BYTES: usize = 96;

    /// The offscreen frame's format. Named, and its texel size taken from
    /// [`PixelFormat::bytes_per_texel`], so the upload stride below cannot drift
    /// from the texture it uploads into — the `* 4` it replaces was one of two
    /// hardcoded texel sizes in this file, and the other one was a real
    /// GPU-side overrun on an `Rgba16Float` destination.
    const SRC_FORMAT: PixelFormat = PixelFormat::Rgba8Unorm;

    /// Compile `shaders/blit.metal` and build every object the pass needs.
    ///
    /// Returns `Err` (never panics) when the process has no Metal device, which
    /// is the same shape the `wgpu` tests already gate on.
    pub(crate) fn new(format: PixelFormat) -> Result<Self, String> {
        let device = Device::system_default().ok_or_else(|| "no Metal device".to_owned())?;
        let queue = device
            .new_command_queue()
            .ok_or_else(|| "MTLCommandQueue allocation failed".to_owned())?;
        // THE PIPELINE STATE IS NOT SPELLED HERE. `Pipeline::Blit` is the row
        // `renderer.rs::ensure_blit_pipeline` builds its `wgpu` pipeline from,
        // and `pipelines::build` maps that same row onto Metal — entry points,
        // `BlendState::REPLACE` (which Metal spells as blending DISABLED: the
        // same fixed-function state, not an approximation), the `ColorWrites::ALL`
        // mask the blit needs because it is what PUBLISHES the alpha the ten
        // RGB-only pipelines were careful not to disturb, and no vertex
        // descriptor at all because `vs_blit` reads `[[vertex_id]]`.
        //
        // This used to be a hand-written descriptor citing `renderer.rs:11062`
        // and `:11080` by line number. Line numbers are not a coupling.
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

    /// Run the real `vs_blit` + `fs_blit` over `src` and read the destination
    /// back as tightly packed RGBA8.
    ///
    /// `src` is `src_w * src_h` RGBA8 texels — the offscreen frame the present
    /// path blits. `uniform` is the 96 bytes of a `BlitUniform`, produced by
    /// the SAME `present_blit_uniform` the `wgpu` path uses, so this function
    /// makes no policy decision of its own.
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
        // SAFETY: `src_tex` was just created 2-D, `SRC_FORMAT`, `sw` x `sh`,
        // with the descriptor's default non-Private storage; `src_row` is that
        // format's own bytes-per-texel times `sw`, and the length check above
        // pins `src` at exactly `src_row * sh` bytes.
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

        // SAFETY: `self.uniform` is a shared-storage buffer of exactly
        // `UNIFORM_BYTES`, checked above, and no command buffer is in flight on
        // it — `run` is `&self` on a non-`Sync` type and the previous call's
        // command buffer was waited on before it returned.
        unsafe { ffi::buffer_write(&self.uniform, uniform) };

        // `self.format`'s OWN bytes-per-texel, not a hardcoded 4. An
        // `Rgba16Float` destination is 8 bytes per texel, and the previous
        // `dw * 4` sized both the buffer and `destinationBytesPerRow` at half
        // what the copy needs — which Metal accepts silently with its
        // validation layer off (status 4, error nil) and only rejects under
        // `MTL_DEBUG_LAYER=1`: "destinationBytesPerRow(32) must be >= (64)".
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
        // THE BINDING MAP IS READ, NOT SPELLED: the row's `BindSpec` column
        // gives every index (see the module header).
        let binds = Pipeline::Blit.spec().binds;
        ffi::draw_and_read(
            &self.queue,
            &ffi::Pass {
                pso: &self.pipeline,
                dst: &dst_tex,
                dst_w: dw,
                dst_h: dh,
                // `wgpu::LoadOp::Clear(wgpu::Color::BLACK)` is opaque black; the
                // pass writes every pixel, so this only decides what an aborted
                // pass would show.
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
        // `draw_and_read` returned only after `waitUntilCompleted`,
        // so the GPU's writes are visible.
        Ok(unsafe { ffi::buffer_bytes(&readback, readback_len) })
    }
}
