// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's textures, buffers and samplers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::device::{Current, GlDevice, GlShared};
use super::ffi::{self, GLenum, GLint, GLuint};
use crate::device_layer::{SamplerKind, TexelFormat};

/// A texel format's GL spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlFormat {
    pub(crate) internal: GLenum,
    pub(crate) format: GLenum,
    pub(crate) ty: GLenum,
    pub(crate) bytes: u32,
}

/// The GL spelling of `f`, or `Err` for a format GLES 3.0 cannot store.
#[inline]
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
    /// The texels in use, from the origin.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The storage behind them, at least `width` x `height` ([`GlDevice::texture_2d_resizing`]).
    stored: (u32, u32),
    /// Whether the storage is `SRGB8_ALPHA8` standing for `Rgba8Unorm`.
    pub(crate) views: bool,
    /// The framebuffer object with this texture at `COLOR_ATTACHMENT0`.
    fbo: AtomicU32,
}

impl std::fmt::Debug for GlTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlTexture")
            .field("name", &self.name)
            .field("size", &(self.width, self.height))
            .field("stored", &self.stored)
            .field("internal", &self.format.internal)
            .finish_non_exhaustive()
    }
}

impl GlTexture {
    /// Whether this texture was made on `dev`'s context.
    #[inline]
    pub(crate) fn on(&self, dev: &GlDevice) -> bool {
        Arc::ptr_eq(&self.shared, dev.shared())
    }

    /// Whether the storage holds texels past the ones in use.
    #[inline]
    pub(crate) fn roomy(&self) -> bool {
        self.stored != (self.width, self.height)
    }

    /// The share of the storage the texels in use fill, per axis: `[1.0, 1.0]` unless roomy.
    pub(crate) fn extent(&self) -> [f32; 2] {
        let (sw, sh) = self.stored;
        [
            self.width as f32 / sw as f32,
            self.height as f32 / sh as f32,
        ]
    }

    /// Let this texture go once the frame is out ([`GlDevice::free_retired`]) instead of now.
    pub(crate) fn retire(mut self) {
        let names = [
            std::mem::take(&mut self.name),
            self.fbo.swap(0, Ordering::Relaxed),
        ];
        self.shared
            .retired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(names);
    }

