// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Multi-backend GPU resource and frame-encoding layer: [`DeviceHandle`] ([`DeviceHandle::Metal`]
//! over [`crate::metal::resources::MetalResourceDevice`], `crate::gl::device::GlDevice`,
//! `crate::d3d11::device::D3dDevice`), [`LayerTexture`] ([`LayerTexture::metal`]),
//! [`LayerBuffer`], [`LayerSampler`],
//! [`FrameEncoder`] ([`FrameEncoder::Metal`]), [`FramePass`], [`TexelFormat`], [`ClearColor4`],
//! [`FrameLoad`], [`metal_try_read_back`], and [`metal_stage_readback`].

#[cfg(d3d11_arm)]
use crate::d3d11::{
    device::D3dDevice,
    encoder::{D3dFrame, D3dPass},
    pipelines::D3dPipeline,
    resources::{D3dBuffer, D3dBufferRole, D3dSampler, D3dTexture},
};
#[cfg(gl_arm)]
use crate::gl::{
    device::GlDevice,
    encoder::{GlFrame, GlPass},
    pipelines::GlPipeline,
    resources::{GlBuffer, GlBufferRole, GlSampler, GlTexture},
};
#[cfg(target_os = "macos")]
use crate::metal::ffi as mtl;
#[cfg(target_os = "macos")]
use crate::metal::resources::{MetalResourceDevice, SealedTexture, SharedBuffer};

/// The Metal arm's 2-D texture dimension ceiling.
#[cfg(target_os = "macos")]
pub(crate) const METAL_MAX_TEXTURE_DIM: u32 = 16384;

/// The six texel formats the Metal foundation models (`metal::ffi::PixelFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TexelFormat {
    R8Unorm,
    Rgba8Unorm,
    /// The sRGB alias of `Rgba8Unorm`.
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Rgba16Float,
}

impl TexelFormat {
    /// Bytes one texel occupies in a linear (buffer/upload) layout.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) const fn bytes_per_texel(self) -> u32 {
        match self {
            Self::R8Unorm => 1,
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm => 4,
            Self::Rgba16Float => 8,
        }
    }

    /// The Metal spelling.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) const fn metal(self) -> mtl::PixelFormat {
        match self {
            Self::R8Unorm => mtl::PixelFormat::R8Unorm,
            Self::Rgba8Unorm => mtl::PixelFormat::Rgba8Unorm,
            Self::Rgba8UnormSrgb => mtl::PixelFormat::Rgba8UnormSrgb,
            Self::Bgra8Unorm => mtl::PixelFormat::Bgra8Unorm,
            Self::Rgba16Float => mtl::PixelFormat::Rgba16Float,
        }
    }
}

/// What a texture is for, backend-neutrally.
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(
    any(gl_arm, d3d11_arm),
    expect(
        dead_code,
        reason = "GL and D3D11 textures declare no usage (every GLES texture, and every \
                  D3D11 texture the arm makes, can be sampled, drawn and copied); the Metal \
                  arm reads it"
    )
)]
pub(crate) struct TexUsage {
    /// Metal `ShaderRead`.
    pub(crate) sampled: bool,
    /// Metal `RenderTarget`.
    pub(crate) render: bool,
    /// Copied from (implicit in Metal's non-framebuffer-only storage).
    pub(crate) copy_src: bool,
    /// Copied or uploaded into (implicit in Metal's non-framebuffer-only storage).
    pub(crate) copy_dst: bool,
}

impl TexUsage {
    /// The upload-then-sample shape every atlas/sprite/overlay texture uses.
    pub(crate) const UPLOADED_SAMPLED: Self = Self {
        sampled: true,
        render: false,
        copy_src: false,
        copy_dst: true,
    };

    /// The offscreen family: drawn into, blitted from, scroll-shifted.
    #[cfg_attr(
        any(gl_arm, d3d11_arm),
        expect(dead_code, reason = "the Metal arm's texture mints")
    )]
    pub(crate) const OFFSCREEN: Self = Self {
        sampled: true,
        render: true,
        copy_src: true,
        copy_dst: true,
    };

    /// Copy-only staging (`COPY_SRC | COPY_DST`).
    pub(crate) const COPY_STAGING: Self = Self {
        sampled: false,
        render: false,
        copy_src: true,
        copy_dst: true,
    };

    #[inline]
    #[cfg(target_os = "macos")]
    const fn metal(self) -> usize {
        let _ = (self.copy_src, self.copy_dst);
        let mut u = 0;
        if self.sampled {
            u |= mtl::TEXTURE_USAGE_SHADER_READ;
        }
        if self.render {
            u |= mtl::TEXTURE_USAGE_RENDER_TARGET;
        }
        u
    }
}

/// The two sampler presets (nearest and linear, clamp to edge, never mipped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SamplerKind {
    /// Exact-texel reads (glyph/deco/sprite atlases, the blit).
    NearestClamp,
    /// Smooth resampling (bloom upsample, shimmer, tray card).
    LinearClamp,
}

/// One backend, borrowed: the handle a construction site creates resources from.
#[derive(Clone, Copy)]
pub(crate) enum DeviceHandle<'a> {
    #[cfg(target_os = "macos")]
    Metal(&'a MetalResourceDevice),
    #[cfg(gl_arm)]
    Gl(&'a GlDevice),
    #[cfg(d3d11_arm)]
    D3d11(&'a D3dDevice),
}

impl DeviceHandle<'_> {
    /// Create a 2-D texture.
    pub(crate) fn try_create_texture_2d(
        &self,
        label: &'static str,
        format: TexelFormat,
        width: u32,
        height: u32,
        usage: TexUsage,
        alias: Option<TexelFormat>,
    ) -> Result<LayerTexture, String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                let _ = (label, alias);
                mint.texture_2d(
                    format.metal(),
                    width as usize,
                    height as usize,
                    usage.metal(),
                )
                .map(LayerTexture::Metal)
            }
            #[cfg(gl_arm)]
            Self::Gl(dev) => {
                let _ = (label, usage);
                dev.texture_2d(format, width, height, alias)
                    .map(LayerTexture::Gl)
            }
            #[cfg(d3d11_arm)]
            Self::D3d11(dev) => {
                // Every D3D11 texture the arm makes is bound as a shader
                // resource and a render target, and any can be copied.
                let _ = (label, usage);
                dev.texture_2d(format, width, height, alias)
                    .map(LayerTexture::D3d11)
            }
        }
    }
}

