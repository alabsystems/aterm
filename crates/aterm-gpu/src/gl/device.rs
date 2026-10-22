// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's device: one EGL display, config and OpenGL ES 3.0 context (on
//! wasm32, one WebGL2 context on a canvas), the GL function table loaded for
//! it, and what the driver can do.
//!
//! # Current-ness
//!
//! A GL context is bound to at most one thread at a time, and every GL call
//! acts on the calling thread's current context. The arm never assumes its
//! context is still current: [`GlDevice::enter`] takes the device's lock, makes
//! the context current on this thread, and the returned [`Current`] guard
//! releases it again on drop. That is what lets two contexts share a thread
//! (the arm and wgpu's GL backend, the differential oracle, which also releases
//! after each operation) and lets the device move between threads (`Send`):
//! nothing stays current behind the guard's back. A nested `enter` on the
//! thread that already holds the device is a no-op guard, so a resource can be
//! dropped inside an encode.
//!
//! # Displays
//!
//! A headless device ([`GlDevice::headless`]) opens a display of its own. A
//! device that presents to windows ([`GlDevice::on_window_system`]) opens the
//! window system's display (an Xlib `Display*` or a `wl_display*`), because an
//! EGL window surface can be made current only with a context of its own
//! display. Its context is made with no config where the display allows it
//! (`EGL_KHR_no_config_context`), so each window's surface can take the config
//! that matches its visual.
//!
//! # WebGL2 (wasm32)
//!
//! A browser has no displays and no headless contexts: a WebGL2 context lives
//! on a canvas ([`GlDevice::on_canvas`]), whose drawing buffer is the
//! context's default framebuffer, so one device is one canvas and its window
//! surface at once. Making it current tells the glue which context the GL
//! imports act on (`ffi::webgl`). The browser loses a context on its own
//! (`webglcontextlost`): `isContextLost()` at every `enter`, and
//! `CONTEXT_LOST_WEBGL` from `getError`, latch it as `EGL_CONTEXT_LOST` does.

use std::cell::Cell;
#[cfg(not(target_arch = "wasm32"))]
use std::ffi::{CStr, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::ffi::{self, GlFns};
#[cfg(not(target_arch = "wasm32"))]
use super::ffi::{EGLConfig, EGLContext, EGLDisplay, EGLSurface, EGLint, Egl};

/// What the driver offers beyond GLES 3.0 core, read once at creation.
#[derive(Clone, Debug)]
pub(crate) struct GlCaps {
    /// `GL_RENDERER` (e.g. `llvmpipe (LLVM 20.1.2, 128 bits)`).
    pub(crate) renderer: String,
    /// `GL_VERSION` (e.g. `OpenGL ES 3.2 Mesa 25.2.8`).
    pub(crate) version: String,
    /// `GL_MAX_TEXTURE_SIZE`.
    pub(crate) max_texture_dim: u32,
    /// `EXT_sRGB_write_control` and `EXT_texture_sRGB_decode` together: one
    /// `SRGB8_ALPHA8` texture can then be drawn and sampled both encoded and
    /// raw, which is the native format plan's Unorm/sRGB view pair
    /// (`format_plan`, `srgb_offscreen`). Without them the arm runs the
    /// downlevel plan, the one wgpu's GL backend runs.
    pub(crate) srgb_views: bool,
}

/// The shared half of a device: the EGL objects (on wasm32, the WebGL2
/// context and its canvas), the function table and the lock. Resources hold
/// an `Arc` of it so their `Drop` can delete their GL names on the context
/// that made them.
pub(crate) struct GlShared {
    #[cfg(not(target_arch = "wasm32"))]
    egl: &'static Egl,
    #[cfg(not(target_arch = "wasm32"))]
    display: EGLDisplay,
    #[cfg(not(target_arch = "wasm32"))]
    context: EGLContext,
    /// The context's draw surface when the display cannot bind without one (a
    /// 1x1 pbuffer), else `EGL_NO_SURFACE` (`EGL_KHR_surfaceless_context`).
    #[cfg(not(target_arch = "wasm32"))]
    surface: EGLSurface,
    /// Whether the display was opened with reference tracking, so this
    /// device's `eglTerminate` releases only its own reference.
    #[cfg(not(target_arch = "wasm32"))]
    tracked: bool,
    /// The config the context was made with, or `EGL_NO_CONFIG_KHR` for a
    /// context any surface of the display can bind.
    #[cfg(not(target_arch = "wasm32"))]
    #[cfg_attr(
        wgpu_arm,
        expect(dead_code, reason = "read by the shipped build's window surfaces")
    )]
    config: EGLConfig,
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
    /// Latched when EGL reports `EGL_CONTEXT_LOST` (or GL `GL_CONTEXT_LOST`)
    /// at any call the arm checks; the device is then dead, and
    /// `GpuRenderer::device_lost` reports it to the frontend's CPU fallback.
    lost: AtomicBool,
    /// The injected-loss drill (`ATERM_GL_INJECT_LOSS`): the next
    /// [`GlShared::enter`] answers its `eglMakeCurrent` as a lost context
    /// does, so the latch is driven through the path a real loss takes.
    inject_loss: AtomicBool,
    pub(crate) gl: GlFns,
    pub(crate) caps: GlCaps,
    lock: Mutex<()>,
}

