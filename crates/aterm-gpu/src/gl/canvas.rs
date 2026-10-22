// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's swapchain in a browser: the WebGL2 canvas's drawing buffer,
//! and the present that posts a frame to it (wasm32; `surface` is the EGL
//! twin on Linux).
//!
//! A present composes into a stand-in texture the surface owns, then
//! [`GlCanvasSurface::present`] copies it into the canvas's default
//! framebuffer with `glBlitFramebuffer`, flipping rows (the default
//! framebuffer counts rows from the bottom; the arm's textures store their
//! top row at row 0), exactly as wgpu's WebGL2 backend posted its swapchain
//! texture. There is no swap: the browser composites the drawing buffer when
//! the task that drew it returns, and clears it after (no
//! `preserveDrawingBuffer`), so every present posts a whole frame.
//!
//! The canvas's size is the drawing buffer's: the configure and every resize
//! set the canvas's `width` and `height` at once (which clears it), as
//! wgpu's surface configure did.

use super::device::GlDevice;
use super::ffi;
use super::resources::GlTexture;
use crate::device_layer::TexelFormat;

/// The canvas a device's context draws into, at its configured size.
#[derive(Debug)]
pub(crate) struct GlCanvasSurface {
    /// The device whose context lives on the canvas.
    dev: GlDevice,
    /// The size the canvas was last configured to.
    width: u32,
    height: u32,
    /// The size the canvas is at now (`None`: not set yet, or a resize that
    /// could not reach the context, retried at the next present).
    applied: Option<(u32, u32)>,
    /// The texture a present composes into, at the configured size.
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

    /// Size the canvas to the configured size, if it is not already.
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

    /// A browser paces presents itself (the host's animation frames); a
    /// post never waits for the display.
    #[inline]
    pub(crate) const fn synced(&self) -> bool {
        false
    }

    /// Move to `width` x `height`: the canvas now (or, if its context cannot
    /// be reached, at the next present), and the next present composes and
    /// posts at it.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        let _ = self.apply_size();
    }

    /// The stand-in at the configured size, minted on `dev` on first use and
    /// after a resize.
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

    /// Post the stand-in: size the canvas if it moved, then copy the
    /// stand-in into the drawing buffer with its rows flipped. `Err` names
    /// the GL failure; a lost context is latched on the device.
    pub(crate) fn present(&mut self, dev: &GlDevice) -> Result<(), String> {
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
        // SAFETY: the context is current; the stand-in's framebuffer is
        // complete; the blit is colour-only and NEAREST between equal-sized
        // rects of two RGBA8 buffers (the drawing buffer is the canvas's).
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
        cur.check("present blit")
    }
}
