// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's textures, buffers and samplers. Each holds its device's shared
//! half and deletes its GL name on drop, on the context that made it.
//!
//! Textures are immutable-storage (`glTexStorage2D`, one level). A texture
//! drawn into gets its framebuffer object on first use and keeps it, so a pass
//! binds one name. The six [`TexelFormat`]s map onto GLES 3.0 internal formats
//! except the BGRA pair, which GLES has no sized format for (the GL arm's
//! swapchain is the EGL surface's own RGBA buffer).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::device::{Current, GlDevice, GlShared};
use super::ffi::{self, GLenum, GLint, GLuint};
use crate::device_layer::{SamplerKind, TexelFormat};

/// A texel format's GL spelling: internal format, upload format and type, and
/// bytes per texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlFormat {
    pub(crate) internal: GLenum,
    pub(crate) format: GLenum,
    pub(crate) ty: GLenum,
    pub(crate) bytes: u32,
}

/// The GL spelling of `f`, or `Err` for a format GLES 3.0 cannot store.
pub(crate) fn gl_format(f: TexelFormat) -> Result<GlFormat, String> {
    let (internal, format, ty, bytes) = match f {
        TexelFormat::R8Unorm => (ffi::GL_R8, ffi::GL_RED, ffi::GL_UNSIGNED_BYTE, 1),
        TexelFormat::Rgba8Unorm => (ffi::GL_RGBA8, ffi::GL_RGBA, ffi::GL_UNSIGNED_BYTE, 4),
        TexelFormat::Rgba8UnormSrgb => {
            (ffi::GL_SRGB8_ALPHA8, ffi::GL_RGBA, ffi::GL_UNSIGNED_BYTE, 4)
        }
        TexelFormat::Rgba16Float => (ffi::GL_RGBA16F, ffi::GL_RGBA, ffi::GL_HALF_FLOAT, 8),
        TexelFormat::Bgra8Unorm => {
            return Err("gl: GLES 3.0 has no BGRA8 texture format".to_owned());
        }
        #[cfg(wgpu_arm)]
        TexelFormat::Bgra8UnormSrgb => {
            return Err("gl: GLES 3.0 has no BGRA8 sRGB texture format".to_owned());
        }
    };
    Ok(GlFormat {
        internal,
        format,
        ty,
        bytes,
    })
}

/// A 2-D texture on the GL arm.
pub(crate) struct GlTexture {
    shared: Arc<GlShared>,
    pub(crate) name: GLuint,
    pub(crate) format: GlFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Whether the storage is `SRGB8_ALPHA8` standing for the native plan's
    /// `Rgba8Unorm` texture with its sRGB alias: drawn and sampled raw (the
    /// Unorm view) or encoded (the sRGB view) per pass, through
    /// `EXT_sRGB_write_control` / `EXT_texture_sRGB_decode`.
    pub(crate) views: bool,
    /// The framebuffer object with this texture at `COLOR_ATTACHMENT0`, made
    /// on the first pass, copy or readback that needs one (0: none yet). An
    /// atomic so a texture can sit in a shared sprite cache; GL itself is
    /// serialized by the device's guard.
    fbo: AtomicU32,
}

impl std::fmt::Debug for GlTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlTexture")
            .field("name", &self.name)
            .field("size", &(self.width, self.height))
            .field("internal", &self.format.internal)
            .finish_non_exhaustive()
    }
}

impl GlTexture {
    /// Whether this texture was made on `dev`'s context (a GL name means
    /// nothing on another context).
    pub(crate) fn on(&self, dev: &GlDevice) -> bool {
        Arc::ptr_eq(&self.shared, dev.shared())
    }

