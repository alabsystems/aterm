// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's swapchain: an EGL window surface on an X11 window or a Wayland
//! surface, and the present that posts a frame to it.
//!
//! A present composes into a stand-in texture the surface owns (the arm's
//! textures store their top row at row 0; see `super` "Coordinates"), then
//! [`GlWindowSurface::present`] makes the window surface current, copies the
//! stand-in into the back buffer with `glBlitFramebuffer`, flipping rows (a
//! window's default framebuffer counts rows from the bottom), and swaps. The
//! stand-in is also what the video tap and the presented snapshot read, so
//! they see exactly the posted bytes.
//!
//! * X11: `eglCreatePlatformWindowSurface` on the window's XID, with the config
//!   whose native visual is the window's (an EGL surface must match its
//!   window's visual). The surface follows the window's size by itself.
//! * Wayland: a `wl_egl_window` from `libwayland-egl.so.1` (opened with
//!   `dlopen`, like EGL) wraps the `wl_surface`; a resize resizes it.
//!
//! The swap interval is the present mode: 1 (wait for the refresh, tear-free)
//! on X11, where an unsynchronized post can tear; 0 on Wayland, where the
//! compositor never tears and a synchronized swap would block on a frame
//! callback a hidden window never gets.

use std::ffi::{CStr, c_int, c_void};
use std::sync::Arc;

use super::device::{GlDevice, GlShared, WindowSystem};
use super::ffi::{self, EGLConfig, EGLSurface, EGLint, EglWindowFns};
use super::resources::GlTexture;
use crate::device_layer::TexelFormat;

/// The native window a surface is made on.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeWindow {
    /// An X11 window and its visual (`None`: unknown, so any config is tried).
    X11 { window: u64, visual: Option<u32> },
    /// A `wl_surface*`.
    Wayland { surface: *mut c_void },
}

