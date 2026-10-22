// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's swapchain: a `GLXWindow` on an X11 window, an EGL window
//! surface on a Wayland surface, and the present that posts a frame to it.
//!
//! A present composes into a stand-in texture the surface owns (the arm's
//! textures store their top row at row 0; see `super` "Coordinates"), then
//! [`GlWindowSurface::present`] makes the window surface current, copies the
//! stand-in into the back buffer with `glBlitFramebuffer`, flipping rows (a
//! window's default framebuffer counts rows from the bottom), and swaps. The
//! stand-in is also what the video tap and the presented snapshot read, so
//! they see exactly the posted bytes.
//!
//! * X11 on a GLX device (`super::glx`): `glXCreateWindow` on the window's
//!   XID with the context's config, whose visual must be the window's. The
//!   window follows its X11 window's size by itself. GLX reports a failed
//!   swap or window only as an X error, which the windowing crate logs, so
//!   the present reads its failures from the binds: a window GLX keeps
//!   refusing to bind latches the device lost ([`bind_failure_is_loss`]),
//!   and so does a reset the context reports (`Current::poll_reset`, asked
//!   at every GLX present before its blit), where either once left the window
//!   frozen on its last frame with every present dropped.
//! * X11 on an EGL device, which `GlDevice::on_window_system` no longer
//!   makes (an X11 device is GLX's or none): `eglCreatePlatformWindowSurface`
//!   on the window's XID, with the config whose native visual is the
//!   window's (an EGL surface must match its window's visual). The surface
//!   follows the window's size by itself.
//! * Wayland: a `wl_egl_window` from `libwayland-egl.so.1` (opened with
//!   `dlopen`, like EGL) wraps the `wl_surface`; a resize resizes it.
//!
//! The swap interval is the present mode: 1 (wait for the refresh, tear-free)
//! on an X11 screen no compositing manager composites, where an
//! unsynchronized post can tear; 0 under an X11 compositing manager, which
//! composites at its own refresh (a synchronized swap there only blocks the
//! posting thread, a refresh per window); 0 on Wayland, where the compositor
//! never tears and a synchronized swap would block on a frame callback a
//! hidden window never gets.

use std::ffi::{CStr, c_int, c_void};
use std::sync::Arc;

use super::device::{EglBinding, GlDevice, GlShared, WindowSystem};
use super::ffi::{self, EGLConfig, EGLSurface, EGLint, EglWindowFns, GLXDrawable};
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
fn window_fns(egl: &EglBinding) -> Result<&'static EglWindowFns, String> {
    static FNS: std::sync::OnceLock<Result<EglWindowFns, String>> = std::sync::OnceLock::new();
    let egl = egl.egl;
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

/// The window's half of a post: what the present binds and swaps.
enum Post {
    /// An EGL window surface.
    Egl {
        fns: &'static EglWindowFns,
        surface: EGLSurface,
        /// The `wl_egl_window` the surface draws into (null on X11),
        /// destroyed after the EGL surface.
        wl_window: *mut c_void,
    },
    /// A `GLXWindow` of the device's GLX context.
    Glx { window: GLXDrawable },
}

/// One window's surface, its stand-in and its present mode.
pub(crate) struct GlWindowSurface {
    shared: Arc<GlShared>,
    post: Post,
    /// The size the window was last configured to.
    width: u32,
    height: u32,
    /// The swap interval (see the module header) and whether the surface has
    /// been given it (it is set with the surface current, at the first post).
    interval: i32,
    interval_set: bool,
    /// The texture a present composes into, at the configured size.
    standin: Option<GlTexture>,
    /// The GLX window's failed binds since its last good one, and whether it
    /// ever bound ([`bind_failure_is_loss`]).
    bind_failures: u32,
    bound: bool,
    /// The window the surface draws into, held so it outlives the surface
    /// (dropped after it).
    _window: Box<dyn std::any::Any + Send + Sync>,
}

