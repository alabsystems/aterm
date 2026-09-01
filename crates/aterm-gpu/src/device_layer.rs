// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DEVICE LAYER — the two-variant backend enum under `renderer.rs`'s
//! resource seams (W3 of `docs/measured/metal-port-map-2026-08-31.md` §5).
//!
//! # Shape and justification
//!
//! A sibling file rather than a module inside `renderer.rs`, for the same
//! reason THE PIPELINE TABLE is one: `renderer.rs` is 18.5k lines, and the
//! map's recommendation (§4) is "an internal device layer shaped like the
//! pipeline table" — one spec surface, two builders, parity tests per axis.
//! The table owns WHAT a pipeline is; this layer owns HOW a resource comes to
//! exist. Everything here is `pub(crate)`; nothing leaks into the gui-facing
//! surface.
//!
//! It wraps EXACTLY the resource classes the map's §3 inventory names, no
//! more: instance/vertex buffers (write-from-0 + geometric grow), textures
//! (create + full/row-ranged upload), uniform buffers, texture views (the
//! Unorm/sRGB alias), samplers (the two clamp presets). Bind groups are
//! deliberately ABSENT — Metal has no bind-group object; the map ports each
//! of the 7 layouts to 1-3 `set*` calls per draw stream, which is W4's encode
//! job, not a resource.
//!
//! # The wgpu variant is the live one
//!
//! Until W6's flip, every production construction site routes through
//! [`DeviceHandle::Wgpu`] and the behavior is IDENTICAL to the direct wgpu
//! calls this layer replaced — same descriptors, same labels, same usage
//! bits, same upload layouts. The proof is the untouched paint-parity suite
//! plus the whole aterm-gui suite running through the routed renderer. The
//! Metal variant is reached by tests only (the W3/W4 differential ladder and
//! the atlas-grow parity), and its resources are minted through
//! [`crate::metal::resources::MetalResourceDevice`] so every texture carries
//! its loss-domain stamp (the W1 judge's two-latch residual, sealed
//! structurally in `metal::resources`).
//!
//! # Crossing back out
//!
//! Seams that are still wgpu-only until later waves (bind-group creation,
//! `encode_frame`, the present path) take the live resource out of the enum
//! via [`LayerTexture::wgpu`]/[`LayerBuffer::wgpu`] and friends. On a Metal
//! resource those accessors PANIC BY NAME — that panic is the W6 flip's todo
//! list, enumerable by grep, and until the flip it is unreachable from
//! production because production only constructs the Wgpu variant.
//!
//! The W3 header recorded ONE production resource outside that greppable
//! list: the scroll-shift scratch in `renderer.rs::shift_offscreen_band_px`.
//! W4 item 3 ROUTED it — the scratch is a [`LayerTexture`] and its two staged
//! copies run through [`FrameEncoder::copy_texture_rect`] — so the greppable
//! crossing list is once again the COMPLETE W6 flip todo.

#[cfg(target_os = "macos")]
use crate::metal::ffi as mtl;
#[cfg(target_os = "macos")]
use crate::metal::resources::{MetalResourceDevice, SealedTexture, SharedBuffer};

/// The texel formats the map's §3 inventory uses — exactly the six the Metal
/// foundation models (`metal::ffi::PixelFormat`), spelled backend-neutrally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TexelFormat {
    R8Unorm,
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    Rgba16Float,
}

impl TexelFormat {
    /// Bytes one texel occupies in a linear (buffer/upload) layout.
    #[allow(
        dead_code,
        reason = "consumed by the W3/W4 differential-ladder tests and by the W6 flip; \
                  the plain lib target has no caller until its wave routes it"
    )]
    pub(crate) const fn bytes_per_texel(self) -> u32 {
        match self {
            Self::R8Unorm => 1,
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm | Self::Bgra8UnormSrgb => 4,
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

    /// Recover the neutral spelling from a wgpu format — for call sites whose
    /// format arrives resolved (e.g. `format_plan::offscreen_format`). `None`
    /// for a format outside the map's closed set of six, which no production
    /// texture uses.
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
            Self::Bgra8UnormSrgb => mtl::PixelFormat::Bgra8UnormSrgb,
            Self::Rgba16Float => mtl::PixelFormat::Rgba16Float,
        }
    }
}

/// What a texture is FOR, backend-neutrally. Maps 1:1 onto the wgpu usage
/// bits the pre-layer descriptors spelled (so the wgpu arm is byte-identical
/// to what it replaced) and onto Metal's smaller usage vocabulary (Metal has
/// no copy bits — blits need none, and `replaceRegion:` needs only non-Private
/// storage, which every shared-mode texture has).
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

    /// The offscreen-family shape: drawn into, blitted from, scroll-shifted
    /// back into, and sampled by the present blit
    /// (`RENDER_ATTACHMENT | COPY_SRC | COPY_DST | TEXTURE_BINDING`).
    pub(crate) const OFFSCREEN: Self = Self {
        sampled: true,
        render: true,
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

/// The two sampler presets the renderer builds (map §3: "4 samplers
/// (nearest/linear clamp)" — two presets, built at up to four sites). The
/// wgpu arm reproduces the exact descriptors the pre-layer sites spelled
/// (clamp-to-edge everywhere, `MipmapFilterMode::Nearest` on the never-mipped
/// textures); the Metal arm is the matching `SamplerDesc` preset, already
/// GPU-verified equivalent by the W1 foundation tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SamplerKind {
    /// Exact-texel reads (glyph/deco/sprite atlases, the blit).
    NearestClamp,
    /// Smooth resampling (bloom upsample, shimmer, tray card).
    LinearClamp,
}