    /// The texture's framebuffer object, made and checked complete on first
    /// use. The context is current (`cur`).
    pub(crate) fn fbo(&self, cur: &Current<'_>) -> Result<GLuint, String> {
        let have = self.fbo.load(Ordering::Relaxed);
        if have != 0 {
            return Ok(have);
        }
        let gl = cur.gl();
        let mut fbo = 0;
        // SAFETY: the context is current; out-pointer live; the texture name
        // belongs to this context and has storage.
        let status = unsafe {
            (gl.gen_framebuffers)(1, &raw mut fbo);
            (gl.bind_framebuffer)(ffi::GL_FRAMEBUFFER, fbo);
            (gl.framebuffer_texture_2d)(
                ffi::GL_FRAMEBUFFER,
                ffi::GL_COLOR_ATTACHMENT0,
                ffi::GL_TEXTURE_2D,
                self.name,
                0,
            );
            (gl.check_framebuffer_status)(ffi::GL_FRAMEBUFFER)
        };
        if status != ffi::GL_FRAMEBUFFER_COMPLETE {
            // SAFETY: the name generated above.
            unsafe { (gl.delete_framebuffers)(1, &raw const fbo) };
            return Err(format!(
                "gl: framebuffer over texture {} ({:#x}) is incomplete: {status:#x}",
                self.name, self.format.internal
            ));
        }
        self.fbo.store(fbo, Ordering::Relaxed);
        Ok(fbo)
    }
}

impl Drop for GlTexture {
    fn drop(&mut self) {
        let Ok(cur) = self.shared.enter() else {
            // The context cannot be made current here (held on another
            // thread): the names leak with the context, which frees them.
            return;
        };
        let gl = cur.gl();
        let fbo = self.fbo.load(Ordering::Relaxed);
        // SAFETY: names made on this context; deleting is always legal.
        unsafe {
            if fbo != 0 {
                (gl.delete_framebuffers)(1, &raw const fbo);
            }
            (gl.delete_textures)(1, &raw const self.name);
        }
    }
}

/// What a buffer is bound as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlBufferRole {
    /// A per-instance vertex stream (`GL_ARRAY_BUFFER`).
    Instance,
    /// A uniform block's storage (`GL_UNIFORM_BUFFER`).
    Uniform,
    /// A pixel-pack target texels are read back into (`GL_PIXEL_PACK_BUFFER`).
    Readback,
}

impl GlBufferRole {
    const fn target(self) -> GLenum {
        match self {
            Self::Instance => ffi::GL_ARRAY_BUFFER,
            Self::Uniform => ffi::GL_UNIFORM_BUFFER,
            Self::Readback => ffi::GL_PIXEL_PACK_BUFFER,
        }
    }

    const fn usage(self) -> GLenum {
        match self {
            Self::Instance => ffi::GL_STREAM_DRAW,
            Self::Uniform => ffi::GL_DYNAMIC_DRAW,
            Self::Readback => ffi::GL_STREAM_READ,
        }
    }
}

/// A buffer on the GL arm.
pub(crate) struct GlBuffer {
    shared: Arc<GlShared>,
    pub(crate) name: GLuint,
    pub(crate) role: GlBufferRole,
    pub(crate) size: usize,
}

impl std::fmt::Debug for GlBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlBuffer")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Drop for GlBuffer {
    fn drop(&mut self) {
        if let Ok(cur) = self.shared.enter() {
            // SAFETY: a name made on this context.
            unsafe { (cur.gl().delete_buffers)(1, &raw const self.name) };
        }
    }
}

/// A sampler object on the GL arm.
pub(crate) struct GlSampler {
    shared: Arc<GlShared>,
    pub(crate) name: GLuint,
}

impl std::fmt::Debug for GlSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlSampler")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Drop for GlSampler {
    fn drop(&mut self) {
        if let Ok(cur) = self.shared.enter() {
            // SAFETY: a name made on this context.
            unsafe { (cur.gl().delete_samplers)(1, &raw const self.name) };
        }
    }
}