/// The failed binds in a row at which a GLX window that bound before is
/// taken as lost.
const BIND_FAILURES_LOST: u32 = 3;

/// Whether a GLX window's failed bind latches the device lost, after
/// `in_a_row` failed binds (this one counted) of a window that `bound_before`
/// or never did, with the context still binding its own pbuffer
/// (`context_binds`) or not. A window GLX keeps refusing would otherwise drop
/// every present while the frontend waits for a loss that never latches, the
/// window frozen; a lost device sends it to the CPU renderer.
///
/// * The context binds nowhere: the context, or the server under it, is gone.
/// * The window never bound: it never showed a frame and never will
///   (NVIDIA's `glXCreateWindow` answers a window and fails later).
/// * [`BIND_FAILURES_LOST`] presents in a row failed: the failure persists.
///
/// One failure of a window that posted before kills nothing: a window in
/// teardown can be refused once, and its present just drops.
const fn bind_failure_is_loss(in_a_row: u32, bound_before: bool, context_binds: bool) -> bool {
    !context_binds || !bound_before || in_a_row >= BIND_FAILURES_LOST
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
fn window_configs(shared: &GlShared, b: &EglBinding) -> Result<Vec<EGLConfig>, String> {
    let egl = b.egl;
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
            b.display,
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
fn config_attrib(b: &EglBinding, fns: &EglWindowFns, config: EGLConfig, attrib: EGLint) -> EGLint {
    let mut v = 0;
    // SAFETY: a config of this display and a live out-pointer.
    unsafe {
        (fns.get_config_attrib)(b.display, config, attrib, &raw mut v);
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
    b: &EglBinding,
    fns: &EglWindowFns,
    native: NativeWindow,
) -> Result<Vec<EGLConfig>, String> {
    if b.config != ffi::EGL_NO_CONFIG_KHR {
        return Ok(vec![b.config]);
    }
    let attrib = |c, a| config_attrib(b, fns, c, a);
    let mut configs: Vec<EGLConfig> = window_configs(shared, b)?
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
        let (w, h) = (width.max(1), height.max(1));
        let post = match shared.glx() {
            Some(glx) => {
                let NativeWindow::X11 {
                    window: xid,
                    visual,
                } = native
                else {
                    return Err("gl: a GLX device posts only to X11 windows".to_owned());
                };
                Post::Glx {
                    window: glx.create_window(xid, visual)?,
                }
            }
            None => Self::egl_post(shared, native, w, h)?,
        };
        // On X11 a post waits for the refresh (interval 1) so it never tears,
        // unless a compositing manager composites the window at its own
        // refresh: a synchronized swap then paces nothing the compositor does
        // not already, and it blocks the posting thread for a refresh per
        // window, so every window shared one refresh (three windows streaming
        // output posted 20 frames a second each, the main thread 99.8% in
        // swaps; mutter on an NVIDIA Xorg, 2026-10-22). The frontend's frame
        // clock paces the posts either way.
        let interval = match (native, ws) {
            (NativeWindow::X11 { .. }, WindowSystem::X11 { display, screen }) => {
                #[cfg(not(wgpu_arm))]
                let composited = super::glx::composited(display as *mut c_void, screen);
                #[cfg(wgpu_arm)]
                let composited = {
                    let _ = (display, screen);
                    false
                };
                i32::from(!composited)
            }
            _ => 0,
        };
        Ok(Self {
            shared: Arc::clone(shared),
            post,
            width: w,
            height: h,
            interval: present_interval_override().unwrap_or(interval),
            interval_set: false,
            standin: None,
            bind_failures: 0,
            bound: false,
            _window: window,
        })
    }

    /// The EGL window surface for `native` on an EGL device.
    fn egl_post(shared: &GlShared, native: NativeWindow, w: u32, h: u32) -> Result<Post, String> {
        let b = shared
            .egl_binding()
            .ok_or("gl: an EGL window surface on a GLX device")?;
        let fns = window_fns(b)?;
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
        for config in candidate_configs(shared, b, fns, native)? {
            // SAFETY: a config of this display, the native window (alive for
            // the call; EGL reads the XID or keeps the wl_egl_window, which
            // outlives the surface), and a terminated attribute list.
            surface = unsafe {
                (fns.create_platform_window_surface)(
                    b.display,
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
        Ok(Post::Egl {
            fns,
            surface,
            wl_window,
        })
    }

    /// Whether a post waits for the display's refresh (swap interval 1).
    #[inline]
    pub(crate) const fn synced(&self) -> bool {
        self.interval > 0
    }

    /// The `GLXWindow` a present binds (`None` on an EGL surface), for a test
    /// that destroys it behind the surface's back.
    #[cfg(test)]
    pub(crate) const fn glx_window(&self) -> Option<GLXDrawable> {
        match self.post {
            Post::Glx { window } => Some(window),
            Post::Egl { .. } => None,
        }
    }

    /// Move to `width` x `height`; the next present composes and posts at it.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if (w, h) == (self.width, self.height) {
            return;
        }
        match &mut self.post {
            Post::Egl { wl_window, .. } => {
                if !wl_window.is_null()
                    && let Ok(wl) = WaylandEgl::get()
                {
                    // SAFETY: the window this surface draws into.
                    unsafe { (wl.resize)(*wl_window, w as c_int, h as c_int, 0, 0) };
                }
            }
            Post::Glx { .. } => {}
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
    /// top-left, and swap. `Err` names the EGL, GLX or GL failure; a lost
    /// context, a reset one and a GLX window that keeps refusing its bind
    /// are latched on the device.
    pub(crate) fn present(&mut self, dev: &GlDevice) -> Result<(), String> {
        let standin = self
            .standin
            .as_ref()
            .ok_or("gl: a present with no composed stand-in")?;
        let cur = dev.enter()?;
        // A refused interval keeps the driver's default; one note.
        let note = |e: String| {
            crate::stderr_line!("aterm-gpu (gl arm): {e}; keeping the driver's swap interval");
        };
        // The back buffer's size: an X11 surface follows its window, which a
        // window manager resizes before the frontend hears of it, so it can
        // differ from the configured size and is asked at every post.
        let (sw, sh) = match &self.post {
            Post::Egl { fns, surface, .. } => {
                cur.bind_window(*surface)?;
                if !self.interval_set {
                    cur.swap_interval(fns, self.interval).unwrap_or_else(note);
                    self.interval_set = true;
                }
                let b = cur
                    .shared()
                    .egl_binding()
                    .ok_or("gl: an EGL window surface on a GLX device")?;
                let (mut sw, mut sh) = (0, 0);
                // SAFETY (both): a surface of this display and a live out-pointer.
                unsafe {
                    (fns.query_surface)(b.display, *surface, ffi::EGL_WIDTH, &raw mut sw);
                    (fns.query_surface)(b.display, *surface, ffi::EGL_HEIGHT, &raw mut sh);
                }
                (sw, sh)
            }
            Post::Glx { window } => {
                let glx = cur.glx().ok_or("gl: a GLX window on an EGL device")?;
                if let Err(e) = glx.bind_window(*window) {
                    self.bind_failures = self.bind_failures.saturating_add(1);
                    // Whether the context still binds at all: its own pbuffer
                    // answers (and holds it until the guard releases it).
                    let context_binds = glx.make_current().is_ok();
                    if !bind_failure_is_loss(self.bind_failures, self.bound, context_binds) {
                        return Err(e);
                    }
                    let why = format!(
                        "{e} on its window ({} in a row; the window {} bound; the context {} \
                         its own pbuffer)",
                        self.bind_failures,
                        if self.bound { "had" } else { "never" },
                        if context_binds {
                            "binds"
                        } else {
                            "cannot bind"
                        },
                    );
                    cur.shared().latch_lost(&why);
                    return Err(why);
                }
                self.bind_failures = 0;
                self.bound = true;
                // Before anything is posted: a reset context's frame is not.
                cur.poll_reset("present")?;
                if !self.interval_set {
                    glx.swap_interval(*window, self.interval)
                        .unwrap_or_else(note);
                    self.interval_set = true;
                }
                let (sw, sh) = glx.size(*window);
                (
                    i32::try_from(sw).unwrap_or(0),
                    i32::try_from(sh).unwrap_or(0),
                )
            }
        };
        let (tw, th) = (standin.width as i32, standin.height as i32);
        let sw = if sw > 0 { sw } else { tw };
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
            // A window larger than the stand-in (it grew before the frontend
            // heard of it) would show the back buffer's undefined bytes past
            // the frame: its right column, its bottom row and its corner pixel
            // are stretched over them instead.
            let extend = |src: [i32; 4], dst: [i32; 4]| {
                (gl.blit_framebuffer)(
                    src[0],
                    src[1],
                    src[2],
                    src[3],
                    dst[0],
                    dst[1],
                    dst[2],
                    dst[3],
                    ffi::GL_COLOR_BUFFER_BIT,
                    ffi::GL_NEAREST as u32,
                );
            };
            if sw > tw {
                extend([tw - 1, 0, tw, th], [tw, top, sw, top - th]);
            }
            if top > th {
                extend([0, th - 1, tw, th], [0, top - th, tw, 0]);
            }
            if sw > tw && top > th {
                extend([tw - 1, th - 1, tw, th], [tw, top - th, sw, 0]);
            }
        }
        cur.check("present blit")?;
        match &self.post {
            Post::Egl { fns, surface, .. } => cur.swap(fns, *surface),
            Post::Glx { window, .. } => {
                cur.glx()
                    .ok_or("gl: a GLX window on an EGL device")?
                    .swap(*window);
                Ok(())
            }
        }
    }
}

impl Drop for GlWindowSurface {
    fn drop(&mut self) {
        // The stand-in first (its GL names), then the surface, then the
        // Wayland window under it.
        self.standin = None;
        match self.post {
            Post::Egl {
                surface, wl_window, ..
            } => {
                if let Some(b) = self.shared.egl_binding() {
                    // SAFETY: a surface of this display; the arm never leaves
                    // it current past a guard, so destroying it here is
                    // immediate.
                    unsafe { (b.egl.fns.destroy_surface)(b.display, surface) };
                }
                if !wl_window.is_null()
                    && let Ok(wl) = WaylandEgl::get()
                {
                    // SAFETY: the window this surface drew into; the surface
                    // is gone.
                    unsafe { (wl.destroy)(wl_window) };
                }
            }
            // Never current past a guard either; the X11 window under it
            // outlives it (`_window`).
            Post::Glx { window, .. } => {
                if let Some(glx) = self.shared.glx() {
                    glx.destroy_window(window);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A GLX window's failed bind latches the device lost when the failure
    /// persists ([`BIND_FAILURES_LOST`] presents in a row), when the window
    /// never bound, or when the context cannot bind its own pbuffer either;
    /// one or two failures of a window that posted before, with the context
    /// still binding, only drop their presents. The live twin, a window whose
    /// `GLXWindow` was destroyed, is `glx`'s
    /// `a_window_glx_keeps_refusing_latches_the_device_lost`.
    #[test]
    fn a_glx_window_s_refused_bind_latches_once_it_persists() {
        assert_eq!(BIND_FAILURES_LOST, 3);
        assert!(!bind_failure_is_loss(1, true, true), "a window in teardown");
        assert!(!bind_failure_is_loss(2, true, true));
        assert!(bind_failure_is_loss(3, true, true), "the failure persists");
        assert!(bind_failure_is_loss(4, true, true));
        assert!(
            bind_failure_is_loss(1, true, false),
            "the context binds nowhere"
        );
        assert!(
            bind_failure_is_loss(1, false, true),
            "the window never bound"
        );
        assert!(bind_failure_is_loss(1, false, false));
    }
}
