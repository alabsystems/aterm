// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's X11 binding. No GLX call is made once the server has hung up the connection
//! ([`GlxContext::gone`]): Xlib keeps queueing on a broken connection until a caller dies of SIGSEGV
//! (Mesa's drisw asks a drawable's geometry inside a swap), and `glXQueryDrawable` reads a failed
//! reply unchecked (an abort); a window's size is asked of Xlib's `XGetGeometry`, which checks it.

use std::ffi::{CStr, c_int, c_uint, c_void};

use super::ffi::{self, GLXContext, GLXDrawable, GLXFBConfig, Glx, XDisplay};

/// How a window takes its swap interval, by the extension GLX offers.
#[derive(Clone, Copy)]
enum SwapControl {
    /// `GLX_EXT_swap_control`: per drawable.
    Ext(ffi::SwapIntervalExtFn),
    /// `GLX_MESA_swap_control`: the current drawable's.
    Mesa(ffi::SwapIntervalMesaFn),
    /// `GLX_SGI_swap_control`: the current drawable's, and never 0.
    Sgi(ffi::SwapIntervalSgiFn),
    /// None offered: the driver's default stands.
    Absent,
}

/// How a GLX context hears of a GPU reset: what it was made with, by what GLX offered and the driver accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Robustness {
    /// No reset notification: GLX offers no `GLX_ARB_create_context_robustness`, or the driver refused it.
    Plain,
    /// Robust access, and the context lost on a reset, which `glGetGraphicsResetStatus` and `GL_CONTEXT_LOST` then report.
    LoseOnReset,
    /// As [`Self::LoseOnReset`], and a purge of the context's video memory reported as a reset (`GL_PURGED_CONTEXT_RESET_NV`).
    LoseOnResetOrPurge,
}

impl Robustness {
    /// Whether `glGetGraphicsResetStatus` can answer anything but `GL_NO_ERROR` on such a context.
    #[inline]
    pub(crate) const fn reports_resets(self) -> bool {
        !matches!(self, Self::Plain)
    }
}

/// GLX OpenGL ES 3 context on an X11 display, with its shared FBConfig and idle pbuffer.
pub(crate) struct GlxContext {
    glx: &'static Glx,
    display: XDisplay,
    config: GLXFBConfig,
    /// Visual ID required for windows posting on this context.
    visual: u32,
    context: GLXContext,
    robustness: Robustness,
    pbuffer: GLXDrawable,
    swap: SwapControl,
    /// Server GLX vendor (`glXQueryServerString`).
    vendor: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConfigFacts {
    rgb: [c_int; 3],
    alpha: c_int,
    sample_buffers: c_int,
    visual: u32,
}

/// Select the first single-sampled RGB888 config with 0- or 8-bit alpha on `visual`.
fn pick_config(configs: &[ConfigFacts], visual: u32) -> Option<usize> {
    configs.iter().position(|c| {
        c.visual == visual
            && c.rgb == [8, 8, 8]
            && matches!(c.alpha, 0 | 8)
            && c.sample_buffers == 0
    })
}

/// Return whether the space-separated extension list `list` contains `ext`.
fn has_ext(list: &str, ext: &str) -> bool {
    list.split_ascii_whitespace().any(|e| e == ext)
}

/// Build the 0-terminated OpenGL ES 3.0 attribute lists to try in order of decreasing robustness.
fn context_attempts(exts: &str) -> Vec<(Robustness, Vec<c_int>)> {
    let es30 = [
        ffi::GLX_CONTEXT_MAJOR_VERSION_ARB,
        3,
        ffi::GLX_CONTEXT_MINOR_VERSION_ARB,
        0,
        ffi::GLX_CONTEXT_PROFILE_MASK_ARB,
        ffi::GLX_CONTEXT_ES2_PROFILE_BIT_EXT,
    ];
    let robust = [
        ffi::GLX_CONTEXT_FLAGS_ARB,
        ffi::GLX_CONTEXT_ROBUST_ACCESS_BIT_ARB,
        ffi::GLX_CONTEXT_RESET_NOTIFICATION_STRATEGY_ARB,
        ffi::GLX_LOSE_CONTEXT_ON_RESET_ARB,
    ];
    let purge = [ffi::GLX_GENERATE_RESET_ON_VIDEO_MEMORY_PURGE_NV, 1];
    let mut attempts = Vec::with_capacity(3);
    if has_ext(exts, "GLX_ARB_create_context_robustness") {
        if has_ext(exts, "GLX_NV_robustness_video_memory_purge") {
            attempts.push((
                Robustness::LoseOnResetOrPurge,
                [&es30[..], &robust, &purge, &[0]].concat(),
            ));
        }
        attempts.push((Robustness::LoseOnReset, [&es30[..], &robust, &[0]].concat()));
    }
    attempts.push((Robustness::Plain, [&es30[..], &[0]].concat()));
    attempts
}

impl GlxContext {
    /// Create a GLX OpenGL ES 3.0 context on `display` for `screen`'s default visual.
    pub(crate) fn new(display: XDisplay, screen: c_int) -> Result<Self, String> {
        Self::offering(display, screen, "")
    }