/// The uploads that panic on a refused write, for callers with no error path.
/// Every D3D11 upload takes [`DeviceHandle::try_upload_texture_region`]
/// instead, since a removed device refuses writes under a frame.
#[cfg_attr(
    d3d11_arm,
    expect(
        dead_code,
        reason = "the shipped Windows build's uploads are all the D3D11 arm's, which take \
                  `try_upload_texture_region`"
    )
)]
impl DeviceHandle<'_> {
    /// Full-extent upload of level 0 (`queue.write_texture` / `replaceRegion:`).
    #[inline]
    pub(crate) fn upload_texture_full(
        &self,
        tex: &LayerTexture,
        bytes: &[u8],
        bytes_per_row: u32,
        width: u32,
        height: u32,
    ) {
        self.upload_texture_rows(tex, 0, height, bytes, bytes_per_row, width);
    }

    /// Row-ranged upload of rows `[y0, y0 + rows)` at full width, for the atlas append.
    #[inline]
    pub(crate) fn upload_texture_rows(
        &self,
        tex: &LayerTexture,
        y0: u32,
        rows: u32,
        bytes: &[u8],
        bytes_per_row: u32,
        width: u32,
    ) {
        self.upload_texture_region(tex, 0, y0, width, rows, bytes, bytes_per_row);
    }

    /// Upload the `w × h` region at `(x, y)` of level 0 from `bytes`, rows `bytes_per_row` apart.
    ///
    /// # Panics
    ///
    /// Where [`Self::try_upload_texture_region`] answers `Err`, and on Metal
    /// where the region leaves the texture or `bytes` is short of it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload_texture_region(
        &self,
        tex: &LayerTexture,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        bytes: &[u8],
        bytes_per_row: u32,
    ) {
        if let Err(e) = self.try_upload_texture_region(tex, x, y, w, h, bytes, bytes_per_row) {
            panic!("device layer: {e}");
        }
    }
}