/// One backend, borrowed — the handle a routed construction site does its
/// resource work through. Production hands out only the Wgpu arm
/// ([`crate::GpuContext::device_layer`]); the Metal arm exists for the
/// differential ladder and becomes the live one at W6.
#[derive(Clone, Copy)]
pub(crate) enum DeviceHandle<'a> {
    #[cfg(wgpu_arm)]
    Wgpu {
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
    },
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W3/W4 differential-ladder tests and by the W6 \
                  flip; the plain lib target has no Metal caller until then"
    )]
    Metal(&'a MetalResourceDevice),
}

impl DeviceHandle<'_> {
    /// Create a 2-D texture. `alias` declares a format the texture may later
    /// be viewed as ([`LayerTexture::alias_view`]): the wgpu arm lists it in
    /// `view_formats` (required there), the Metal arm needs no declaration for
    /// the one alias pair in use — Unorm<->sRGB is Metal's documented
    /// sRGB-variant exemption (measured in `metal::ffi::texture_view`).
    pub(crate) fn create_texture_2d(
        &self,
        label: &'static str,
        format: TexelFormat,
        width: u32,
        height: u32,
        usage: TexUsage,
        alias: Option<TexelFormat>,
    ) -> LayerTexture {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => {
                let view_formats: &[wgpu::TextureFormat] = match alias {
                    Some(a) => &[a.wgpu()],
                    None => &[],
                };
                LayerTexture::Wgpu(device.create_texture(&wgpu::TextureDescriptor {
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
                }))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => LayerTexture::Metal(
                mint.texture_2d(
                    format.metal(),
                    width as usize,
                    height as usize,
                    usage.metal(),
                )
                .unwrap_or_else(|e| panic!("device layer: {label}: {e}")),
            ),
        }
    }

    /// Full-extent upload of level 0 — `queue.write_texture` from origin
    /// (0,0) / `replaceRegion:` over the whole `width` x `height`. `bytes`
    /// is tightly packed at `bytes_per_row`.
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

    /// Row-ranged upload: rows `[y0, y0 + rows)` of level 0, full width — the
    /// atlas-grow in-place append (map §3: "atlas grow via row-ranged
    /// writes"). `bytes` holds exactly the band, tightly packed.
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
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { queue, .. } => {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex.wgpu(),
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
            Self::Metal(_) => {
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
                // SAFETY: the mint only creates shared-storage 2-D textures;
                // the band length is asserted just above against the stride,
                // and `replaceRegion:` copies synchronously before returning.
                unsafe { tex.metal().upload(region, bytes, bytes_per_row as usize) };
            }
        }
    }

    /// A per-frame instance-stream buffer (`VERTEX | COPY_DST` on wgpu; a
    /// shared-storage `MTLBuffer` on Metal, where vertex-stream use needs no
    /// usage declaration). Size 0 is legal on the wgpu arm (the lazy streams
    /// start empty); the Metal arm floors at 1 byte because
    /// `newBufferWithLength:0` returns nil.
    pub(crate) fn create_instance_buffer(&self, label: &'static str, size: u64) -> LayerBuffer {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => {
                LayerBuffer::Wgpu(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => LayerBuffer::Metal(SharedBuffer::new(
                mint.buffer(usize::try_from(size).unwrap_or(usize::MAX).max(1))
                    .unwrap_or_else(|e| panic!("device layer: {label}: {e}")),
            )),
        }
    }

    /// A uniform buffer (`UNIFORM | COPY_DST` on wgpu; a shared-storage
    /// `MTLBuffer` on Metal).
    pub(crate) fn create_uniform_buffer(&self, label: &'static str, size: u64) -> LayerBuffer {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => {
                LayerBuffer::Wgpu(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => LayerBuffer::Metal(SharedBuffer::new(
                mint.buffer(usize::try_from(size).unwrap_or(usize::MAX).max(1))
                    .unwrap_or_else(|e| panic!("device layer: {label}: {e}")),
            )),
        }
    }

    /// Write `bytes` into `buf` from offset 0 — the ONLY write shape the
    /// renderer uses (map §3: "offset-0-only writes suffice; every stream
    /// writes from 0").
    ///
    /// # Safety
    /// The Wgpu arm has no precondition (wgpu stages the write itself). The
    /// Metal arm memcpys into shared storage the GPU may also read: the
    /// caller must ensure no in-flight GPU work reads `buf` — the discipline
    /// every Metal-arm caller (the differential/parity tests) upholds by
    /// waiting out its command buffers before rewriting a stream.
    pub(crate) unsafe fn write_buffer_from_zero(&self, buf: &LayerBuffer, bytes: &[u8]) {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { queue, .. } => queue.write_buffer(buf.wgpu(), 0, bytes),
            #[cfg(target_os = "macos")]
            Self::Metal(_) => {
                assert!(
                    bytes.len() <= mtl::buffer_length(buf.metal()),
                    "device layer: write of {} bytes into a {}-byte MTLBuffer",
                    bytes.len(),
                    mtl::buffer_length(buf.metal())
                );
                // SAFETY: length asserted above; exclusivity is the caller's
                // documented precondition.
                unsafe { mtl::buffer_write(buf.metal(), bytes) }
            }
        }
    }

    /// One of the two clamp samplers.
    pub(crate) fn create_sampler(&self, label: &'static str, kind: SamplerKind) -> LayerSampler {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => {
                let filter = match kind {
                    SamplerKind::NearestClamp => wgpu::FilterMode::Nearest,
                    SamplerKind::LinearClamp => wgpu::FilterMode::Linear,
                };
                LayerSampler::Wgpu(device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some(label),
                    address_mode_u: wgpu::AddressMode::ClampToEdge,
                    address_mode_v: wgpu::AddressMode::ClampToEdge,
                    mag_filter: filter,
                    min_filter: filter,
                    mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                    ..Default::default()
                }))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => {
                let desc = match kind {
                    SamplerKind::NearestClamp => mtl::SamplerDesc::NEAREST_CLAMP,
                    SamplerKind::LinearClamp => mtl::SamplerDesc::LINEAR_CLAMP,
                };
                LayerSampler::Metal(
                    mint.sampler(desc)
                        .unwrap_or_else(|e| panic!("device layer: {label}: {e}")),
                )
            }
        }
    }

    /// The live wgpu device — for construction seams that stay wgpu-typed
    /// until their own wave (bind-group layouts, bind groups, pipelines,
    /// shader modules). Panics by name on the Metal arm, exactly like
    /// [`LayerTexture::wgpu`].
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu_device(&self) -> &wgpu::Device {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => device,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL handle reached a wgpu-only construction                  seam (bind-group layout / pipeline / shader module) — that                  seam's wave has not routed it yet"
            ),
        }
    }

    /// The device's buffer-size ceiling, for the geometric-grow cap.
    pub(crate) fn max_buffer_size(&self) -> u64 {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => device.limits().max_buffer_size,
            #[cfg(target_os = "macos")]
            Self::Metal(mint) => mint.device().max_buffer_length() as u64,
        }
    }

    /// The device's 2-D texture dimension ceiling, for the atlas clamps.
    pub(crate) fn max_texture_dim(&self) -> u32 {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu { device, .. } => device.limits().max_texture_dimension_2d,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => {
                // Every Apple-silicon family (Apple4+) supports 16384; the
                // foundation targets nothing older. Metal exposes no direct
                // property; the family floor is the honest constant.
                16384
            }
        }
    }
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
    /// The live wgpu texture — the crossing into seams that stay wgpu-typed
    /// until their own wave (bind groups, encode, present). Panics by name on
    /// the Metal variant: that panic is unreachable from production until the
    /// W6 flip re-routes those seams.
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu(&self) -> &wgpu::Texture {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(t) => t,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL texture reached a wgpu-only seam \
                 (bind group / encode / present) — that seam's wave has not \
                 routed it yet; see the W6 flip list in device_layer.rs"
            ),
        }
    }

    /// [`Self::wgpu`], by value.
    #[cfg(wgpu_arm)]
    pub(crate) fn into_wgpu(self) -> wgpu::Texture {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(t) => t,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL texture reached a wgpu-only seam \
                 (bind group / encode / present) — that seam's wave has not \
                 routed it yet; see the W6 flip list in device_layer.rs"
            ),
        }
    }

    /// The sealed Metal texture. Panics by name on the wgpu variant.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &SealedTexture {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(_) => panic!(
                "device layer: a WGPU texture reached a Metal-only seam — \
                 the caller mixed handles across backends"
            ),
            Self::Metal(t) => t,
        }
    }

    /// The texture's texel width, on either backend (the scroll-shift
    /// scratch's reuse key).
    pub(crate) fn width(&self) -> u32 {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(t) => t.width(),
            #[cfg(target_os = "macos")]
            Self::Metal(t) => t.width() as u32,
        }
    }

    /// The texture's texel height, on either backend.
    pub(crate) fn height(&self) -> u32 {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(t) => t.height(),
            #[cfg(target_os = "macos")]
            Self::Metal(t) => t.height() as u32,
        }
    }

    /// A view of this texture as `format` — the Unorm/sRGB alias (the FORMAT
    /// LAW). On wgpu the format must have been declared at creation
    /// (`alias`); on Metal the sRGB pair vends exempt from declaration and
    /// the view INHERITS the texture's loss-domain stamp.
    #[allow(
        dead_code,
        reason = "consumed by the W3/W4 differential-ladder tests and by the W6 flip; \
                  the plain lib target has no caller until its wave routes it"
    )]
    pub(crate) fn alias_view(&self, format: TexelFormat) -> LayerTextureView {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(t) => LayerTextureView::Wgpu(t.create_view(&wgpu::TextureViewDescriptor {
                format: Some(format.wgpu()),
                ..Default::default()
            })),
            #[cfg(target_os = "macos")]
            Self::Metal(t) => LayerTextureView::Metal(
                t.alias_view(format.metal())
                    .expect("newTextureViewWithPixelFormat: of the declared alias pair"),
            ),
        }
    }
}

