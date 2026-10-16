// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The device layer: one enum per resource kind under `renderer.rs`'s
//! resource and encode seams, with one variant per backend arm compiled into
//! the build. The pipeline table owns what a pipeline is; this layer owns how
//! a resource comes to exist and how a frame is encoded. Everything is
//! `pub(crate)`.
//!
//! It wraps instance/vertex buffers (write from 0, geometric grow), textures
//! (create, full and row-ranged upload), uniform and readback buffers, texture
//! views (the Unorm/sRGB alias) and samplers (two clamp presets). Bind groups
//! are absent: Metal has none, and each layout becomes a few `set*` calls per
//! draw.
//!
//! # Arms
//!
//! Every enum here ([`DeviceHandle`], [`LayerTexture`], [`LayerBuffer`],
//! [`LayerSampler`], [`FrameEncoder`], [`FramePass`] and the `Frame*` handles)
//! has one variant per backend arm in the build, gated by that arm's own cfg
//! and no other:
//!
//! * `Wgpu`, under `cfg(wgpu_arm)`: production on Linux, Windows and wasm; the
//!   differential oracle in macOS test builds.
//! * `Metal`, under `cfg(target_os = "macos")`: production on macOS. The armed
//!   renderer (`renderer.rs::MetalArmLive`) mints, uploads and writes every
//!   texture, buffer and sampler through [`DeviceHandle::Metal`], over
//!   [`crate::metal::resources::MetalResourceDevice`], so every texture
//!   carries its loss-domain stamp.
//!
//! An arm still to come (GL at docs/AUDIT-third-party-2026-10-15.md GW.4,
//! D3D11 at GW.5) is one more variant per enum under its own cfg, plus its
//! count in build.rs's `multi_arm`.
//!
//! # Fallible and infallible creation
//!
//! The `try_create_*` verbs return the backend's allocation error: an
//! explicit-API arm (Metal) reports it synchronously and the caller degrades.
//! wgpu reports allocation errors out of band (error scopes, the
//! uncaptured-error handler), so its arm never returns `Err`; the infallible
//! `create_*` spellings, under `cfg(wgpu_arm)`, serve its callers and panic by
//! name on an `Err` (which only a Metal handle, in a test build, can return).
//!
//! The frame-seam half ([`TexelFormat`], [`ClearColor4`], [`FrameLoad`],
//! [`FrameEncoder`], [`FramePass`], the `Frame*` handle enums,
//! [`metal_try_read_back`], [`metal_stage_readback`]) works the same way.
//! `renderer.rs::run_frame_plan` drives a [`FrameEncoder::Metal`] in macOS
//! production; the scroll shift (`metal_shift_offscreen_band_px`) and capture
//! readback ([`metal_try_read_back`]) use it too.
//!
//! # Crossing
//!
//! Verbs dispatch on the handles they are given. A handle from one arm that
//! reaches another arm's verb panics by name ("device layer"): wgpu-typed seams
//! take resources out via [`LayerTexture::wgpu`], [`LayerBuffer::wgpu`] and
//! friends, the Metal arm via [`LayerTexture::metal`] and friends, and the
//! frame-seam verbs check their handles' arms. Those refusals compile only
//! under `cfg(multi_arm)` (build.rs), the builds with more than one arm; a
//! single-arm build has no other arm to refuse. Production never holds a
//! mismatched pair.

#[cfg(target_os = "macos")]
use crate::metal::ffi as mtl;
#[cfg(target_os = "macos")]
use crate::metal::resources::{MetalResourceDevice, SealedTexture, SharedBuffer};

/// The crossing refusal: a `what` minted by one backend arm reached `verb` of
/// another. Only builds with more than one arm can express the mistake.
#[cfg(multi_arm)]
#[cold]
#[track_caller]
fn crossed(what: &str, verb: &str) -> ! {
    panic!(
        "device layer: a {what} from one backend reached {verb} — the caller \
         mixed handles across backends"
    )
}

/// The Metal arm's 2-D texture dimension ceiling: every Apple-silicon family
/// supports 16384, and Metal exposes no property for it.
#[cfg(target_os = "macos")]
pub(crate) const METAL_MAX_TEXTURE_DIM: u32 = 16384;

/// The six texel formats the Metal foundation models
/// (`metal::ffi::PixelFormat`), spelled backend-neutrally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TexelFormat {
    R8Unorm,
    Rgba8Unorm,
    /// The sRGB alias of `Rgba8Unorm`: the offscreen's base-pass view, and on
    /// GLES/WebGL2 the offscreen's own format.
    Rgba8UnormSrgb,
    Bgra8Unorm,
    /// The sRGB alias of `Bgra8Unorm` (the wgpu arm's surface views and the
    /// oracle only).
    #[cfg(wgpu_arm)]
    Bgra8UnormSrgb,
    Rgba16Float,
}

impl TexelFormat {
    /// Bytes one texel occupies in a linear (buffer/upload) layout.
    #[cfg(target_os = "macos")]
    pub(crate) const fn bytes_per_texel(self) -> u32 {
        match self {
            Self::R8Unorm => 1,
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm => 4,
            #[cfg(wgpu_arm)]
            Self::Bgra8UnormSrgb => 4,
            Self::Rgba16Float => 8,
        }
    }

    /// The wgpu spelling.
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::R8Unorm => wgpu::TextureFormat::R8Unorm,
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            Self::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
            Self::Bgra8UnormSrgb => wgpu::TextureFormat::Bgra8UnormSrgb,
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        }
    }

    /// Recover the neutral format from a wgpu format (for already-resolved
    /// formats such as `format_plan::offscreen_format`). `None` outside the six
    /// formats in use.
    #[cfg(wgpu_arm)]
    pub(crate) const fn from_wgpu(f: wgpu::TextureFormat) -> Option<Self> {
        match f {
            wgpu::TextureFormat::R8Unorm => Some(Self::R8Unorm),
            wgpu::TextureFormat::Rgba8Unorm => Some(Self::Rgba8Unorm),
            wgpu::TextureFormat::Rgba8UnormSrgb => Some(Self::Rgba8UnormSrgb),
            wgpu::TextureFormat::Bgra8Unorm => Some(Self::Bgra8Unorm),
            wgpu::TextureFormat::Bgra8UnormSrgb => Some(Self::Bgra8UnormSrgb),
            wgpu::TextureFormat::Rgba16Float => Some(Self::Rgba16Float),
            _ => None,
        }
    }

    /// The Metal spelling.
    #[cfg(target_os = "macos")]
    pub(crate) const fn metal(self) -> mtl::PixelFormat {
        match self {
            Self::R8Unorm => mtl::PixelFormat::R8Unorm,
            Self::Rgba8Unorm => mtl::PixelFormat::Rgba8Unorm,
            Self::Rgba8UnormSrgb => mtl::PixelFormat::Rgba8UnormSrgb,
            Self::Bgra8Unorm => mtl::PixelFormat::Bgra8Unorm,
            #[cfg(wgpu_arm)]
            Self::Bgra8UnormSrgb => mtl::PixelFormat::Bgra8UnormSrgb,
            Self::Rgba16Float => mtl::PixelFormat::Rgba16Float,
        }
    }
}