    /// Create a context as [`Self::new`], appending `also` to GLX's extension list.
    fn offering(display: XDisplay, screen: c_int, also: &str) -> Result<Self, String> {
        let glx = Glx::get()?;
        if hung_up(glx, display) {
            return Err(GONE.to_owned());
        }
        let fns = &glx.fns;
        // SAFETY: a live display and one of its screens; the string is GLX's.
        let mut exts = unsafe { ffi::c_string((fns.query_extensions_string)(display, screen)) };
        exts.push(' ');
        exts.push_str(also);
        if !has_ext(&exts, "GLX_ARB_create_context")
            || !has_ext(&exts, "GLX_EXT_create_context_es_profile")
        {
            return Err(
                "gl: this GLX makes no OpenGL ES 3.0 context (GLX_EXT_create_context_es_profile)"
                    .to_owned(),
            );
        }
        let attribs = [
            ffi::GLX_X_RENDERABLE,
            1,
            ffi::GLX_DRAWABLE_TYPE,
            ffi::GLX_WINDOW_BIT | ffi::GLX_PBUFFER_BIT,
            ffi::GLX_RENDER_TYPE,
            ffi::GLX_RGBA_BIT,
            ffi::GLX_X_VISUAL_TYPE,
            ffi::GLX_TRUE_COLOR,
            ffi::GLX_RED_SIZE,
            8,
            ffi::GLX_GREEN_SIZE,
            8,
            ffi::GLX_BLUE_SIZE,
            8,
            ffi::GLX_DOUBLEBUFFER,
            1,
            0,
        ];
        let mut n = 0;
        // SAFETY: a live display, one of its screens, a 0-terminated list.
        let list = unsafe { (fns.choose_fb_config)(display, screen, attribs.as_ptr(), &raw mut n) };
        if list.is_null() {
            return Err("gl: GLX has no RGB888 double-buffered window config".to_owned());
        }
        // SAFETY: GLX returned `n` configs at `list`; the configs outlive the array, which is GLX's to free (`XFree`).
        let configs =
            unsafe { std::slice::from_raw_parts(list, usize::try_from(n).unwrap_or(0)) }.to_vec();
        // SAFETY: the array `glXChooseFBConfig` allocated, freed once.
        unsafe { (glx.x.free)(list.cast()) };
        let attrib = |config, name| {
            let mut v = 0;
            // SAFETY: a config of this display and a live out-pointer.
            unsafe { (fns.get_fb_config_attrib)(display, config, name, &raw mut v) };
            v
        };
        let facts: Vec<ConfigFacts> = configs
            .iter()
            .map(|&c| ConfigFacts {
                rgb: [
                    attrib(c, ffi::GLX_RED_SIZE),
                    attrib(c, ffi::GLX_GREEN_SIZE),
                    attrib(c, ffi::GLX_BLUE_SIZE),
                ],
                alpha: attrib(c, ffi::GLX_ALPHA_SIZE),
                sample_buffers: attrib(c, ffi::GLX_SAMPLE_BUFFERS),
                visual: u32::try_from(attrib(c, ffi::GLX_VISUAL_ID)).unwrap_or(0),
            })
            .collect();
        // SAFETY: a live display and one of its screens; the visual is the display's own.
        let visual =
            unsafe { (glx.x.visual_id_from_visual)((glx.x.default_visual)(display, screen)) };
        let visual = u32::try_from(visual).unwrap_or(0);
        let config = pick_config(&facts, visual)
            .map(|i| configs[i])
            .ok_or_else(|| {
                format!(
                    "gl: GLX has no RGB888 single-sampled config on the default visual {visual:#x}"
                )
            })?;
        // SAFETY: `GLX_ARB_create_context` is offered (checked above), so the name resolves to the function its registry signature declares.
        let create = unsafe {
            proc_address(glx, c"glXCreateContextAttribsARB")
                .map(|p| std::mem::transmute::<*const c_void, ffi::CreateContextAttribsFn>(p))
        }
        .ok_or("gl: GLX has no glXCreateContextAttribsARB")?;
        let mut refused = Vec::new();
        let mut made = None;
        for (robustness, attribs) in context_attempts(&exts) {
            // SAFETY: a config of this display, no share context, direct rendering, a 0-terminated list.
            let context =
                unsafe { create(display, config, std::ptr::null_mut(), 1, attribs.as_ptr()) };
            if !context.is_null() {
                made = Some((context, robustness));
                break;
            }
            refused.push(robustness);
        }
        let (context, robustness) =
            made.ok_or("gl: GLX refused an OpenGL ES 3.0 context".to_owned())?;
        if !refused.is_empty() {
            let unheard = if robustness.reports_resets() {
                "a video memory purge"
            } else {
                "a GPU reset"
            };
            crate::stderr_line!(
                "aterm-gpu (gl arm): GLX refused a robust OpenGL ES 3.0 context ({refused:?}); \
                 the context is {robustness:?}, and {unheard} may go unheard"
            );
        }
        let pbuffer_attribs = [ffi::GLX_PBUFFER_WIDTH, 1, ffi::GLX_PBUFFER_HEIGHT, 1, 0];
        // SAFETY: a pbuffer-capable config (`GLX_PBUFFER_BIT` above) and a 0-terminated list.
        let pbuffer = unsafe { (fns.create_pbuffer)(display, config, pbuffer_attribs.as_ptr()) };
        if pbuffer == 0 {
            // SAFETY: the context made above, never current.
            unsafe { (fns.destroy_context)(display, context) };
            return Err("gl: GLX refused a 1x1 pbuffer".to_owned());
        }
        // SAFETY (each arm): the extension that defines the name is offered, so it resolves to the
        // function its alias type declares.
        let swap = unsafe {
            let named = |ext: &str, name: &CStr| {
                has_ext(&exts, ext)
                    .then(|| proc_address(glx, name))
                    .flatten()
            };
            if let Some(p) = named("GLX_EXT_swap_control", c"glXSwapIntervalEXT") {
                SwapControl::Ext(std::mem::transmute::<*const c_void, ffi::SwapIntervalExtFn>(p))
            } else if let Some(p) = named("GLX_MESA_swap_control", c"glXSwapIntervalMESA") {
                SwapControl::Mesa(std::mem::transmute::<*const c_void, ffi::SwapIntervalMesaFn>(p))
            } else if let Some(p) = named("GLX_SGI_swap_control", c"glXSwapIntervalSGI") {
                SwapControl::Sgi(std::mem::transmute::<*const c_void, ffi::SwapIntervalSgiFn>(p))
            } else {
                SwapControl::Absent
            }
        };
        // SAFETY: a live display and one of its screens; the string is GLX's.
        let vendor =
            unsafe { ffi::c_string((fns.query_server_string)(display, screen, ffi::GLX_VENDOR)) };
        Ok(Self {
            glx,
            display,
            config,
            visual,
            context,
            robustness,
            pbuffer,
            swap,
            vendor,
        })
    }

