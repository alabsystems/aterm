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

/// The GL arm's 2-D texture dimension ceiling, whatever the driver reports: Metal's, and the
/// widest placement raster (`aterm_render::MAX_PLACEMENT_RASTER_SIDE`).
#[cfg(any(gl_arm, test))]
pub(crate) const GL_MAX_TEXTURE_DIM: u32 = 16384;

/// The GL arm's texture dimension ceiling: the driver's `GL_MAX_TEXTURE_SIZE` answer, 0 for a
/// negative one, and never above [`GL_MAX_TEXTURE_DIM`]. Only [`GlMaxTexture::from_driver`] makes
/// one, so a `GlCaps` cannot carry the driver's raw answer. Unclamped, a driver that reports 65,536
/// lets an image plane a 16,384-texel raster wide stack 65,536 rows, whose `tw * th * 4` bytes are
/// 2^32 and whose staging buffer no frame should allocate.
#[cfg(any(gl_arm, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlMaxTexture(u32);

#[cfg(any(gl_arm, test))]
impl GlMaxTexture {
    /// The ceiling for the driver's answer `raw`.
    pub(crate) fn from_driver(raw: i32) -> Self {
        Self(u32::try_from(raw).unwrap_or(0).min(GL_MAX_TEXTURE_DIM))
    }

    /// Its texels per side.
    pub(crate) const fn get(self) -> u32 {
        self.0
    }
}

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
    /// where the region leaves the texture, `bytes_per_row` is short or
    /// unaligned for the texel width, or `bytes` is short of it.
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

/// Why `replaceRegion:` of the `w × h` region at `(x, y)` of a `tex_w × tex_h` texture whose
/// texels are `bpt` bytes (`None`: a format this layer does not know) would read or write outside
/// what it is given: the region leaves the texture, a row of `bytes_per_row` is short of `w`
/// texels or not a whole number of them, or `len` bytes are short of `h` such rows. `None` when
/// the upload is sound.
#[cfg(target_os = "macos")]
fn metal_region_refusal(
    (tex_w, tex_h): (usize, usize),
    bpt: Option<usize>,
    (x, y, w, h): (u32, u32, u32, u32),
    len: usize,
    bytes_per_row: u32,
) -> Option<String> {
    let inside = |at: u32, span: u32, size: usize| {
        (at as usize)
            .checked_add(span as usize)
            .is_some_and(|end| end <= size)
    };
    if !inside(x, w, tex_w) || !inside(y, h, tex_h) {
        return Some(format!(
            "region upload of {w}x{h} at ({x}, {y}) leaves a {tex_w}x{tex_h} texture"
        ));
    }
    let Some(bpt) = bpt else {
        return Some(format!(
            "region upload of {w}x{h} into a texture of unknown texel size"
        ));
    };
    let stride = bytes_per_row as usize;
    if (w as usize).checked_mul(bpt).is_none_or(|row| stride < row) || !stride.is_multiple_of(bpt) {
        return Some(format!(
            "region upload of {w}x{h} ({bpt}-byte texels) needs a stride of at least {w} whole \
             texels, got {bytes_per_row} bytes"
        ));
    }
    if stride.checked_mul(h as usize).is_none_or(|need| len < need) {
        return Some(format!(
            "region upload of {w}x{h} rows needs {bytes_per_row}*{h} bytes, got {len}"
        ));
    }
    None
}

impl DeviceHandle<'_> {
    /// [`Self::upload_texture_region`], answering `Err` where the GL or D3D11
    /// device refuses the write (a region outside the texture, or a removed
    /// D3D11 device), so a caller on a device that can be lost under it
    /// fails the frame instead of the process. Metal answers `Ok`.
    ///
    /// # Panics
    ///
    /// On Metal, where the region leaves the texture, `bytes_per_row` is short
    /// or unaligned for the texel width, or `bytes` is short of it.
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
                let bpt = mtl::PixelFormat::from_raw(mtl::texture_pixel_format_raw(texture.obj()))
                    .map(mtl::PixelFormat::bytes_per_texel);
                if let Some(why) = metal_region_refusal(
                    (texture.width(), texture.height()),
                    bpt,
                    (x, y, w, h),
                    bytes.len(),
                    bytes_per_row,
                ) {
                    panic!("device layer: {why}");
                }
                // SAFETY: a `LayerTexture::Metal` comes only from this layer's mint, which creates
                // Managed 2-D textures, so `replaceRegion:` is legal; `metal_region_refusal` found
                // the region inside the texture, each row at least `w` whole texels of its format,
                // and `bytes` at least `h` rows of `bytes_per_row`.
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

/// How long [`metal_stage_readback`] waits for its committed command buffer to reach a terminal
/// status: a minute hang detector in every build. A committed command buffer ends, and a GPU
/// other work loads only slows it; scheduling delay does not prove readback failed.
#[cfg(target_os = "macos")]
const READBACK_HANG: std::time::Duration = std::time::Duration::from_secs(60);

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
    let size = padded
        .checked_mul(h.max(1))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| format!("metal readback: {w}x{h} staging size overflows"))?;
    let readback = DeviceHandle::Metal(mint)
        .try_create_readback_buffer("aterm-gpu metal readback", size)?
        .into_metal()
        .into_obj();
    let mut cb = session.begin()?;
    cb.copy_texture_to_buffer(texture, w, h, &readback, padded)?;
    let submitted = cb.commit();
    wait_readback(aterm_time::Instant::now(), || submitted.try_outcome())?;
    Ok((readback, padded))
}

#[cfg(target_os = "macos")]
fn wait_readback(
    started: aterm_time::Instant,
    mut read: impl FnMut() -> Option<crate::metal::loss::CbOutcome>,
) -> Result<(), String> {
    let deadline = started + READBACK_HANG;
    let outcome = loop {
        if let Some(outcome) = read() {
            break outcome;
        }
        if aterm_time::Instant::now() >= deadline {
            return Err(format!(
                "metal readback: command buffer not terminal within {READBACK_HANG:?} (status polling)"
            ));
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    };
    if outcome != crate::metal::loss::CbOutcome::Completed {
        return Err(format!("metal readback failed: {outcome:?}"));
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
#[path = "device_layer_readback_wait_tests.rs"]
mod readback_wait_tests;

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
#[path = "device_layer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "device_layer_gl_texture_dim_tests.rs"]
mod gl_texture_dim_tests;