/// A texture VIEW on whichever backend made it. On Metal a view IS a sealed
/// texture (same object kind, inherited stamp), which is exactly how the
/// encoder consumes it.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "consumed by the W3/W4 differential-ladder tests and by the W6 flip; \
                  the plain lib target has no caller until its wave routes it"
)]
pub(crate) enum LayerTextureView {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::TextureView),
    #[cfg(target_os = "macos")]
    Metal(SealedTexture),
}

#[allow(
    dead_code,
    reason = "consumed by the W3/W4 differential-ladder tests and by the W6 flip; \
                  the plain lib target has no caller until its wave routes it"
)]
impl LayerTextureView {
    /// The live wgpu view — same crossing contract as [`LayerTexture::wgpu`].
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu(&self) -> &wgpu::TextureView {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(v) => v,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL texture view reached a wgpu-only seam \
                 — that seam's wave has not routed it yet"
            ),
        }
    }

    /// The sealed Metal view. Panics by name on the wgpu variant.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &SealedTexture {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(_) => panic!(
                "device layer: a WGPU texture view reached a Metal-only seam \
                 — the caller mixed handles across backends"
            ),
            Self::Metal(v) => v,
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
            #[cfg(wgpu_arm)]
            Self::Wgpu(b) => b,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL buffer reached a wgpu-only seam \
                 (bind group / encode / present) — that seam's wave has not \
                 routed it yet; see the W6 flip list in device_layer.rs"
            ),
        }
    }

    /// [`Self::wgpu`], by value.
    #[cfg(wgpu_arm)]
    pub(crate) fn into_wgpu(self) -> wgpu::Buffer {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(b) => b,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL buffer reached a wgpu-only seam \
                 (bind group / encode / present) — that seam's wave has not \
                 routed it yet; see the W6 flip list in device_layer.rs"
            ),
        }
    }

    /// `wgpu::Buffer::slice`, forwarded — so the pre-existing
    /// `.buf.slice(..)` call sites in the (untouched, W5) present path keep
    /// compiling verbatim over the routed field.
    #[cfg(wgpu_arm)]
    pub(crate) fn slice<S: std::ops::RangeBounds<wgpu::BufferAddress>>(
        &self,
        bounds: S,
    ) -> wgpu::BufferSlice<'_> {
        self.wgpu().slice(bounds)
    }

    /// The Metal buffer object. Panics by name on the wgpu variant.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &mtl::Obj {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(_) => panic!(
                "device layer: a WGPU buffer reached a Metal-only seam — \
                 the caller mixed handles across backends"
            ),
            Self::Metal(b) => b.obj(),
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