impl DeviceHandle<'_> {
    /// [`Self::upload_texture_region`], answering `Err` where the GL or D3D11
    /// device refuses the write (a region outside the texture, or a removed
    /// D3D11 device), so a caller on a device that can be lost under it
    /// fails the frame instead of the process. Metal answers `Ok`.
    ///
    /// # Panics
    ///
    /// On Metal, where the region leaves the texture or `bytes` is short of
    /// it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_upload_texture_region(
        &self,
        tex: &LayerTexture,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        bytes: &[u8],
        bytes_per_row: u32,
    ) -> Result<(), String> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        match (self, tex) {
            #[cfg(target_os = "macos")]
            (Self::Metal(_), LayerTexture::Metal(texture)) => {
                let region = mtl::MtlRegion {
                    origin: mtl::MtlOrigin {
                        x: x as usize,
                        y: y as usize,
                        z: 0,
                    },
                    size: mtl::MtlSize {
                        width: w as usize,
                        height: h as usize,
                        depth: 1,
                    },
                };
                assert!(
                    bytes.len() >= bytes_per_row as usize * h as usize,
                    "device layer: region upload of {w}x{h} rows needs \
                     {bytes_per_row}*{h} bytes, got {}",
                    bytes.len()
                );
                assert!(
                    (x + w) as usize <= texture.width() && (y + h) as usize <= texture.height(),
                    "device layer: region upload of {w}x{h} at ({x}, {y}) leaves a {}x{} \
                     texture",
                    texture.width(),
                    texture.height()
                );
                // SAFETY: a `LayerTexture::Metal` comes only from this layer's mint, which creates
                // Managed 2-D textures, so `replaceRegion:` is legal; the region lies in the
                // texture and the band length is asserted above.
                unsafe { texture.upload(region, bytes, bytes_per_row as usize) };
                Ok(())
            }
            #[cfg(gl_arm)]
            (Self::Gl(dev), LayerTexture::Gl(texture)) => {
                dev.upload_region(texture, x, y, w, h, bytes, bytes_per_row)
            }
            #[cfg(d3d11_arm)]
            (Self::D3d11(dev), LayerTexture::D3d11(texture)) => {
                dev.upload_region(texture, x, y, w, h, bytes, bytes_per_row)
            }
        }
    }

    /// A per-frame instance-stream buffer.
    #[inline]
    #[cfg_attr(
        any(gl_arm, d3d11_arm),
        expect(
            dead_code,
            reason = "the GL and D3D11 arms mint their buffers on their devices \
                      (`GlDevice::buffer`, `D3dDevice::buffer`); the Metal arm uses the \
                      layer's"
        )
    )]
    pub(crate) fn try_create_instance_buffer(
        &self,
        label: &'static str,
        size: u64,
    ) -> Result<LayerBuffer, String> {
        self.try_create_buffer(label, size, BufferRole::Instance)
    }

    /// A uniform buffer (a shared-storage `MTLBuffer` on Metal).
    #[inline]
    #[cfg_attr(
        any(gl_arm, d3d11_arm),
        expect(
            dead_code,
            reason = "the GL and D3D11 arms mint their buffers on their devices \
                      (`GlDevice::buffer`, `D3dDevice::buffer`); the Metal arm uses the \
                      layer's"
        )
    )]
    pub(crate) fn try_create_uniform_buffer(
        &self,
        label: &'static str,
        size: u64,
    ) -> Result<LayerBuffer, String> {
        self.try_create_buffer(label, size, BufferRole::Uniform)
    }

    /// A readback staging buffer a texture is copied into for the CPU to read.
    #[inline]
    #[cfg_attr(
        any(gl_arm, d3d11_arm),
        expect(
            dead_code,
            reason = "the GL and D3D11 arms mint their buffers on their devices \
                      (`GlDevice::buffer`, `D3dDevice::buffer`); the Metal arm uses the \
                      layer's"
        )
    )]
    pub(crate) fn try_create_readback_buffer(
        &self,
        label: &'static str,
        size: u64,
    ) -> Result<LayerBuffer, String> {
        self.try_create_buffer(label, size, BufferRole::Readback)
    }

    /// The one buffer constructor behind the three roles.
    fn try_create_buffer(
        &self,
        label: &'static str,
        size: u64,
        role: BufferRole,
    ) -> Result<LayerBuffer, String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                let _ = (label, role);
                mint.buffer(usize::try_from(size).unwrap_or(usize::MAX).max(1))
                    .map(|b| LayerBuffer::Metal(SharedBuffer::new(b)))
            }
            #[cfg(gl_arm)]
            Self::Gl(dev) => {
                let _ = label;
                let role = match role {
                    BufferRole::Instance => GlBufferRole::Instance,
                    BufferRole::Uniform => GlBufferRole::Uniform,
                    BufferRole::Readback => GlBufferRole::Readback,
                };
                dev.buffer(role, usize::try_from(size).unwrap_or(usize::MAX))
                    .map(LayerBuffer::Gl)
            }
            #[cfg(d3d11_arm)]
            Self::D3d11(dev) => {
                let _ = label;
                let role = match role {
                    BufferRole::Instance => D3dBufferRole::Instance,
                    BufferRole::Uniform => D3dBufferRole::Uniform,
                    BufferRole::Readback => {
                        return Err("d3d11: a texture reads back through a staging texture \
                                    (`D3dDevice::read_rgba8`), never a buffer"
                            .to_owned());
                    }
                };
                dev.buffer(role, usize::try_from(size).unwrap_or(usize::MAX))
                    .map(LayerBuffer::D3d11)
            }
        }
    }

    /// Write `bytes` into `buf` from offset 0, the only write shape the renderer uses.
    ///
    /// # Safety
    /// No in-flight GPU work may alias `buf` (Metal).
    #[cfg_attr(
        any(gl_arm, d3d11_arm),
        expect(
            dead_code,
            reason = "the GL and D3D11 arms mint their buffers on their devices \
                      (`GlDevice::buffer`, `D3dDevice::buffer`); the Metal arm uses the \
                      layer's"
        )
    )]
    pub(crate) unsafe fn write_buffer_from_zero(&self, buf: &LayerBuffer, bytes: &[u8]) {
        match (self, buf) {
            #[cfg(target_os = "macos")]
            (Self::Metal(_), LayerBuffer::Metal(b)) => {
                let obj = b.obj();
                assert!(
                    bytes.len() <= mtl::buffer_length(obj),
                    "device layer: write of {} bytes into a {}-byte MTLBuffer",
                    bytes.len(),
                    mtl::buffer_length(obj)
                );
                // SAFETY: length asserted above; exclusivity is the caller's
                // documented precondition.
                unsafe { mtl::buffer_write(obj, bytes) }
            }
            #[cfg(gl_arm)]
            (Self::Gl(dev), LayerBuffer::Gl(b)) => {
                if let Err(e) = dev.write_buffer(b, bytes) {
                    panic!("device layer: {e}");
                }
            }
            #[cfg(d3d11_arm)]
            (Self::D3d11(dev), LayerBuffer::D3d11(b)) => {
                if let Err(e) = dev.write_buffer(b, bytes) {
                    panic!("device layer: {e}");
                }
            }
        }
    }

    /// One of the two clamp samplers.
    pub(crate) fn try_create_sampler(
        &self,
        label: &'static str,
        kind: SamplerKind,
    ) -> Result<LayerSampler, String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                let _ = label;
                let desc = match kind {
                    SamplerKind::NearestClamp => mtl::SamplerDesc::NEAREST_CLAMP,
                    SamplerKind::LinearClamp => mtl::SamplerDesc::LINEAR_CLAMP,
                };
                mint.sampler(desc).map(LayerSampler::Metal)
            }
            #[cfg(gl_arm)]
            Self::Gl(dev) => {
                let _ = label;
                dev.sampler(kind).map(LayerSampler::Gl)
            }
            #[cfg(d3d11_arm)]
            Self::D3d11(dev) => {
                let _ = label;
                dev.sampler(kind).map(LayerSampler::D3d11)
            }
        }
    }
}

/// What a buffer is for: the GL and D3D11 arms bind it by role.
#[derive(Clone, Copy, Debug)]
enum BufferRole {
    Instance,
    Uniform,
    Readback,
}

/// A texture on whichever backend created it.
#[derive(Debug)]
pub(crate) enum LayerTexture {
    #[cfg(target_os = "macos")]
    Metal(SealedTexture),
    #[cfg(gl_arm)]
    Gl(GlTexture),
    #[cfg(d3d11_arm)]
    D3d11(D3dTexture),
}

impl LayerTexture {
    /// The D3D11 texture.
    #[inline]
    #[cfg(d3d11_arm)]
    pub(crate) fn d3d11(&self) -> &D3dTexture {
        match self {
            Self::D3d11(t) => t,
        }
    }

    /// [`Self::d3d11`], by value.
    #[inline]
    #[cfg(d3d11_arm)]
    pub(crate) fn into_d3d11(self) -> D3dTexture {
        match self {
            Self::D3d11(t) => t,
        }
    }