/// The window-surface entry points, read once through `eglGetProcAddress`
/// (EGL 1.5 hands out core EGL and GL functions alike, independent of the
/// display and context).
fn window_fns(shared: &GlShared) -> Result<&'static EglWindowFns, String> {
    static FNS: std::sync::OnceLock<Result<EglWindowFns, String>> = std::sync::OnceLock::new();
    let egl = shared.egl();
    FNS.get_or_init(|| {
        // SAFETY: eglGetProcAddress returns null or the named function, whose
        // registry signature the table declares.
        unsafe {
            EglWindowFns::load(&mut |sym: &CStr| -> *const c_void {
                (egl.fns.get_proc_address)(sym.as_ptr())
            })
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// `libwayland-egl.so.1`'s three entry points.
struct WaylandEgl {
    create: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    resize: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
}

impl WaylandEgl {
    /// The process's `libwayland-egl`, opened on first use and never closed.
    fn get() -> Result<&'static WaylandEgl, String> {
        static LIB: std::sync::OnceLock<Result<WaylandEgl, String>> = std::sync::OnceLock::new();
        LIB.get_or_init(Self::open).as_ref().map_err(Clone::clone)
    }

    fn open() -> Result<WaylandEgl, String> {
        // SAFETY: a NUL-terminated library name; the handle is never closed.
        let handle = unsafe { libc::dlopen(c"libwayland-egl.so.1".as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() {
            return Err("gl: no libwayland-egl.so.1 on this machine".to_owned());
        }
        let sym = |name: &CStr| -> Result<*mut c_void, String> {
            // SAFETY: `dlsym` on a live handle returns the address or null.
            let p = unsafe { libc::dlsym(handle, name.as_ptr()) };
            if p.is_null() {
                Err(format!(
                    "gl: libwayland-egl is missing {}",
                    name.to_string_lossy()
                ))
            } else {
                Ok(p)
            }
        };
        let (create, destroy, resize) = (
            sym(c"wl_egl_window_create")?,
            sym(c"wl_egl_window_destroy")?,
            sym(c"wl_egl_window_resize")?,
        );
        // SAFETY: the three symbols' signatures from wayland-egl-core.h.
        unsafe {
            Ok(WaylandEgl {
                create: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *mut c_void,
                >(create),
                destroy: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(
                    destroy,
                ),
                resize: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
                >(resize),
            })
        }
    }
}

/// One window's EGL surface, its stand-in and its present mode.
pub(crate) struct GlWindowSurface {
    shared: Arc<GlShared>,
    fns: &'static EglWindowFns,
    surface: EGLSurface,
    /// The `wl_egl_window` the surface draws into (null on X11), destroyed
    /// after the EGL surface.
    wl_window: *mut c_void,
    /// The size the window was last configured to.
    width: u32,
    height: u32,
    /// The swap interval (see the module header) and whether the surface has
    /// been given it (it is set with the surface current, at the first post).
    interval: i32,
    interval_set: bool,
    /// The texture a present composes into, at the configured size.
    standin: Option<GlTexture>,
    /// The window the surface draws into, held so it outlives the surface
    /// (dropped after it).
    _window: Box<dyn std::any::Any + Send + Sync>,
}

impl std::fmt::Debug for GlWindowSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlWindowSurface")
            .field("size", &(self.width, self.height))
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// Every RGB888 GLES 3 window config of the display (at most 64).
fn window_configs(shared: &GlShared) -> Result<Vec<EGLConfig>, String> {
    let egl = shared.egl();
    let attribs = [
        ffi::EGL_RENDERABLE_TYPE,
        ffi::EGL_OPENGL_ES3_BIT,
        ffi::EGL_SURFACE_TYPE,
        ffi::EGL_WINDOW_BIT,
        ffi::EGL_RED_SIZE,
        8,
        ffi::EGL_GREEN_SIZE,
        8,
        ffi::EGL_BLUE_SIZE,
        8,
        ffi::EGL_NONE,
    ];
    let mut configs = [std::ptr::null_mut(); 64];
    let mut n = 0;
    // SAFETY: an initialized display, a terminated list, room for 64.
    let ok = unsafe {
        (egl.fns.choose_config)(
            shared.display(),
            attribs.as_ptr(),
            configs.as_mut_ptr(),
            configs.len() as EGLint,
            &raw mut n,
        )
    };
    if ok != ffi::EGL_TRUE {
        return Err(shared.egl_failure("eglChooseConfig(window)"));
    }
    let n = usize::try_from(n).unwrap_or(0).min(configs.len());
    Ok(configs[..n].to_vec())
}

/// One attribute of `config`.
fn config_attrib(
    shared: &GlShared,
    fns: &EglWindowFns,
    config: EGLConfig,
    attrib: EGLint,
) -> EGLint {
    let mut v = 0;
    // SAFETY: a config of this display and a live out-pointer.
    unsafe {
        (fns.get_config_attrib)(shared.display(), config, attrib, &raw mut v);
    }
    v
}

/// The configs to try for `native`, best first: the context's own config
/// when it has one (a surface must share it); else the configs whose colour
/// buffer is exactly RGB888 (`EGL_RED_SIZE` is a minimum, so the display also
/// lists 10-bit configs; a 2101010 back buffer would rescale every byte the
/// stand-in carries), with alpha 0 or 8. On X11 the window's visual comes
/// first; on Wayland the opaque (alpha-less) configs do.
fn candidate_configs(
    shared: &GlShared,
    fns: &EglWindowFns,
    native: NativeWindow,
) -> Result<Vec<EGLConfig>, String> {
    if shared.config() != ffi::EGL_NO_CONFIG_KHR {
        return Ok(vec![shared.config()]);
    }
    let attrib = |c, a| config_attrib(shared, fns, c, a);
    let mut configs: Vec<EGLConfig> = window_configs(shared)?
        .into_iter()
        .filter(|&c| {
            [ffi::EGL_RED_SIZE, ffi::EGL_GREEN_SIZE, ffi::EGL_BLUE_SIZE]
                .iter()
                .all(|&a| attrib(c, a) == 8)
                && matches!(attrib(c, ffi::EGL_ALPHA_SIZE), 0 | 8)
        })
        .collect();
    match native {
        NativeWindow::X11 {
            visual: Some(visual),
            ..
        } => {
            let want = visual as EGLint;
            configs.sort_by_key(|&c| attrib(c, ffi::EGL_NATIVE_VISUAL_ID) != want);
        }
        NativeWindow::X11 { visual: None, .. } => {}
        NativeWindow::Wayland { .. } => {
            configs.sort_by_key(|&c| attrib(c, ffi::EGL_ALPHA_SIZE) != 0);
        }
    }
    if configs.is_empty() {
        return Err("gl: the display offers no RGB888 GLES 3 window config".to_owned());
    }
    Ok(configs)
}

impl GlWindowSurface {
    /// Make the window surface for `native` on `dev`, whose display must be
    /// the window's (`GlDevice::on_window_system`), at `width` x `height`.
    pub(crate) fn create(
        dev: &GlDevice,
        native: NativeWindow,
        width: u32,
        height: u32,
        window: Box<dyn std::any::Any + Send + Sync>,
    ) -> Result<Self, String> {
        let shared = dev.shared();
        let ws = shared
            .window_system()
            .ok_or("gl: a window surface needs a device on the window system's display")?;
        match (ws, native) {
            (WindowSystem::X11 { .. }, NativeWindow::X11 { .. })
            | (WindowSystem::Wayland { .. }, NativeWindow::Wayland { .. }) => {}
            _ => return Err("gl: the window is not on the device's window system".to_owned()),
        }
        let fns = window_fns(shared)?;
        let (w, h) = (width.max(1), height.max(1));
        let wl = match native {
            NativeWindow::Wayland { .. } => Some(WaylandEgl::get()?),
            NativeWindow::X11 { .. } => None,
        };
        let wl_window = match (native, wl) {
            (NativeWindow::Wayland { surface }, Some(wl)) => {
                // SAFETY: the caller's live `wl_surface*`; positive size.
                let p = unsafe { (wl.create)(surface, w as c_int, h as c_int) };
                if p.is_null() {
                    return Err("gl: wl_egl_window_create failed".to_owned());
                }
                p
            }
            _ => std::ptr::null_mut(),
        };
        let mut xid: u64 = match native {
            NativeWindow::X11 { window, .. } => window,
            NativeWindow::Wayland { .. } => 0,
        };
        let native_ptr: *mut c_void = if wl_window.is_null() {
            // EGL_KHR_platform_x11: the native window is a pointer to the XID.
            (&raw mut xid).cast()
        } else {
            wl_window
        };
        let attribs = [ffi::EGL_NONE as ffi::EGLAttrib];
        let mut surface = ffi::EGL_NO_SURFACE;
        let mut last_err = String::new();
        for config in candidate_configs(shared, fns, native)? {
            // SAFETY: a config of this display, the native window (alive for
            // the call; EGL reads the XID or keeps the wl_egl_window, which
            // outlives the surface), and a terminated attribute list.
            surface = unsafe {
                (fns.create_platform_window_surface)(
                    shared.display(),
                    config,
                    native_ptr,
                    attribs.as_ptr(),
                )
            };
            if surface != ffi::EGL_NO_SURFACE {
                break;
            }
            last_err = shared.egl_failure("eglCreatePlatformWindowSurface");
        }
        if surface == ffi::EGL_NO_SURFACE {
            if let Some(wl) = wl {
                // SAFETY: the window made above, unused.
                unsafe { (wl.destroy)(wl_window) };
            }
            return Err(last_err);
        }
        let interval = match native {
            NativeWindow::X11 { .. } => 1,
            NativeWindow::Wayland { .. } => 0,
        };
        Ok(Self {
            shared: Arc::clone(shared),
            fns,
            surface,
            wl_window,
            width: w,
            height: h,
            interval: present_interval_override().unwrap_or(interval),
            interval_set: false,
            standin: None,
            _window: window,
        })
    }

    /// Whether a post waits for the display's refresh (swap interval 1).
    #[inline]
    pub(crate) const fn synced(&self) -> bool {
        self.interval > 0
    }

    /// Move to `width` x `height`; the next present composes and posts at it.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if (w, h) == (self.width, self.height) {
            return;
        }
        if !self.wl_window.is_null()
            && let Ok(wl) = WaylandEgl::get()
        {
            // SAFETY: the window this surface draws into.
            unsafe { (wl.resize)(self.wl_window, w as c_int, h as c_int, 0, 0) };
        }
        self.width = w;
        self.height = h;
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

    /// Post the stand-in: bind the window surface, copy the stand-in into the
    /// back buffer with its rows flipped and its top-left at the window's
    /// top-left, and swap. `Err` names the EGL or GL failure; a lost context
    /// is latched on the device.
    pub(crate) fn present(&mut self, dev: &GlDevice) -> Result<(), String> {
        let standin = self
            .standin
            .as_ref()
            .ok_or("gl: a present with no composed stand-in")?;
        let cur = dev.enter()?;
        cur.bind_window(self.surface)?;
        if !self.interval_set {
            // A refused interval keeps the driver's default; one note.
            if let Err(e) = cur.swap_interval(self.fns, self.interval) {
                crate::stderr_line!("aterm-gpu (gl arm): {e}; keeping the driver's swap interval");
            }
            self.interval_set = true;
        }
        let shared = cur.shared();
        // The back buffer's height now (an X11 surface follows its window, so
        // it can differ from the configured size until the next resize).
        let mut sh = 0;
        // SAFETY: a surface of this display and a live out-pointer.
        unsafe {
            (self.fns.query_surface)(shared.display(), self.surface, ffi::EGL_HEIGHT, &raw mut sh);
        }
        let (tw, th) = (standin.width as i32, standin.height as i32);
        // The window's top row is its default framebuffer's last row.
        let top = if sh > 0 { sh } else { th };
        let fbo = standin.fbo(&cur)?;
        let gl = cur.gl();
        // SAFETY: the context and the window surface are current; the
        // stand-in's framebuffer is complete; the blit is colour-only and
        // NEAREST between equal-sized rects (no filtering, no conversion: the
        // back buffer and the stand-in are both linear RGBA8).
        unsafe {
            (gl.disable)(ffi::GL_SCISSOR_TEST);
            if dev.caps().srgb_views {
                (gl.disable)(ffi::GL_FRAMEBUFFER_SRGB_EXT);
            }
            (gl.color_mask)(ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE);
            (gl.bind_framebuffer)(ffi::GL_READ_FRAMEBUFFER, fbo);
            (gl.bind_framebuffer)(ffi::GL_DRAW_FRAMEBUFFER, 0);
            (gl.blit_framebuffer)(
                0,
                0,
                tw,
                th,
                0,
                top,
                tw,
                top - th,
                ffi::GL_COLOR_BUFFER_BIT,
                ffi::GL_NEAREST as u32,
            );
        }
        cur.check("present blit")?;
        cur.swap(self.fns, self.surface)
    }
}

impl Drop for GlWindowSurface {
    fn drop(&mut self) {
        // The stand-in first (its GL names), then the surface, then the
        // Wayland window under it.
        self.standin = None;
        // SAFETY: a surface of this display; the arm never leaves it current
        // past a guard, so destroying it here is immediate.
        unsafe {
            (self.shared.egl().fns.destroy_surface)(self.shared.display(), self.surface);
        }
        if !self.wl_window.is_null()
            && let Ok(wl) = WaylandEgl::get()
        {
            // SAFETY: the window this surface drew into; the surface is gone.
            unsafe { (wl.destroy)(self.wl_window) };
        }
    }
}

/// The `ATERM_GPU_PRESENT_MODE=fifo|immediate` development seam, as the swap
/// interval it forces (announced on stderr).
fn present_interval_override() -> Option<i32> {
    let v = aterm_types::dev_seam!("ATERM_GPU_PRESENT_MODE")
        .map(|v| v.to_string_lossy().into_owned())?;
    match v.trim() {
        "fifo" => {
            crate::stderr_line!(
                "aterm-gpu: $ATERM_GPU_PRESENT_MODE=fifo — forcing swap interval 1 on the GL arm"
            );
            Some(1)
        }
        "immediate" => {
            crate::stderr_line!(
                "aterm-gpu: $ATERM_GPU_PRESENT_MODE=immediate — forcing swap interval 0 on the \
                 GL arm"
            );
            Some(0)
        }
        other => {
            crate::stderr_line!(
                "aterm-gpu: unknown ATERM_GPU_PRESENT_MODE {other:?} (want fifo|immediate); \
                 using the default"
            );
            None
        }
    }
}
