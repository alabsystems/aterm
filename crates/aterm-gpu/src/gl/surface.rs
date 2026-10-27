// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GLX/EGL window surface creation and frame presentation ([`GlWindowSurface::present`],
//! [`bind_failure_is_loss`], [`crate::FrameSink`]).

use std::ffi::{CStr, c_int, c_void};
use std::sync::Arc;

use super::GlPresentError;
use super::device::{EglBinding, GlDevice, GlShared, WindowSystem};
use super::ffi::{self, EGLConfig, EGLSurface, EGLint, EglWindowFns, GLXDrawable};
use super::resources::GlTexture;
use crate::SurfacePresentFailure;
use crate::device_layer::TexelFormat;

/// The native window a surface is made on.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeWindow {
    /// An X11 window and its visual (`None`: unknown, so any config is tried).
    X11 { window: u64, visual: Option<u32> },
    /// A `wl_surface*`.
    Wayland { surface: *mut c_void },
}

fn window_fns(egl: &EglBinding) -> Result<&'static EglWindowFns, String> {
    static FNS: std::sync::OnceLock<Result<EglWindowFns, String>> = std::sync::OnceLock::new();
    let egl = egl.egl;
    FNS.get_or_init(|| {
        // SAFETY: eglGetProcAddress returns null or the named function, whose registry signature
        // the table declares.
        unsafe {
            EglWindowFns::load(&mut |sym: &CStr| -> *const c_void {
                (egl.fns.get_proc_address)(sym.as_ptr())
            })
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

struct WaylandEgl {
    create: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    resize: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_int),
}

impl WaylandEgl {
    fn get() -> Result<&'static WaylandEgl, String> {
        static LIB: std::sync::OnceLock<Result<WaylandEgl, String>> = std::sync::OnceLock::new();
        LIB.get_or_init(Self::open).as_ref().map_err(Clone::clone)
    }

    fn open() -> Result<WaylandEgl, String> {
        // SAFETY: A NUL-terminated library name; the handle is never closed.
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
        // SAFETY: The three symbols' signatures from wayland-egl-core.h.
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

enum Post {
    /// An EGL window surface.
    Egl {
        fns: &'static EglWindowFns,
        surface: EGLSurface,
        /// The `wl_egl_window` the surface draws into (null on X11).
        wl_window: *mut c_void,
    },
    /// A `GLXWindow` of the device's GLX context, on the X11 window `xid`.
    Glx { window: GLXDrawable, xid: u64 },
    /// No window surface.
    Readback { sink: Box<dyn crate::FrameSink> },
}

/// One window's surface, its stand-in and its present mode.
pub(crate) struct GlWindowSurface {
    shared: Arc<GlShared>,
    post: Post,
    /// The size the window was last configured to.
    width: u32,
    height: u32,
    /// The swap interval (by default 1, waiting for the refresh, for a window surface on an X11
    /// screen no compositing manager composites, else 0) and whether the surface was given it.
    interval: i32,
    interval_set: bool,
    /// The texture a present composes into, at the configured size.
    standin: Option<GlTexture>,
    /// The GLX window's failed binds since its last good one, and whether it ever bound.
    bind_failures: u32,
    bound: bool,
    /// A test's answers for the back buffer's size in place of the window.
    #[cfg(test)]
    pub(crate) asked_size: Option<(i32, i32)>,
    #[cfg(test)]
    pub(crate) asked_once: std::cell::Cell<Option<(i32, i32)>>,
    /// A test's order to read a post by readback as `GL_RGBA` (and swap red and blue).
    #[cfg(test)]
    pub(crate) read_rgba: bool,
    /// The window the surface draws into, held so it outlives the surface (dropped after it).
    _window: Box<dyn std::any::Any + Send + Sync>,
}

const PAST: i32 = 1 << 15;

const REPOSTS: u32 = 2;

const BIND_FAILURES_LOST: u32 = 3;

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
    // SAFETY: An initialized display, a terminated list, room for 64.
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

fn config_attrib(b: &EglBinding, fns: &EglWindowFns, config: EGLConfig, attrib: EGLint) -> EGLint {
    let mut v = 0;
    // SAFETY: A config of this display and a live out-pointer.
    unsafe {
        (fns.get_config_attrib)(b.display, config, attrib, &raw mut v);
    }
    v
}

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
    /// Make the window surface for `native` on `dev` ([`Post::Readback`], [`readback_wanted_on`]).
    pub(crate) fn create(
        dev: &GlDevice,
        native: NativeWindow,
        width: u32,
        height: u32,
        window: Box<dyn std::any::Any + Send + Sync>,
        sink: Option<Box<dyn crate::FrameSink>>,
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
        let composited = match (native, ws) {
            (NativeWindow::X11 { .. }, WindowSystem::X11 { display, screen }) => {
                let composited = super::glx::composited(display as *mut c_void, screen);
                Some(composited)
            }
            _ => None,
        };
        let mut interval = composited.map_or(0, |composited| i32::from(!composited));
        let post = match shared.glx() {
            Some(glx) => {
                let NativeWindow::X11 {
                    window: xid,
                    visual,
                } = native
                else {
                    return Err("gl: a GLX device posts only to X11 windows".to_owned());
                };
                match sink.and_then(attached) {
                    Some(sink) => {
                        interval = 0;
                        Post::Readback { sink }
                    }
                    None => Post::Glx {
                        window: glx.create_window(xid, visual)?,
                        xid,
                    },
                }
            }
            None => Self::egl_post(shared, native, w, h)?,
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
            #[cfg(test)]
            asked_size: None,
            #[cfg(test)]
            asked_once: std::cell::Cell::new(None),
            #[cfg(test)]
            read_rgba: false,
            _window: window,
        })
    }

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
                // SAFETY: The caller's live `wl_surface*`; positive size.
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
            (&raw mut xid).cast()
        } else {
            wl_window
        };
        let attribs = [ffi::EGL_NONE as ffi::EGLAttrib];
        let mut surface = ffi::EGL_NO_SURFACE;
        let mut last_err = String::new();
        for config in candidate_configs(shared, b, fns, native)? {
            // SAFETY: A config of this display, the native window (alive for the call; EGL reads
            // the XID or keeps the wl_egl_window, which outlives the surface), and a terminated
            // attribute list.
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
                // SAFETY: The window made above, unused.
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

    /// The `GLXWindow` a present binds (`None` on an EGL surface).
    #[cfg(test)]
    pub(crate) const fn glx_window(&self) -> Option<GLXDrawable> {
        match self.post {
            Post::Glx { window, .. } => Some(window),
            Post::Egl { .. } | Post::Readback { .. } => None,
        }
    }

    /// Move to `width` x `height`.
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
                    // SAFETY: The window this surface draws into.
                    unsafe { (wl.resize)(*wl_window, w as c_int, h as c_int, 0, 0) };
                }
            }
            Post::Glx { .. } | Post::Readback { .. } => {}
        }
        self.width = w;
        self.height = h;
    }

    /// The stand-in at the configured size, minted on `dev` on first use and refitted after a
    /// resize ([`GlDevice::texture_2d_resizing`]).
    pub(crate) fn standin(&mut self, dev: &GlDevice) -> Result<&GlTexture, String> {
        dev.texture_2d_resizing(
            &mut self.standin,
            TexelFormat::Rgba8Unorm,
            self.width,
            self.height,
            None,
        )?;
        Ok(self.standin.as_ref().expect("minted above"))
    }

    /// Bind the window surface, blit the stand-in, and swap; a readback post hands its sink the
    /// stand-in's `damage` instead.
    pub(crate) fn present(
        &mut self,
        dev: &GlDevice,
        fill: u32,
        damage: Option<[u32; 4]>,
    ) -> Result<(), GlPresentError> {
        let standin = self
            .standin
            .as_ref()
            .ok_or("gl: a present with no composed stand-in")?;
        let cur = dev.enter()?;
        if let Post::Readback { sink } = &mut self.post {
            cur.poll_reset("present")?;
            let fbo = standin.fbo(&cur)?;
            let size = (standin.width, standin.height);
            let bgra = dev.caps().bgra_read;
            #[cfg(test)]
            let bgra = bgra && !self.read_rgba;
            return sink
                .post(size.0, size.1, damage, &mut |words, rect| {
                    read_standin(&cur, fbo, size, rect, bgra, words)
                })
                .map_err(|detail| GlPresentError {
                    kind: SurfacePresentFailure::Unavailable,
                    detail,
                });
        }
        let note = |e: String| {
            crate::stderr_line!("aterm-gpu (gl arm): {e}; keeping the driver's swap interval");
        };
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
            Post::Glx { window, xid } => {
                let glx = cur.glx().ok_or("gl: a GLX window on an EGL device")?;
                // Asked before the bind: binding has Mesa's drisw ask the size
                // again unchecked, and a server dying mid-ask leaves llvmpipe an
                // uninitialized size (SIGSEGV). This checked ask fails instead.
                let asked = self.ask(glx, *xid);
                if asked == (0, 0) {
                    if let Some(why) = glx.hung_up_reason() {
                        cur.shared().latch_lost(why);
                        return Err(why.into());
                    }
                    return Err(GlPresentError {
                        kind: SurfacePresentFailure::Unavailable,
                        detail: "gl: the X server answered no size for the window".to_owned(),
                    });
                }
                if let Err(e) = glx.bind_window(*window) {
                    self.bind_failures = self.bind_failures.saturating_add(1);
                    let context_binds = glx.make_current().is_ok();
                    if !bind_failure_is_loss(self.bind_failures, self.bound, context_binds) {
                        return Err(GlPresentError {
                            kind: SurfacePresentFailure::Unavailable,
                            detail: e,
                        });
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
                    return Err(why.into());
                }
                self.bind_failures = 0;
                self.bound = true;
                cur.poll_reset("present")?;
                if !self.interval_set {
                    glx.swap_interval(*window, self.interval)
                        .unwrap_or_else(note);
                    self.interval_set = true;
                }
                asked
            }
            Post::Readback { .. } => unreachable!("posted by readback above"),
        };
        let (tw, th) = (standin.width as i32, standin.height as i32);
        let [_, r, g, b] = fill.to_be_bytes();
        let fill = [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ];
        let fbo = standin.fbo(&cur)?;
        let mut asked = (sw, sh);
        let mut reposts = 0;
        loop {
            draw_standin(&cur, dev.caps().srgb_views, fbo, (tw, th), asked, &fill);
            cur.check("present blit")?;
            let (window, xid) = match &self.post {
                Post::Egl { fns, surface, .. } => {
                    return cur.swap(fns, *surface).map_err(Into::into);
                }
                Post::Glx { window, xid } => (*window, *xid),
                Post::Readback { .. } => unreachable!("posted by readback above"),
            };
            let glx = cur.glx().ok_or("gl: a GLX window on an EGL device")?;
            if let Err(e) = glx.swap(window) {
                cur.shared().latch_lost(&e);
                return Err(e.into());
            }
            // An unanswered ask binds nothing: the next present's ask finds why.
            let now = self.ask(glx, xid);
            if reposts == REPOSTS || now == asked || now == (0, 0) {
                return Ok(());
            }
            if glx
                .make_current()
                .and_then(|()| glx.bind_window(window))
                .is_err()
            {
                return Ok(());
            }
            asked = self.ask(glx, xid);
            reposts += 1;
        }
    }

    fn ask(&self, glx: &super::glx::GlxContext, xid: u64) -> (i32, i32) {
        #[cfg(test)]
        if let Some(size) = self.asked_once.take().or(self.asked_size) {
            return size;
        }
        let (w, h) = glx.size(xid);
        (i32::try_from(w).unwrap_or(0), i32::try_from(h).unwrap_or(0))
    }
}