    /// The GL texture.
    #[inline]
    #[cfg(gl_arm)]
    pub(crate) fn gl(&self) -> &GlTexture {
        match self {
            Self::Gl(t) => t,
        }
    }

    /// [`Self::gl`], by value.
    #[inline]
    #[cfg(gl_arm)]
    pub(crate) fn into_gl(self) -> GlTexture {
        match self {
            Self::Gl(t) => t,
        }
    }

    /// The sealed Metal texture, for binding.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &SealedTexture {
        match self {
            Self::Metal(t) => t,
        }
    }

    /// [`Self::metal`], by value.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn into_metal(self) -> SealedTexture {
        match self {
            Self::Metal(t) => t,
        }
    }
}

/// A buffer on whichever backend created it.
#[derive(Debug)]
pub(crate) enum LayerBuffer {
    #[cfg(target_os = "macos")]
    Metal(SharedBuffer),
    #[cfg(gl_arm)]
    Gl(GlBuffer),
    #[cfg(d3d11_arm)]
    D3d11(D3dBuffer),
}

impl LayerBuffer {
    /// The Metal buffer object, for binding.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &mtl::Obj {
        match self {
            Self::Metal(b) => b.obj(),
        }
    }

    /// The shared Metal buffer, by value.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn into_metal(self) -> SharedBuffer {
        match self {
            Self::Metal(b) => b,
        }
    }
}

/// A sampler on whichever backend created it.
#[derive(Debug)]
pub(crate) enum LayerSampler {
    #[cfg(target_os = "macos")]
    Metal(mtl::Obj),
    #[cfg(gl_arm)]
    Gl(GlSampler),
    #[cfg(d3d11_arm)]
    D3d11(D3dSampler),
}

impl LayerSampler {
    /// The D3D11 sampler, by value.
    #[inline]
    #[cfg(d3d11_arm)]
    pub(crate) fn into_d3d11(self) -> D3dSampler {
        match self {
            Self::D3d11(s) => s,
        }
    }

    /// The live GL sampler, by value.
    #[inline]
    #[cfg(gl_arm)]
    pub(crate) fn into_gl(self) -> GlSampler {
        match self {
            Self::Gl(s) => s,
        }
    }

    /// The `MTLSamplerState`, by value.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn into_metal(self) -> mtl::Obj {
        match self {
            Self::Metal(s) => s,
        }
    }
}

#[cfg(target_os = "macos")]
use crate::metal::encoder::{
    CommandBuffer as MtlCommandBuffer, EncodeSession, PassEncoder as MtlPassEncoder,
    RenderPassDesc as MtlRenderPassDesc, StoreAction, Submitted as MtlSubmitted,
};
#[cfg(target_os = "macos")]
use crate::metal::ffi::LoadAction;

/// A neutral f64 colour quadruple with `wgpu::Color`'s shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClearColor4 {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
    pub(crate) a: f64,
}

/// What happens to a pass target's texels before the first draw.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FrameLoad {
    /// Clear to this colour.
    Clear(ClearColor4),
    /// Keep the texels the previous pass/frame stored.
    Load,
}