    /// How the context hears of a GPU reset.
    #[inline]
    pub(crate) const fn robustness(&self) -> Robustness {
        self.robustness
    }

    /// The server's GLX vendor (`NVIDIA Corporation`, `SGI` for Mesa's).
    #[inline]
    pub(crate) fn server_vendor(&self) -> &str {
        &self.vendor
    }

    /// The GL (or GLX) function named `sym`, for the GL table; null when GLX has none.
    pub(crate) fn proc_address(&self, sym: &CStr) -> *const c_void {
        proc_address(self.glx, sym).unwrap_or(std::ptr::null())
    }

    /// Make the context current on its pbuffer (the binding between posts).
    pub(crate) fn make_current(&self) -> Result<(), String> {
        self.bind(self.pbuffer)
    }

    /// Make the context current on `window`, a `GLXWindow` of this context's ([`Self::create_window`]).
    pub(crate) fn bind_window(&self, window: GLXDrawable) -> Result<(), String> {
        self.bind(window)
    }

    fn bind(&self, drawable: GLXDrawable) -> Result<(), String> {
        if self.gone() {
            return Err(GONE.to_owned());
        }
        // SAFETY: a drawable made on this display with the context's config; the device lock keeps every other thread off the context.
        let ok = unsafe {
            (self.glx.fns.make_context_current)(self.display, drawable, drawable, self.context)
        };
        if ok == 0 {
            Err("gl: glXMakeContextCurrent failed".to_owned())
        } else {
            Ok(())
        }
    }