#[allow(
    dead_code,
    reason = "consumed by the W3/W4 differential-ladder tests and by the W6 flip; \
                  the plain lib target has no caller until its wave routes it"
)]
impl LayerSampler {
    /// The live wgpu sampler — same crossing contract as
    /// [`LayerTexture::wgpu`].
    #[cfg(wgpu_arm)]
    pub(crate) fn wgpu(&self) -> &wgpu::Sampler {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(s) => s,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL sampler reached a wgpu-only seam — \
                 that seam's wave has not routed it yet"
            ),
        }
    }

    /// [`Self::wgpu`], by value.
    #[cfg(wgpu_arm)]
    pub(crate) fn into_wgpu(self) -> wgpu::Sampler {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(s) => s,
            #[cfg(target_os = "macos")]
            Self::Metal(_) => panic!(
                "device layer: a METAL sampler reached a wgpu-only seam — \
                 that seam's wave has not routed it yet"
            ),
        }
    }

    /// The Metal sampler state. Panics by name on the wgpu variant.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal(&self) -> &mtl::Obj {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(_) => panic!(
                "device layer: a WGPU sampler reached a Metal-only seam — \
                 the caller mixed handles across backends"
            ),
            Self::Metal(s) => s,
        }
    }
}

// ======================================================================
// W4 — THE ABSTRACT ENCODE SEAM (map §5 W4; the verbs under
// `renderer.rs::run_frame_plan`).
//
// The same two-variant discipline as the resource layer above, extended to
// the ENCODE vocabulary the map's §2 pass graph measured: open a pass
// (Clear-or-Load, one optional scissor, the shared 16-byte uniforms bound
// once), switch pipeline/atlas only on change, bind one instance stream,
// draw instanced quads, copy texture rects mid-frame, submit. The Wgpu arm
// forwards to EXACTLY the wgpu calls `encode_frame`'s macro ladder issued
// before this seam existed — same labels, same descriptor literals, same
// call order — so the live path is unchanged. The Metal arm forwards to the
// W1 encoder (`metal::encoder`) verbatim: no new FFI, and it inherits the
// W3 loss-domain seal (a foreign-domain target/copy refuses inside
// `render_pass`/`copy_texture_sub_rect`) plus the W1 draw-state refusals
// (no-pipeline draw, no-stream instanced draw, overlapping self-copy).
//
// Handles come in per-arm pairs; crossing a handle into the wrong arm
// panics BY NAME ("device layer"), exactly like `LayerTexture::wgpu` — the
// crossing guards are armed in the tests below.
// ======================================================================