impl GlDevice {
    /// Create a 2-D texture of one level. `alias` is the device layer's view
    /// alias: `Rgba8Unorm` storage aliased as `Rgba8UnormSrgb` becomes one
    /// `SRGB8_ALPHA8` texture with both views ([`GlTexture::views`]), which
    /// needs [`super::device::GlCaps::srgb_views`]. Any other alias is refused.
    pub(crate) fn texture_2d(
        &self,
        format: TexelFormat,
        width: u32,
        height: u32,
        alias: Option<TexelFormat>,
    ) -> Result<GlTexture, String> {
        let (storage, views) = match alias {
            None => (format, false),
            Some(TexelFormat::Rgba8UnormSrgb) if format == TexelFormat::Rgba8Unorm => {
                if !self.caps().srgb_views {
                    return Err("gl: an sRGB alias of an Rgba8Unorm texture needs \
                                EXT_sRGB_write_control and EXT_texture_sRGB_decode"
                        .to_owned());
                }
                (TexelFormat::Rgba8UnormSrgb, true)
            }
            Some(other) => {
                return Err(format!("gl: no {other:?} view of a {format:?} texture"));
            }
        };
        let f = gl_format(storage)?;
        let max = self.caps().max_texture_dim;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(format!(
                "gl: a {width}x{height} texture is outside 1..={max} per side"
            ));
        }
        let cur = self.enter()?;
        let gl = cur.gl();
        let mut name = 0;
        // SAFETY: the context is current; dimensions checked against the
        // device's maximum; one level of a sized internal format.
        unsafe {
            (gl.gen_textures)(1, &raw mut name);
            (gl.bind_texture)(ffi::GL_TEXTURE_2D, name);
            (gl.tex_storage_2d)(
                ffi::GL_TEXTURE_2D,
                1,
                f.internal,
                width as i32,
                height as i32,
            );
            // Level 0 is the only level: sampling never consults mips.
            (gl.tex_parameteri)(ffi::GL_TEXTURE_2D, ffi::GL_TEXTURE_MAX_LEVEL, 0);
        }
        let tex = GlTexture {
            shared: Arc::clone(self.shared()),
            name,
            format: f,
            width,
            height,
            views,
            fbo: AtomicU32::new(0),
        };
        cur.check("texture storage")?;
        Ok(tex)
    }

    /// Upload rows `[y0, y0 + rows)` at full width from `bytes`, tightly
    /// packed at `bytes_per_row`: [`Self::upload_region`]'s full-width case.
    pub(crate) fn upload_rows(
        &self,
        tex: &GlTexture,
        y0: u32,
        rows: u32,
        bytes: &[u8],
        bytes_per_row: u32,
        width: u32,
    ) -> Result<(), String> {
        self.upload_region(tex, 0, y0, width, rows, bytes, bytes_per_row)
    }

    /// Upload the `w × h` region at `(x, y)` from `bytes`, rows
    /// `bytes_per_row` apart, at unpack alignment 1 (an R8 region of odd
    /// width is legal).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload_region(
        &self,
        tex: &GlTexture,
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
                "gl: upload of {w}x{h} at ({x}, {y}) (stride {bytes_per_row}) does not \
                 fit a {}x{} texture of {bpp}-byte texels",
                tex.width, tex.height
            ));
        }
        let need = bytes_per_row as usize * (h as usize - 1) + (w * bpp) as usize;
        if bytes.len() < need {
            return Err(format!(
                "gl: upload of {h} rows needs {need} bytes, got {}",
                bytes.len()
            ));
        }
        let cur = self.enter()?;
        let gl = cur.gl();
        // SAFETY: the context is current; the region is inside the texture and
        // `bytes` covers it at the stated row length (checked above). The
        // unpack state is set for this call and restored.
        unsafe {
            (gl.bind_texture)(ffi::GL_TEXTURE_2D, tex.name);
            (gl.pixel_storei)(ffi::GL_UNPACK_ALIGNMENT, 1);
            (gl.pixel_storei)(ffi::GL_UNPACK_ROW_LENGTH, (bytes_per_row / bpp) as GLint);
            (gl.tex_sub_image_2d)(
                ffi::GL_TEXTURE_2D,
                0,
                x as GLint,
                y as GLint,
                w as GLint,
                h as GLint,
                tex.format.format,
                tex.format.ty,
                bytes.as_ptr().cast(),
            );
            (gl.pixel_storei)(ffi::GL_UNPACK_ROW_LENGTH, 0);
            (gl.pixel_storei)(ffi::GL_UNPACK_ALIGNMENT, 4);
        }
        cur.check("texture upload")
    }

    /// A buffer of `size` bytes (at least 1) for `role`, contents undefined.
    pub(crate) fn buffer(&self, role: GlBufferRole, size: usize) -> Result<GlBuffer, String> {
        let size = size.max(1);
        let len = isize::try_from(size).map_err(|_| format!("gl: a {size}-byte buffer"))?;
        let cur = self.enter()?;
        let gl = cur.gl();
        let mut name = 0;
        // SAFETY: the context is current; a null data pointer allocates.
        unsafe {
            (gl.gen_buffers)(1, &raw mut name);
            (gl.bind_buffer)(role.target(), name);
            (gl.buffer_data)(role.target(), len, std::ptr::null(), role.usage());
        }
        let buf = GlBuffer {
            shared: Arc::clone(self.shared()),
            name,
            role,
            size,
        };
        cur.check("buffer storage")?;
        Ok(buf)
    }

    /// Write `bytes` into `buf` from offset 0. GL orders the write after every
    /// draw already issued that reads the buffer, so no caller has to fence.
    pub(crate) fn write_buffer(&self, buf: &GlBuffer, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > buf.size {
            return Err(format!(
                "gl: write of {} bytes into a {}-byte buffer",
                bytes.len(),
                buf.size
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let cur = self.enter()?;
        let gl = cur.gl();
        // SAFETY: the context is current; the write is inside the buffer.
        unsafe {
            (gl.bind_buffer)(buf.role.target(), buf.name);
            (gl.buffer_sub_data)(
                buf.role.target(),
                0,
                bytes.len() as isize,
                bytes.as_ptr().cast(),
            );
        }
        cur.check("buffer write")
    }

    /// One of the two clamp samplers (never mipped: every texture has one
    /// level).
    pub(crate) fn sampler(&self, kind: SamplerKind) -> Result<GlSampler, String> {
        let filter = match kind {
            SamplerKind::NearestClamp => ffi::GL_NEAREST,
            SamplerKind::LinearClamp => ffi::GL_LINEAR,
        };
        let cur = self.enter()?;
        let gl = cur.gl();
        let mut name = 0;
        // SAFETY: the context is current; valid sampler parameters.
        unsafe {
            (gl.gen_samplers)(1, &raw mut name);
            (gl.sampler_parameteri)(name, ffi::GL_TEXTURE_MIN_FILTER, filter);
            (gl.sampler_parameteri)(name, ffi::GL_TEXTURE_MAG_FILTER, filter);
            (gl.sampler_parameteri)(name, ffi::GL_TEXTURE_WRAP_S, ffi::GL_CLAMP_TO_EDGE);
            (gl.sampler_parameteri)(name, ffi::GL_TEXTURE_WRAP_T, ffi::GL_CLAMP_TO_EDGE);
        }
        let s = GlSampler {
            shared: Arc::clone(self.shared()),
            name,
        };
        cur.check("sampler")?;
        Ok(s)
    }

    /// Read `width` x `height` texels of an RGBA8 `tex` (either sRGB or
    /// Unorm storage; the stored bytes, never converted) into rows of
    /// `padded` bytes. Waits for every draw into it.
    pub(crate) fn read_rgba8(
        &self,
        tex: &GlTexture,
        width: u32,
        height: u32,
        padded: usize,
    ) -> Result<Vec<u8>, String> {
        if tex.format.bytes != 4 || tex.format.ty != ffi::GL_UNSIGNED_BYTE {
            return Err(format!(
                "gl: readback of a {:#x} texture; only RGBA8 storage reads back",
                tex.format.internal
            ));
        }
        if width > tex.width
            || height > tex.height
            || padded < width as usize * 4
            || !padded.is_multiple_of(4)
        {
            return Err(format!(
                "gl: readback of {width}x{height} at stride {padded} from a {}x{} texture",
                tex.width, tex.height
            ));
        }
        let mut out = vec![0u8; padded * height as usize];
        if width == 0 || height == 0 {
            return Ok(out);
        }
        let cur = self.enter()?;
        let fbo = tex.fbo(&cur)?;
        let gl = cur.gl();
        // SAFETY: the context is current; `out` holds `height` rows of
        // `padded` bytes and the pack row length is `padded / 4` texels, so
        // the copy stays inside it; the pack state is restored.
        unsafe {
            (gl.bind_framebuffer)(ffi::GL_READ_FRAMEBUFFER, fbo);
            (gl.pixel_storei)(ffi::GL_PACK_ALIGNMENT, 4);
            (gl.pixel_storei)(ffi::GL_PACK_ROW_LENGTH, (padded / 4) as GLint);
            (gl.read_pixels)(
                0,
                0,
                width as GLint,
                height as GLint,
                ffi::GL_RGBA,
                ffi::GL_UNSIGNED_BYTE,
                out.as_mut_ptr().cast(),
            );
            (gl.pixel_storei)(ffi::GL_PACK_ROW_LENGTH, 0);
        }
        cur.check("readback")?;
        Ok(out)
    }
}