    /// Release the calling thread's context (flushing it).
    pub(crate) fn release(&self) {
        if self.gone() {
            return;
        }
        // SAFETY: unbinding with no drawables and no context is always legal.
        unsafe {
            (self.glx.fns.make_context_current)(self.display, 0, 0, std::ptr::null_mut());
        }
    }

    /// A `GLXWindow` on the X11 window `window`, whose visual (`None`: unknown) must be the context's.
    pub(crate) fn create_window(
        &self,
        window: u64,
        visual: Option<u32>,
    ) -> Result<GLXDrawable, String> {
        if let Some(v) = visual
            && v != self.visual
        {
            return Err(format!(
                "gl: the window's visual {v:#x} is not the GLX context's {:#x}",
                self.visual
            ));
        }
        let attribs = [0];
        // SAFETY: a live window of this display on the config's visual, and a 0-terminated (empty) list.
        let drawable = unsafe {
            (self.glx.fns.create_window)(self.display, self.config, window, attribs.as_ptr())
        };
        if drawable == 0 {
            Err("gl: glXCreateWindow failed".to_owned())
        } else {
            Ok(drawable)
        }
    }

    /// Destroy a `GLXWindow` this context made, which no thread holds current
    /// (on a gone connection the server freed it).
    pub(crate) fn destroy_window(&self, window: GLXDrawable) {
        if self.gone() {
            return;
        }
        // SAFETY: as documented; the X11 window under it is still alive.
        unsafe { (self.glx.fns.destroy_window)(self.display, window) };
    }

    /// The X11 window `xid`'s width and height now, which its `GLXWindow` follows: one round trip; (0, 0) when the server answers none.
    pub(crate) fn size(&self, xid: u64) -> (u32, u32) {
        let (mut root, mut x, mut y) = (0, 0, 0);
        let (mut w, mut h, mut border, mut depth): (c_uint, c_uint, c_uint, c_uint) = (0, 0, 0, 0);
        // SAFETY: a window of this display and live out-pointers.
        let ok = unsafe {
            (self.glx.x.get_geometry)(
                self.display,
                xid,
                &raw mut root,
                &raw mut x,
                &raw mut y,
                &raw mut w,
                &raw mut h,
                &raw mut border,
                &raw mut depth,
            )
        };
        if ok == 0 { (0, 0) } else { (w, h) }
    }