#[cfg(target_os = "macos")]
use crate::metal::encoder::{
    CommandBuffer as MtlCommandBuffer, EncodeSession, PassEncoder as MtlPassEncoder,
    RenderPassDesc as MtlRenderPassDesc, StoreAction, Submitted as MtlSubmitted,
};
#[cfg(target_os = "macos")]
use crate::metal::ffi::LoadAction;

/// THE FLIP's neutral f64 colour quadruple — `wgpu::Color`'s exact shape,
/// spelled without the crate so the shared frame planner compiles on the
/// wgpu-free macOS production build. The Wgpu arm converts loss-lessly at the
/// encode boundary (`ClearColor4::wgpu`); the Metal arm builds its
/// `MTLClearColor` from the same four doubles, as it always did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ClearColor4 {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
    pub(crate) a: f64,
}

impl ClearColor4 {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the frame-plan tests spell their loads with these; the \
                      production planner derives every clear from theme_color_alpha"
        )
    )]
    pub(crate) const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    #[allow(
        dead_code,
        reason = "kept beside BLACK as the two canonical clears; the frame-plan \
                  spellings reach for whichever a case needs"
    )]
    pub(crate) const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
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

/// What happens to the pass target's texels before the first draw — the
/// backend-neutral spelling of `wgpu::LoadOp<wgpu::Color>` restricted to the
/// two states the frame ladder uses (map §2: "pass 0 Clear-or-Load, later
/// passes Load"). Carries the f64 quadruple on BOTH arms so the
/// Wgpu arm round-trips the exact value it was handed and the Metal arm's
/// `MTLClearColor` is built from the same four doubles.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FrameLoad {
    /// Clear to this colour (linear for an sRGB-typed view, raw for Unorm —
    /// the call site's `theme_color_alpha` already speaks the view's space).
    Clear(ClearColor4),
    /// Keep the texels the previous pass/frame stored.
    Load,
}

/// A pass TARGET view on one backend: the offscreen's Unorm or sRGB-alias
/// view. On Metal a view IS a sealed texture (inherited loss-domain stamp),
/// which is exactly what `render_pass` seals against.
#[derive(Clone, Copy)]
pub(crate) enum FrameView<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::TextureView),
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    Metal(&'a SealedTexture),
}

/// A render pipeline on one backend.
#[derive(Clone, Copy)]
pub(crate) enum FramePipeline<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::RenderPipeline),
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    Metal(&'a mtl::Obj),
}

/// An ATLAS bind on one backend: wgpu's bind group 1 (texture + sampler), or
/// the Metal texture/sampler pair with its `BindSpec` slots spelled by the
/// caller (the table's `binds` column — the indices are DATA here, never this
/// module's opinion).
#[derive(Clone, Copy)]
pub(crate) enum FrameAtlas<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::BindGroup),
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    Metal {
        tex: &'a mtl::Obj,
        sampler: &'a mtl::Obj,
        /// The row's fragment `[[texture(n)]]` slot (`BindSpec::fragment_textures`).
        tex_slot: u32,
        /// The row's fragment `[[sampler(n)]]` slot (`BindSpec::fragment_samplers`).
        sampler_slot: u32,
    },
}

/// A bound instance stream on one backend: wgpu vertex-buffer slot 0, or the
/// Metal stream at `INSTANCE_STREAM_SLOT` (via the W1 encoder's dedicated
/// `set_instance_stream`, which is what arms the no-stream draw refusal).
#[derive(Clone, Copy)]
pub(crate) enum FrameStream<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::BufferSlice<'a>),
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    Metal(&'a mtl::Obj),
}

/// The shared 16-byte frame uniforms on one backend: wgpu's bind group 0, or
/// the Metal buffer with its `BindSpec`-derived stage slots (vertex
/// `[[buffer(vu)]]`, plus the fragment re-bind `fs_glyph` needs — Metal
/// argument tables are per-stage, so one buffer costs two binds there).
#[derive(Clone, Copy)]
pub(crate) enum FrameUniforms<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::BindGroup),
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    Metal {
        buf: &'a mtl::Obj,
        /// The vertex-stage `[[buffer(n)]]` slot (`BindSpec::vertex_uniform`).
        vertex_slot: u32,
        /// The fragment-stage `[[buffer(n)]]` slot, when any drawn row reads
        /// the block in its fragment stage (`BindSpec::fragment_buffers`).
        fragment_slot: Option<u32>,
    },
}