/// What a texture is for, backend-neutrally. Maps onto the wgpu usage bits and
/// onto Metal's smaller vocabulary (no copy bits; `replaceRegion:` needs only
/// non-Private storage, which the Managed mint provides; CPU reads go through
/// a blit into a Shared buffer, see `crate::metal::ffi::Device::new_texture_2d`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TexUsage {
    /// wgpu `TEXTURE_BINDING` / Metal `ShaderRead`.
    pub(crate) sampled: bool,
    /// wgpu `RENDER_ATTACHMENT` / Metal `RenderTarget`.
    pub(crate) render: bool,
    /// wgpu `COPY_SRC` / Metal: nothing to declare.
    pub(crate) copy_src: bool,
    /// wgpu `COPY_DST` / Metal: nothing to declare.
    pub(crate) copy_dst: bool,
}

impl TexUsage {
    /// The upload-then-sample shape every atlas/sprite/overlay texture uses
    /// (`TEXTURE_BINDING | COPY_DST`).
    pub(crate) const UPLOADED_SAMPLED: Self = Self {
        sampled: true,
        render: false,
        copy_src: false,
        copy_dst: true,
    };

    /// The offscreen family: drawn into, blitted from, scroll-shifted into, and
    /// sampled by the present blit
    /// (`RENDER_ATTACHMENT | COPY_SRC | COPY_DST | TEXTURE_BINDING`).
    pub(crate) const OFFSCREEN: Self = Self {
        sampled: true,
        render: true,
        copy_src: true,
        copy_dst: true,
    };

    /// Copy-only staging (`COPY_SRC | COPY_DST`): the scroll shift's scratch
    /// and the apron row, which only copies read and write.
    pub(crate) const COPY_STAGING: Self = Self {
        sampled: false,
        render: false,
        copy_src: true,
        copy_dst: true,
    };

    #[cfg(wgpu_arm)]
    const fn wgpu(self) -> wgpu::TextureUsages {
        let mut u = wgpu::TextureUsages::empty();
        if self.sampled {
            u = u.union(wgpu::TextureUsages::TEXTURE_BINDING);
        }
        if self.render {
            u = u.union(wgpu::TextureUsages::RENDER_ATTACHMENT);
        }
        if self.copy_src {
            u = u.union(wgpu::TextureUsages::COPY_SRC);
        }
        if self.copy_dst {
            u = u.union(wgpu::TextureUsages::COPY_DST);
        }
        u
    }

