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
//! first-party backend has no such deriver, so the map is written by hand in
//! exactly two places and they must agree:
//!
//! | resource | WGSL (`renderer.rs::BLIT_SHADER`) | MSL (`shaders/blit.metal`) | bound by |
//! |---|---|---|---|
//! | source texture | `@group(0) @binding(0)` | `[[texture(0)]]` | [`ffi::draw_fullscreen_and_read`] |
//! | sampler | `@group(0) @binding(1)` | `[[sampler(0)]]` | idem |
//! | `Blit` uniform | `@group(0) @binding(2)` | `[[buffer(2)]]` | idem |
//!
//! Nothing checks that table but the differential test. A swapped texture slot
//! samples the wrong atlas and a swapped uniform draws at the wrong size;
//! neither is a compile error. That is the cost the decision document priced,
//! and this is where it is paid.
//!
//! # What this is NOT
//!
//! It is not a renderer. It builds no swapchain, owns no `CAMetalLayer`, and
//! nothing in `renderer.rs` calls it. `GpuRenderer` is still `wgpu` on every
//! cell, and `crates/aterm-gpu/Cargo.toml` still names `wgpu` for macOS.

use super::ffi::{self, Device, Obj, PixelFormat};

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
    /// NEAREST/NEAREST/clamp — `MTLSamplerDescriptor`'s defaults, which are
    /// exactly `build_blit_resources`'s `FilterMode::Nearest` triple. Bound
    /// because the shader declares it; `fs_blit` fetches with `read()`, so it
    /// is never actually sampled through (the same is true under `wgpu`).
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

    /// Compile `shaders/blit.metal` and build every object the pass needs.
    ///
    /// Returns `Err` (never panics) when the process has no Metal device, which
    /// is the same shape the `wgpu` tests already gate on.
    pub(crate) fn new(format: PixelFormat) -> Result<Self, String> {
        let device = Device::system_default().ok_or_else(|| "no Metal device".to_owned())?;
        let queue = device
            .new_command_queue()
            .ok_or_else(|| "MTLCommandQueue allocation failed".to_owned())?;
        let library = device.new_library(super::shaders::BLIT)?;
        let vs = library
            .function("vs_blit")
            .ok_or_else(|| "blit.metal exports no vs_blit".to_owned())?;
        let fs = library
            .function("fs_blit")
            .ok_or_else(|| "blit.metal exports no fs_blit".to_owned())?;

        let desc = ffi::RenderPipelineDescriptor::new()
            .ok_or_else(|| "MTLRenderPipelineDescriptor allocation failed".to_owned())?;
        desc.set_vertex_function(&vs);
        desc.set_fragment_function(&fs);
        // `ensure_blit_pipeline` (renderer.rs:11062) uses `BlendState::REPLACE`,
        // which is (One, Zero) on both channels — i.e. no blending at all. Metal
        // spells that as blending DISABLED, which is the same fixed-function
        // state and not an approximation.
        desc.set_color_attachment(format, None);
        // No vertex descriptor: `vs_blit` reads `[[vertex_id]]` and binds no
        // vertex buffer, matching the WGSL `buffers: &[]`.
        let pipeline = device.new_render_pipeline(&desc)?;

        let sampler = device
            .new_sampler()
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
        let (sw, sh) = (src_w as usize, src_h as usize);
        let (dw, dh) = (dst_w as usize, dst_h as usize);
        if src.len() != sw * sh * 4 {
            return Err(format!(
                "source is {} bytes, expected {} for {src_w}x{src_h} RGBA8",
                src.len(),
                sw * sh * 4
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
            .new_texture_2d(
                PixelFormat::Rgba8Unorm,
                sw,
                sh,
                ffi::TEXTURE_USAGE_SHADER_READ,
            )
            .ok_or_else(|| "blit source texture allocation failed".to_owned())?;
        // SAFETY: `src_tex` was just created 2-D, `Rgba8Unorm` (4 bytes/texel),
        // `sw` x `sh`, with the descriptor's default non-Private storage; the
        // length check above pins `src` at exactly `sw * 4 * sh` bytes.
        unsafe {
            ffi::texture_upload(&src_tex, ffi::MtlRegion::full_2d(sw, sh), src, sw * 4);
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

        let row = dw * 4;
        let readback = self
            .device
            .new_buffer(row * dh)
            .ok_or_else(|| "blit readback buffer allocation failed".to_owned())?;
        ffi::draw_fullscreen_and_read(
            &self.queue,
            &self.pipeline,
            &dst_tex,
            dw,
            dh,
            &src_tex,
            &self.sampler,
            &self.uniform,
            &readback,
            row,
        );
        // SAFETY: `readback` is shared storage of exactly `row * dh` bytes, and
        // `draw_fullscreen_and_read` returned only after `waitUntilCompleted`,
        // so the GPU's writes are visible.
        Ok(unsafe { ffi::buffer_bytes(&readback, row * dh) })
    }
}