/// A texture on one backend for the mid-frame COPY verbs (scroll shift). The
/// Wgpu side is a raw `wgpu::Texture` (the offscreen family stays wgpu-typed
/// until W5/W6); the Metal side is sealed, so the copy verbs can refuse a
/// foreign loss domain.
#[derive(Clone, Copy)]
pub(crate) enum FrameCopyTexture<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(&'a wgpu::Texture),
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    Metal(&'a SealedTexture),
}

/// The committed frame — proof the encode was submitted, and (on Metal) the
/// non-blocking completion handle the harvest polls.
#[must_use = "an unsubmitted or unobserved frame commit hides device loss"]
pub(crate) enum SubmittedFrame {
    /// wgpu owns completion tracking; there is nothing further to hold.
    #[cfg(wgpu_arm)]
    Wgpu,
    /// The W1 `Submitted`: wait on it, or poll `try_outcome` (the map's
    /// completion-handler substitute).
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "waited on by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target only ever commits the Wgpu arm"
    )]
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

    /// The Metal arm: one W1 command buffer on the session's ONE queue.
    #[cfg(target_os = "macos")]
    #[allow(
        dead_code,
        reason = "constructed by the W4 full-frame differential and by the W6 flip; \
                  the plain lib target has no Metal caller until then"
    )]
    pub(crate) fn metal(session: &'s EncodeSession) -> Result<Self, String> {
        Ok(Self::Metal(session.begin()?))
    }

    /// Open one render pass: attach `view` with `load`/Store, apply the
    /// optional scissor, bind the shared frame uniforms once. Argument order
    /// IS the wgpu call order the ladder used (pass, then scissor, then bind
    /// group 0); the Metal arm opens the W1 pass with the scissor in its
    /// descriptor and then binds the uniform block at its `BindSpec` slots.
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
                let FrameView::Wgpu(view) = view else {
                    panic!(
                        "device layer: a METAL pass target reached the WGPU frame \
                         encoder — the caller mixed handles across backends"
                    );
                };
                let FrameUniforms::Wgpu(uniform_bg) = uniforms else {
                    panic!(
                        "device layer: METAL frame uniforms reached the WGPU frame \
                         encoder — the caller mixed handles across backends"
                    );
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
                let FrameView::Metal(target) = view else {
                    panic!(
                        "device layer: a WGPU pass target reached the METAL frame \
                         encoder — the caller mixed handles across backends"
                    );
                };
                let FrameUniforms::Metal {
                    buf,
                    vertex_slot,
                    fragment_slot,
                } = uniforms
                else {
                    panic!(
                        "device layer: WGPU frame uniforms reached the METAL frame \
                         encoder — the caller mixed handles across backends"
                    );
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

    /// Texture→texture sub-rect copy — the scroll shift's two staged moves.
    /// The Wgpu arm issues the exact `copy_texture_to_texture` calls the
    /// pre-seam `shift_offscreen_band_px` issued; the Metal arm routes through
    /// the W1 verb, which validates rects off the live textures and REFUSES an
    /// overlapping same-texture copy (Metal documents it undefined).
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
                let (FrameCopyTexture::Wgpu(src), FrameCopyTexture::Wgpu(dst)) = (src, dst) else {
                    panic!(
                        "device layer: a METAL copy texture reached the WGPU frame \
                         encoder — the caller mixed handles across backends"
                    );
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
                let (FrameCopyTexture::Metal(src), FrameCopyTexture::Metal(dst)) = (src, dst)
                else {
                    panic!(
                        "device layer: a WGPU copy texture reached the METAL frame \
                         encoder — the caller mixed handles across backends"
                    );
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

    /// Commit the frame. Wgpu: `queue.submit([enc.finish()])`, exactly the
    /// ladder's single submit. Metal: the W1 `commit`, whose `Submitted`
    /// handle the caller waits on or polls.
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
            #[cfg(target_os = "macos")]
            _ => panic!(
                "device layer: a frame PIPELINE crossed backends inside one pass \
                 — the caller mixed handles"
            ),
        }
    }

    /// Bind the atlas — wgpu bind group 1, or the Metal fragment
    /// texture/sampler pair at the row's `BindSpec` slots. Issued on change
    /// only; both argument tables persist across pipeline switches, so the
    /// dedup is sound on either arm.
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
            #[cfg(target_os = "macos")]
            _ => panic!(
                "device layer: a frame ATLAS crossed backends inside one pass \
                 — the caller mixed handles"
            ),
        }
    }

    /// Bind the per-draw instance stream — wgpu vertex-buffer slot 0, or the
    /// W1 `set_instance_stream` at `INSTANCE_STREAM_SLOT`.
    pub(crate) fn set_stream(&mut self, stream: FrameStream<'_>) {
        match (self, stream) {
            #[cfg(wgpu_arm)]
            (Self::Wgpu { pass, .. }, FrameStream::Wgpu(slice)) => {
                pass.set_vertex_buffer(0, slice);
            }
            #[cfg(target_os = "macos")]
            (Self::Metal(pass), FrameStream::Metal(buf)) => pass.set_instance_stream(buf),
            #[cfg(target_os = "macos")]
            _ => panic!(
                "device layer: a frame STREAM crossed backends inside one pass \
                 — the caller mixed handles"
            ),
        }
    }

    /// The frame ladder's one draw shape: `instances` quads of 6 vertices
    /// (`draw(0..6, 0..n)` / `drawPrimitives(Triangle, 6, n)`). The Metal arm
    /// can refuse (the W1 no-pipeline / no-stream guards); the wgpu arm
    /// cannot fail.
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