    #[cfg(target_os = "macos")]
    const fn metal(self) -> usize {
        // Metal declares no copy usage: blits may read and write any texture.
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
/// The Metal arm uses the matching `SamplerDesc` preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SamplerKind {
    /// Exact-texel reads (glyph/deco/sprite atlases, the blit).
    NearestClamp,
    /// Smooth resampling (bloom upsample, shimmer, tray card).
    LinearClamp,
}

/// One backend, borrowed: the handle a construction site creates resources
/// through. The wgpu arm is [`crate::GpuContext::device_layer`]; the Metal arm
/// wraps the armed renderer's mint (`MetalArmLive::layer`) and, in tests, any
/// [`MetalResourceDevice`].
#[derive(Clone, Copy)]
pub(crate) enum DeviceHandle<'a> {
    #[cfg(wgpu_arm)]
    Wgpu {
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    },
    #[cfg(target_os = "macos")]
    Metal(&'a MetalResourceDevice),
}

impl DeviceHandle<'_> {
    /// Create a 2-D texture. `alias` declares a format the texture may later be
    /// viewed as (the Unorm/sRGB alias): wgpu lists it in `view_formats`; Metal
    /// needs no declaration for the sRGB-variant pair. The Metal arm's error is
    /// the mint's, verbatim.
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
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => {
                let view_formats: &[wgpu::TextureFormat] = match alias {
                    Some(a) => &[a.wgpu()],
                    None => &[],
                };
                Ok(LayerTexture::Wgpu(device.create_texture(
                    &wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: format.wgpu(),
                        usage: usage.wgpu(),
                        view_formats,
                    },
                )))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                // Metal textures carry no label, and the sRGB alias pair needs
                // no declaration.
                let _ = (label, alias);
                mint.texture_2d(
                    format.metal(),
                    width as usize,
                    height as usize,
                    usage.metal(),
                )
                .map(LayerTexture::Metal)
            }
        }
    }

    /// [`Self::try_create_texture_2d`] for the wgpu arm's callers, which have
    /// no synchronous allocation error to handle.
    #[cfg(wgpu_arm)]
    pub(crate) fn create_texture_2d(
        &self,
        label: &'static str,
        format: TexelFormat,
        width: u32,
        height: u32,
        usage: TexUsage,
        alias: Option<TexelFormat>,
    ) -> LayerTexture {
        self.try_create_texture_2d(label, format, width, height, usage, alias)
            .unwrap_or_else(|e| panic!("device layer: {label}: {e}"))
    }

    /// Full-extent upload of level 0 (`queue.write_texture` / `replaceRegion:`).
    /// `bytes` is tightly packed at `bytes_per_row`.
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

    /// Row-ranged upload of rows `[y0, y0 + rows)` at full width, for the
    /// atlas append. `bytes` holds exactly the band.
    ///
    /// The Metal arm's `replaceRegion:` is a CPU copy outside the command
    /// queue's hazard tracking: a caller rewriting texels a submitted command
    /// buffer may still sample settles those readers first
    /// (`MetalArmLive::await_atlas_readers`).
    pub(crate) fn upload_texture_rows(
        &self,
        tex: &LayerTexture,
        y0: u32,
        rows: u32,
        bytes: &[u8],
        bytes_per_row: u32,
        width: u32,
    ) {
        if rows == 0 {
            return;
        }
        match (self, tex) {
            #[cfg(wgpu_arm)]
            (Self::Wgpu { queue, .. }, LayerTexture::Wgpu(texture)) => {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(rows),
                    },
                    wgpu::Extent3d {
                        width,
                        height: rows,
                        depth_or_array_layers: 1,
                    },
                );
            }
            #[cfg(target_os = "macos")]
            (Self::Metal(_), LayerTexture::Metal(texture)) => {
                let region = mtl::MtlRegion {
                    origin: mtl::MtlOrigin {
                        x: 0,
                        y: y0 as usize,
                        z: 0,
                    },
                    size: mtl::MtlSize {
                        width: width as usize,
                        height: rows as usize,
                        depth: 1,
                    },
                };
                assert!(
                    bytes.len() >= bytes_per_row as usize * rows as usize,
                    "device layer: row-ranged upload of {rows} rows needs \
                     {bytes_per_row}*{rows} bytes, got {}",
                    bytes.len()
                );
                // SAFETY: a `LayerTexture::Metal` comes only from this layer's
                // mint, which creates Managed 2-D textures, so `replaceRegion:`
                // is legal; the band length is asserted above and the copy is
                // synchronous.
                unsafe { texture.upload(region, bytes, bytes_per_row as usize) };
            }
            #[cfg(multi_arm)]
            _ => crossed("texture", "another backend's upload"),
        }
    }

    /// A per-frame instance-stream buffer (`VERTEX | COPY_DST` on wgpu, a
    /// shared-storage `MTLBuffer` on Metal). Size 0 is legal on wgpu; Metal
    /// floors at 1 byte because `newBufferWithLength:0` returns nil.
    pub(crate) fn try_create_instance_buffer(
        &self,
        label: &'static str,
        size: u64,
    ) -> Result<LayerBuffer, String> {
        self.try_create_buffer(label, size, BufferRole::Instance)
    }

    /// [`Self::try_create_instance_buffer`] for the wgpu arm's callers.
    #[cfg(wgpu_arm)]
    pub(crate) fn create_instance_buffer(&self, label: &'static str, size: u64) -> LayerBuffer {
        self.try_create_instance_buffer(label, size)
            .unwrap_or_else(|e| panic!("device layer: {label}: {e}"))
    }

    /// A uniform buffer (`UNIFORM | COPY_DST` on wgpu; a shared-storage
    /// `MTLBuffer` on Metal).
    pub(crate) fn try_create_uniform_buffer(
        &self,
        label: &'static str,
        size: u64,
    ) -> Result<LayerBuffer, String> {
        self.try_create_buffer(label, size, BufferRole::Uniform)
    }

    /// [`Self::try_create_uniform_buffer`] for the wgpu arm's callers.
    #[cfg(wgpu_arm)]
    pub(crate) fn create_uniform_buffer(&self, label: &'static str, size: u64) -> LayerBuffer {
        self.try_create_uniform_buffer(label, size)
            .unwrap_or_else(|e| panic!("device layer: {label}: {e}"))
    }

    /// A readback staging buffer a texture is copied into for the CPU to read
    /// (`COPY_DST | MAP_READ` on wgpu; a shared-storage `MTLBuffer` on Metal,
    /// read in place once the copy's command buffer is terminal).
    pub(crate) fn try_create_readback_buffer(
        &self,
        label: &'static str,
        size: u64,
    ) -> Result<LayerBuffer, String> {
        self.try_create_buffer(label, size, BufferRole::Readback)
    }

    /// [`Self::try_create_readback_buffer`] for the wgpu arm's callers.
    #[cfg(wgpu_arm)]
    pub(crate) fn create_readback_buffer(&self, label: &'static str, size: u64) -> LayerBuffer {
        self.try_create_readback_buffer(label, size)
            .unwrap_or_else(|e| panic!("device layer: {label}: {e}"))
    }

    /// The one buffer constructor behind the three roles. Metal storage is
    /// shared for every role, so only wgpu reads `role`.
    fn try_create_buffer(
        &self,
        label: &'static str,
        size: u64,
        role: BufferRole,
    ) -> Result<LayerBuffer, String> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => Ok(LayerBuffer::Wgpu(device.create_buffer(
                &wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: match role {
                        BufferRole::Instance => {
                            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST
                        }
                        BufferRole::Uniform => {
                            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST
                        }
                        BufferRole::Readback => {
                            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ
                        }
                    },
                    mapped_at_creation: false,
                },
            ))),
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                let _ = (label, role);
                mint.buffer(usize::try_from(size).unwrap_or(usize::MAX).max(1))
                    .map(|b| LayerBuffer::Metal(SharedBuffer::new(b)))
            }
        }
    }

    /// Write `bytes` into `buf` from offset 0, the only write shape the
    /// renderer uses.
    ///
    /// # Safety
    /// None on the wgpu arm. On the Metal arm this copies into shared storage
    /// the GPU may read: the caller must ensure no in-flight GPU work reads
    /// `buf`.
    pub(crate) unsafe fn write_buffer_from_zero(&self, buf: &LayerBuffer, bytes: &[u8]) {
        match (self, buf) {
            #[cfg(wgpu_arm)]
            (Self::Wgpu { queue, .. }, LayerBuffer::Wgpu(b)) => queue.write_buffer(b, 0, bytes),
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
            #[cfg(multi_arm)]
            _ => crossed("buffer", "another backend's write"),
        }
    }

    /// One of the two clamp samplers.
    pub(crate) fn try_create_sampler(
        &self,
        label: &'static str,
        kind: SamplerKind,
    ) -> Result<LayerSampler, String> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => {
                let filter = match kind {
                    SamplerKind::NearestClamp => wgpu::FilterMode::Nearest,
                    SamplerKind::LinearClamp => wgpu::FilterMode::Linear,
                };
                Ok(LayerSampler::Wgpu(device.create_sampler(
                    &wgpu::SamplerDescriptor {
                        label: Some(label),
                        address_mode_u: wgpu::AddressMode::ClampToEdge,
                        address_mode_v: wgpu::AddressMode::ClampToEdge,
                        mag_filter: filter,
                        min_filter: filter,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        ..Default::default()
                    },
                )))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                let _ = label;
                let desc = match kind {
                    SamplerKind::NearestClamp => mtl::SamplerDesc::NEAREST_CLAMP,
                    SamplerKind::LinearClamp => mtl::SamplerDesc::LINEAR_CLAMP,
                };
                mint.sampler(desc).map(LayerSampler::Metal)
            }
        }
    }

    /// [`Self::try_create_sampler`] for the wgpu arm's callers.
    #[cfg(wgpu_arm)]
    pub(crate) fn create_sampler(&self, label: &'static str, kind: SamplerKind) -> LayerSampler {
        self.try_create_sampler(label, kind)
            .unwrap_or_else(|e| panic!("device layer: {label}: {e}"))
    }

    /// The live wgpu device, for wgpu-typed construction (layouts, bind
    /// groups, pipelines, modules). Panics by name on another arm.
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu_device(&self) -> &wgpu::Device {
        match self {
            Self::Wgpu { device, .. } => device,
            #[cfg(multi_arm)]
            _ => crossed(
                "device handle",
                "a wgpu-only construction seam (bind-group layout / pipeline / shader module)",
            ),
        }
    }

    /// The device's buffer-size ceiling, for the geometric-grow cap. Only the
    /// wgpu arm's streams grow against it (`VertexBuffer::upload_via`).
    #[cfg(wgpu_arm)]
    pub(crate) fn max_buffer_size(&self) -> u64 {
        match self {
            Self::Wgpu { device, .. } => device.limits().max_buffer_size,
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => mint.device().max_buffer_length() as u64,
        }
    }

    /// The device's 2-D texture dimension ceiling, for the atlas clamps. The
    /// Metal arm's answer is [`METAL_MAX_TEXTURE_DIM`], which the renderer's
    /// framebuffer clamp reads directly.
    #[cfg(wgpu_arm)]
    pub(crate) fn max_texture_dim(&self) -> u32 {
        match self {
            Self::Wgpu { device, .. } => device.limits().max_texture_dimension_2d,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => METAL_MAX_TEXTURE_DIM,
        }
    }
}