    /// Set `window`'s swap interval (0: post at once; 1: wait for the refresh).
    pub(crate) fn swap_interval(&self, window: GLXDrawable, interval: i32) -> Result<(), String> {
        // SAFETY (each call): the function the offered extension defines; the window is current,
        // which the MESA and SGI forms act on.
        match self.swap {
            SwapControl::Ext(f) => {
                unsafe { f(self.display, window, interval) };
                Ok(())
            }
            SwapControl::Mesa(f) => match unsafe { f(c_uint::try_from(interval).unwrap_or(0)) } {
                0 => Ok(()),
                e => Err(format!("gl: glXSwapIntervalMESA({interval}): error {e}")),
            },
            SwapControl::Sgi(f) if interval > 0 => match unsafe { f(interval) } {
                0 => Ok(()),
                e => Err(format!("gl: glXSwapIntervalSGI({interval}): error {e}")),
            },
            SwapControl::Sgi(_) => {
                Err("gl: GLX_SGI_swap_control cannot post unsynchronized (interval 0)".to_owned())
            }
            SwapControl::Absent => Err("gl: this GLX offers no swap control".to_owned()),
        }
    }

    /// Post `window`'s back buffer, current on the calling thread.
    pub(crate) fn swap(&self, window: GLXDrawable) -> Result<(), String> {
        if self.gone() {
            return Err(GONE.to_owned());
        }
        // SAFETY: a `GLXWindow` of this display, current here.
        unsafe { (self.glx.fns.swap_buffers)(self.display, window) };
        Ok(())
    }

    /// Whether the X server has hung up this context's connection
    /// ([`hung_up`]): no GLX call is made on it after (see the module header).
    pub(crate) fn gone(&self) -> bool {
        hung_up(self.glx, self.display)
    }

    /// The hang-up as a loss's reason, where the server has hung up.
    pub(crate) fn hung_up_reason(&self) -> Option<&'static str> {
        self.gone().then_some(GONE)
    }
}

impl Drop for GlxContext {
    fn drop(&mut self) {
        // On a gone connection the server freed both with it.
        if self.gone() {
            return;
        }
        // SAFETY: the device owning this context is gone, so no thread holds
        // it current; the pbuffer and the context were made on this display.
        unsafe {
            (self.glx.fns.destroy_pbuffer)(self.display, self.pbuffer);
            (self.glx.fns.destroy_context)(self.display, self.context);
        }
    }
}

/// What a GLX call answers once the server has hung up its connection.
const GONE: &str = "gl: the X server hung up the connection";

/// Whether the X server has hung up `display`'s connection (it died, or
/// closed it), read off the socket before Xlib has: a hang-up or an error
/// polled, or a readable socket whose next byte is its end. Bytes still queued
/// ahead of the end hide it until Xlib has read them.
fn hung_up(glx: &Glx, display: XDisplay) -> bool {
    // SAFETY: a live display; `XConnectionNumber` reads its socket's number
    // and sends nothing.
    let fd = unsafe { (glx.x.connection_number)(display) };
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live pollfd; a zero timeout never blocks.
    if unsafe { libc::poll(&raw mut p, 1, 0) } <= 0 {
        return false;
    }
    if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    let mut byte = 0u8;
    // SAFETY: one byte into a live local; a peek that does not block takes
    // nothing off the socket.
    let peeked = unsafe {
        libc::recv(
            fd,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    peeked == 0
}

/// Whether a compositing manager runs on `display`'s screen `screen` (it owns the `_NET_WM_CM_S<screen>` selection, EWMH).
pub(crate) fn composited(display: XDisplay, screen: c_int) -> bool {
    let Ok(glx) = Glx::get() else {
        return false;
    };
    let Ok(name) = std::ffi::CString::new(format!("_NET_WM_CM_S{screen}")) else {
        return false;
    };
    // SAFETY: a live display and a NUL-terminated name; `only_if_exists` false, so the atom is made if no client named it yet.
    unsafe {
        let selection = (glx.x.intern_atom)(display, name.as_ptr(), 0);
        selection != 0 && (glx.x.get_selection_owner)(display, selection) != 0
    }
}

/// `glXGetProcAddressARB(sym)`, or `None` when GLX resolves nothing.
fn proc_address(glx: &Glx, sym: &CStr) -> Option<*const c_void> {
    // SAFETY: a NUL-terminated name; GLX returns null or an entry point.
    let p = unsafe { (glx.fns.get_proc_address)(sym.as_ptr().cast()) };
    (!p.is_null()).then_some(p)
}

#[cfg(test)]
#[path = "glx_tests.rs"]
mod tests;