/// A pass target view: the offscreen's Unorm or sRGB alias view.
#[derive(Clone, Copy)]
pub(crate) enum FrameView<'a> {
    #[cfg(target_os = "macos")]
    Metal(&'a SealedTexture),
    /// The texture, and which of its views the pass draws through (see `GlTexture::views`).
    #[cfg(gl_arm)]
    Gl { tex: &'a GlTexture, srgb: bool },
    /// The texture, and which of its views the pass draws through (see
    /// `D3dTexture::views`).
    #[cfg(d3d11_arm)]
    D3d11 { tex: &'a D3dTexture, srgb: bool },
}

/// A render pipeline on one backend.
#[derive(Clone, Copy)]
pub(crate) enum FramePipeline<'a> {
    #[cfg(target_os = "macos")]
    Metal(&'a mtl::Obj),
    #[cfg(gl_arm)]
    Gl(&'a GlPipeline),
    #[cfg(d3d11_arm)]
    D3d11(&'a D3dPipeline),
}

/// An atlas bind: an atlas texture and its sampler.
#[derive(Clone, Copy)]
pub(crate) enum FrameAtlas<'a> {
    #[cfg(target_os = "macos")]
    Metal {
        tex: &'a mtl::Obj,
        sampler: &'a mtl::Obj,
        /// The row's fragment `[[texture(n)]]` slot (`BindSpec::fragment_textures`).
        tex_slot: u32,
        /// The row's fragment `[[sampler(n)]]` slot (`BindSpec::fragment_samplers`).
        sampler_slot: u32,
    },
    /// The texture and its sampler object at one texture unit.
    #[cfg(gl_arm)]
    Gl {
        tex: &'a GlTexture,
        sampler: &'a GlSampler,
        unit: u32,
    },
    /// The fx bind (`BindSpec::FX`).
    #[cfg(target_os = "macos")]
    MetalFx {
        r8: &'a mtl::Obj,
        rgba: &'a mtl::Obj,
        palette: &'a mtl::Obj,
        tex_slots: [u32; 2],
        palette_slot: u32,
    },
    /// The fx bind on GL: the two atlases and this frame's palette texture at units `units`.
    #[cfg(gl_arm)]
    GlFx {
        r8: &'a GlTexture,
        rgba: &'a GlTexture,
        palette: &'a GlTexture,
        sampler: &'a GlSampler,
        units: [u32; 3],
    },
    /// The texture's shader-resource view and its sampler at one slot (the
    /// row's `BindSpec::fragment_textures` slot: `t<n>` with `s<n>`).
    #[cfg(d3d11_arm)]
    D3d11 {
        tex: &'a D3dTexture,
        sampler: &'a D3dSampler,
        unit: u32,
    },
    /// The fx bind on D3D11: the two atlases and this frame's palette texture
    /// at slots `units` (`t0`, `t1`, `t2`), read by `Load` (the sampler binds
    /// beside them, as on GL, and is never consulted).
    #[cfg(d3d11_arm)]
    D3d11Fx {
        r8: &'a D3dTexture,
        rgba: &'a D3dTexture,
        palette: &'a D3dTexture,
        sampler: &'a D3dSampler,
        units: [u32; 3],
    },
}

/// A bound instance stream.
#[derive(Clone, Copy)]
pub(crate) enum FrameStream<'a> {
    #[cfg(target_os = "macos")]
    Metal(&'a mtl::Obj),
    #[cfg(gl_arm)]
    Gl(&'a GlBuffer),
    #[cfg(d3d11_arm)]
    D3d11(&'a D3dBuffer),
}

/// The shared 16-byte frame uniforms.
#[derive(Clone, Copy)]
pub(crate) enum FrameUniforms<'a> {
    #[cfg(target_os = "macos")]
    Metal {
        buf: &'a mtl::Obj,
        /// The vertex-stage `[[buffer(n)]]` slot (`BindSpec::vertex_uniform`).
        vertex_slot: u32,
        /// The fragment-stage `[[buffer(n)]]` slot.
        fragment_slot: Option<u32>,
    },
    /// The block's buffer at its binding point.
    #[cfg(gl_arm)]
    Gl { buf: &'a GlBuffer, binding: u32 },
    /// The constant buffer at its register (the row's
    /// `BindSpec::vertex_uniform`), bound to both stages.
    #[cfg(d3d11_arm)]
    D3d11 { buf: &'a D3dBuffer, binding: u32 },
}

/// A texture for the mid-frame copy verbs (scroll shift).
#[derive(Clone, Copy)]
pub(crate) enum FrameCopyTexture<'a> {
    #[cfg(target_os = "macos")]
    Metal(&'a SealedTexture),
    #[cfg(gl_arm)]
    Gl(&'a GlTexture),
    #[cfg(d3d11_arm)]
    D3d11(&'a D3dTexture),
}

/// The committed frame.
#[must_use = "an unsubmitted or unobserved frame commit hides device loss"]
pub(crate) enum SubmittedFrame {
    /// The Metal `Submitted`: wait on it, or poll `try_outcome`.
    #[cfg(target_os = "macos")]
    Metal(MtlSubmitted),
    /// GL ran the commands as they were issued.
    #[cfg(gl_arm)]
    Gl(Result<(), String>),
    /// The immediate context flushed the frame's commands; this is whether
    /// the device was still there.
    #[cfg(d3d11_arm)]
    D3d11(Result<(), String>),
}

/// One frame's command encoder on one backend.
pub(crate) enum FrameEncoder<'s> {
    #[cfg(target_os = "macos")]
    Metal(MtlCommandBuffer<'s>),
    #[cfg(gl_arm)]
    Gl(GlFrame<'s>),
    #[cfg(d3d11_arm)]
    D3d11(D3dFrame<'s>),
}

impl<'s> FrameEncoder<'s> {
    /// The GL arm: the device's context, current for the frame.
    #[cfg(gl_arm)]
    pub(crate) fn gl(dev: &'s GlDevice) -> Result<Self, String> {
        Ok(Self::Gl(GlFrame::begin(dev)?))
    }

    /// The D3D11 arm: the device's immediate context, held for the frame.
    #[cfg(d3d11_arm)]
    pub(crate) fn d3d11(dev: &'s D3dDevice) -> Result<Self, String> {
        Ok(Self::D3d11(D3dFrame::begin(dev)?))
    }

    /// The Metal arm: one command buffer on the session's one queue.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(session: &'s EncodeSession) -> Result<Self, String> {
        Ok(Self::Metal(session.begin()?))
    }

    /// Open one render pass: attach `view` with `load`/Store, apply the optional scissor.
    pub(crate) fn begin_pass<'e>(
        &'e mut self,
        label: &'static str,
        view: FrameView<'_>,
        load: FrameLoad,
        scissor: Option<(u32, u32, u32, u32)>,
        uniforms: FrameUniforms<'_>,
    ) -> Result<FramePass<'e, 's>, String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(cb) => {
                let _ = label;
                let (target, buf, vertex_slot, fragment_slot) = match (view, uniforms) {
                    (
                        FrameView::Metal(target),
                        FrameUniforms::Metal {
                            buf,
                            vertex_slot,
                            fragment_slot,
                        },
                    ) => (target, buf, vertex_slot, fragment_slot),
                };
                let load = match load {
                    FrameLoad::Clear(c) => LoadAction::Clear(mtl::ClearColor {
                        r: c.r,
                        g: c.g,
                        b: c.b,
                        a: c.a,
                    }),
                    FrameLoad::Load => LoadAction::Load,
                };
                let pass = cb.render_pass(&MtlRenderPassDesc {
                    target,
                    load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: scissor.map(|(sx, sy, sw, sh)| mtl::MtlScissorRect {
                        x: sx as usize,
                        y: sy as usize,
                        width: sw as usize,
                        height: sh as usize,
                    }),
                })?;
                pass.set_vertex_buffer(buf, vertex_slot as usize)?;
                if let Some(fb) = fragment_slot {
                    pass.set_fragment_buffer(buf, fb as usize);
                }
                Ok(FramePass::Metal(pass))
            }
            #[cfg(gl_arm)]
            Self::Gl(frame) => {
                let _ = label;
                let ((tex, srgb), (buf, binding)) = match (view, uniforms) {
                    (FrameView::Gl { tex, srgb }, FrameUniforms::Gl { buf, binding }) => {
                        ((tex, srgb), (buf, binding))
                    }
                };
                Ok(FramePass::Gl(frame.begin_pass(
                    tex,
                    srgb,
                    load,
                    scissor,
                    (buf, binding),
                )?))
            }
            #[cfg(d3d11_arm)]
            Self::D3d11(frame) => {
                let _ = label;
                let ((tex, srgb), (buf, binding)) = match (view, uniforms) {
                    (FrameView::D3d11 { tex, srgb }, FrameUniforms::D3d11 { buf, binding }) => {
                        ((tex, srgb), (buf, binding))
                    }
                };
                Ok(FramePass::D3d11(frame.begin_pass(
                    tex,
                    srgb,
                    load,
                    scissor,
                    (buf, binding),
                )?))
            }
        }
    }

    /// Texture-to-texture sub-rect copy (the scroll shift's staged moves).
    pub(crate) fn copy_texture_rect(
        &mut self,
        src: FrameCopyTexture<'_>,
        src_origin: (u32, u32),
        dst: FrameCopyTexture<'_>,
        dst_origin: (u32, u32),
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(cb) => {
                let (src, dst) = match (src, dst) {
                    (FrameCopyTexture::Metal(src), FrameCopyTexture::Metal(dst)) => (src, dst),
                };
                cb.copy_texture_sub_rect(
                    src,
                    (src_origin.0 as usize, src_origin.1 as usize),
                    dst,
                    (dst_origin.0 as usize, dst_origin.1 as usize),
                    width as usize,
                    height as usize,
                )
            }
            #[cfg(gl_arm)]
            Self::Gl(frame) => {
                let (src, dst) = match (src, dst) {
                    (FrameCopyTexture::Gl(src), FrameCopyTexture::Gl(dst)) => (src, dst),
                };
                frame.copy_rect(src, src_origin, dst, dst_origin, width, height)
            }
            #[cfg(d3d11_arm)]
            Self::D3d11(frame) => {
                let (src, dst) = match (src, dst) {
                    (FrameCopyTexture::D3d11(src), FrameCopyTexture::D3d11(dst)) => (src, dst),
                };
                frame.copy_rect(src, src_origin, dst, dst_origin, width, height)
            }
        }
    }

    /// Commit the frame.
    pub(crate) fn submit(self) -> SubmittedFrame {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(cb) => SubmittedFrame::Metal(cb.commit()),
            #[cfg(gl_arm)]
            Self::Gl(frame) => SubmittedFrame::Gl(frame.submit()),
            #[cfg(d3d11_arm)]
            Self::D3d11(frame) => SubmittedFrame::D3d11(frame.submit()),
        }
    }
}

/// One open frame pass on one backend.
#[allow(
    clippy::large_enum_variant,
    reason = "one FramePass lives on the stack per render pass (1..6 per frame) \
              and dies before the next opens; boxing the largest arm's pass would \
              put a heap allocation on the keystroke-echo path to save nothing"
)]
pub(crate) enum FramePass<'e, 's> {
    #[cfg(target_os = "macos")]
    Metal(MtlPassEncoder<'e, 's>),
    #[cfg(gl_arm)]
    Gl(GlPass<'e, 's>),
    #[cfg(d3d11_arm)]
    D3d11(D3dPass<'e, 's>),
}

impl FramePass<'_, '_> {
    /// `set_pipeline` — the mid-pass pipeline switch.
    #[inline]
    pub(crate) fn set_pipeline(&mut self, pipeline: FramePipeline<'_>) {
        match (self, pipeline) {
            #[cfg(target_os = "macos")]
            (Self::Metal(pass), FramePipeline::Metal(pso)) => pass.set_pipeline(pso),
            #[cfg(gl_arm)]
            (Self::Gl(pass), FramePipeline::Gl(p)) => pass.set_pipeline(p),
            #[cfg(d3d11_arm)]
            (Self::D3d11(pass), FramePipeline::D3d11(p)) => pass.set_pipeline(p),
        }
    }

    /// Bind the atlas: its texture and sampler at the row's slots (Metal) or unit (GL, D3D11).
    #[inline]
    pub(crate) fn bind_atlas(&mut self, atlas: FrameAtlas<'_>) {
        match (self, atlas) {
            #[cfg(target_os = "macos")]
            (
                Self::Metal(pass),
                FrameAtlas::Metal {
                    tex,
                    sampler,
                    tex_slot,
                    sampler_slot,
                },
            ) => {
                pass.set_fragment_texture(tex, tex_slot as usize);
                pass.set_fragment_sampler(sampler, sampler_slot as usize);
            }
            #[cfg(gl_arm)]
            (Self::Gl(pass), FrameAtlas::Gl { tex, sampler, unit }) => {
                pass.bind_texture(tex, sampler, unit, false);
            }
            #[cfg(target_os = "macos")]
            (
                Self::Metal(pass),
                FrameAtlas::MetalFx {
                    r8,
                    rgba,
                    palette,
                    tex_slots,
                    palette_slot,
                },
            ) => {
                pass.set_fragment_texture(r8, tex_slots[0] as usize);
                pass.set_fragment_texture(rgba, tex_slots[1] as usize);
                pass.set_fragment_buffer(palette, palette_slot as usize);
            }
            #[cfg(gl_arm)]
            (
                Self::Gl(pass),
                FrameAtlas::GlFx {
                    r8,
                    rgba,
                    palette,
                    sampler,
                    units,
                },
            ) => {
                pass.bind_texture(r8, sampler, units[0], false);
                pass.bind_texture(rgba, sampler, units[1], false);
                pass.bind_texture(palette, sampler, units[2], false);
            }
            #[cfg(d3d11_arm)]
            (Self::D3d11(pass), FrameAtlas::D3d11 { tex, sampler, unit }) => {
                // Frame atlases have no sRGB view; `raw` is moot.
                pass.bind_texture(tex, sampler, unit, false);
            }
            #[cfg(d3d11_arm)]
            (
                Self::D3d11(pass),
                FrameAtlas::D3d11Fx {
                    r8,
                    rgba,
                    palette,
                    sampler,
                    units,
                },
            ) => {
                pass.bind_texture(r8, sampler, units[0], false);
                pass.bind_texture(rgba, sampler, units[1], false);
                pass.bind_texture(palette, sampler, units[2], false);
            }
        }
    }

    /// Bind the per-draw instance stream.
    #[inline]
    pub(crate) fn set_stream(&mut self, stream: FrameStream<'_>) {
        match (self, stream) {
            #[cfg(target_os = "macos")]
            (Self::Metal(pass), FrameStream::Metal(buf)) => pass.set_instance_stream(buf),
            #[cfg(gl_arm)]
            (Self::Gl(pass), FrameStream::Gl(buf)) => pass.set_stream(buf),
            #[cfg(d3d11_arm)]
            (Self::D3d11(pass), FrameStream::D3d11(buf)) => pass.set_stream(buf),
        }
    }

    /// Draw `instances` quads of 6 vertices.
    #[inline]
    pub(crate) fn draw_quads(&mut self, instances: u32) -> Result<(), String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::Metal(pass) => pass.draw_instanced(
                crate::metal::ffi::PrimitiveType::Triangle,
                6,
                instances as usize,
            ),
            #[cfg(gl_arm)]
            Self::Gl(pass) => pass.draw(6, instances),
            #[cfg(d3d11_arm)]
            Self::D3d11(pass) => pass.draw(6, instances),
        }
    }
}