/// The native display a windowed device was opened on, as raw handles carry
/// it (addresses, so the type stays `Send`).
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

// SAFETY: every GL and EGL call on these handles happens inside a `Current`
// guard, which holds `lock` and makes the context current on the calling
// thread for the guard's life; no handle is used on two threads at once.
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Send for GlShared {}
// SAFETY: as above: `&GlShared` reaches GL only through `enter`, serialized by
// `lock`.
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Sync for GlShared {}

thread_local! {
    /// The device this thread holds current inside a `Current` guard (null:
    /// none), so a nested `enter` on the same device is a no-op.
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

/// The context made current on this thread; released on drop (see the module
/// header).
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

    /// Drain the GL error queue; `Err` names every error raised since the last
    /// check, with `what` for context.
    pub(crate) fn check(&self, what: &str) -> Result<(), String> {
        let mut errors = Vec::new();
        // Bounded: a lost context can report an error on every call.
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
                self.shared.lost.store(true, Ordering::Release);
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
    /// Make `surface`, a window surface of this device's display, the
    /// context's draw and read surface for the rest of the guard: the default
    /// framebuffer (name 0) is then the window's back buffer. The guard's drop
    /// releases it with the context.
    pub(crate) fn bind_window(&self, surface: EGLSurface) -> Result<(), String> {
        let s = self.shared;
        // SAFETY: the surface was made on this display; the guard holds the
        // device lock, so no other thread uses the context.
        let ok = unsafe { (s.egl.fns.make_current)(s.display, surface, surface, s.context) };
        if ok == ffi::EGL_TRUE {
            Ok(())
        } else {
            Err(s.egl_failure("eglMakeCurrent(window)"))
        }
    }

    /// Post the bound window surface's back buffer (`eglSwapBuffers`).
    pub(crate) fn swap(&self, fns: &ffi::EglWindowFns, surface: EGLSurface) -> Result<(), String> {
        let s = self.shared;
        // SAFETY: as `bind_window`; the surface is current on this thread.
        let ok = unsafe { (fns.swap_buffers)(s.display, surface) };
        if ok == ffi::EGL_TRUE {
            Ok(())
        } else {
            Err(s.egl_failure("eglSwapBuffers"))
        }
    }

    /// Set the bound window surface's swap interval (0: post at once; 1: wait
    /// for the display's next refresh).
    pub(crate) fn swap_interval(
        &self,
        fns: &ffi::EglWindowFns,
        interval: i32,
    ) -> Result<(), String> {
        let s = self.shared;
        // SAFETY: a window surface is current on this thread.
        let ok = unsafe { (fns.swap_interval)(s.display, interval) };
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
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        if self.outer.is_none() {
            return;
        }
        let s = self.shared;
        // SAFETY: the display is initialized; unbinding this thread's context
        // is always legal and flushes it.
        #[cfg(not(target_arch = "wasm32"))]
        let _ = unsafe {
            (s.egl.fns.make_current)(
                s.display,
                ffi::EGL_NO_SURFACE,
                ffi::EGL_NO_SURFACE,
                ffi::EGL_NO_CONTEXT,
            )
        };
        // No context for the GL imports to act on until the next guard.
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
    /// The EGL entry points.
    #[inline]
    pub(crate) const fn egl(&self) -> &'static Egl {
        self.egl
    }

    /// The display the context lives on.
    #[inline]
    pub(crate) const fn display(&self) -> EGLDisplay {
        self.display
    }

    /// The context's config (`EGL_NO_CONFIG_KHR` for a config-less context).
    #[inline]
    pub(crate) const fn config(&self) -> EGLConfig {
        self.config
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl GlShared {
    /// The last EGL error as a message naming `call`, read once (`eglGetError`
    /// clears it); `EGL_CONTEXT_LOST` latches the device lost ([`Self::lost`]).
    /// Every EGL failure on a live device goes through here.
    pub(crate) fn egl_failure(&self, call: &str) -> String {
        // SAFETY: eglGetError takes no arguments and is callable at any time.
        let code = unsafe { (self.egl.fns.get_error)() };
        self.egl_error(call, code)
    }

    /// `code`, an EGL error `call` raised, as a message; `EGL_CONTEXT_LOST`
    /// latches the device lost.
    fn egl_error(&self, call: &str, code: EGLint) -> String {
        if code == ffi::EGL_CONTEXT_LOST {
            self.lost.store(true, Ordering::Release);
        }
        format!("gl: {call}: {}", ffi::egl_error_name(code))
    }

    /// The window system whose display this is (`None`: headless).
    #[inline]
    pub(crate) const fn window_system(&self) -> Option<WindowSystem> {
        self.window_system
    }
}

impl GlShared {
    /// Whether EGL or GL has reported the context lost: at a frame's first
    /// `eglMakeCurrent` ([`Self::enter`]), a window's bind or swap, or a GL
    /// error check. A lost device stays lost.
    #[inline]
    pub(crate) fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Arm the injected-loss drill: the next [`Self::enter`] fails as a lost
    /// context's `eglMakeCurrent` does (`EGL_CONTEXT_LOST`), and latches.
    #[inline]
    pub(crate) fn inject_context_loss(&self) {
        self.inject_loss.store(true, Ordering::Release);
    }

    /// Make the context current on this thread (see the module header).
    pub(crate) fn enter(&self) -> Result<Current<'_>, String> {
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
        if self.lost() {
            return Err("gl: the context is lost (EGL_CONTEXT_LOST)".to_owned());
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
    /// Bind the context on this thread (`eglMakeCurrent`), or fail as a lost
    /// context's bind does when the drill is armed.
    fn make_current(&self) -> Result<(), String> {
        if self.inject_loss.swap(false, Ordering::AcqRel) {
            return Err(self.egl_error("eglMakeCurrent", ffi::EGL_CONTEXT_LOST));
        }
        // SAFETY: the display, surface and context were created together on
        // this display; the lock keeps every other thread off them.
        let ok = unsafe {
            (self.egl.fns.make_current)(self.display, self.surface, self.surface, self.context)
        };
        if ok != ffi::EGL_TRUE {
            return Err(self.egl_failure("eglMakeCurrent"));
        }
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
impl GlShared {
    /// Point the GL imports at this context, and latch a context the browser
    /// lost (or the drill's) as lost.
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
        // The resources holding this `Arc` are gone; the glue forgets the
        // context, and the canvas's slot goes with `_canvas`.
        ffi::webgl::context_drop(self.context);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for GlShared {
    fn drop(&mut self) {
        // The resources holding this `Arc` are gone; the context and its
        // surface go with it. The display stays initialized unless it is
        // reference-tracked (see `ffi::EGL_TRACK_REFERENCES_KHR`): an
        // untracked `eglTerminate` would end every other user of the display.
        // SAFETY: nothing else holds these handles any more.
        unsafe {
            if self.surface != ffi::EGL_NO_SURFACE {
                (self.egl.fns.destroy_surface)(self.display, self.surface);
            }
            (self.egl.fns.destroy_context)(self.display, self.context);
            if self.tracked {
                (self.egl.fns.terminate)(self.display);
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
    /// A headless device: no window system, rendering only into textures it
    /// reads back. `Err` says why GL is unavailable: no `libEGL.so.1`, no
    /// display, no GLES 3.0 config or context.
    ///
    /// The display is the arm's own: a device-platform display
    /// (`EGL_EXT_platform_device`) on the first hardware device, else the
    /// first device, where the driver enumerates devices; else
    /// `EGL_MESA_platform_surfaceless`; else the default display. Not the
    /// surfaceless display first, because that is the one wgpu's GL backend
    /// (the test oracle) opens and `eglTerminate`s when its last instance
    /// drops, and without `EGL_KHR_display_reference` (Mesa 25 has none) a
    /// terminate ends every context on the display, the arm's included.
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
            // SAFETY: a known platform enum, the default native display and an
            // EGL_NONE-terminated attribute list.
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

    /// A device on a window system's display, the one its windows' surfaces
    /// are made on. `display` is the native display handle (an Xlib
    /// `Display*`, a `wl_display*`), which the caller keeps open for the
    /// device's life.
    pub(crate) fn on_window_system(ws: WindowSystem) -> Result<Self, String> {
        if !HELD.with(Cell::get).is_null() {
            return Err(
                "gl: a GL device is held on this thread; create devices outside a guard".to_owned(),
            );
        }
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
        // The EXT spellings of the two platforms share the KHR enums.
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
        // SAFETY: a platform this EGL advertises, the caller's live native
        // display, and an EGL_NONE-terminated attribute list.
        let display = unsafe {
            (egl.fns.get_platform_display)(platform, native as *mut c_void, attribs.as_ptr())
        };
        if display == ffi::EGL_NO_DISPLAY {
            return Err(format!(
                "gl: no EGL display on the window system ({})",
                egl.error_name()
            ));
        }
        // Untracked: the window system's display is this process's one EGL
        // user of it, and it stays initialized for the process.
        Self::on_display(egl, display, false, Some(ws))
    }

    /// The device a headless display opens on: the first with a DRM node (a
    /// hardware GPU), else the first enumerated (Mesa's software device).
    fn pick_device(egl: &Egl, client_exts: &str) -> Option<ffi::EGLDeviceEXT> {
        // SAFETY: eglGetProcAddress returns null or the named extension
        // function, whose registry signature the alias types declare.
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

    /// Initialize `display`, pick an RGBA8 GLES 3 config, create the context
    /// and load GL through it. A headless display's config draws pbuffers; a
    /// window system's draws windows, and its context is config-less where the
    /// display allows it (see the module header).
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
        // From here an early return leaves the display initialized; a tracked
        // one is released by this closure.
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
            // A minimum: a window system's opaque visuals have no alpha.
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
        // A window system's context binds any of its windows' configs, when
        // the display allows a config-less context; a context that must also
        // bind a pbuffer keeps its config.
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
        // SAFETY: a config of this display (or no config, which the display
        // allows); no share context.
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
            // SAFETY: a pbuffer-capable config (surface_bit) and a terminated
            // attribute list.
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
        // Load GL with the context current, then release it.
        // SAFETY: handles created above on this display.
        if unsafe { (egl.fns.make_current)(display, surface, surface, context) } != ffi::EGL_TRUE {
            let e = egl.error_name();
            // SAFETY: as above, unused.
            unsafe { (egl.fns.destroy_context)(display, context) };
            return Err(fail(format!("gl: eglMakeCurrent: {e}")));
        }
        let loaded = Self::load_gl(egl);
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
            egl,
            display,
            context,
            surface,
            tracked,
            config: context_config,
            window_system,
            lost: AtomicBool::new(false),
            inject_loss: AtomicBool::new(false),
            gl,
            caps,
            lock: Mutex::new(()),
        };
        let dev = GlDevice(Arc::new(shared));
        dev.init_state()?;
        Ok(dev)
    }

    /// Load the GL table through `eglGetProcAddress` and read the caps. The
    /// context is current.
    fn load_gl(egl: &Egl) -> Result<(GlFns, GlCaps), String> {
        // SAFETY: eglGetProcAddress returns null or the named GL function,
        // whose registry signature the table declares.
        let gl = unsafe {
            GlFns::load(&mut |sym: &CStr| -> *const c_void {
                (egl.fns.get_proc_address)(sym.as_ptr())
            })
        }?;
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
            renderer,
            version,
            max_texture_dim: u32::try_from(max_tex).unwrap_or(0),
            srgb_views: has("GL_EXT_sRGB_write_control") && has("GL_EXT_texture_sRGB_decode"),
        };
        Ok((gl, caps))
    }
}

#[cfg(target_arch = "wasm32")]
impl GlDevice {
    /// A device on `canvas`'s WebGL2 context (see the module header). `Err`
    /// when the canvas gives no WebGL2 context, in wgpu's words for it.
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
        };
        let dev = GlDevice(Arc::new(shared));
        dev.init_state()?;
        Ok(dev)
    }

    /// Read the caps of the current WebGL2 context. The GPU's name is the
    /// unmasked renderer where the browser offers it (as wgpu's WebGL2
    /// backend named its adapter), else the browser's `GL_RENDERER`. WebGL2
    /// has neither `EXT_sRGB_write_control` nor `EXT_texture_sRGB_decode`, so
    /// the arm runs the downlevel plan, as wgpu's WebGL2 backend did.
    fn load_webgl(gl: &GlFns) -> Result<GlCaps, String> {
        // SAFETY: the context is current; glGetString returns null or a
        // NUL-terminated string the module owns for the context's life.
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
    /// The state every encode assumes and nothing changes: no dithering, depth,
    /// culling or blending until a pipeline asks.
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

    /// The device makes its context current under the guard, releases it on
    /// drop (another thread can then take it), nests on one thread, and
    /// refuses a second device while the first is held on the thread.
    #[test]
    fn the_context_is_current_only_inside_a_guard_and_moves_between_threads() {
        let dev = device();
        let other = device();
        let egl = Egl::get().expect("the device loaded EGL");
        // SAFETY: eglGetProcAddress returns the core entry point (EGL 1.5),
        // whose registry signature is `EGLContext eglGetCurrentContext(void)`.
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
            assert_eq!(current(), dev.0.context, "current inside the guard");
            {
                let _inner = dev.enter().expect("a nested enter on the holding thread");
                assert_eq!(current(), dev.0.context);
            }
            assert_eq!(
                current(),
                dev.0.context,
                "the nested guard released nothing"
            );
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

    /// A lost context latches at the first `eglMakeCurrent` that reports it,
    /// the call every frame and resource operation starts with: the device
    /// reads lost from then on and refuses every later guard. The drill
    /// answers the bind the way a lost context does; an undrilled device on
    /// the same driver (the negative control) is neither refused nor lost.
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
    }
}