/// What a buffer is for; only the wgpu arm's usage bits differ by role.
#[derive(Clone, Copy, Debug)]
enum BufferRole {
    Instance,
    Uniform,
    Readback,
}

/// A texture on whichever backend created it.
#[derive(Debug)]
pub(crate) enum LayerTexture {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::Texture),
    #[cfg(target_os = "macos")]
    Metal(SealedTexture),
}

impl LayerTexture {
    /// The live wgpu texture, for the wgpu-typed seams. Panics by name on
    /// another arm's texture.
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu(&self) -> &wgpu::Texture {
        match self {
            Self::Wgpu(t) => t,
            #[cfg(multi_arm)]
            _ => crossed(
                "texture",
                "a wgpu-only seam (bind group / encode / present)",
            ),
        }
    }

    /// [`Self::wgpu`], by value.
    #[cfg(wgpu_arm)]
    pub(crate) fn into_wgpu(self) -> wgpu::Texture {
        match self {
            Self::Wgpu(t) => t,
            #[cfg(multi_arm)]
            _ => crossed(
                "texture",
                "a wgpu-only seam (bind group / encode / present)",
            ),
        }
    }

    /// The sealed Metal texture, for binding. Panics by name on another arm's
    /// texture.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &SealedTexture {
        match self {
            Self::Metal(t) => t,
            #[cfg(multi_arm)]
            _ => crossed("texture", "a Metal-only seam"),
        }
    }

    /// [`Self::metal`], by value: the armed renderer keeps render targets it
    /// never uploads into as bare sealed textures.
    #[cfg(target_os = "macos")]
    pub(crate) fn into_metal(self) -> SealedTexture {
        match self {
            Self::Metal(t) => t,
            #[cfg(multi_arm)]
            _ => crossed("texture", "a Metal-only seam"),
        }
    }

    /// The texture's texel width (the wgpu arm's scroll-shift scratch reuse
    /// key).
    #[cfg(wgpu_arm)]
    pub(crate) fn width(&self) -> u32 {
        match self {
            Self::Wgpu(t) => t.width(),
            #[cfg(target_os = "macos")]
            Self::Metal(t) => t.width() as u32,
        }
    }

    /// The texture's texel height.
    #[cfg(wgpu_arm)]
    pub(crate) fn height(&self) -> u32 {
        match self {
            Self::Wgpu(t) => t.height(),
            #[cfg(target_os = "macos")]
            Self::Metal(t) => t.height() as u32,
        }
    }
}

/// A buffer on whichever backend created it.
#[derive(Debug)]
pub(crate) enum LayerBuffer {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::Buffer),
    #[cfg(target_os = "macos")]
    Metal(SharedBuffer),
}

impl LayerBuffer {
    /// The live wgpu buffer — same crossing contract as
    /// [`LayerTexture::wgpu`].
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu(&self) -> &wgpu::Buffer {
        match self {
            Self::Wgpu(b) => b,
            #[cfg(multi_arm)]
            _ => crossed("buffer", "a wgpu-only seam (bind group / encode / present)"),
        }
    }

    /// [`Self::wgpu`], by value.
    #[cfg(wgpu_arm)]
    pub(crate) fn into_wgpu(self) -> wgpu::Buffer {
        match self {
            Self::Wgpu(b) => b,
            #[cfg(multi_arm)]
            _ => crossed("buffer", "a wgpu-only seam (bind group / encode / present)"),
        }
    }

    /// `wgpu::Buffer::slice`, forwarded.
    #[cfg(wgpu_arm)]
    pub(crate) fn slice<S: std::ops::RangeBounds<wgpu::BufferAddress>>(
        &self,
        bounds: S,
    ) -> wgpu::BufferSlice<'_> {
        self.wgpu().slice(bounds)
    }

    /// The Metal buffer object, for binding. Panics by name on another arm's
    /// buffer.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &mtl::Obj {
        match self {
            Self::Metal(b) => b.obj(),
            #[cfg(multi_arm)]
            _ => crossed("buffer", "a Metal-only seam"),
        }
    }

    /// The shared Metal buffer, by value.
    #[cfg(target_os = "macos")]
    pub(crate) fn into_metal(self) -> SharedBuffer {
        match self {
            Self::Metal(b) => b,
            #[cfg(multi_arm)]
            _ => crossed("buffer", "a Metal-only seam"),
        }
    }
}

/// A sampler on whichever backend created it.
#[derive(Debug)]
pub(crate) enum LayerSampler {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::Sampler),
    #[cfg(target_os = "macos")]
    Metal(mtl::Obj),
}

impl LayerSampler {
    /// The live wgpu sampler — same crossing contract as
    /// [`LayerTexture::wgpu`].
    #[cfg(all(wgpu_arm, test, target_os = "macos"))]
    pub(crate) fn wgpu(&self) -> &wgpu::Sampler {
        match self {
            Self::Wgpu(s) => s,
            _ => crossed("sampler", "a wgpu-only seam"),
        }
    }

    /// [`Self::wgpu`], by value.
    #[cfg(wgpu_arm)]
    pub(crate) fn into_wgpu(self) -> wgpu::Sampler {
        match self {
            Self::Wgpu(s) => s,
            #[cfg(multi_arm)]
            _ => crossed("sampler", "a wgpu-only seam"),
        }
    }

    /// The `MTLSamplerState`, by value. Panics by name on another arm's
    /// sampler.
    #[cfg(target_os = "macos")]
    pub(crate) fn into_metal(self) -> mtl::Obj {
        match self {
            Self::Metal(s) => s,
            #[cfg(multi_arm)]
            _ => crossed("sampler", "a Metal-only seam"),
        }
    }
}

// ======================================================================
// The encode seam (the verbs under `renderer.rs::run_frame_plan`): open a
// pass (Clear or Load, optional scissor, shared uniforms bound once), switch
// pipeline and atlas only on change, bind one instance stream, draw instanced
// quads, copy texture rects, submit. The wgpu arm issues the same wgpu calls
// in the same order as a direct encode; the Metal arm forwards to
// `metal::encoder`, inheriting its loss-domain seal and draw-state refusals.
// Crossing a handle into the wrong arm panics by name ("device layer").
// ======================================================================