/// W4 item 2 — `try_read_back`'s METAL ARM: copy `texture`'s `width` x
/// `height` Rgba8 texels into a shared staging buffer and unpack them into an
/// `aterm_render::Frame`, reproducing the wgpu arm's byte contract EXACTLY:
///
/// * the staging stride is [`crate::padded_bytes_per_row`] — the SAME 256-byte
///   row alignment wgpu imposes, even though Metal itself would accept the
///   tight stride (the stride bug's cautionary sibling, map §3: the Frame
///   byte contract must match, not merely the final pixels);
/// * the unpack is [`crate::frame_from_padded_rgba`] — the one shared
///   spelling of the `0xTTRRGGBB` transmittance packing;
/// * completion is STATUS POLLING on the W1 [`MtlSubmitted`] (the map's
///   substitute for `addCompletedHandler:`), bounded by a deadline so a hung
///   command buffer is an `Err`, not a hang — and the poll's first terminal
///   answer feeds the loss latch exactly once, so device loss propagates the
///   same way the wgpu arm's failed `map_async` does.
///
/// The copy itself is the W1 `copy_texture_to_buffer`, which validates the
/// extent, the format-derived minimum stride, and the destination length off
/// the LIVE objects, and refuses a foreign loss domain.
#[cfg(target_os = "macos")]
#[allow(
    dead_code,
    reason = "called by the W4 full-frame differential (test cfg) and by the W6 \
              flip's render_input; the plain lib target keeps the wgpu arm live"
)]
pub(crate) fn metal_try_read_back(
    session: &EncodeSession,
    mint: &MetalResourceDevice,
    texture: &SealedTexture,
    width: u32,
    height: u32,
) -> Result<aterm_render::Frame, String> {
    let (w, h) = (width as usize, height as usize);
    let (readback, stride) = metal_stage_readback(session, mint, texture, w, h)?;
    // SAFETY: `readback` is shared storage sized `stride * h`, and
    // `metal_stage_readback` returned only after the command buffer that
    // wrote it reported Completed.
    let bytes = unsafe { mtl::buffer_bytes(&readback, stride * h) };
    Ok(crate::frame_from_padded_rgba(&bytes, w, h, stride))
}