    /// The texture's framebuffer object, made and checked complete on first use.
    pub(crate) fn fbo(&self, cur: &Current<'_>) -> Result<GLuint, String> {
        let have = self.fbo.load(Ordering::Relaxed);
        if have != 0 {
            return Ok(have);
        }
        let gl = cur.gl();
        let mut fbo = 0;
        // SAFETY: the context is current; out-pointer live; the texture name belongs to this
        // context and has storage.
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
        if self.name == 0 {
            return;
        }
        let Ok(cur) = self.shared.enter() else {
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
    #[inline]
    const fn target(self) -> GLenum {
        match self {
            Self::Instance => ffi::GL_ARRAY_BUFFER,
            Self::Uniform => ffi::GL_UNIFORM_BUFFER,
            Self::Readback => ffi::GL_PIXEL_PACK_BUFFER,
        }
    }

    #[inline]
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
    /// `slot`'s texture at `width` x `height`, made as [`Self::texture_2d`] makes it.
    pub(crate) fn texture_2d_resizing(
        &self,
        slot: &mut Option<GlTexture>,
        format: TexelFormat,
        width: u32,
        height: u32,
        alias: Option<TexelFormat>,
    ) -> Result<bool, String> {
        let (storage, views) = self.storage_of(format, alias)?;
        let internal = gl_format(storage)?.internal;
        let kept = slot
            .as_mut()
            .filter(|t| t.on(self) && t.format.internal == internal && t.views == views);
        if let Some(t) = kept {
            if (t.width, t.height) == (width, height) {
                return Ok(false);
            }
            let (sw, sh) = t.stored;
            if width <= sw
                && height <= sh
                && u64::from(width) * u64::from(height) * 4 >= u64::from(sw) * u64::from(sh)
            {
                t.width = width;
                t.height = height;
                return Ok(true);
            }
        }
        let resized = slot.is_some();
        if let Some(old) = slot.take() {
            old.retire();
        }
        let max = self.caps().max_texture_dim;
        let room = |n: u32| {
            if resized {
                n.saturating_add(n / 4).next_multiple_of(64).min(max).max(n)
            } else {
                n
            }
        };
        let mut tex = self.texture_2d(format, room(width), room(height), alias)?;
        tex.width = width;
        tex.height = height;
        *slot = Some(tex);
        Ok(true)
    }

    /// Delete the textures retired since the last call ([`GlTexture::retire`]).
    pub(crate) fn free_retired(&self) {
        let names = std::mem::take(
            &mut *self
                .shared()
                .retired
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if names.is_empty() {
            return;
        }
        let Ok(cur) = self.enter() else {
            return;
        };
        let gl = cur.gl();
        for [texture, fbo] in names {
            // SAFETY: names made on this context, retired by their only holder.
            unsafe {
                if fbo != 0 {
                    (gl.delete_framebuffers)(1, &raw const fbo);
                }
                (gl.delete_textures)(1, &raw const texture);
            }
        }
    }

    /// The storage format of a `format` texture with the view `alias`.
    fn storage_of(
        &self,
        format: TexelFormat,
        alias: Option<TexelFormat>,
    ) -> Result<(TexelFormat, bool), String> {
        match alias {
            None => Ok((format, false)),
            Some(TexelFormat::Rgba8UnormSrgb) if format == TexelFormat::Rgba8Unorm => {
                if !self.caps().srgb_views {
                    return Err("gl: an sRGB alias of an Rgba8Unorm texture needs \
                                EXT_sRGB_write_control and EXT_texture_sRGB_decode"
                        .to_owned());
                }
                Ok((TexelFormat::Rgba8UnormSrgb, true))
            }
            Some(other) => Err(format!("gl: no {other:?} view of a {format:?} texture")),
        }
    }

    /// Create a 2-D texture of one level.
    pub(crate) fn texture_2d(
        &self,
        format: TexelFormat,
        width: u32,
        height: u32,
        alias: Option<TexelFormat>,
    ) -> Result<GlTexture, String> {
        let (storage, views) = self.storage_of(format, alias)?;
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
        // SAFETY: the context is current; dimensions checked against the device's maximum; one
        // level of a sized internal format.
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
            (gl.tex_parameteri)(ffi::GL_TEXTURE_2D, ffi::GL_TEXTURE_MAX_LEVEL, 0);
        }
        let tex = GlTexture {
            shared: Arc::clone(self.shared()),
            name,
            format: f,
            width,
            height,
            stored: (width, height),
            views,
            fbo: AtomicU32::new(0),
        };
        cur.check("texture storage")?;
        Ok(tex)
    }

    /// Upload rows `[y0, y0 + rows)` at full width from `bytes`, tightly packed at `bytes_per_row`:
    /// `Self::upload_region`'s full-width case.
    #[inline]
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

    /// Upload the `w × h` region at `(x, y)` from `bytes`, rows `bytes_per_row` apart.
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
        // SAFETY: the context is current; the region is inside the texture and `bytes` covers it
        // at the stated row length (checked above).
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

    /// Write `bytes` into `buf` from offset 0.
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

    /// One of the two clamp samplers (never mipped: every texture has one level).
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

    /// Read `width` x `height` texels of an RGBA8 `tex` into rows of `padded` bytes.
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
        // SAFETY: the context is current; `out` holds `height` rows of `padded` bytes at a pack
        // row length of `padded / 4` texels, so the copy stays inside it.
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::super::encoder::GlFrame;
    use super::super::test_device as device;
    use super::*;
    use crate::device_layer::{ClearColor4, FrameLoad};

    fn clear(dev: &GlDevice, tex: &GlTexture, [r, g, b, a]: [f64; 4]) {
        let uniforms = dev
            .buffer(GlBufferRole::Uniform, 256)
            .expect("a uniform buffer");
        let mut frame = GlFrame::begin(dev).expect("a frame");
        let load = FrameLoad::Clear(ClearColor4 { r, g, b, a });
        frame
            .begin_pass(tex, false, load, None, (&uniforms, 0))
            .expect("a pass");
        frame.submit().expect("submitted");
    }

    fn retired(dev: &GlDevice) -> Vec<[GLuint; 2]> {
        dev.shared()
            .retired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn a_resized_texture_keeps_its_storage_while_that_holds_the_new_size() {
        let dev = device();
        let rgba = TexelFormat::Rgba8Unorm;
        let mut slot = None;
        assert!(
            dev.texture_2d_resizing(&mut slot, rgba, 100, 80, None)
                .expect("minted")
        );
        let first = slot.as_ref().expect("minted");
        assert_eq!(
            (first.width, first.height, first.stored),
            (100, 80, (100, 80))
        );
        let first = first.name;
        assert!(
            !dev.texture_2d_resizing(&mut slot, rgba, 100, 80, None)
                .expect("kept"),
            "the same size keeps its texels"
        );

        assert!(
            dev.texture_2d_resizing(&mut slot, rgba, 200, 90, None)
                .expect("grown")
        );
        let grown = slot.as_ref().expect("grown");
        assert_ne!(grown.name, first);
        assert_eq!(
            (grown.width, grown.height, grown.stored),
            (200, 90, (256, 128)),
            "a quarter more per side, in 64s"
        );
        let grown = grown.name;
        assert_eq!(
            retired(&dev),
            [[first, 0]],
            "the old one waits for the frame"
        );
        dev.free_retired();
        assert!(retired(&dev).is_empty());

        assert!(
            dev.texture_2d_resizing(&mut slot, rgba, 256, 128, None)
                .expect("refit")
        );
        clear(&dev, slot.as_ref().expect("refit"), [1.0, 0.0, 0.0, 1.0]);
        assert!(
            dev.texture_2d_resizing(&mut slot, rgba, 150, 70, None)
                .expect("refit")
        );
        let inside = slot.as_ref().expect("refit");
        assert_eq!((inside.name, inside.width, inside.height), (grown, 150, 70));
        assert!(inside.roomy());
        clear(&dev, inside, [0.0, 0.0, 1.0, 1.0]);
        assert!(
            dev.texture_2d_resizing(&mut slot, rgba, 256, 128, None)
                .expect("refit")
        );
        let whole = slot.as_ref().expect("refit");
        assert_eq!(whole.name, grown, "nothing minted inside the storage");
        let texels = dev.read_rgba8(whole, 256, 128, 256 * 4).expect("read back");
        for (i, px) in texels.as_chunks::<4>().0.iter().enumerate() {
            let (x, y) = (i % 256, i / 256);
            let want: [u8; 4] = if x < 150 && y < 70 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            };
            assert_eq!(
                *px, want,
                "texel ({x}, {y}): the clear kept to the texels in use"
            );
        }
        assert!(retired(&dev).is_empty());

        assert!(
            dev.texture_2d_resizing(&mut slot, rgba, 100, 60, None)
                .expect("shrunk")
        );
        let shrunk = slot.as_ref().expect("shrunk");
        assert_ne!(
            shrunk.name, grown,
            "under a quarter of the storage mints again"
        );
        assert_eq!(shrunk.stored, (128, 128));
        assert_eq!(retired(&dev).len(), 1);
        let shrunk = shrunk.name;
        let r8 = TexelFormat::R8Unorm;
        assert!(
            dev.texture_2d_resizing(&mut slot, r8, 100, 60, None)
                .expect("reformatted")
        );
        assert_ne!(
            slot.as_ref().expect("reformatted").name,
            shrunk,
            "another format mints"
        );
        dev.free_retired();
        assert!(retired(&dev).is_empty());
    }
}