#[cfg(target_os = "macos")]
use crate::metal::encoder::{
    CommandBuffer as MtlCommandBuffer, EncodeSession, PassEncoder as MtlPassEncoder,
    RenderPassDesc as MtlRenderPassDesc, StoreAction, Submitted as MtlSubmitted,
};
#[cfg(target_os = "macos")]
use crate::metal::ffi::LoadAction;

/// A neutral f64 colour quadruple with `wgpu::Color`'s shape, so the frame
/// planner compiles without wgpu. The wgpu arm converts losslessly
/// (`ClearColor4::wgpu`); Metal builds its `MTLClearColor` from the same four
/// doubles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClearColor4 {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
    pub(crate) a: f64,
}

impl ClearColor4 {
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    /// The wgpu spelling (bit-identical: four f64 fields either way).
    #[cfg(wgpu_arm)]
    pub(crate) const fn wgpu(self) -> wgpu::Color {
        wgpu::Color {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }
}

/// What happens to a pass target's texels before the first draw: Clear or Load
/// (pass 0 Clear-or-Load, later passes Load), carrying the f64 clear colour on
/// both arms.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FrameLoad {
    /// Clear to this colour (linear for an sRGB-typed view, raw for Unorm —
    /// the call site's `theme_color_alpha` already speaks the view's space).
    Clear(ClearColor4),
    /// Keep the texels the previous pass/frame stored.
    Load,
}

/// A pass target view: the offscreen's Unorm or sRGB alias view. On Metal a
/// view is a sealed texture with the inherited loss-domain stamp.
#[derive(Clone, Copy)]
pub(crate) enum FrameView<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::TextureView),
    #[cfg(target_os = "macos")]
    Metal(&'a SealedTexture),
}

/// A render pipeline on one backend.
#[derive(Clone, Copy)]
pub(crate) enum FramePipeline<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::RenderPipeline),
    #[cfg(target_os = "macos")]
    Metal(&'a mtl::Obj),
}

/// An atlas bind: wgpu bind group 1, or the Metal texture/sampler pair with the
/// caller's `BindSpec` slots.
#[derive(Clone, Copy)]
pub(crate) enum FrameAtlas<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::BindGroup),
    #[cfg(target_os = "macos")]
    Metal {
        tex: &'a mtl::Obj,
        sampler: &'a mtl::Obj,
        /// The row's fragment `[[texture(n)]]` slot (`BindSpec::fragment_textures`).
        tex_slot: u32,
        /// The row's fragment `[[sampler(n)]]` slot (`BindSpec::fragment_samplers`).
        sampler_slot: u32,
    },
}

/// A bound instance stream: wgpu vertex slot 0, or the Metal stream at
/// `INSTANCE_STREAM_SLOT` (via `set_instance_stream`, which arms the no-stream
/// draw refusal).
#[derive(Clone, Copy)]
pub(crate) enum FrameStream<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::BufferSlice<'a>),
    #[cfg(target_os = "macos")]
    Metal(&'a mtl::Obj),
}

/// The shared 16-byte frame uniforms: wgpu bind group 0, or the Metal buffer
/// at its `BindSpec` slots (vertex, plus the fragment re-bind `fs_glyph`
/// needs, since Metal argument tables are per stage).
#[derive(Clone, Copy)]
pub(crate) enum FrameUniforms<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::BindGroup),
    #[cfg(target_os = "macos")]
    Metal {
        buf: &'a mtl::Obj,
        /// The vertex-stage `[[buffer(n)]]` slot (`BindSpec::vertex_uniform`).
        vertex_slot: u32,
        /// The fragment-stage `[[buffer(n)]]` slot, when any drawn row reads
        /// the block in its fragment stage (`BindSpec::fragment_buffers`).
        fragment_slot: Option<u32>,
    },
}

/// A texture for the mid-frame copy verbs (scroll shift): a raw
/// `wgpu::Texture`, or a sealed Metal texture so copies can refuse a foreign
/// loss domain.
#[derive(Clone, Copy)]
pub(crate) enum FrameCopyTexture<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::Texture),
    #[cfg(target_os = "macos")]
    Metal(&'a SealedTexture),
}

/// The committed frame — proof the encode was submitted, and (on Metal) the
/// non-blocking completion handle the harvest polls.
#[must_use = "an unsubmitted or unobserved frame commit hides device loss"]
pub(crate) enum SubmittedFrame {
    /// wgpu owns completion tracking; there is nothing further to hold.
    #[cfg(wgpu_arm)]
    Wgpu,
    /// The Metal `Submitted`: wait on it, or poll `try_outcome` (the
    /// completion-handler substitute).
    #[cfg(target_os = "macos")]
    Metal(MtlSubmitted),
}

/// One frame's command encoder on one backend — the object the shared plan
/// walker (`renderer.rs::run_frame_plan`) drives.
pub(crate) enum FrameEncoder<'s> {
    #[cfg(wgpu_arm)]
    Wgpu {
        enc: wgpu::CommandEncoder,
        queue: &'s wgpu::Queue,
    },
    #[cfg(target_os = "macos")]
    Metal(MtlCommandBuffer<'s>),
}

