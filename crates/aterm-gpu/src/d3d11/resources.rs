// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm's textures, buffers and samplers. Each holds its device's
//! shared half (so a texture can say which device made it) and releases its
//! objects on drop.
//!
//! A texture is one level, bound as a shader resource and a render target,
//! with its views made at creation: the `Rgba8Unorm` texture aliased as
//! `Rgba8UnormSrgb` (the offscreen) is `R8G8B8A8_TYPELESS` with a `UNORM` and
//! a `UNORM_SRGB` view of each kind, the resource wgpu-dx12 makes for a
//! texture with a view format. Uploads are `UpdateSubresource` and readbacks
//! copy through a staging texture, so both are ordered after every draw
//! already issued, as on the GL arm.
//!
//! Buffers are `DYNAMIC` and written with `Map(WRITE_DISCARD)`: a write
//! renames the buffer, so a draw already issued reads the bytes it was issued
//! with.

use std::sync::Arc;

use aterm_win32::d3d11 as d3d;
use aterm_win32::dxgi;

use super::device::{Child, D3dDevice, D3dShared};
use crate::device_layer::{SamplerKind, TexelFormat};

/// A texel format's D3D11 spelling and bytes per texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct D3dFormat {
    pub(crate) dxgi: dxgi::DXGI_FORMAT,
    pub(crate) bytes: u32,
}

/// The D3D11 spelling of `f`.
#[inline]
pub(crate) const fn d3d_format(f: TexelFormat) -> D3dFormat {
    let (dxgi, bytes) = match f {
        TexelFormat::R8Unorm => (dxgi::DXGI_FORMAT_R8_UNORM, 1),
        TexelFormat::Rgba8Unorm => (dxgi::DXGI_FORMAT_R8G8B8A8_UNORM, 4),
        TexelFormat::Rgba8UnormSrgb => (dxgi::DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, 4),
        TexelFormat::Bgra8Unorm => (dxgi::DXGI_FORMAT_B8G8R8A8_UNORM, 4),
        TexelFormat::Rgba16Float => (dxgi::DXGI_FORMAT_R16G16B16A16_FLOAT, 8),
    };
    D3dFormat { dxgi, bytes }
}

/// One texture view of each kind for one format.
#[derive(Debug)]
struct Views {
    srv: Child,
    rtv: Child,
}

/// A 2-D texture on the D3D11 arm.
pub(crate) struct D3dTexture {
    shared: Arc<D3dShared>,
    pub(crate) tex: Child,
    /// The resource's format (`R8G8B8A8_TYPELESS` for a texture with views).
    pub(crate) format: D3dFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Whether this is the `Rgba8Unorm` texture with its sRGB alias: drawn and
    /// sampled raw (the Unorm views) or encoded (the sRGB views) per pass.
    pub(crate) views: bool,
    /// The texture's own views; on a texture with views, the Unorm pair.
    raw: Views,
    /// The sRGB pair of a texture with views.
    srgb: Option<Views>,
}

impl std::fmt::Debug for D3dTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dTexture")
            .field("size", &(self.width, self.height))
            .field("format", &self.format.dxgi)
            .field("views", &self.views)
            .finish_non_exhaustive()
    }
}

impl D3dTexture {
    /// Whether this texture was made on `dev`.
    #[inline]
    pub(crate) fn on(&self, dev: &D3dDevice) -> bool {
        Arc::ptr_eq(&self.shared, dev.shared())
    }

    /// The shader-resource view a pass samples: on a texture with views the
    /// Unorm view when `raw`, else the sRGB one; otherwise its own.
    #[inline]
    pub(crate) fn srv(&self, raw: bool) -> &Child {
        match (&self.srgb, raw) {
            (Some(s), false) => &s.srv,
            _ => &self.raw.srv,
        }
    }

    /// The render-target view a pass draws through: on a texture with views
    /// the sRGB view when `srgb` (encode on write, blend in linear light), else
    /// the Unorm one; otherwise its own.
    #[inline]
    pub(crate) fn rtv(&self, srgb: bool) -> &Child {
        match (&self.srgb, srgb) {
            (Some(s), true) => &s.rtv,
            _ => &self.raw.rtv,
        }
    }

