// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WebGL2 canvas drawing-buffer surface and present path.

use super::GlPresentError;
use super::device::GlDevice;
use super::ffi;
use super::resources::GlTexture;
use crate::device_layer::TexelFormat;

/// The canvas a device's context draws into, at its configured size.
#[derive(Debug)]
pub(crate) struct GlCanvasSurface {
    dev: GlDevice,
    width: u32,
    height: u32,
    applied: Option<(u32, u32)>,
    standin: Option<GlTexture>,
}

impl GlCanvasSurface {
    /// `dev`'s canvas, sized to `width` x `height` now.
    pub(crate) fn new(dev: &GlDevice, width: u32, height: u32) -> Result<Self, String> {
        let mut s = Self {
            dev: dev.clone(),
            width: width.max(1),
            height: height.max(1),
            applied: None,
            standin: None,
        };
        s.apply_size()?;
        Ok(s)
    }

    fn apply_size(&mut self) -> Result<(), String> {
        let want = (self.width, self.height);
        if self.applied == Some(want) {
            return Ok(());
        }
        let _cur = self.dev.enter()?;
        ffi::webgl::canvas_resize(want.0, want.1);
        self.applied = Some(want);
        Ok(())
    }

    /// A browser paces presents itself (the host's animation frames).
    #[inline]
    pub(crate) const fn synced(&self) -> bool {
        false
    }

    /// Resize the canvas and stand-in target to `width` x `height`.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        let _ = self.apply_size();
    }

    /// The stand-in at the configured size, minted on `dev` on first use and after a resize.
    pub(crate) fn standin(&mut self, dev: &GlDevice) -> Result<&GlTexture, String> {
        let fits = self
            .standin
            .as_ref()
            .is_some_and(|t| t.on(dev) && (t.width, t.height) == (self.width, self.height));
        if !fits {
            self.standin = None;
            self.standin =
                Some(dev.texture_2d(TexelFormat::Rgba8Unorm, self.width, self.height, None)?);
        }
        Ok(self.standin.as_ref().expect("minted above"))
    }

    /// Blit the composed stand-in into the canvas drawing buffer.
    pub(crate) fn present(
        &mut self,
        dev: &GlDevice,
        _fill: u32,
        _damage: Option<[u32; 4]>,
    ) -> Result<(), GlPresentError> {
        let standin = self
            .standin
            .as_ref()
            .ok_or("gl: a present with no composed stand-in")?;
        let cur = dev.enter()?;
        let (tw, th) = (standin.width, standin.height);
        if self.applied != Some((tw, th)) {
            ffi::webgl::canvas_resize(tw, th);
            self.applied = Some((tw, th));
        }
        let fbo = standin.fbo(&cur)?;
        let gl = cur.gl();
        let (w, h) = (tw as i32, th as i32);
        // SAFETY: the context is current; the stand-in's framebuffer is complete; the blit is
        // colour-only and NEAREST between equal-sized rects of two RGBA8 buffers (the drawing
        // buffer is the canvas's).
        unsafe {
            (gl.disable)(ffi::GL_SCISSOR_TEST);
            (gl.color_mask)(ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE);
            (gl.bind_framebuffer)(ffi::GL_READ_FRAMEBUFFER, fbo);
            (gl.bind_framebuffer)(ffi::GL_DRAW_FRAMEBUFFER, 0);
            (gl.blit_framebuffer)(
                0,
                h,
                w,
                0,
                0,
                0,
                w,
                h,
                ffi::GL_COLOR_BUFFER_BIT,
                ffi::GL_NEAREST as u32,
            );
        }
        cur.check("present blit").map_err(Into::into)
    }
}