impl<'s> FrameEncoder<'s> {
    /// The live production arm: a wgpu command encoder with the exact
    /// descriptor the pre-seam `encode_frame` created ("aterm-gpu frame").
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu(device: &wgpu::Device, queue: &'s wgpu::Queue, label: &'static str) -> Self {
        Self::Wgpu {
            enc: device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) }),
            queue,
        }
    }

    /// The Metal arm: one command buffer on the session's one queue.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(session: &'s EncodeSession) -> Result<Self, String> {
        Ok(Self::Metal(session.begin()?))
    }

    /// Open one render pass: attach `view` with `load`/Store, apply the optional
    /// scissor, bind the shared uniforms once (in wgpu call order: pass,
    /// scissor, bind group 0).
    pub(crate) fn begin_pass<'e>(
        &'e mut self,
        label: &'static str,
        view: FrameView<'_>,
        load: FrameLoad,
        scissor: Option<(u32, u32, u32, u32)>,
        uniforms: FrameUniforms<'_>,
    ) -> Result<FramePass<'e, 's>, String> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { enc, .. } => {
                let (view, uniform_bg) = match (view, uniforms) {
                    (FrameView::Wgpu(view), FrameUniforms::Wgpu(uniform_bg)) => (view, uniform_bg),
                    #[cfg(multi_arm)]
                    _ => crossed("pass target or uniform block", "the WGPU frame encoder"),
                };
                let load = match load {
                    FrameLoad::Clear(c) => wgpu::LoadOp::Clear(c.wgpu()),
                    FrameLoad::Load => wgpu::LoadOp::Load,
                };
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                if let Some((sx, sy, sw, sh)) = scissor {
                    pass.set_scissor_rect(sx, sy, sw, sh);
                }
                pass.set_bind_group(0, uniform_bg, &[]);
                Ok(FramePass::Wgpu {
                    pass,
                    _s: std::marker::PhantomData,
                })
            }
            #[cfg(target_os = "macos")]
            Self::Metal(cb) => {
                let _ = label; // Metal render encoders carry no pass label.
                let (target, buf, vertex_slot, fragment_slot) = match (view, uniforms) {
                    (
                        FrameView::Metal(target),
                        FrameUniforms::Metal {
                            buf,
                            vertex_slot,
                            fragment_slot,
                        },
                    ) => (target, buf, vertex_slot, fragment_slot),
                    #[cfg(multi_arm)]
                    _ => crossed("pass target or uniform block", "the METAL frame encoder"),
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
        }
    }

    /// Texture-to-texture sub-rect copy (the scroll shift's staged moves). The
    /// Metal arm validates rects off the live textures and refuses an
    /// overlapping same-texture copy.
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
            #[cfg(wgpu_arm)]
            Self::Wgpu { enc, .. } => {
                let (src, dst) = match (src, dst) {
                    (FrameCopyTexture::Wgpu(src), FrameCopyTexture::Wgpu(dst)) => (src, dst),
                    #[cfg(multi_arm)]
                    _ => crossed("copy texture", "the WGPU frame encoder"),
                };
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: src,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: src_origin.0,
                            y: src_origin.1,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: dst,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: dst_origin.0,
                            y: dst_origin.1,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Metal(cb) => {
                let (src, dst) = match (src, dst) {
                    (FrameCopyTexture::Metal(src), FrameCopyTexture::Metal(dst)) => (src, dst),
                    #[cfg(multi_arm)]
                    _ => crossed("copy texture", "the METAL frame encoder"),
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
        }
    }

    /// Commit the frame (wgpu `queue.submit`, or Metal `commit`, whose
    /// `Submitted` the caller waits on or polls).
    pub(crate) fn submit(self) -> SubmittedFrame {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { enc, queue } => {
                queue.submit([enc.finish()]);
                SubmittedFrame::Wgpu
            }
            #[cfg(target_os = "macos")]
            Self::Metal(cb) => SubmittedFrame::Metal(cb.commit()),
        }
    }
}

/// One open frame pass on one backend — the draw-stream vocabulary the plan
/// walker speaks between `begin_pass` and drop.
#[allow(
    clippy::large_enum_variant,
    reason = "one FramePass lives on the stack per render pass (1..6 per frame) \
              and dies before the next opens; boxing wgpu's 1KB RenderPass would \
              put a heap allocation on the keystroke-echo path to save nothing"
)]
pub(crate) enum FramePass<'e, 's> {
    #[cfg(wgpu_arm)]
    Wgpu {
        pass: wgpu::RenderPass<'e>,
        /// Ties the unused-on-wgpu session lifetime so the enum's Metal arm
        /// can borrow its command buffer with the same parameters.
        _s: std::marker::PhantomData<&'s ()>,
    },
    #[cfg(target_os = "macos")]
    Metal(MtlPassEncoder<'e, 's>),
}

impl FramePass<'_, '_> {
    /// `set_pipeline` — the mid-pass pipeline switch (issued on change only;
    /// the walker owns the tracker).
    pub(crate) fn set_pipeline(&mut self, pipeline: FramePipeline<'_>) {
        match (self, pipeline) {
            #[cfg(wgpu_arm)]
            (Self::Wgpu { pass, .. }, FramePipeline::Wgpu(p)) => pass.set_pipeline(p),
            #[cfg(target_os = "macos")]
            (Self::Metal(pass), FramePipeline::Metal(pso)) => pass.set_pipeline(pso),
            #[cfg(multi_arm)]
            _ => crossed("frame pipeline", "an open pass"),
        }
    }

    /// Bind the atlas (wgpu group 1, or the Metal texture/sampler pair at the
    /// row's slots). Issued on change only; both argument tables persist across
    /// pipeline switches.
    pub(crate) fn bind_atlas(&mut self, atlas: FrameAtlas<'_>) {
        match (self, atlas) {
            #[cfg(wgpu_arm)]
            (Self::Wgpu { pass, .. }, FrameAtlas::Wgpu(bg)) => pass.set_bind_group(1, bg, &[]),
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
            #[cfg(multi_arm)]
            _ => crossed("frame atlas", "an open pass"),
        }
    }

    /// Bind the per-draw instance stream: wgpu vertex-buffer slot 0, or
    /// Metal `set_instance_stream` at `INSTANCE_STREAM_SLOT`.
    pub(crate) fn set_stream(&mut self, stream: FrameStream<'_>) {
        match (self, stream) {
            #[cfg(wgpu_arm)]
            (Self::Wgpu { pass, .. }, FrameStream::Wgpu(slice)) => {
                pass.set_vertex_buffer(0, slice);
            }
            #[cfg(target_os = "macos")]
            (Self::Metal(pass), FrameStream::Metal(buf)) => pass.set_instance_stream(buf),
            #[cfg(multi_arm)]
            _ => crossed("frame stream", "an open pass"),
        }
    }

    /// Draw `instances` quads of 6 vertices. The Metal arm can refuse (no
    /// pipeline or no stream); the wgpu arm cannot fail.
    pub(crate) fn draw_quads(&mut self, instances: u32) -> Result<(), String> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { pass, .. } => {
                pass.draw(0..6, 0..instances);
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Metal(pass) => pass.draw_instanced(
                crate::metal::ffi::PrimitiveType::Triangle,
                6,
                instances as usize,
            ),
        }
    }
}

/// Metal arm of `try_read_back`: copy `texture`'s `width` x `height` Rgba8
/// texels into a shared staging buffer and unpack into an
/// `aterm_render::Frame` with the wgpu arm's byte contract:
///
/// * stride [`crate::padded_bytes_per_row`] (256-byte rows, although Metal
///   would accept tight rows);
/// * unpack via [`crate::frame_from_padded_rgba`];
/// * completion by status polling on [`MtlSubmitted`] with a deadline, so a
///   hung command buffer is an `Err`; the first terminal answer feeds the loss
///   latch once.
///
/// The copy validates extent, stride and destination length against the live
/// objects and refuses a foreign loss domain.
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

/// The staging step of [`metal_try_read_back`]: copy the texture into a fresh
/// shared buffer at [`crate::padded_bytes_per_row`], poll to terminal, and
/// return the buffer with its stride. Separate so
/// `the_padding_tail_is_neither_written_nor_consulted` can check the stride and
/// row placement, which `Frame` consumers cannot see.
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
    // aterm_time, not std: the wasm clock guard covers this crate, and the
    // shim is byte-identical on native targets.
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

#[cfg(all(target_os = "macos", test))]
mod tests {
    use super::*;
    use crate::metal::ffi::Device;
    use crate::metal::loss::LossLatch;
    use std::sync::Arc;