    /// The closed-set format a tap reads this texture's stored texels as:
    /// `Rgba16Float` for an f16 texture (an EDR swap chain's buffer), else the
    /// RGBA8 bytes every other present destination stores.
    #[inline]
    pub(crate) const fn texel_format(&self) -> TexelFormat {
        if self.format.dxgi == dxgi::DXGI_FORMAT_R16G16B16A16_FLOAT {
            TexelFormat::Rgba16Float
        } else {
            TexelFormat::Rgba8Unorm
        }
    }
}

/// What a buffer is bound as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum D3dBufferRole {
    /// A per-instance vertex stream.
    Instance,
    /// A constant buffer (its size rounded up to 16 bytes, D3D11's unit).
    Uniform,
}

/// A buffer on the D3D11 arm.
pub(crate) struct D3dBuffer {
    pub(crate) buf: Child,
    pub(crate) role: D3dBufferRole,
    /// The size asked for (the constant buffer's storage may be larger).
    pub(crate) size: usize,
}

impl std::fmt::Debug for D3dBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dBuffer")
            .field("role", &self.role)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// A sampler state on the D3D11 arm.
pub(crate) struct D3dSampler {
    pub(crate) state: Child,
}

impl std::fmt::Debug for D3dSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dSampler").finish_non_exhaustive()
    }
}

