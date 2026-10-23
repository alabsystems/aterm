// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's device: one EGL display, config and OpenGL ES 3.0 context, or on an X11 window
//! system a GLX one (on wasm32, one WebGL2 context on a canvas), the GL function table loaded for
//! it, and what the driver can do.

use std::cell::Cell;
#[cfg(not(target_arch = "wasm32"))]
use std::ffi::{CStr, c_void};
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::ffi::{self, GlFns};
#[cfg(not(target_arch = "wasm32"))]
use super::ffi::{EGLConfig, EGLContext, EGLDisplay, EGLSurface, EGLint, Egl};

/// What the driver offers beyond GLES 3.0 core, read once at creation.
#[derive(Clone, Debug)]
pub(crate) struct GlCaps {
    /// `GL_RENDERER` string reported by the driver.
    pub(crate) renderer: String,
    /// `GL_VERSION` string reported by the driver.
    pub(crate) version: String,
    /// `GL_MAX_TEXTURE_SIZE`.
    pub(crate) max_texture_dim: u32,
    /// Whether both `EXT_sRGB_write_control` and `EXT_texture_sRGB_decode` are supported.
    pub(crate) srgb_views: bool,
    /// Symbol name of the context's `glGetGraphicsResetStatus` entry point.
    #[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
    pub(crate) reset_query: Option<&'static CStr>,
    /// `EXT_read_format_bgra`: a readback can be `GL_BGRA_EXT`, the byte order of an X11 window's
    /// pixels (`surface`'s readback post).
    #[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
    pub(crate) bgra_read: bool,
}

/// The shared half of a device: the EGL or GLX objects (on wasm32, the WebGL2 context and its
/// canvas), the function table and the lock.
pub(crate) struct GlShared {
    /// The API the context lives on: EGL, or GLX on an X11 window system.
    #[cfg(not(target_arch = "wasm32"))]
    binding: Binding,
    /// The window system whose display this is (`None`: a headless display).
    #[cfg(not(target_arch = "wasm32"))]
    window_system: Option<WindowSystem>,
    /// The glue's id of the WebGL2 context (`ffi::webgl`).
    #[cfg(target_arch = "wasm32")]
    context: u32,
    /// The canvas the context draws into, held in the glue's object table
    /// for the context's life.
    #[cfg(target_arch = "wasm32")]
    _canvas: aterm_wasm_abi::js::JsObject,
    /// Latched when the device is lost; the device is then dead, and
    /// `GpuRenderer::device_lost` reports it to the frontend's CPU fallback.
    lost: AtomicBool,
    /// The first latched loss's cause, for the frontend's downgrade line.
    #[cfg(not(target_arch = "wasm32"))]
    loss_reason: std::sync::OnceLock<String>,
    /// The injected-loss drill (`ATERM_GL_INJECT_LOSS`) on an EGL device: the
    /// next [`GlShared::enter`] answers its `eglMakeCurrent` as a lost context
    /// does, so the latch is driven through the path a real loss takes.
    inject_loss: AtomicBool,
    /// The context's `glGetGraphicsResetStatus`, where the context was made
    /// to report resets (a robust GLX context); `None` elsewhere, where it
    /// could only answer `GL_NO_ERROR`.
    #[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
    reset_status: Option<ffi::GetGraphicsResetStatusFn>,
    /// A reset status the next [`Current::poll_reset`] reads in place of the
    /// driver's (`GL_NO_ERROR`: none): the drill on a GLX device, so its
    /// latch comes from the check a real reset hits, after the present's GLX
    /// calls; and the tests' statuses.
    #[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
    inject_reset: AtomicU32,
    pub(crate) gl: GlFns,
    pub(crate) caps: GlCaps,
    lock: Mutex<()>,
    /// The texture and framebuffer names of textures a resize replaced
    /// ([`super::resources::GlTexture::retire`]), deleted once the frame is out
    /// ([`GlDevice::free_retired`]).
    pub(crate) retired: Mutex<Vec<[ffi::GLuint; 2]>>,
}

/// The native display a windowed device was opened on, as raw handles carry it (addresses, so the
/// type stays `Send`).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    wgpu_arm,
    expect(
        dead_code,
        reason = "a build carrying wgpu attaches windows on wgpu; the shipped build's \
                  window attach names these"
    )
)]
pub(crate) enum WindowSystem {
    /// An Xlib `Display*` and the screen number.
    X11 { display: usize, screen: i32 },
    /// A `wl_display*`.
    Wayland { display: usize },
}

/// The API a native device's context lives on.
#[cfg(not(target_arch = "wasm32"))]
enum Binding {
    Egl(EglBinding),
    /// An X11 window system's GLX context (`super::glx`).
    #[cfg(not(wgpu_arm))]
    Glx(super::glx::GlxContext),
}

/// An EGL device's display, context and config.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct EglBinding {
    pub(crate) egl: &'static Egl,
    pub(crate) display: EGLDisplay,
    context: EGLContext,
    /// The context's draw surface when the display cannot bind without one (a
    /// 1x1 pbuffer), else `EGL_NO_SURFACE` (`EGL_KHR_surfaceless_context`).
    surface: EGLSurface,
    /// Whether the display was opened with reference tracking, so this
    /// device's `eglTerminate` releases only its own reference.
    tracked: bool,
    /// The config the context was made with, or `EGL_NO_CONFIG_KHR` for a context any surface of
    /// the display can bind.
    #[cfg_attr(
        wgpu_arm,
        expect(dead_code, reason = "read by the shipped build's window surfaces")
    )]
    pub(crate) config: EGLConfig,
}

// SAFETY: every GL and EGL call on these handles happens inside a `Current` guard, which holds
// `lock` and makes the context current on the calling thread for the guard's life; no handle is
// used on two threads at once.
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Send for GlShared {}
// SAFETY: as above: `&GlShared` reaches GL only through `enter`, serialized by `lock`.
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Sync for GlShared {}

thread_local! {
    static HELD: Cell<*const GlShared> = const { Cell::new(std::ptr::null()) };
}

/// The device as the renderer holds it.
#[derive(Clone)]
pub(crate) struct GlDevice(pub(crate) Arc<GlShared>);