    /// Every route outside `metal/` by which a texture minted in loss domain 2
    /// could reach a domain-1 encode or present (alias view, copy destination,
    /// a second swapchain's drawable and alias, cross present) refuses by name.
    /// Forging constructors are private. A CPU-synchronous `replaceRegion:`
    /// upload across domains is allowed, since no queue is involved.
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
        // Hold one pool across the test for `Obj::drop` autoreleases.
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

        // ROUTE A: an alias view of a foreign texture as a render target —
        // the inherited stamp must refuse exactly like the parent's.
        let rogue_alias = rogue
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("alias view");
        let mut cb1 = session1.begin().expect("cb1");
        let err = cb1
            .render_pass(&pass_desc(&rogue_alias))
            .expect_err("an aliased foreign texture must not become a domain-1 target");
        assert!(err.contains("loss domain"), "route A names the seal: {err}");

        // ROUTE B: the alias as a COPY destination (dst-side smuggling).
        let err = cb1
            .copy_texture_to_texture(&native1, &rogue_alias)
            .expect_err("an aliased foreign texture must not become a copy dst");
        assert!(err.contains("loss domain"), "route B names the seal: {err}");

        // Route C: a second swapchain in domain 2, its drawable and alias
        // rendered on session 1, and its present via session 1.
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

        // A CPU-synchronous upload from a domain-1 handle into a domain-2
        // texture is allowed: `replaceRegion:` has no queue. If uploads ever
        // become queue-based, this must start refusing.
        let handle1 = DeviceHandle::Metal(&mint1);
        let wrapped = LayerTexture::Metal(rogue);
        handle1.upload_texture_rows(&wrapped, 2, 2, &[7u8; 64], 32, 8);

        // Positive control, and both sessions' command buffers commit EMPTY
        // and clean — every refusal happened before any encoding.
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

    /// A texture minted by the Metal arm carries the mint's latch, and an alias
    /// view inherits it (pointer identity).
    #[test]
    fn metal_texture_carries_its_mints_latch_and_alias_inherits_it() {
        let Some(mint) = mint() else {
            aterm_spec::host::could_not_run("no Metal device");
        };
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let handle = DeviceHandle::Metal(&mint);
        let tex = handle.create_texture_2d(
            "device-layer stamp probe",
            TexelFormat::Rgba8Unorm,
            4,
            4,
            TexUsage::OFFSCREEN,
            Some(TexelFormat::Rgba8UnormSrgb),
        );
        assert!(Arc::ptr_eq(tex.metal().latch(), mint.latch()));
        let view = tex
            .metal()
            .alias_view(TexelFormat::Rgba8UnormSrgb.metal())
            .expect("newTextureViewWithPixelFormat: of the declared alias pair");
        assert!(
            Arc::ptr_eq(view.latch(), mint.latch()),
            "the sRGB alias view must inherit its texture's loss-domain stamp"
        );
        assert_eq!(
            TexelFormat::Rgba8UnormSrgb.bytes_per_texel(),
            4,
            "the alias pair shares a texel size (the aliasing precondition)"
        );
    }