fn draw_standin(
    cur: &super::device::Current<'_>,
    srgb_views: bool,
    fbo: ffi::GLuint,
    (tw, th): (i32, i32),
    (sw, sh): (i32, i32),
    fill: &[f32; 4],
) {
    let sw = if sw > 0 { sw } else { tw };
    let top = if sh > 0 { sh } else { th };
    let gl = cur.gl();
    // SAFETY: The context and the window surface are current; the stand-in's framebuffer is
    // complete; the blits are colour-only and NEAREST, the frame's between equal-sized rects;
    // each clear reads four floats from `fill` for the one colour buffer.
    unsafe {
        (gl.disable)(ffi::GL_SCISSOR_TEST);
        if srgb_views {
            (gl.disable)(ffi::GL_FRAMEBUFFER_SRGB_EXT);
        }
        (gl.color_mask)(ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE, ffi::GL_TRUE);
        (gl.bind_framebuffer)(ffi::GL_READ_FRAMEBUFFER, fbo);
        (gl.bind_framebuffer)(ffi::GL_DRAW_FRAMEBUFFER, 0);
        (gl.enable)(ffi::GL_SCISSOR_TEST);
        for [x, y, w, h] in [[sw, 0, PAST, PAST], [0, top, sw, PAST]] {
            (gl.scissor)(x, y, w, h);
            (gl.clear_bufferfv)(ffi::GL_COLOR, 0, fill.as_ptr());
        }
        (gl.disable)(ffi::GL_SCISSOR_TEST);
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
}

impl Drop for GlWindowSurface {
    fn drop(&mut self) {
        self.standin = None;
        match self.post {
            Post::Egl {
                surface, wl_window, ..
            } => {
                if let Some(b) = self.shared.egl_binding() {
                    // SAFETY: A surface of this display; the arm never leaves it current past a
                    // guard, so destroying it here is immediate.
                    unsafe { (b.egl.fns.destroy_surface)(b.display, surface) };
                }
                if !wl_window.is_null()
                    && let Ok(wl) = WaylandEgl::get()
                {
                    // SAFETY: The window this surface drew into; the surface is gone.
                    unsafe { (wl.destroy)(wl_window) };
                }
            }
            Post::Glx { window, .. } => {
                if let Some(glx) = self.shared.glx() {
                    glx.destroy_window(window);
                }
            }
            Post::Readback { .. } => {}
        }
    }
}

fn readback_wanted(vendor: &str, composited: bool, forced: Option<&str>) -> bool {
    match forced {
        Some("readback") => true,
        Some("window") => false,
        _ => cfg!(target_endian = "little") && composited && vendor.starts_with("NVIDIA"),
    }
}

/// Whether a window on `dev`'s X11 display is posted by readback ([`readback_wanted`]).
pub(crate) fn readback_wanted_on(dev: &GlDevice) -> bool {
    let shared = dev.shared();
    let (Some(glx), Some(WindowSystem::X11 { display, screen })) =
        (shared.glx(), shared.window_system())
    else {
        return false;
    };
    let composited = super::glx::composited(display as *mut c_void, screen);
    readback_wanted(glx.server_vendor(), composited, post_override().as_deref())
}

fn post_override() -> Option<String> {
    aterm_types::dev_seam!("ATERM_GL_POST").map(|v| v.to_string_lossy().trim().to_owned())
}

fn attached(mut sink: Box<dyn crate::FrameSink>) -> Option<Box<dyn crate::FrameSink>> {
    match sink.attach() {
        Ok(()) => Some(sink),
        Err(e) => {
            crate::stderr_line!(
                "aterm-gpu (gl arm): the window's presenter cannot attach ({e}); posting it \
                 through a GLXWindow"
            );
            None
        }
    }
}

fn read_standin(
    cur: &super::device::Current<'_>,
    fbo: ffi::GLuint,
    (tw, th): (u32, u32),
    [x0, y0, x1, y1]: [u32; 4],
    bgra: bool,
    words: &mut [u32],
) -> Result<(), String> {
    if x0 >= x1 || y0 >= y1 || x1 > tw || y1 > th {
        return Err(format!(
            "gl: a read of {x0},{y0} to {x1},{y1} from a {tw}x{th} frame"
        ));
    }
    let n = tw as usize * th as usize;
    let len = words.len();
    let words = words
        .get_mut(..n)
        .ok_or_else(|| format!("gl: a {tw}x{th} frame in a store of {len} words"))?;
    let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let first = y0 as usize * tw as usize + x0 as usize;
    let gl = cur.gl();
    // SAFETY: the context is current; with no pack buffer, alignment 4 and row length `tw`, the
    // read writes `rh` rows of `rw` texels from `first`, ending at `(y1 - 1) * tw + x1 <= tw * th`,
    // inside `words` (checked above).
    unsafe {
        (gl.bind_buffer)(ffi::GL_PIXEL_PACK_BUFFER, 0);
        (gl.bind_framebuffer)(ffi::GL_READ_FRAMEBUFFER, fbo);
        (gl.pixel_storei)(ffi::GL_PACK_ALIGNMENT, 4);
        (gl.pixel_storei)(ffi::GL_PACK_ROW_LENGTH, tw as c_int);
        (gl.read_pixels)(
            x0 as c_int,
            y0 as c_int,
            rw as c_int,
            rh as c_int,
            if bgra { ffi::GL_BGRA_EXT } else { ffi::GL_RGBA },
            ffi::GL_UNSIGNED_BYTE,
            words[first..].as_mut_ptr().cast(),
        );
        (gl.pixel_storei)(ffi::GL_PACK_ROW_LENGTH, 0);
    }
    cur.check("readback post")?;
    if !bgra {
        for row in words[first..].chunks_mut(tw as usize).take(rh) {
            for w in &mut row[..rw] {
                *w = (*w & 0xff00_ff00) | ((*w >> 16) & 0xff) | ((*w & 0xff) << 16);
            }
        }
    }
    Ok(())
}

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

    #[test]
    fn only_nvidia_s_composited_glx_posts_by_readback() {
        let little = cfg!(target_endian = "little");
        assert_eq!(readback_wanted("NVIDIA Corporation", true, None), little);
        assert!(
            !readback_wanted("NVIDIA Corporation", false, None),
            "no compositing manager"
        );
        assert!(!readback_wanted("SGI", true, None), "Mesa's GLX");
        assert!(readback_wanted("SGI", false, Some("readback")));
        assert!(!readback_wanted("NVIDIA Corporation", true, Some("window")));
        assert_eq!(
            readback_wanted("NVIDIA Corporation", true, Some("other")),
            little,
            "an unknown seam value is the default"
        );
    }

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