impl D3dDevice {
    /// Create a 2-D texture of one level. `alias` is the device layer's view
    /// alias: `Rgba8Unorm` storage aliased as `Rgba8UnormSrgb` becomes one
    /// `R8G8B8A8_TYPELESS` texture with both views ([`D3dTexture::views`]).
    /// Any other alias is refused.
    pub(crate) fn texture_2d(
        &self,
        format: TexelFormat,
        width: u32,
        height: u32,
        alias: Option<TexelFormat>,
    ) -> Result<D3dTexture, String> {
        let views = match alias {
            None => false,
            Some(TexelFormat::Rgba8UnormSrgb) if format == TexelFormat::Rgba8Unorm => true,
            Some(other) => {
                return Err(format!("d3d11: no {other:?} view of a {format:?} texture"));
            }
        };
        let f = d3d_format(format);
        let max = self.caps().max_texture_dim;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(format!(
                "d3d11: a {width}x{height} texture is outside 1..={max} per side"
            ));
        }
        let resource = if views {
            dxgi::DXGI_FORMAT_R8G8B8A8_TYPELESS
        } else {
            f.dxgi
        };
        let desc = d3d::D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: resource,
            SampleDesc: dxgi::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: d3d::D3D11_USAGE_DEFAULT,
            BindFlags: (d3d::D3D11_BIND_SHADER_RESOURCE | d3d::D3D11_BIND_RENDER_TARGET) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let shared = &self.0;
        let mut out = std::ptr::null_mut();
        // SAFETY: a valid one-level description checked against the device's
        // maximum; no initial data; one owned texture written on success.
        let hr = unsafe {
            (shared.device_vt().CreateTexture2D)(
                shared.device_raw(),
                &desc,
                std::ptr::null(),
                &mut out,
            )
        };
        let tex = shared.created("CreateTexture2D", hr, out)?;
        let raw = self.views_of(&tex, f.dxgi)?;
        let srgb = if views {
            Some(self.views_of(&tex, dxgi::DXGI_FORMAT_R8G8B8A8_UNORM_SRGB)?)
        } else {
            None
        };
        Ok(D3dTexture {
            shared: Arc::clone(shared),
            tex,
            format: D3dFormat {
                dxgi: resource,
                bytes: f.bytes,
            },
            width,
            height,
            views,
            raw,
            srgb,
        })
    }

    /// A texture the arm did not create (a swap chain's buffer, made with
    /// shader-input usage), of `format` at `width` x `height`, with its own
    /// views.
    pub(crate) fn adopt_texture(
        &self,
        tex: Child,
        format: TexelFormat,
        width: u32,
        height: u32,
    ) -> Result<D3dTexture, String> {
        let f = d3d_format(format);
        let raw = self.views_of(&tex, f.dxgi)?;
        Ok(D3dTexture {
            shared: Arc::clone(&self.0),
            tex,
            format: f,
            width,
            height,
            views: false,
            raw,
            srgb: None,
        })
    }

    /// A shader-resource and a render-target view of `tex` as `format`.
    fn views_of(&self, tex: &Child, format: dxgi::DXGI_FORMAT) -> Result<Views, String> {
        let shared = &self.0;
        let srv_desc = d3d::D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: d3d::D3D_SRV_DIMENSION_TEXTURE2D,
            Anonymous: d3d::D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: d3d::D3D11_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                },
            },
        };
        let mut out = std::ptr::null_mut();
        // SAFETY: a live texture of this device; a view of its one level in a
        // format of its family; one owned view written on success.
        let hr = unsafe {
            (shared.device_vt().CreateShaderResourceView)(
                shared.device_raw(),
                tex.raw(),
                &srv_desc,
                &mut out,
            )
        };
        let srv = shared.created("CreateShaderResourceView", hr, out)?;
        let rtv_desc = d3d::D3D11_RENDER_TARGET_VIEW_DESC {
            Format: format,
            ViewDimension: d3d::D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: d3d::D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2D: d3d::D3D11_TEX2D_RTV { MipSlice: 0 },
            },
        };
        let mut out = std::ptr::null_mut();
        // SAFETY: as above, for a render-target view.
        let hr = unsafe {
            (shared.device_vt().CreateRenderTargetView)(
                shared.device_raw(),
                tex.raw(),
                &rtv_desc,
                &mut out,
            )
        };
        let rtv = shared.created("CreateRenderTargetView", hr, out)?;
        Ok(Views { srv, rtv })
    }

    /// Upload rows `[y0, y0 + rows)` at full width from `bytes`, tightly
    /// packed at `bytes_per_row`: [`Self::upload_region`]'s full-width case.
    #[inline]
    pub(crate) fn upload_rows(
        &self,
        tex: &D3dTexture,
        y0: u32,
        rows: u32,
        bytes: &[u8],
        bytes_per_row: u32,
        width: u32,
    ) -> Result<(), String> {
        self.upload_region(tex, 0, y0, width, rows, bytes, bytes_per_row)
    }

    /// Upload the `w × h` region at `(x, y)` from `bytes`, rows
    /// `bytes_per_row` apart.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload_region(
        &self,
        tex: &D3dTexture,
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
        let bpp = tex.format.bytes;
        if !bytes_per_row.is_multiple_of(bpp)
            || bytes_per_row < w * bpp
            || x.checked_add(w).is_none_or(|end| end > tex.width)
            || y.checked_add(h).is_none_or(|end| end > tex.height)
        {
            return Err(format!(
                "d3d11: upload of {w}x{h} at ({x}, {y}) (stride {bytes_per_row}) does not \
                 fit a {}x{} texture of {bpp}-byte texels",
                tex.width, tex.height
            ));
        }
        let need = bytes_per_row as usize * (h as usize - 1) + (w * bpp) as usize;
        if bytes.len() < need {
            return Err(format!(
                "d3d11: upload of {h} rows needs {need} bytes, got {}",
                bytes.len()
            ));
        }
        let region = d3d::D3D11_BOX {
            left: x,
            top: y,
            front: 0,
            right: x + w,
            bottom: y + h,
            back: 1,
        };
        let cur = self.enter()?;
        let ctx = cur.ctx();
        // SAFETY: the context is held; the box is inside the texture's one
        // level and `bytes` covers it at the stated row pitch (checked above).
        unsafe {
            (ctx.vt.UpdateSubresource)(
                ctx.raw,
                tex.tex.raw(),
                0,
                &region,
                bytes.as_ptr().cast(),
                bytes_per_row,
                0,
            );
        }
        cur.check("texture upload")
    }

    /// A buffer of `size` bytes (at least 1) for `role`, contents undefined.
    pub(crate) fn buffer(&self, role: D3dBufferRole, size: usize) -> Result<D3dBuffer, String> {
        let size = size.max(1);
        let width = match role {
            D3dBufferRole::Instance => size,
            D3dBufferRole::Uniform => size.next_multiple_of(16),
        };
        let width = u32::try_from(width).map_err(|_| format!("d3d11: a {size}-byte buffer"))?;
        let desc = d3d::D3D11_BUFFER_DESC {
            ByteWidth: width,
            Usage: d3d::D3D11_USAGE_DYNAMIC,
            BindFlags: match role {
                D3dBufferRole::Instance => d3d::D3D11_BIND_VERTEX_BUFFER,
                D3dBufferRole::Uniform => d3d::D3D11_BIND_CONSTANT_BUFFER,
            } as u32,
            CPUAccessFlags: d3d::D3D11_CPU_ACCESS_WRITE as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let shared = &self.0;
        let mut out = std::ptr::null_mut();
        // SAFETY: a valid dynamic buffer description; no initial data; one
        // owned buffer written on success.
        let hr = unsafe {
            (shared.device_vt().CreateBuffer)(
                shared.device_raw(),
                &desc,
                std::ptr::null(),
                &mut out,
            )
        };
        Ok(D3dBuffer {
            buf: shared.created("CreateBuffer", hr, out)?,
            role,
            size,
        })
    }

    /// Write `bytes` into `buf` from offset 0 (the rest of the buffer is then
    /// undefined: the write renames it, see the module header).
    pub(crate) fn write_buffer(&self, buf: &D3dBuffer, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > buf.size {
            return Err(format!(
                "d3d11: write of {} bytes into a {}-byte buffer",
                bytes.len(),
                buf.size
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let cur = self.enter()?;
        let ctx = cur.ctx();
        let mut mapped = d3d::D3D11_MAPPED_SUBRESOURCE {
            pData: std::ptr::null_mut(),
            RowPitch: 0,
            DepthPitch: 0,
        };
        // SAFETY: the context is held; a dynamic buffer with CPU write access,
        // mapped whole.
        let hr = unsafe {
            (ctx.vt.Map)(
                ctx.raw,
                buf.buf.raw(),
                0,
                d3d::D3D11_MAP_WRITE_DISCARD,
                0,
                &mut mapped,
            )
        };
        self.0.ok("Map(WRITE_DISCARD)", hr)?;
        if mapped.pData.is_null() {
            // SAFETY: the subresource mapped above.
            unsafe { (ctx.vt.Unmap)(ctx.raw, buf.buf.raw(), 0) };
            return Err("d3d11: Map(WRITE_DISCARD) returned null".to_owned());
        }
        // SAFETY: the mapping covers the whole buffer, at least `buf.size`
        // bytes, and `bytes` is no longer; then the subresource is unmapped.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.pData.cast::<u8>(), bytes.len());
            (ctx.vt.Unmap)(ctx.raw, buf.buf.raw(), 0);
        }
        Ok(())
    }

    /// One of the two clamp samplers, wgpu-dx12's for the same descriptors
    /// (`mipmap_filter: Nearest`, so a POINT mip filter; every texture has one
    /// level).
    pub(crate) fn sampler(&self, kind: SamplerKind) -> Result<D3dSampler, String> {
        let desc = d3d::D3D11_SAMPLER_DESC {
            Filter: match kind {
                SamplerKind::NearestClamp => d3d::D3D11_FILTER_MIN_MAG_MIP_POINT,
                SamplerKind::LinearClamp => d3d::D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT,
            },
            AddressU: d3d::D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: d3d::D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: d3d::D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: d3d::D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: 32.0,
        };
        let shared = &self.0;
        let mut out = std::ptr::null_mut();
        // SAFETY: a valid sampler description; one owned state written on
        // success.
        let hr = unsafe {
            (shared.device_vt().CreateSamplerState)(shared.device_raw(), &desc, &mut out)
        };
        Ok(D3dSampler {
            state: shared.created("CreateSamplerState", hr, out)?,
        })
    }

    /// Read `width` x `height` texels of an RGBA8 `tex` (the stored bytes,
    /// never converted) into rows of `padded` bytes, through a staging copy.
    /// Waits for every draw into it.
    pub(crate) fn read_rgba8(
        &self,
        tex: &D3dTexture,
        width: u32,
        height: u32,
        padded: usize,
    ) -> Result<Vec<u8>, String> {
        let rgba8 = matches!(
            tex.format.dxgi,
            dxgi::DXGI_FORMAT_R8G8B8A8_TYPELESS
                | dxgi::DXGI_FORMAT_R8G8B8A8_UNORM
                | dxgi::DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
        );
        if !rgba8 {
            return Err(format!(
                "d3d11: readback of a format {} texture; only RGBA8 storage reads back",
                tex.format.dxgi
            ));
        }
        self.read_texels(tex, width, height, padded)
    }

    /// Read `width` x `height` texels of an RGBA8 or RGBA16F `tex` (the stored
    /// bytes, 4 or 8 per texel, never converted) into rows of `padded` bytes,
    /// through a staging copy. Waits for every draw into it.
    pub(crate) fn read_texels(
        &self,
        tex: &D3dTexture,
        width: u32,
        height: u32,
        padded: usize,
    ) -> Result<Vec<u8>, String> {
        let readable = matches!(
            tex.format.dxgi,
            dxgi::DXGI_FORMAT_R8G8B8A8_TYPELESS
                | dxgi::DXGI_FORMAT_R8G8B8A8_UNORM
                | dxgi::DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
                | dxgi::DXGI_FORMAT_R16G16B16A16_FLOAT
        );
        if !readable {
            return Err(format!(
                "d3d11: readback of a format {} texture; only RGBA8 and RGBA16F storage reads back",
                tex.format.dxgi
            ));
        }
        let bpp = tex.format.bytes as usize;
        if width > tex.width
            || height > tex.height
            || padded < width as usize * bpp
            || !padded.is_multiple_of(bpp)
        {
            return Err(format!(
                "d3d11: readback of {width}x{height} at stride {padded} from a {}x{} texture",
                tex.width, tex.height
            ));
        }
        let mut out = vec![0u8; padded * height as usize];
        if width == 0 || height == 0 {
            return Ok(out);
        }
        let shared = &self.0;
        let desc = d3d::D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: tex.format.dxgi,
            SampleDesc: dxgi::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: d3d::D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: d3d::D3D11_CPU_ACCESS_READ as u32,
            MiscFlags: 0,
        };
        let mut staging = std::ptr::null_mut();
        // SAFETY: a valid staging description; one owned texture on success.
        let hr = unsafe {
            (shared.device_vt().CreateTexture2D)(
                shared.device_raw(),
                &desc,
                std::ptr::null(),
                &mut staging,
            )
        };
        let staging = shared.created("CreateTexture2D (staging)", hr, staging)?;
        let cur = self.enter()?;
        let ctx = cur.ctx();
        let region = d3d::D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        let mut mapped = d3d::D3D11_MAPPED_SUBRESOURCE {
            pData: std::ptr::null_mut(),
            RowPitch: 0,
            DepthPitch: 0,
        };
        // SAFETY: the context is held; same-format textures of this device,
        // the box inside the source; the staging texture is mapped for read
        // (which waits for the copy) and unmapped below.
        let hr = unsafe {
            (ctx.vt.CopySubresourceRegion)(
                ctx.raw,
                staging.raw(),
                0,
                0,
                0,
                0,
                tex.tex.raw(),
                0,
                &region,
            );
            (ctx.vt.Map)(
                ctx.raw,
                staging.raw(),
                0,
                d3d::D3D11_MAP_READ,
                0,
                &mut mapped,
            )
        };
        shared.ok("Map(READ)", hr)?;
        let pitch = mapped.RowPitch as usize;
        let row = width as usize * bpp;
        if mapped.pData.is_null() || pitch < row {
            // SAFETY: the subresource mapped above.
            unsafe { (ctx.vt.Unmap)(ctx.raw, staging.raw(), 0) };
            return Err(format!(
                "d3d11: Map(READ) gave row pitch {pitch} for {row}-byte rows"
            ));
        }
        for (y, dst) in out.chunks_exact_mut(padded).enumerate() {
            // SAFETY: the mapping holds `height` rows `pitch` bytes apart, each
            // at least `row` bytes (checked above).
            let src = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(y * pitch), row)
            };
            dst[..row].copy_from_slice(src);
        }
        // SAFETY: the subresource mapped above.
        unsafe { (ctx.vt.Unmap)(ctx.raw, staging.raw(), 0) };
        cur.check("readback")?;
        Ok(out)
    }
}