impl std::fmt::Debug for GlDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlDevice")
            .field("renderer", &self.0.caps.renderer)
            .field("version", &self.0.caps.version)
            .finish_non_exhaustive()
    }
}

/// The context made current on this thread; released on drop.
pub(crate) struct Current<'a> {
    shared: &'a GlShared,
    /// `None` for a nested guard, which releases nothing.
    outer: Option<MutexGuard<'a, ()>>,
}

impl Current<'_> {
    /// The GL function table.
    #[inline]
    pub(crate) const fn gl(&self) -> &GlFns {
        &self.shared.gl
    }

    /// Drain the GL error queue; `Err` names every error raised since the last check, with `what`
    /// for context.
    pub(crate) fn check(&self, what: &str) -> Result<(), String> {
        let mut errors = Vec::new();
        for _ in 0..16 {
            // SAFETY: the context is current (this guard).
            let e = unsafe { (self.gl().get_error)() };
            if e == ffi::GL_NO_ERROR {
                break;
            }
            #[cfg(target_arch = "wasm32")]
            let lost = e == ffi::GL_CONTEXT_LOST || e == ffi::GL_CONTEXT_LOST_WEBGL;
            #[cfg(not(target_arch = "wasm32"))]
            let lost = e == ffi::GL_CONTEXT_LOST;
            if lost {
                self.shared
                    .latch_lost(&format!("gl: {what}: {}", ffi::gl_error_name(e)));
            }
            errors.push(ffi::gl_error_name(e));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("gl: {what}: {}", errors.join(", ")))
        }
    }
}

#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
impl Current<'_> {
    /// Binds `surface` as the context's draw and read surface for the rest of the guard.
    pub(crate) fn bind_window(&self, surface: EGLSurface) -> Result<(), String> {
        let s = self.shared;
        let b = s
            .egl_binding()
            .ok_or("gl: an EGL window surface on a GLX device")?;
        // SAFETY: the surface was made on this display; the guard holds the device lock, so no
        // other thread uses the context.
        let ok = unsafe { (b.egl.fns.make_current)(b.display, surface, surface, b.context) };
        if ok == ffi::EGL_TRUE {
            Ok(())
        } else {
            Err(s.egl_failure("eglMakeCurrent(window)"))
        }
    }

    /// Post the bound window surface's back buffer (`eglSwapBuffers`).
    pub(crate) fn swap(&self, fns: &ffi::EglWindowFns, surface: EGLSurface) -> Result<(), String> {
        let s = self.shared;
        let b = s.egl_binding().ok_or("gl: an EGL swap on a GLX device")?;
        // SAFETY: as `bind_window`; the surface is current on this thread.
        let ok = unsafe { (fns.swap_buffers)(b.display, surface) };
        if ok == ffi::EGL_TRUE {
            Ok(())
        } else {
            Err(s.egl_failure("eglSwapBuffers"))
        }
    }

    /// Set the bound window surface's swap interval (0: post at once; 1: wait for the display's
    /// next refresh).
    pub(crate) fn swap_interval(
        &self,
        fns: &ffi::EglWindowFns,
        interval: i32,
    ) -> Result<(), String> {
        let s = self.shared;
        let b = s
            .egl_binding()
            .ok_or("gl: an EGL swap interval on a GLX device")?;
        // SAFETY: a window surface is current on this thread.
        let ok = unsafe { (fns.swap_interval)(b.display, interval) };
        if ok == ffi::EGL_TRUE {
            Ok(())
        } else {
            Err(s.egl_failure("eglSwapInterval"))
        }
    }

    /// The shared half, for the window-surface code.
    #[inline]
    pub(crate) const fn shared(&self) -> &GlShared {
        self.shared
    }

    /// The GLX context this guard holds current (`None` on an EGL device), for the window-surface
    /// code's binds and posts.
    #[inline]
    pub(crate) fn glx(&self) -> Option<&super::glx::GlxContext> {
        self.shared.glx()
    }

    /// Polls the driver's graphics-reset status and latches the device lost on any reset.
    pub(crate) fn poll_reset(&self, at: &str) -> Result<(), String> {
        let s = self.shared;
        let injected = s.inject_reset.swap(ffi::GL_NO_ERROR, Ordering::AcqRel);
        let status = if injected != ffi::GL_NO_ERROR {
            injected
        } else if let Some(query) = s.reset_status {
            // SAFETY: the context is current (this guard), and the function is its version's or
            // extension's (`reset_query`).
            unsafe { query() }
        } else {
            return Ok(());
        };
        if status == ffi::GL_NO_ERROR {
            return Ok(());
        }
        let e = format!(
            "gl: {at}: the context was reset ({})",
            ffi::gl_reset_name(status)
        );
        s.latch_lost(&e);
        Err(e)
    }
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        if self.outer.is_none() {
            return;
        }
        let s = self.shared;
        #[cfg(not(target_arch = "wasm32"))]
        s.release();
        #[cfg(target_arch = "wasm32")]
        {
            let _ = s;
            ffi::webgl::make_current(0);
        }
        HELD.with(|h| h.set(std::ptr::null()));
    }
}