    /// A Metal resource at a wgpu-only seam, and a wgpu handle at a Metal-only
    /// seam, panic by name ("device layer").
    #[test]
    fn crossing_a_metal_resource_into_a_wgpu_seam_panics_by_name() {
        let Some(mint) = mint() else {
            aterm_spec::host::could_not_run("no Metal device");
        };
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let handle = DeviceHandle::Metal(&mint);
        let tex = handle.create_texture_2d(
            "device-layer crossing probe",
            TexelFormat::R8Unorm,
            2,
            2,
            TexUsage::UPLOADED_SAMPLED,
            None,
        );
        let buf = handle.create_instance_buffer("device-layer crossing probe buf", 16);
        let smp =
            handle.create_sampler("device-layer crossing probe smp", SamplerKind::NearestClamp);
        for (what, f) in [
            (
                "texture",
                Box::new(move || {
                    let _ = tex.wgpu();
                }) as Box<dyn FnOnce()>,
            ),
            (
                "buffer",
                Box::new(move || {
                    let _ = buf.wgpu();
                }),
            ),
            (
                "sampler",
                Box::new(move || {
                    let _ = smp.wgpu();
                }),
            ),
            (
                "device",
                Box::new(move || {
                    let _ = handle.wgpu_device();
                }),
            ),
        ] {
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .expect_err("a Metal resource in a wgpu seam must refuse");
            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
                .unwrap_or_default();
            assert!(
                msg.contains("device layer"),
                "{what}: the refusal must name the device layer, got: {msg}"
            );
        }
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
        let tex = handle.create_texture_2d(
            "device-layer short-upload probe",
            TexelFormat::R8Unorm,
            8,
            8,
            TexUsage::UPLOADED_SAMPLED,
            None,
        );
        let short = [0u8; 8]; // one row where two are named
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle.upload_texture_rows(&tex, 0, 2, &short, 8, 8);
        }))
        .expect_err("a short row band must refuse");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|m| (*m).to_owned()))
            .unwrap_or_default();
        assert!(msg.contains("row-ranged upload"), "got: {msg}");

        let buf = handle.create_instance_buffer("device-layer short-buffer probe", 4);
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

    /// The same RGBA8 texels uploaded on each backend read back as identical
    /// `Frame`s at width 17 (68 bytes per row padded to 256), and both match the
    /// expected packing computed from the source bytes.
    #[test]
    fn the_metal_readback_reproduces_the_wgpu_padding_contract() {
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let Ok(ctx) = crate::GpuContext::new() else {
            aterm_spec::host::could_not_run("no wgpu context");
        };
        let Some(dev) = Device::preferred() else {
            aterm_spec::host::could_not_run("no Metal device");
        };
        let latch = Arc::new(LossLatch::new());
        let session =
            crate::metal::encoder::EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = MetalResourceDevice::new(&dev, Arc::clone(&latch));

        const W: u32 = 17; // 17 * 4 = 68 -> padded 256: the padding is live.
        const H: u32 = 5;
        assert_eq!(
            crate::padded_bytes_per_row(W as usize),
            256,
            "the fixture width must exercise the padding contract"
        );
        let bytes: Vec<u8> = (0..(W * H * 4) as usize)
            .map(|i| ((i * 7 + 13) % 256) as u8)
            .collect();
        let usage = TexUsage {
            sampled: true,
            render: false,
            copy_src: true,
            copy_dst: true,
        };

        // The wgpu arm: the shipped `try_read_back`.
        let wgpu_handle = ctx.device_layer();
        let wt = wgpu_handle.create_texture_2d(
            "w4 padding-contract wgpu",
            TexelFormat::Rgba8Unorm,
            W,
            H,
            usage,
            None,
        );
        wgpu_handle.upload_texture_full(&wt, &bytes, W * 4, W, H);
        let wgpu_frame = ctx
            .try_read_back(wt.wgpu(), W, H)
            .expect("wgpu readback succeeds");

        // The Metal arm: the SAME bytes through `metal_try_read_back`.
        let metal_handle = DeviceHandle::Metal(&mint);
        let mt = metal_handle.create_texture_2d(
            "w4 padding-contract metal",
            TexelFormat::Rgba8Unorm,
            W,
            H,
            usage,
            None,
        );
        metal_handle.upload_texture_full(&mt, &bytes, W * 4, W, H);
        let metal_frame = super::metal_try_read_back(&session, &mint, mt.metal(), W, H)
            .expect("metal readback succeeds");

        // The independent expectation, computed straight from the source.
        let expected: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                ((255 - u32::from(c[3])) << 24)
                    | (u32::from(c[0]) << 16)
                    | (u32::from(c[1]) << 8)
                    | u32::from(c[2])
            })
            .collect();
        assert_eq!(
            (wgpu_frame.width, wgpu_frame.height),
            (W as usize, H as usize)
        );
        assert_eq!(
            (metal_frame.width, metal_frame.height),
            (W as usize, H as usize)
        );
        assert_eq!(
            wgpu_frame.pixels, expected,
            "the wgpu arm's unpack drifted from the packing law"
        );
        assert_eq!(
            metal_frame.pixels, expected,
            "the METAL arm's unpack/stride drifted from the wgpu byte contract"
        );
        assert!(!latch.is_lost(), "a readback is not a device loss");
    }

    /// Padding bytes at widths 17 and 97:
    ///
    ///  * not written: the Metal copy lands each row at its padded base and
    ///    leaves the poisoned tail untouched;
    ///  * not consulted: `frame_from_padded_rgba` gives identical `Frame`s from
    ///    a poisoned and a zeroed tail;
    ///  * both arms' readbacks equal the expected packing.
    #[test]
    fn the_padding_tail_is_neither_written_nor_consulted() {
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let Ok(ctx) = crate::GpuContext::new() else {
            aterm_spec::host::could_not_run("no wgpu context");
        };
        let Some(dev) = Device::preferred() else {
            aterm_spec::host::could_not_run("no Metal device");
        };
        let latch = Arc::new(LossLatch::new());
        let session =
            crate::metal::encoder::EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = MetalResourceDevice::new(&dev, Arc::clone(&latch));
        let usage = TexUsage {
            sampled: true,
            render: false,
            copy_src: true,
            copy_dst: true,
        };

        for (w, h) in [(17u32, 5u32), (97, 3)] {
            let (wu, hu) = (w as usize, h as usize);
            let padded = crate::padded_bytes_per_row(wu);
            assert!(
                padded != wu * 4,
                "width {w} must have LIVE padding for this arm to bite"
            );
            let bytes: Vec<u8> = (0..(wu * hu * 4))
                .map(|i| ((i * 7 + 13) % 256) as u8)
                .collect();
            let expected: Vec<u32> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| {
                    ((255 - u32::from(c[3])) << 24)
                        | (u32::from(c[0]) << 16)
                        | (u32::from(c[1]) << 8)
                        | u32::from(c[2])
                })
                .collect();

            // Metal texture with the pattern.
            let handle = DeviceHandle::Metal(&mint);
            let mt = handle.create_texture_2d(
                "w4-judge padding-tail metal",
                TexelFormat::Rgba8Unorm,
                w,
                h,
                usage,
                None,
            );
            handle.upload_texture_full(&mt, &bytes, w * 4, w, h);

            // The staged copy into a POISONED buffer — the raw staging shape.
            let staging = mint.buffer(padded * hu).expect("staging buffer");
            let poison = vec![0xABu8; padded * hu];
            // SAFETY: fresh shared buffer, exactly sized, no GPU work yet.
            unsafe { crate::metal::ffi::buffer_write(&staging, &poison) };
            let mut cb = session.begin().expect("command buffer");
            cb.copy_texture_to_buffer(mt.metal(), wu, hu, &staging, padded)
                .expect("texture->buffer copy");
            let outcome = cb.commit().wait_outcome();
            assert_eq!(outcome, crate::metal::loss::CbOutcome::Completed);
            // SAFETY: shared storage, command buffer Completed above.
            let raw = unsafe { mtl::buffer_bytes(&staging, padded * hu) };
            for row in 0..hu {
                let base = row * padded;
                assert_eq!(
                    &raw[base..base + wu * 4],
                    &bytes[row * wu * 4..(row + 1) * wu * 4],
                    "width {w}: row {row}'s texel span landed wrong"
                );
                assert!(
                    raw[base + wu * 4..base + padded].iter().all(|&b| b == 0xAB),
                    "width {w}: the copy WROTE into row {row}'s padding tail"
                );
            }

            // Drive the staging helper `metal_try_read_back` uses and check the
            // returned stride and where the rows landed.
            let (buf, stride) =
                super::metal_stage_readback(&session, &mint, mt.metal(), wu, hu).expect("stage");
            assert_eq!(
                stride, padded,
                "width {w}: the production staging broke the 256-byte stride contract"
            );
            // SAFETY: shared storage, staged copy Completed inside the helper.
            let raw2 = unsafe { mtl::buffer_bytes(&buf, stride * hu) };
            for row in 0..hu {
                assert_eq!(
                    &raw2[row * stride..row * stride + wu * 4],
                    &bytes[row * wu * 4..(row + 1) * wu * 4],
                    "width {w}: the production staging landed row {row} off its padded base"
                );
            }

            // The tail is not consulted: poisoned vs zeroed tails unpack
            // identically (this pins the wgpu arm too — one shared spelling).
            let mut zeroed = raw.clone();
            for row in 0..hu {
                zeroed[row * padded + wu * 4..(row + 1) * padded].fill(0);
            }
            let from_poison = crate::frame_from_padded_rgba(&raw, wu, hu, padded);
            let from_zero = crate::frame_from_padded_rgba(&zeroed, wu, hu, padded);
            assert_eq!(
                from_poison.pixels, from_zero.pixels,
                "width {w}: the unpack CONSULTED the padding tail"
            );
            assert_eq!(from_poison.pixels, expected, "width {w}: unpack drifted");

            // End to end, both arms.
            let metal_frame = super::metal_try_read_back(&session, &mint, mt.metal(), w, h)
                .expect("metal readback");
            assert_eq!(
                metal_frame.pixels, expected,
                "width {w}: the METAL end-to-end readback drifted"
            );
            let wgpu_handle = ctx.device_layer();
            let wt = wgpu_handle.create_texture_2d(
                "w4-judge padding-tail wgpu",
                TexelFormat::Rgba8Unorm,
                w,
                h,
                usage,
                None,
            );
            wgpu_handle.upload_texture_full(&wt, &bytes, w * 4, w, h);
            let wgpu_frame = ctx.try_read_back(wt.wgpu(), w, h).expect("wgpu readback");
            assert_eq!(
                wgpu_frame.pixels, expected,
                "width {w}: the WGPU end-to-end readback drifted"
            );
        }
        assert!(!latch.is_lost(), "a readback is not a device loss");
    }
}