/// Metal arm of `try_read_back`.
#[cfg(target_os = "macos")]
pub(crate) fn metal_try_read_back(
    session: &EncodeSession,
    mint: &MetalResourceDevice,
    texture: &SealedTexture,
    width: u32,
    height: u32,
) -> Result<aterm_render::Frame, String> {
    let (w, h) = (width as usize, height as usize);
    let (readback, stride) = metal_stage_readback(session, mint, texture, w, h)?;
    // SAFETY: `readback` is shared storage of `stride * h` bytes, and
    // `metal_stage_readback` returned after the writing command buffer
    // completed.
    let bytes = unsafe { mtl::buffer_bytes(&readback, stride * h) };
    Ok(crate::frame_from_padded_rgba(&bytes, w, h, stride))
}

/// The staging step of [`metal_try_read_back`].
#[cfg(target_os = "macos")]
pub(crate) fn metal_stage_readback(
    session: &EncodeSession,
    mint: &MetalResourceDevice,
    texture: &SealedTexture,
    w: usize,
    h: usize,
) -> Result<(mtl::Obj, usize), String> {
    let padded = crate::padded_bytes_per_row(w);
    let readback = DeviceHandle::Metal(mint)
        .try_create_readback_buffer("aterm-gpu metal readback", (padded * h.max(1)) as u64)?
        .into_metal()
        .into_obj();
    let mut cb = session.begin()?;
    cb.copy_texture_to_buffer(texture, w, h, &readback, padded)?;
    let submitted = cb.commit();
    let deadline = aterm_time::Instant::now() + std::time::Duration::from_secs(5);
    let outcome = loop {
        if let Some(outcome) = submitted.try_outcome() {
            break outcome;
        }
        if aterm_time::Instant::now() >= deadline {
            return Err(
                "metal readback: command buffer not terminal within 5s (status polling)".to_owned(),
            );
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    };
    if outcome != crate::metal::loss::CbOutcome::Completed {
        return Err(format!("metal readback failed: {outcome:?}"));
    }
    Ok((readback, padded))
}

/// GL arm of `try_read_back`: read `texture`'s `width` x `height` RGBA8
/// texels at the shared 256-byte row stride and unpack them into an
/// `aterm_render::Frame` with the shared byte contract
/// ([`crate::frame_from_padded_rgba`]).
#[cfg(gl_arm)]
pub(crate) fn gl_try_read_back(
    dev: &GlDevice,
    texture: &GlTexture,
    width: u32,
    height: u32,
) -> Result<aterm_render::Frame, String> {
    let (w, h) = (width as usize, height as usize);
    let padded = crate::padded_bytes_per_row(w);
    let bytes = dev.read_rgba8(texture, width, height, padded)?;
    Ok(crate::frame_from_padded_rgba(&bytes, w, h, padded))
}

/// D3D11 arm of `try_read_back`: copy `texture`'s `width` x `height` RGBA8
/// texels through a staging texture into rows of the shared 256-byte
/// stride and unpack them into an `aterm_render::Frame` with the shared byte
/// contract ([`crate::frame_from_padded_rgba`]). The staging map waits for
/// every draw into the texture, so the answer is the frame as rendered.
#[cfg(d3d11_arm)]
pub(crate) fn d3d11_try_read_back(
    dev: &D3dDevice,
    texture: &D3dTexture,
    width: u32,
    height: u32,
) -> Result<aterm_render::Frame, String> {
    let (w, h) = (width as usize, height as usize);
    let padded = crate::padded_bytes_per_row(w);
    let bytes = dev.read_rgba8(texture, width, height, padded)?;
    Ok(crate::frame_from_padded_rgba(&bytes, w, h, padded))
}

#[cfg(all(target_os = "macos", test))]
mod tests {
    use super::*;
    use crate::metal::ffi::Device;
    use crate::metal::loss::LossLatch;
    use std::sync::Arc;

    /// Every route outside `metal/` by which a texture minted in loss domain 2
    /// could reach a domain-1 encode or present (alias view, copy destination,
    /// a second swapchain's drawable and alias, cross present) refuses by name.
    #[test]
    fn no_texture_launders_across_loss_domains() {
        use crate::metal::encoder::{EncodeSession, RenderPassDesc, StoreAction};
        use crate::metal::ffi::{
            ClearColor, LoadAction, PixelFormat, TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
        };
        use crate::metal::swapchain::{Swapchain, SwapchainConfig};

        let Some(dev) = Device::preferred() else {
            aterm_spec::host::could_not_run("no Metal device");
        };
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let latch1 = Arc::new(LossLatch::new());
        let session1 = EncodeSession::new(&dev, Arc::clone(&latch1)).expect("session 1");
        let mint1 = MetalResourceDevice::new(&dev, Arc::clone(&latch1));
        let latch2 = Arc::new(LossLatch::new());
        let session2 = EncodeSession::new(&dev, Arc::clone(&latch2)).expect("session 2");
        let mint2 = MetalResourceDevice::new(&dev, Arc::clone(&latch2));

        let usage_rt = TEXTURE_USAGE_RENDER_TARGET
            | TEXTURE_USAGE_SHADER_READ
            | TEXTURE_USAGE_PIXEL_FORMAT_VIEW;
        let rogue = mint2
            .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, usage_rt)
            .expect("rogue texture");
        let native1 = mint1
            .texture_2d(PixelFormat::Rgba8Unorm, 8, 8, usage_rt)
            .expect("domain-1 texture");

        let pass_desc = |t| RenderPassDesc {
            target: t,
            load: LoadAction::Clear(ClearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            store: StoreAction::Store,
            viewport: None,
            scissor: None,
        };

        let rogue_alias = rogue
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("alias view");
        let mut cb1 = session1.begin().expect("cb1");
        let err = cb1
            .render_pass(&pass_desc(&rogue_alias))
            .expect_err("an aliased foreign texture must not become a domain-1 target");
        assert!(err.contains("loss domain"), "route A names the seal: {err}");

        let err = cb1
            .copy_texture_to_texture(&native1, &rogue_alias)
            .expect_err("an aliased foreign texture must not become a copy dst");
        assert!(err.contains("loss domain"), "route B names the seal: {err}");

        let mut sc2 = Swapchain::standalone(
            &dev,
            &SwapchainConfig {
                format: PixelFormat::Bgra8Unorm,
                width: 8,
                height: 8,
                framebuffer_only: false,
                display_sync: false,
                maximum_drawables: 2,
                opaque: true,
            },
            Arc::clone(&latch2),
        )
        .expect("swapchain 2");
        let frame2 = sc2.acquire().expect("acquire on sc2");
        let rt2 = frame2.render_target();
        let err = cb1
            .render_pass(&pass_desc(&rt2))
            .expect_err("a domain-2 drawable must not render on session 1");
        assert!(err.contains("loss domain"), "route C render: {err}");
        let rt2_alias = rt2.alias_view(PixelFormat::Bgra8UnormSrgb);
        if let Some(rt2_alias) = rt2_alias.as_ref() {
            let err = cb1
                .render_pass(&pass_desc(rt2_alias))
                .expect_err("a domain-2 drawable ALIAS must not render on session 1");
            assert!(err.contains("loss domain"), "route C alias: {err}");
        }
        let err = frame2
            .present(&session1)
            .expect_err("a domain-2 frame must not present via session 1");
        assert!(
            err.contains("loss domain") || err.contains("latch"),
            "route C present: {err}"
        );

        let handle1 = DeviceHandle::Metal(&mint1);
        let wrapped = LayerTexture::Metal(rogue);
        handle1.upload_texture_rows(&wrapped, 2, 2, &[7u8; 64], 32, 8);

        cb1.render_pass(&pass_desc(&native1))
            .expect("same-domain control");
        assert_eq!(
            cb1.commit().wait_outcome(),
            crate::metal::loss::CbOutcome::Completed,
            "cb1 completes"
        );
        let cb2 = session2.begin().expect("cb2");
        assert_eq!(
            cb2.commit().wait_outcome(),
            crate::metal::loss::CbOutcome::Completed,
            "the untouched domain-2 cb completes"
        );
        assert!(
            !latch1.is_lost() && !latch2.is_lost(),
            "laundering attempts are wiring errors, not device losses"
        );
    }

    fn mint() -> Option<MetalResourceDevice> {
        let dev = Device::preferred()?;
        Some(MetalResourceDevice::new(&dev, Arc::new(LossLatch::new())))
    }

    /// The Metal arm's length guards: a short row band and an overlong buffer
    /// write both fail the layer's assert before `replaceRegion:` / `memcpy`.
    #[test]
    fn the_metal_arm_refuses_short_uploads_and_oversized_writes() {
        let Some(mint) = mint() else {
            aterm_spec::host::could_not_run("no Metal device");
        };
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let handle = DeviceHandle::Metal(&mint);
        let tex = handle
            .try_create_texture_2d(
                "device-layer short-upload probe",
                TexelFormat::R8Unorm,
                8,
                8,
                TexUsage::UPLOADED_SAMPLED,
                None,
            )
            .expect("an 8x8 R8 texture on a live Metal device");
        let short = [0u8; 8];
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle.upload_texture_rows(&tex, 0, 2, &short, 8, 8);
        }))
        .expect_err("a short row band must refuse");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
            .unwrap_or_default();
        assert!(msg.contains("region upload"), "got: {msg}");

        let buf = handle
            .try_create_instance_buffer("device-layer short-buffer probe", 4)
            .expect("a 4-byte MTLBuffer on a live Metal device");
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: no GPU work exists on this fresh buffer; the call must
            // refuse on length before any copy.
            unsafe { handle.write_buffer_from_zero(&buf, &[0u8; 16]) };
        }))
        .expect_err("an oversized buffer write must refuse");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
            .unwrap_or_default();
        assert!(msg.contains("MTLBuffer"), "got: {msg}");
    }
}