#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
impl GlShared {
    /// The GLX context, on an X11 window system's GLX device.
    #[inline]
    pub(crate) fn glx(&self) -> Option<&super::glx::GlxContext> {
        match &self.binding {
            Binding::Glx(g) => Some(g),
            Binding::Egl(_) => None,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl GlShared {
    /// Reads the last EGL error for `call` and latches [`Self::lost`] on `EGL_CONTEXT_LOST`.
    pub(crate) fn egl_failure(&self, call: &str) -> String {
        let Some(b) = self.egl_binding() else {
            return format!("gl: {call} failed");
        };
        // SAFETY: eglGetError takes no arguments and is callable at any time.
        let code = unsafe { (b.egl.fns.get_error)() };
        self.egl_error(call, code)
    }

    /// The EGL half of an EGL device (`None` on a GLX device).
    #[inline]
    pub(crate) fn egl_binding(&self) -> Option<&EglBinding> {
        match &self.binding {
            Binding::Egl(b) => Some(b),
            #[cfg(not(wgpu_arm))]
            Binding::Glx(_) => None,
        }
    }

    fn egl_error(&self, call: &str, code: EGLint) -> String {
        let e = format!("gl: {call}: {}", ffi::egl_error_name(code));
        if code == ffi::EGL_CONTEXT_LOST {
            self.latch_lost(&e);
        }
        e
    }

    /// The window system whose display this is (`None`: headless).
    #[inline]
    pub(crate) const fn window_system(&self) -> Option<WindowSystem> {
        self.window_system
    }

    /// The first latched loss's cause (`None` while the device is live).
    #[inline]
    pub(crate) fn loss_reason(&self) -> Option<&str> {
        self.loss_reason.get().map(String::as_str)
    }
}

impl GlShared {
    /// Returns whether EGL or GL has reported the context lost.
    #[inline]
    pub(crate) fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Latch the device lost, keeping `why` when it is the first cause.
    pub(crate) fn latch_lost(&self, why: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = self.loss_reason.set(why.to_owned());
        #[cfg(target_arch = "wasm32")]
        let _ = why;
        self.lost.store(true, Ordering::Release);
    }

    /// Arm the injected-loss drill, and say what it will do.
    #[inline]
    pub(crate) fn inject_context_loss(&self) -> &'static str {
        #[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
        if self.glx().is_some() {
            self.inject_reset(ffi::GL_UNKNOWN_CONTEXT_RESET);
            return "the GL device's next present reads GL_UNKNOWN_CONTEXT_RESET from its \
                    GLX context's reset check";
        }
        self.inject_loss.store(true, Ordering::Release);
        if cfg!(target_arch = "wasm32") {
            "the WebGL2 context's next bind reports it lost (webglcontextlost)"
        } else {
            "the GL device's next eglMakeCurrent reports EGL_CONTEXT_LOST"
        }
    }

    /// Have the next [`Current::poll_reset`] read `status` in place of the driver's answer.
    #[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
    #[inline]
    pub(crate) fn inject_reset(&self, status: ffi::GLenum) {
        self.inject_reset.store(status, Ordering::Release);
    }

    /// Whether the reset check asks the driver (the context was made to report resets and its
    /// `glGetGraphicsResetStatus` was found).
    #[cfg(all(test, not(wgpu_arm), not(target_arch = "wasm32")))]
    pub(crate) const fn asks_the_driver_for_resets(&self) -> bool {
        self.reset_status.is_some()
    }

    /// Make the context current on this thread under the device's lock.
    pub(crate) fn enter(&self) -> Result<Current<'_>, String> {
        // First, so a nested guard refuses too: a present holds one guard
        // throughout, and a loss latched in it ends the present at its next
        // step's guard, as when every step bound the context itself.
        if self.lost() {
            #[cfg(not(target_arch = "wasm32"))]
            let why = self.loss_reason().unwrap_or("EGL_CONTEXT_LOST");
            #[cfg(target_arch = "wasm32")]
            let why = "EGL_CONTEXT_LOST";
            return Err(format!("gl: the context is lost ({why})"));
        }
        if HELD.with(Cell::get) == std::ptr::from_ref(self) {
            return Ok(Current {
                shared: self,
                outer: None,
            });
        }
        if !HELD.with(Cell::get).is_null() {
            return Err(
                "gl: another GL device is current on this thread; the arm does not nest devices"
                    .to_owned(),
            );
        }
        let guard = self
            .lock
            .lock()
            .map_err(|_| "gl: the device lock is poisoned".to_owned())?;
        self.make_current()?;
        HELD.with(|h| h.set(std::ptr::from_ref(self)));
        Ok(Current {
            shared: self,
            outer: Some(guard),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl GlShared {
    fn make_current(&self) -> Result<(), String> {
        if self.inject_loss.swap(false, Ordering::AcqRel) {
            return Err(self.egl_error("eglMakeCurrent", ffi::EGL_CONTEXT_LOST));
        }
        match &self.binding {
            Binding::Egl(b) => {
                // SAFETY: the display, surface and context were created together on this display;
                // the lock keeps every other thread off them.
                let ok =
                    unsafe { (b.egl.fns.make_current)(b.display, b.surface, b.surface, b.context) };
                if ok != ffi::EGL_TRUE {
                    return Err(self.egl_failure("eglMakeCurrent"));
                }
                Ok(())
            }
            #[cfg(not(wgpu_arm))]
            Binding::Glx(g) => g
                .make_current()
                .inspect_err(|e| self.latch_lost(&format!("{e} (on its own pbuffer)"))),
        }
    }

    fn release(&self) {
        match &self.binding {
            // SAFETY: the display is initialized; unbinding this thread's context is always legal.
            Binding::Egl(b) => unsafe {
                (b.egl.fns.make_current)(
                    b.display,
                    ffi::EGL_NO_SURFACE,
                    ffi::EGL_NO_SURFACE,
                    ffi::EGL_NO_CONTEXT,
                );
            },
            #[cfg(not(wgpu_arm))]
            Binding::Glx(g) => g.release(),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl GlShared {
    fn make_current(&self) -> Result<(), String> {
        let lost = "gl: the WebGL2 context is lost (webglcontextlost)";
        if self.inject_loss.swap(false, Ordering::AcqRel) {
            self.lost.store(true, Ordering::Release);
            return Err(lost.to_owned());
        }
        if ffi::webgl::make_current(self.context) == 0 {
            return Err("gl: the WebGL2 context is gone".to_owned());
        }
        if ffi::webgl::context_lost() != 0 {
            ffi::webgl::make_current(0);
            self.lost.store(true, Ordering::Release);
            return Err(lost.to_owned());
        }
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for GlShared {
    fn drop(&mut self) {
        ffi::webgl::context_drop(self.context);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for GlShared {
    fn drop(&mut self) {
        let Some(b) = self.egl_binding() else {
            return;
        };
        // SAFETY: nothing else holds these handles any more.
        unsafe {
            if b.surface != ffi::EGL_NO_SURFACE {
                (b.egl.fns.destroy_surface)(b.display, b.surface);
            }
            (b.egl.fns.destroy_context)(b.display, b.context);
            if b.tracked {
                (b.egl.fns.terminate)(b.display);
            }
        }
    }
}

/// Whether the space-separated extension list `list` names `ext`.
#[cfg(not(target_arch = "wasm32"))]
fn has_ext(list: &str, ext: &str) -> bool {
    list.split_ascii_whitespace().any(|e| e == ext)
}

#[cfg(not(target_arch = "wasm32"))]
impl GlDevice {
    /// A headless device: no window system, rendering only into textures it reads back.
    pub(crate) fn headless() -> Result<Self, String> {
        // Creation binds the new context on this thread, which would unbind a
        // device held here.
        if !HELD.with(Cell::get).is_null() {
            return Err(
                "gl: a GL device is held on this thread; create devices outside a guard".to_owned(),
            );
        }
        let egl = Egl::get()?;
        // SAFETY: querying client extensions on EGL_NO_DISPLAY is defined by
        // EGL_EXT_client_extensions; a null answer means none.
        let client_exts = unsafe {
            ffi::c_string((egl.fns.query_string)(
                ffi::EGL_NO_DISPLAY,
                ffi::EGL_EXTENSIONS,
            ))
        };
        let tracked = has_ext(&client_exts, "EGL_KHR_display_reference");
        let attribs: &[ffi::EGLAttrib] = if tracked {
            &[
                ffi::EGL_TRACK_REFERENCES_KHR,
                ffi::EGL_TRUE as ffi::EGLAttrib,
                ffi::EGL_NONE as ffi::EGLAttrib,
            ]
        } else {
            &[ffi::EGL_NONE as ffi::EGLAttrib]
        };
        let mut display = ffi::EGL_NO_DISPLAY;
        if has_ext(&client_exts, "EGL_EXT_platform_device")
            && has_ext(&client_exts, "EGL_EXT_device_enumeration")
            && let Some(device) = Self::pick_device(egl, &client_exts)
        {
            // SAFETY: a device enumerated just now and a terminated list.
            display = unsafe {
                (egl.fns.get_platform_display)(
                    ffi::EGL_PLATFORM_DEVICE_EXT,
                    device,
                    attribs.as_ptr(),
                )
            };
        }
        if display == ffi::EGL_NO_DISPLAY && has_ext(&client_exts, "EGL_MESA_platform_surfaceless")
        {
            // SAFETY: a known platform enum, the default native display and an EGL_NONE-terminated
            // attribute list.
            display = unsafe {
                (egl.fns.get_platform_display)(
                    ffi::EGL_PLATFORM_SURFACELESS_MESA,
                    ffi::EGL_DEFAULT_DISPLAY,
                    attribs.as_ptr(),
                )
            };
        }
        let mut tracked = tracked && display != ffi::EGL_NO_DISPLAY;
        if display == ffi::EGL_NO_DISPLAY {
            // SAFETY: EGL_DEFAULT_DISPLAY is always a legal argument.
            display = unsafe { (egl.fns.get_display)(ffi::EGL_DEFAULT_DISPLAY) };
            tracked = false;
        }
        if display == ffi::EGL_NO_DISPLAY {
            return Err(format!("gl: no EGL display ({})", egl.error_name()));
        }
        Self::on_display(egl, display, tracked, None)
    }

    /// A device on a window system's display, the one its windows' surfaces are made on.
    #[cfg(not(wgpu_arm))]
    pub(crate) fn on_window_system(ws: WindowSystem) -> Result<Self, String> {
        Self::on_window_system_via(ws, Self::on_glx)
    }

    /// [`Self::on_window_system`] in a build carrying wgpu, which attaches windows on wgpu and has
    /// no GLX: the display's EGL platform.
    #[cfg(wgpu_arm)]
    pub(crate) fn on_window_system(ws: WindowSystem) -> Result<Self, String> {
        if !HELD.with(Cell::get).is_null() {
            return Err(
                "gl: a GL device is held on this thread; create devices outside a guard".to_owned(),
            );
        }
        Self::on_egl_window_system(ws)
    }

    /// [`Self::on_window_system`] with `glx` as the X11 device's mint (a test refuses it).
    #[cfg(not(wgpu_arm))]
    pub(super) fn on_window_system_via(
        ws: WindowSystem,
        glx: impl FnOnce(usize, i32, WindowSystem) -> Result<Self, String>,
    ) -> Result<Self, String> {
        if !HELD.with(Cell::get).is_null() {
            return Err(
                "gl: a GL device is held on this thread; create devices outside a guard".to_owned(),
            );
        }
        if let WindowSystem::X11 { display, screen } = ws {
            return glx(display, screen, ws).map_err(|e| {
                format!(
                    "{e}; X11 windows draw on the CPU renderer (no EGL fallback: NVIDIA's X11 \
                     EGL posts nothing on Xorg)"
                )
            });
        }
        Self::on_egl_window_system(ws)
    }

    /// [`Self::on_window_system`]'s EGL device: the display's EGL platform.
    pub(super) fn on_egl_window_system(ws: WindowSystem) -> Result<Self, String> {
        let egl = Egl::get()?;
        // SAFETY: as in `headless`.
        let client_exts = unsafe {
            ffi::c_string((egl.fns.query_string)(
                ffi::EGL_NO_DISPLAY,
                ffi::EGL_EXTENSIONS,
            ))
        };
        let (platform, native, ext, screen) = match ws {
            WindowSystem::X11 { display, screen } => (
                ffi::EGL_PLATFORM_X11_KHR,
                display,
                "EGL_KHR_platform_x11",
                Some(screen),
            ),
            WindowSystem::Wayland { display } => (
                ffi::EGL_PLATFORM_WAYLAND_KHR,
                display,
                "EGL_KHR_platform_wayland",
                None,
            ),
        };
        let ext_alias = ext.replace("KHR", "EXT");
        if !has_ext(&client_exts, ext) && !has_ext(&client_exts, &ext_alias) {
            return Err(format!("gl: this EGL has no {ext}"));
        }
        let mut attribs: Vec<ffi::EGLAttrib> = Vec::with_capacity(3);
        if let Some(screen) = screen {
            attribs.push(ffi::EGL_PLATFORM_X11_SCREEN_KHR);
            attribs.push(screen as ffi::EGLAttrib);
        }
        attribs.push(ffi::EGL_NONE as ffi::EGLAttrib);
        // SAFETY: a platform this EGL advertises, the caller's live native display, and an
        // EGL_NONE-terminated attribute list.
        let display = unsafe {
            (egl.fns.get_platform_display)(platform, native as *mut c_void, attribs.as_ptr())
        };
        if display == ffi::EGL_NO_DISPLAY {
            return Err(format!(
                "gl: no EGL display on the window system ({})",
                egl.error_name()
            ));
        }
        Self::on_display(egl, display, false, Some(ws))
    }

    /// A device on an X11 display's GLX (see `super::glx` for why X11 is
    /// GLX's): a GLX OpenGL ES 3 context, current on its pbuffer between
    /// posts, which a present asks for resets where the context reports them.
    #[cfg(not(wgpu_arm))]
    fn on_glx(display: usize, screen: i32, ws: WindowSystem) -> Result<Self, String> {
        Self::on_glx_context(
            super::glx::GlxContext::new(display as *mut c_void, screen)?,
            ws,
        )
    }

    /// [`Self::on_glx`]'s device on the context `glx` (a test makes one robust where GLX lists no
    /// robustness).
    #[cfg(not(wgpu_arm))]
    pub(super) fn on_glx_context(
        glx: super::glx::GlxContext,
        ws: WindowSystem,
    ) -> Result<Self, String> {
        glx.make_current()?;
        let loaded = Self::load_gl(&mut |sym: &CStr| glx.proc_address(sym));
        glx.release();
        let (gl, caps) = loaded?;
        let reset_status = caps
            .reset_query
            .filter(|_| glx.robustness().reports_resets())
            .map(|name| glx.proc_address(name))
            .filter(|p| !p.is_null())
            // SAFETY: the name the context's version or extension defines (`reset_query`), whose
            // registry signature the alias declares.
            .map(|p| unsafe {
                std::mem::transmute::<*const c_void, ffi::GetGraphicsResetStatusFn>(p)
            });
        let shared = GlShared {
            binding: Binding::Glx(glx),
            window_system: Some(ws),
            lost: AtomicBool::new(false),
            loss_reason: std::sync::OnceLock::new(),
            inject_loss: AtomicBool::new(false),
            reset_status,
            inject_reset: AtomicU32::new(ffi::GL_NO_ERROR),
            gl,
            caps,
            lock: Mutex::new(()),
            retired: Mutex::new(Vec::new()),
        };
        let dev = GlDevice(Arc::new(shared));
        dev.init_state()?;
        Ok(dev)
    }

    fn pick_device(egl: &Egl, client_exts: &str) -> Option<ffi::EGLDeviceEXT> {
        // SAFETY: eglGetProcAddress returns null or the named extension function, whose registry
        // signature the alias types declare.
        let (query_devices, query_string) = unsafe {
            let q = (egl.fns.get_proc_address)(c"eglQueryDevicesEXT".as_ptr());
            let s = (egl.fns.get_proc_address)(c"eglQueryDeviceStringEXT".as_ptr());
            if q.is_null() {
                return None;
            }
            (
                std::mem::transmute::<*const c_void, ffi::QueryDevicesFn>(q),
                (!s.is_null() && has_ext(client_exts, "EGL_EXT_device_query"))
                    .then(|| std::mem::transmute::<*const c_void, ffi::QueryDeviceStringFn>(s)),
            )
        };
        let mut devices = [std::ptr::null_mut(); 16];
        let mut n = 0;
        // SAFETY: room for 16 devices; out-pointer live.
        let ok =
            unsafe { query_devices(devices.len() as EGLint, devices.as_mut_ptr(), &raw mut n) };
        let found = &devices[..usize::try_from(n).unwrap_or(0).min(devices.len())];
        if ok != ffi::EGL_TRUE || found.is_empty() {
            return None;
        }
        let hardware = query_string.and_then(|query| {
            found.iter().copied().find(|&d| {
                // SAFETY: an enumerated device; the string is static.
                let exts = unsafe { ffi::c_string(query(d, ffi::EGL_EXTENSIONS)) };
                has_ext(&exts, "EGL_EXT_device_drm")
            })
        });
        Some(hardware.unwrap_or(found[0]))
    }

    fn on_display(
        egl: &'static Egl,
        display: EGLDisplay,
        tracked: bool,
        window_system: Option<WindowSystem>,
    ) -> Result<Self, String> {
        let surface_bit = if window_system.is_some() {
            ffi::EGL_WINDOW_BIT
        } else {
            ffi::EGL_PBUFFER_BIT
        };
        let (mut major, mut minor) = (0, 0);
        // SAFETY: a display handle from eglGet*Display; out-pointers live.
        if unsafe { (egl.fns.initialize)(display, &raw mut major, &raw mut minor) } != ffi::EGL_TRUE
        {
            return Err(format!("gl: eglInitialize: {}", egl.error_name()));
        }
        let fail = |e: String| -> String {
            if tracked {
                // SAFETY: balances the eglInitialize above (reference-tracked).
                unsafe { (egl.fns.terminate)(display) };
            }
            e
        };
        if (major, minor) < (1, 5) {
            return Err(fail(format!(
                "gl: EGL {major}.{minor}; the GL arm needs EGL 1.5"
            )));
        }
        // SAFETY: an initialized display.
        let display_exts =
            unsafe { ffi::c_string((egl.fns.query_string)(display, ffi::EGL_EXTENSIONS)) };
        // SAFETY: binding the GLES API on this thread is always legal.
        if unsafe { (egl.fns.bind_api)(ffi::EGL_OPENGL_ES_API) } != ffi::EGL_TRUE {
            return Err(fail(format!("gl: eglBindAPI(GLES): {}", egl.error_name())));
        }
        let config_attribs = [
            ffi::EGL_RENDERABLE_TYPE,
            ffi::EGL_OPENGL_ES3_BIT,
            ffi::EGL_SURFACE_TYPE,
            surface_bit,
            ffi::EGL_RED_SIZE,
            8,
            ffi::EGL_GREEN_SIZE,
            8,
            ffi::EGL_BLUE_SIZE,
            8,
            ffi::EGL_ALPHA_SIZE,
            if window_system.is_some() { 0 } else { 8 },
            ffi::EGL_NONE,
        ];
        let mut config: EGLConfig = std::ptr::null_mut();
        let mut n = 0;
        // SAFETY: an EGL_NONE-terminated attribute list, room for one config.
        let ok = unsafe {
            (egl.fns.choose_config)(
                display,
                config_attribs.as_ptr(),
                &raw mut config,
                1,
                &raw mut n,
            )
        };
        if ok != ffi::EGL_TRUE || n < 1 {
            return Err(fail(format!(
                "gl: no RGBA8 GLES 3 EGL config ({})",
                egl.error_name()
            )));
        }
        let surfaceless = has_ext(&display_exts, "EGL_KHR_surfaceless_context");
        let context_config = if window_system.is_some()
            && surfaceless
            && has_ext(&display_exts, "EGL_KHR_no_config_context")
        {
            ffi::EGL_NO_CONFIG_KHR
        } else {
            config
        };
        let context_attribs = [
            ffi::EGL_CONTEXT_MAJOR_VERSION,
            3,
            ffi::EGL_CONTEXT_MINOR_VERSION,
            0,
            ffi::EGL_NONE,
        ];
        // SAFETY: a config of this display (or no config, which the display allows); no share
        // context.
        let context = unsafe {
            (egl.fns.create_context)(
                display,
                context_config,
                ffi::EGL_NO_CONTEXT,
                context_attribs.as_ptr(),
            )
        };
        if context == ffi::EGL_NO_CONTEXT {
            return Err(fail(format!(
                "gl: eglCreateContext(GLES 3.0): {}",
                egl.error_name()
            )));
        }
        let surface = if surfaceless {
            ffi::EGL_NO_SURFACE
        } else if window_system.is_some() {
            // SAFETY: the context made just above, unused.
            unsafe { (egl.fns.destroy_context)(display, context) };
            return Err(fail(
                "gl: the window system's EGL display has no EGL_KHR_surfaceless_context".to_owned(),
            ));
        } else {
            let pbuffer = [ffi::EGL_WIDTH, 1, ffi::EGL_HEIGHT, 1, ffi::EGL_NONE];
            // SAFETY: a pbuffer-capable config (surface_bit) and a terminated attribute list.
            let s = unsafe { (egl.fns.create_pbuffer_surface)(display, config, pbuffer.as_ptr()) };
            if s == ffi::EGL_NO_SURFACE {
                // SAFETY: the context made just above, unused.
                unsafe { (egl.fns.destroy_context)(display, context) };
                return Err(fail(format!(
                    "gl: eglCreatePbufferSurface: {}",
                    egl.error_name()
                )));
            }
            s
        };
        // SAFETY: handles created above on this display.
        if unsafe { (egl.fns.make_current)(display, surface, surface, context) } != ffi::EGL_TRUE {
            let e = egl.error_name();
            // SAFETY: as above, unused.
            unsafe { (egl.fns.destroy_context)(display, context) };
            return Err(fail(format!("gl: eglMakeCurrent: {e}")));
        }
        // SAFETY: eglGetProcAddress returns null or the named GL function, whose registry signature
        // the table declares.
        let loaded =
            Self::load_gl(&mut |sym: &CStr| unsafe { (egl.fns.get_proc_address)(sym.as_ptr()) });
        // SAFETY: unbinding is always legal.
        unsafe {
            (egl.fns.make_current)(
                display,
                ffi::EGL_NO_SURFACE,
                ffi::EGL_NO_SURFACE,
                ffi::EGL_NO_CONTEXT,
            )
        };
        let (gl, caps) = match loaded {
            Ok(v) => v,
            Err(e) => {
                // SAFETY: as above, unused.
                unsafe { (egl.fns.destroy_context)(display, context) };
                return Err(fail(e));
            }
        };
        let shared = GlShared {
            binding: Binding::Egl(EglBinding {
                egl,
                display,
                context,
                surface,
                tracked,
                config: context_config,
            }),
            window_system,
            lost: AtomicBool::new(false),
            loss_reason: std::sync::OnceLock::new(),
            inject_loss: AtomicBool::new(false),
            #[cfg(not(wgpu_arm))]
            reset_status: None,
            #[cfg(not(wgpu_arm))]
            inject_reset: AtomicU32::new(ffi::GL_NO_ERROR),
            gl,
            caps,
            lock: Mutex::new(()),
            retired: Mutex::new(Vec::new()),
        };
        let dev = GlDevice(Arc::new(shared));
        dev.init_state()?;
        Ok(dev)
    }

    fn load_gl(lookup: &mut dyn FnMut(&CStr) -> *const c_void) -> Result<(GlFns, GlCaps), String> {
        // SAFETY: the lookup returns null or the named GL function, whose registry signature the
        // table declares.
        let gl = unsafe { GlFns::load(lookup) }?;
        // SAFETY: the context is current; glGetString returns a static string.
        let string = |name| unsafe { ffi::c_string((gl.get_string)(name).cast()) };
        let renderer = string(ffi::GL_RENDERER);
        let version = string(ffi::GL_VERSION);
        if !version.starts_with("OpenGL ES 3") {
            return Err(format!("gl: the context is {version:?}, not OpenGL ES 3.x"));
        }
        let mut max_tex = 0;
        // SAFETY: a valid enum and a live out-pointer.
        unsafe { (gl.get_integerv)(ffi::GL_MAX_TEXTURE_SIZE, &raw mut max_tex) };
        let mut n_ext = 0;
        // SAFETY: as above.
        unsafe { (gl.get_integerv)(ffi::GL_NUM_EXTENSIONS, &raw mut n_ext) };
        let mut exts = Vec::new();
        for i in 0..u32::try_from(n_ext).unwrap_or(0) {
            // SAFETY: i < GL_NUM_EXTENSIONS.
            exts.push(unsafe { ffi::c_string((gl.get_stringi)(ffi::GL_EXTENSIONS, i).cast()) });
        }
        let has = |e: &str| exts.iter().any(|x| x == e);
        let caps = GlCaps {
            #[cfg(not(wgpu_arm))]
            reset_query: reset_query(&version, has),
            #[cfg(not(wgpu_arm))]
            bgra_read: has("GL_EXT_read_format_bgra"),
            renderer,
            version,
            max_texture_dim: u32::try_from(max_tex).unwrap_or(0),
            srgb_views: has("GL_EXT_sRGB_write_control") && has("GL_EXT_texture_sRGB_decode"),
        };
        Ok((gl, caps))
    }
}

/// The name of `glGetGraphicsResetStatus` on a context whose `GL_VERSION` is
/// `version` and whose extensions `has` names: GLES 3.2's core name, else the
/// spelling of the robustness extension it lists (`KHR`, `EXT`, then `ARB`);
/// `None` when it has none.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
fn reset_query(version: &str, has: impl Fn(&str) -> bool) -> Option<&'static CStr> {
    let minor = version
        .strip_prefix("OpenGL ES 3.")
        .map(|rest| {
            rest.bytes()
                .take_while(u8::is_ascii_digit)
                .fold(0_u32, |n, d| {
                    n.saturating_mul(10).saturating_add(u32::from(d - b'0'))
                })
        })
        .unwrap_or(0);
    if minor >= 2 {
        return Some(c"glGetGraphicsResetStatus");
    }
    [
        ("GL_KHR_robustness", c"glGetGraphicsResetStatusKHR"),
        ("GL_EXT_robustness", c"glGetGraphicsResetStatusEXT"),
        ("GL_ARB_robustness", c"glGetGraphicsResetStatusARB"),
    ]
    .into_iter()
    .find_map(|(ext, name)| has(ext).then_some(name))
}

#[cfg(target_arch = "wasm32")]
impl GlDevice {
    /// A device on `canvas`'s WebGL2 context.
    pub(crate) fn on_canvas(canvas: aterm_wasm_abi::js::JsObject) -> Result<Self, String> {
        if !HELD.with(Cell::get).is_null() {
            return Err(
                "gl: a GL device is held on this thread; create devices outside a guard".to_owned(),
            );
        }
        let context = ffi::webgl::context_create(canvas.index());
        if context == 0 {
            return Err(
                "canvas.getContext() returned null; webgl2 not available or canvas already in use"
                    .to_owned(),
            );
        }
        let gl = GlFns::imported();
        ffi::webgl::make_current(context);
        let caps = Self::load_webgl(&gl);
        ffi::webgl::make_current(0);
        let caps = caps.inspect_err(|_| ffi::webgl::context_drop(context))?;
        let shared = GlShared {
            context,
            _canvas: canvas,
            lost: AtomicBool::new(false),
            inject_loss: AtomicBool::new(false),
            gl,
            caps,
            lock: Mutex::new(()),
            retired: Mutex::new(Vec::new()),
        };
        let dev = GlDevice(Arc::new(shared));
        dev.init_state()?;
        Ok(dev)
    }

    fn load_webgl(gl: &GlFns) -> Result<GlCaps, String> {
        // SAFETY: the context is current; glGetString returns null or a NUL-terminated string the
        // module owns for the context's life.
        let string = |name| unsafe { ffi::c_string((gl.get_string)(name).cast()) };
        let version = string(ffi::GL_VERSION);
        if !version.starts_with("WebGL 2") {
            return Err(format!(
                "gl: the canvas context is {version:?}, not WebGL 2"
            ));
        }
        let unmasked = string(ffi::GL_UNMASKED_RENDERER_WEBGL);
        let renderer = if unmasked.is_empty() {
            string(ffi::GL_RENDERER)
        } else {
            unmasked
        };
        let mut max_tex = 0;
        // SAFETY: a valid enum and a live out-pointer.
        unsafe { (gl.get_integerv)(ffi::GL_MAX_TEXTURE_SIZE, &raw mut max_tex) };
        Ok(GlCaps {
            renderer,
            version,
            max_texture_dim: u32::try_from(max_tex).unwrap_or(0),
            srgb_views: false,
        })
    }
}

impl GlDevice {
    fn init_state(&self) -> Result<(), String> {
        let cur = self.0.enter()?;
        let gl = cur.gl();
        // SAFETY: the context is current (`cur`); valid capability enums.
        unsafe {
            (gl.disable)(ffi::GL_DITHER);
            (gl.disable)(ffi::GL_DEPTH_TEST);
            (gl.disable)(ffi::GL_CULL_FACE);
            (gl.disable)(ffi::GL_BLEND);
            (gl.disable)(ffi::GL_SCISSOR_TEST);
        }
        cur.check("initial state")
    }

    /// Make the context current on this thread for the guard's life.
    #[inline]
    pub(crate) fn enter(&self) -> Result<Current<'_>, String> {
        self.0.enter()
    }

    /// The device's capabilities.
    #[inline]
    pub(crate) fn caps(&self) -> &GlCaps {
        &self.0.caps
    }

    /// The shared half, for resources to hold.
    #[inline]
    pub(crate) const fn shared(&self) -> &Arc<GlShared> {
        &self.0
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    use super::super::test_device as device;

    #[test]
    fn the_context_is_current_only_inside_a_guard_and_moves_between_threads() {
        let dev = device();
        let other = device();
        let egl = Egl::get().expect("the device loaded EGL");
        // SAFETY: eglGetProcAddress returns the core entry point (EGL 1.5), whose registry
        // signature is `EGLContext eglGetCurrentContext(void)`.
        let get_current: unsafe extern "C" fn() -> ffi::EGLContext =
            unsafe {
                std::mem::transmute::<*const c_void, unsafe extern "C" fn() -> ffi::EGLContext>(
                    (egl.fns.get_proc_address)(c"eglGetCurrentContext".as_ptr()),
                )
            };
        // SAFETY: callable at any time.
        let current = || unsafe { get_current() };
        assert!(current().is_null(), "nothing is current before a guard");
        {
            let outer = dev.enter().expect("enter");
            let context = dev
                .0
                .egl_binding()
                .expect("a headless device is EGL's")
                .context;
            assert_eq!(current(), context, "current inside the guard");
            {
                let _inner = dev.enter().expect("a nested enter on the holding thread");
                assert_eq!(current(), context);
            }
            assert_eq!(current(), context, "the nested guard released nothing");
            outer.check("a clean context").expect("no GL error");
            let err = other
                .enter()
                .err()
                .expect("a second device while the first is held");
            assert!(err.contains("another GL device"), "{err}");
            let err = GlDevice::headless().expect_err("creation while a device is held");
            assert!(err.contains("held on this thread"), "{err}");
        }
        assert!(current().is_null(), "the outer guard released the context");
        {
            let _held = other.enter().expect("enter the other device");
            other.0.latch_lost("gl: drilled");
            let err = other
                .enter()
                .err()
                .expect("a nested enter after a latched loss");
            assert!(err.contains("lost (gl: drilled)"), "{err}");
        }
        assert!(current().is_null(), "the refused nested guard kept nothing");
        let moved = dev.clone();
        std::thread::spawn(move || {
            let cur = moved.enter().expect("enter on another thread");
            cur.check("other thread").expect("no GL error");
        })
        .join()
        .expect("the other thread");
        assert!(
            dev.caps().max_texture_dim >= 2048,
            "GLES 3.0 guarantees 2048"
        );
    }

    #[test]
    fn a_lost_context_at_make_current_latches_the_device_lost() {
        let dev = device();
        let control = device();
        assert!(!dev.0.lost(), "a fresh device is live");
        dev.0.inject_context_loss();
        let err = dev
            .enter()
            .err()
            .expect("the drilled bind fails as a lost context's");
        assert!(err.contains("EGL_CONTEXT_LOST"), "{err}");
        assert!(dev.0.lost(), "the failed bind latched the loss");
        let err = dev
            .enter()
            .err()
            .expect("a lost device refuses every guard");
        assert!(err.contains("lost"), "{err}");
        assert!(dev.0.lost(), "a lost device stays lost");
        let cur = control.enter().expect("the undrilled device binds");
        cur.check("the control").expect("no GL error");
        drop(cur);
        assert!(!control.0.lost(), "the latch is per device");
        assert_eq!(control.0.loss_reason(), None);
        assert!(
            dev.0
                .loss_reason()
                .is_some_and(|r| r.contains("EGL_CONTEXT_LOST")),
            "the latch keeps its cause"
        );
    }

    /// A present's reset check latches the device lost on any reset status
    /// but `GL_NO_ERROR`, NVIDIA's purge among them, and names the status as
    /// the cause the frontend's downgrade line prints.
    #[cfg(not(wgpu_arm))]
    #[test]
    fn a_reset_status_other_than_no_error_latches_the_device_lost() {
        for status in [
            ffi::GL_GUILTY_CONTEXT_RESET,
            ffi::GL_INNOCENT_CONTEXT_RESET,
            ffi::GL_UNKNOWN_CONTEXT_RESET,
            ffi::GL_PURGED_CONTEXT_RESET_NV,
        ] {
            let name = ffi::gl_reset_name(status);
            let dev = device();
            dev.0.inject_reset(status);
            let cur = dev.enter().expect("enter");
            let err = cur
                .poll_reset("present")
                .expect_err("a reset fails the check");
            drop(cur);
            assert!(
                err.contains(&format!("present: the context was reset ({name})")),
                "{err}"
            );
            assert!(dev.0.lost(), "{name} latched the device lost");
            assert_eq!(dev.0.loss_reason(), Some(err.as_str()));
            let refused = dev
                .enter()
                .err()
                .expect("a lost device refuses every guard");
            assert!(refused.contains(&name), "{refused}");
        }
        let control = device();
        let cur = control.enter().expect("enter");
        cur.poll_reset("present")
            .expect("the driver reports no reset on a live context");
        control.0.inject_reset(ffi::GL_NO_ERROR);
        cur.poll_reset("present").expect("GL_NO_ERROR is no reset");
        drop(cur);
        assert!(!control.0.lost(), "a live device stays live");
        assert_eq!(control.0.loss_reason(), None);
    }

    /// The reset check's function is read by name, never guessed (glvnd
    /// hands out a stub for any name): GLES 3.2's core
    /// `glGetGraphicsResetStatus` (NVIDIA's and Mesa's ES contexts on this
    /// desk are 3.2), else the spelling of the robustness extension the
    /// context lists, `KHR` before `EXT` before `ARB`; none on a 3.0 or 3.1
    /// context that lists none.
    #[cfg(not(wgpu_arm))]
    #[test]
    fn the_reset_query_is_gles_3_2_s_or_the_robustness_extension_s() {
        let none = |_: &str| false;
        let only = |ext: &'static str| move |e: &str| e == ext;
        assert_eq!(
            reset_query("OpenGL ES 3.2 NVIDIA 580.159.03", none),
            Some(c"glGetGraphicsResetStatus")
        );
        assert_eq!(
            reset_query("OpenGL ES 3.2 Mesa 25.2.8", none),
            Some(c"glGetGraphicsResetStatus")
        );
        assert_eq!(reset_query("OpenGL ES 3.0 Mesa 25.2.8", none), None);
        assert_eq!(reset_query("OpenGL ES 3.1 v1.r38p1", none), None);
        assert_eq!(
            reset_query("OpenGL ES 3.1 v1.r38p1", only("GL_KHR_robustness")),
            Some(c"glGetGraphicsResetStatusKHR")
        );
        assert_eq!(
            reset_query("OpenGL ES 3.0", only("GL_EXT_robustness")),
            Some(c"glGetGraphicsResetStatusEXT")
        );
        assert_eq!(
            reset_query("OpenGL ES 3.0", only("GL_ARB_robustness")),
            Some(c"glGetGraphicsResetStatusARB")
        );
        assert_eq!(
            reset_query("OpenGL ES 3.0", |e| matches!(
                e,
                "GL_ARB_robustness" | "GL_EXT_robustness" | "GL_KHR_robustness"
            )),
            Some(c"glGetGraphicsResetStatusKHR")
        );
    }
}