/// The ONE staging spelling under [`metal_try_read_back`]: copy the texture
/// into a fresh shared buffer at the [`crate::padded_bytes_per_row`] stride,
/// poll to a terminal outcome, and return the buffer WITH the stride it was
/// written at.
///
/// Split out (rather than inlined above) so the padding CONTRACT is armable:
/// a coherent tight-stride rewrite of the readback — same `Frame`, silently
/// divergent staging shape, the stride bug's sibling the map warns about —
/// is observationally invisible through `Frame` consumers, and a W4-judge
/// plant proved the whole suite stays green under one. The judge's
/// `the_padding_tail_is_neither_written_nor_consulted` therefore drives THIS
/// function and asserts both the returned stride and where the copy actually
/// landed the rows, so the drift has somewhere to die.
#[cfg(target_os = "macos")]
pub(crate) fn metal_stage_readback(
    session: &EncodeSession,
    mint: &MetalResourceDevice,
    texture: &SealedTexture,
    w: usize,
    h: usize,
) -> Result<(mtl::Obj, usize), String> {
    let padded = crate::padded_bytes_per_row(w);
    let readback = mint.buffer(padded * h.max(1))?;
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

    /// THE LAUNDERING PROBE (the W3 judge's follow-up to the two-latch
    /// probe): every route by which a texture minted in loss domain 2 could
    /// reach a domain-1 encode or present THROUGH CONSTRUCTORS THIS CRATE
    /// EXPOSES OUTSIDE `metal/` — the alias view, the copy destination, a
    /// second swapchain's drawable and its alias, the cross present — must
    /// refuse by name. The two forging constructors are unspeakable here by
    /// visibility (`SealedTexture::from_parts` is `pub(super)`, the fields
    /// are private — both verified as compile errors when this probe was
    /// cut), so runtime refusal of every REACHABLE route closes the set. The
    /// one deliberate non-refusal is pinned at the end: a CPU-synchronous
    /// `replaceRegion:` upload crosses domains legally because no queue is
    /// involved — there is nothing to cross-wire.
    #[test]
    fn no_texture_launders_across_loss_domains() {
        use crate::metal::encoder::{EncodeSession, RenderPassDesc, StoreAction};
        use crate::metal::ffi::{
            ClearColor, LoadAction, PixelFormat, TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
        };
        use crate::metal::swapchain::{Swapchain, SwapchainConfig};

        let Some(dev) = Device::system_default() else {
            eprintln!("SKIP: no Metal device");
            return;
        };
        // The house convention for first-party Metal tests (see the ffi
        // module docs and every swapchain/encoder test): hold one pool across
        // the test so `Obj::drop`-time autoreleases have somewhere to land —
        // measured under OBJC_DEBUG_MISSING_POOLS=YES, this takes the
        // first-party unpooled count to zero.
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

        // ROUTE C: a second swapchain in domain 2 — its drawable rendered on
        // session 1, the drawable's alias likewise, and its present via
        // session 1.
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

        // THE PINNED NON-HAZARD: a CPU-synchronous upload through a domain-1
        // handle into a domain-2 texture is allowed — `replaceRegion:` has no
        // queue, so there is no ordering to cross-wire. If this ever starts
        // refusing, the seal grew past its hazard; if uploads ever become
        // queue-based (a blit-encoder staging path), THIS is the line that
        // must start refusing instead.
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
        let dev = Device::system_default()?;
        Some(MetalResourceDevice::new(&dev, Arc::new(LossLatch::new())))
    }

    /// The seal's raw material: a texture minted by the layer's Metal arm
    /// carries the mint's latch, and an alias view INHERITS it (one storage,
    /// one loss domain) — pointer identity, not equality.
    #[test]
    fn metal_texture_carries_its_mints_latch_and_alias_inherits_it() {
        let Some(mint) = mint() else {
            eprintln!("SKIP: no Metal device");
            return;
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
        let view = tex.alias_view(TexelFormat::Rgba8UnormSrgb);
        assert!(
            Arc::ptr_eq(view.metal().latch(), mint.latch()),
            "the sRGB alias view must inherit its texture's loss-domain stamp"
        );
        assert_eq!(
            TexelFormat::Rgba8UnormSrgb.bytes_per_texel(),
            4,
            "the alias pair shares a texel size (the aliasing precondition)"
        );
    }

    /// ARMS the crossing guards: a Metal resource reaching a wgpu-only seam
    /// (and a wgpu handle reaching a Metal-only seam) must panic BY NAME —
    /// "device layer" — never misbehave silently. These panics are the W6
    /// flip's todo list; this test is what keeps them honest messages.
    #[test]
    fn crossing_a_metal_resource_into_a_wgpu_seam_panics_by_name() {
        let Some(mint) = mint() else {
            eprintln!("SKIP: no Metal device");
            return;
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

    /// ARMS the Metal arm's length guards: a row band shorter than
    /// `bytes_per_row * rows` and a buffer write longer than the buffer must
    /// both die on the layer's own assert — never reach `replaceRegion:` /
    /// `memcpy` with a short source (the read-past-the-slice class).
    #[test]
    fn the_metal_arm_refuses_short_uploads_and_oversized_writes() {
        let Some(mint) = mint() else {
            eprintln!("SKIP: no Metal device");
            return;
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

    /// W4 item 2 — THE PADDING-CONTRACT PROOF: the same tightly-packed RGBA8
    /// texels, uploaded to a texture on EACH backend, must read back as
    /// byte-identical `Frame`s through each backend's own readback arm — at a
    /// width (17: 68 bytes/row, padded to 256) where the 256-byte stride is
    /// LIVE, so a Metal arm using Metal's legal tight stride, mis-sizing the
    /// staging buffer, or unpacking rows at the wrong offset all diverge.
    /// Both frames are ALSO checked against the expected packing computed
    /// straight from the source bytes, so the two arms cannot agree by
    /// sharing a bug.
    #[test]
    fn the_metal_readback_reproduces_the_wgpu_padding_contract() {
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let Ok(ctx) = crate::GpuContext::new() else {
            eprintln!("SKIP: no wgpu context");
            return;
        };
        let Some(dev) = Device::system_default() else {
            eprintln!("SKIP: no Metal device");
            return;
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

    /// W4-judge item 4 — THE PADDING BYTES' HANDLING, pinned in BOTH
    /// directions at TWO live-padding widths (one padding block: 17 px, 68 ->
    /// 256; a multi-block stride: 97 px, 388 -> 512):
    ///
    ///  * the tail is NOT WRITTEN: the Metal `copy_texture_to_buffer` lands
    ///    each row's `w*4` texel bytes at its padded base and leaves the
    ///    poisoned `[w*4, padded)` tail untouched, byte for byte;
    ///  * the tail is NOT CONSULTED: `frame_from_padded_rgba` — the ONE
    ///    unpack both arms share — produces identical `Frame`s from a
    ///    poisoned-tail and a zeroed-tail staging buffer;
    ///  * both arms' end-to-end readbacks equal the independent expectation
    ///    computed straight from the source bytes.
    #[test]
    fn the_padding_tail_is_neither_written_nor_consulted() {
        let _test_pool = crate::metal::ffi::AutoreleasePool::new();
        let Ok(ctx) = crate::GpuContext::new() else {
            eprintln!("SKIP: no wgpu context");
            return;
        };
        let Some(dev) = Device::system_default() else {
            eprintln!("SKIP: no Metal device");
            return;
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

            // THE PRODUCTION STAGING SPELLING, pinned: drive the exact
            // helper `metal_try_read_back` stages through and assert the
            // stride it RETURNS and the bases it actually WROTE — the arm a
            // planted coherent tight-stride readback (same Frame, divergent
            // staging shape) proved the Frame-level tests cannot provide.
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
