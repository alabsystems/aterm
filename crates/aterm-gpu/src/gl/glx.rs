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
mod tests {
    use super::super::device::{GlDevice, WindowSystem};
    use super::super::surface::{GlWindowSurface, NativeWindow};
    use super::*;

    fn config(rgb: [c_int; 3], alpha: c_int, sample_buffers: c_int, visual: u32) -> ConfigFacts {
        ConfigFacts {
            rgb,
            alpha,
            sample_buffers,
            visual,
        }
    }

    /// The shared config is the first single-sampled RGB888 config on the window's visual.
    #[test]
    fn the_shared_config_is_exactly_rgb888_single_sampled_on_the_visual() {
        let listed = [
            config([10, 10, 10], 2, 0, 0x21),
            config([8, 8, 8], 8, 1, 0x21),
            config([8, 8, 8], 0, 0, 0x27),
            config([8, 8, 8], 0, 0, 0x21),
            config([8, 8, 8], 8, 0, 0x21),
        ];
        assert_eq!(pick_config(&listed, 0x21), Some(3));
        assert_eq!(pick_config(&listed, 0x27), Some(2));
        assert_eq!(pick_config(&listed, 0x99), None);
        assert_eq!(
            pick_config(&[config([8, 8, 8], 8, 0, 0x24)], 0x24),
            Some(0),
            "an RGBA8 config serves its visual"
        );
        assert_eq!(pick_config(&[config([8, 8, 8], 4, 0, 0x24)], 0x24), None);
    }

    /// Context attribute lists request robustness and purge reset notification when offered, falling back to plain ES 3.0.
    #[test]
    fn the_context_asks_for_robustness_where_glx_offers_it() {
        use Robustness::{LoseOnReset, LoseOnResetOrPurge, Plain};
        let pairs = |attribs: &[c_int]| -> Vec<(c_int, c_int)> {
            assert_eq!(attribs.len() % 2, 1, "pairs and a terminator: {attribs:?}");
            assert_eq!(attribs.last(), Some(&0), "0-terminated: {attribs:?}");
            let (pairs, _terminator) = attribs.as_chunks::<2>();
            pairs.iter().map(|&[name, value]| (name, value)).collect()
        };
        let kinds = |exts: &str| -> Vec<Robustness> {
            context_attempts(exts).into_iter().map(|(r, _)| r).collect()
        };
        let es30 = [
            (ffi::GLX_CONTEXT_MAJOR_VERSION_ARB, 3),
            (ffi::GLX_CONTEXT_MINOR_VERSION_ARB, 0),
            (
                ffi::GLX_CONTEXT_PROFILE_MASK_ARB,
                ffi::GLX_CONTEXT_ES2_PROFILE_BIT_EXT,
            ),
        ];
        let robust = [
            (
                ffi::GLX_CONTEXT_FLAGS_ARB,
                ffi::GLX_CONTEXT_ROBUST_ACCESS_BIT_ARB,
            ),
            (
                ffi::GLX_CONTEXT_RESET_NOTIFICATION_STRATEGY_ARB,
                ffi::GLX_LOSE_CONTEXT_ON_RESET_ARB,
            ),
        ];
        let purge = [(ffi::GLX_GENERATE_RESET_ON_VIDEO_MEMORY_PURGE_NV, 1)];
        let nvidia = "GLX_ARB_create_context GLX_EXT_create_context_es_profile \
                      GLX_ARB_create_context_robustness GLX_NV_robustness_video_memory_purge";
        let attempts = context_attempts(nvidia);
        assert_eq!(kinds(nvidia), [LoseOnResetOrPurge, LoseOnReset, Plain]);
        assert_eq!(pairs(&attempts[0].1), [&es30[..], &robust, &purge].concat());
        assert_eq!(pairs(&attempts[1].1), [&es30[..], &robust].concat());
        assert_eq!(pairs(&attempts[2].1), es30);
        assert_eq!(
            kinds("GLX_ARB_create_context GLX_ARB_create_context_robustness"),
            [LoseOnReset, Plain]
        );
        let mesa = "GLX_ARB_create_context GLX_ARB_create_context_profile \
                    GLX_EXT_create_context_es_profile GLX_MESA_query_renderer";
        assert_eq!(kinds(mesa), [Plain]);
        assert_eq!(kinds("GLX_NV_robustness_video_memory_purge"), [Plain]);
        assert_eq!(
            kinds("GLX_ARB_create_context_robustness_v2"),
            [Plain],
            "an extension is named whole"
        );
        assert!(LoseOnResetOrPurge.reports_resets() && LoseOnReset.reports_resets());
        assert!(!Plain.reports_resets());
    }

    /// Minimal Xlib bindings for creating a test window and reading pixels back via `XGetImage`.
    mod xlib {
        use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub(super) type Display = *mut c_void;
        pub(super) type ErrorHandler = unsafe extern "C" fn(Display, *mut c_void) -> c_int;

        crate::gl::ffi::fn_table! {
            /// Xlib entry points used by the tests.
            pub(super) struct Xlib {
                init_threads = "XInitThreads": fn() -> c_int;
                set_error_handler = "XSetErrorHandler":
                    fn(Option<ErrorHandler>) -> Option<ErrorHandler>;
                open_display = "XOpenDisplay": fn(*const c_char) -> Display;
                close_display = "XCloseDisplay": fn(Display) -> c_int;
                default_screen = "XDefaultScreen": fn(Display) -> c_int;
                root_window = "XRootWindow": fn(Display, c_int) -> c_ulong;
                create_window = "XCreateWindow": fn(
                    Display, c_ulong, c_int, c_int, c_uint, c_uint, c_uint, c_int, c_uint,
                    *mut c_void, c_ulong, *mut XSetWindowAttributes,
                ) -> c_ulong;
                destroy_window = "XDestroyWindow": fn(Display, c_ulong) -> c_int;
                map_window = "XMapWindow": fn(Display, c_ulong) -> c_int;
                resize_window = "XResizeWindow": fn(Display, c_ulong, c_uint, c_uint) -> c_int;
                pending = "XPending": fn(Display) -> c_int;
                next_event = "XNextEvent": fn(Display, *mut XEvent) -> c_int;
                sync = "XSync": fn(Display, c_int) -> c_int;
                get_image = "XGetImage": fn(
                    Display, c_ulong, c_int, c_int, c_uint, c_uint, c_ulong, c_int,
                ) -> *mut XImage;
                free = "XFree": fn(*mut c_void) -> c_int;
                intern_atom = "XInternAtom": fn(Display, *const c_char, c_int) -> c_ulong;
                get_selection_owner = "XGetSelectionOwner": fn(Display, c_ulong) -> c_ulong;
                connection_number = "XConnectionNumber": fn(Display) -> c_int;
                set_io_error_handler = "XSetIOErrorHandler":
                    fn(Option<IoErrorHandler>) -> Option<IoErrorHandler>;
                set_io_error_exit_handler = "XSetIOErrorExitHandler":
                    fn(Display, Option<IoErrorExitHandler>, *mut c_void);
            }
        }

        pub(super) type IoErrorHandler = unsafe extern "C" fn(Display) -> c_int;
        pub(super) type IoErrorExitHandler = unsafe extern "C" fn(Display, *mut c_void);

        /// An IO error handler that returns, as aterm-window's does (Xlib's
        /// default ends the process).
        pub(super) unsafe extern "C" fn io_error_returns(_: Display) -> c_int {
            0
        }

        /// An IO error exit handler that returns, as aterm-window's does.
        pub(super) unsafe extern "C" fn io_error_exit_returns(_: Display, _: *mut c_void) {}

        #[repr(C)]
        pub(super) struct XEvent {
            pub(super) kind: c_int,
            _rest: [c_long; 24],
        }

        impl XEvent {
            pub(super) fn zeroed() -> Self {
                Self {
                    kind: 0,
                    _rest: [0; 24],
                }
            }
        }

        #[repr(C)]
        #[derive(Default)]
        pub(super) struct XSetWindowAttributes {
            pub(super) background_pixmap: c_ulong,
            pub(super) background_pixel: c_ulong,
            pub(super) border_pixmap: c_ulong,
            pub(super) border_pixel: c_ulong,
            pub(super) bit_gravity: c_int,
            pub(super) win_gravity: c_int,
            pub(super) backing_store: c_int,
            pub(super) backing_planes: c_ulong,
            pub(super) backing_pixel: c_ulong,
            pub(super) save_under: c_int,
            pub(super) event_mask: c_long,
            pub(super) do_not_propagate_mask: c_long,
            pub(super) override_redirect: c_int,
            pub(super) colormap: c_ulong,
            pub(super) cursor: c_ulong,
        }

        #[repr(C)]
        pub(super) struct XImage {
            pub(super) width: c_int,
            pub(super) height: c_int,
            pub(super) xoffset: c_int,
            pub(super) format: c_int,
            pub(super) data: *mut u8,
            pub(super) byte_order: c_int,
            pub(super) bitmap_unit: c_int,
            pub(super) bitmap_bit_order: c_int,
            pub(super) bitmap_pad: c_int,
            pub(super) depth: c_int,
            pub(super) bytes_per_line: c_int,
            pub(super) bits_per_pixel: c_int,
            pub(super) red_mask: c_ulong,
            pub(super) green_mask: c_ulong,
            pub(super) blue_mask: c_ulong,
        }

        pub(super) const EXPOSURE_MASK: c_long = 1 << 15;
        pub(super) const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;
        pub(super) const EXPOSE: c_int = 12;
        pub(super) const MAP_NOTIFY: c_int = 19;
        pub(super) const CW_BACK_PIXMAP: c_ulong = 1 << 0;
        pub(super) const CW_OVERRIDE_REDIRECT: c_ulong = 1 << 9;
        pub(super) const CW_EVENT_MASK: c_ulong = 1 << 11;
        /// `XCreateWindow`'s depth from the parent, and its class.
        pub(super) const COPY_FROM_PARENT: c_int = 0;
        pub(super) const INPUT_OUTPUT: c_uint = 1;
        pub(super) const Z_PIXMAP: c_int = 2;
        pub(super) const LSB_FIRST: c_int = 0;

        impl Xlib {
            /// libX11, opened for the test (never closed).
            pub(super) fn open() -> Xlib {
                // SAFETY: a NUL-terminated name.
                let h = unsafe { libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_LAZY) };
                if h.is_null() {
                    aterm_spec::host::could_not_run(
                        "$DISPLAY names an X server, but libX11.so.6 does not open",
                    );
                }
                // SAFETY: `dlsym` on a live handle; the table declares each function's Xlib.h signature.
                unsafe { Xlib::load(&mut |sym: &CStr| libc::dlsym(h, sym.as_ptr()).cast_const()) }
                    .expect("libX11 has the calls the test makes")
            }
        }

        /// The X errors the connection raised.
        pub(super) static ERRORS: AtomicUsize = AtomicUsize::new(0);

        /// An X error handler that counts the error and returns, as aterm-window's does (Xlib's default handler exits the process).
        pub(super) unsafe extern "C" fn count_errors(_: Display, _: *mut c_void) -> c_int {
            ERRORS.fetch_add(1, Ordering::Relaxed);
            0
        }
    }

    /// Child display and window guard that restores the X error handler on drop.
    struct Desk<'x> {
        x: &'x xlib::Xlib,
        dpy: xlib::Display,
        screen: c_int,
        win: std::ffi::c_ulong,
        previous: Option<xlib::ErrorHandler>,
    }

    #[derive(Default)]
    struct Seen {
        mapped: bool,
        exposed: bool,
    }

    impl<'x> Desk<'x> {
        /// Open `$DISPLAY` with [`xlib::count_errors`] and map an override-redirect `w` x `h` window.
        fn open(x: &'x xlib::Xlib, w: u32, h: u32) -> Self {
            use aterm_spec::host::could_not_run;
            use aterm_spec::wait::{MUST_HAPPEN, until_within};

            // SAFETY: the first Xlib calls this process makes; the handler only counts.
            let (previous, dpy) = unsafe {
                (x.init_threads)();
                let previous = (x.set_error_handler)(Some(xlib::count_errors));
                (previous, (x.open_display)(std::ptr::null()))
            };
            if dpy.is_null() {
                // SAFETY: the handler Xlib had before.
                unsafe { (x.set_error_handler)(previous) };
                could_not_run(format!(
                    "$DISPLAY ({}) names no X server this process can open",
                    std::env::var("DISPLAY").unwrap_or_default()
                ));
            }
            let mut attributes = xlib::XSetWindowAttributes {
                override_redirect: 1,
                event_mask: xlib::STRUCTURE_NOTIFY_MASK | xlib::EXPOSURE_MASK,
                ..Default::default()
            };
            // SAFETY: a live display; a top-level window on its default screen with the root's
            // depth and visual, its attributes read from `attributes`, then mapped.
            let (screen, win) = unsafe {
                let screen = (x.default_screen)(dpy);
                let root = (x.root_window)(dpy, screen);
                let win = (x.create_window)(
                    dpy,
                    root,
                    0,
                    0,
                    w,
                    h,
                    0,
                    xlib::COPY_FROM_PARENT,
                    xlib::INPUT_OUTPUT,
                    std::ptr::null_mut(),
                    xlib::CW_BACK_PIXMAP | xlib::CW_OVERRIDE_REDIRECT | xlib::CW_EVENT_MASK,
                    &raw mut attributes,
                );
                (x.map_window)(dpy, win);
                (screen, win)
            };
            let desk = Desk {
                x,
                dpy,
                screen,
                win,
                previous,
            };
            until_within(MUST_HAPPEN, "the window's map", || {
                desk.drain().mapped.then_some(())
            });
            desk
        }

        fn window_system(&self) -> WindowSystem {
            WindowSystem::X11 {
                display: self.dpy as usize,
                screen: self.screen,
            }
        }

        fn native(&self) -> NativeWindow {
            NativeWindow::X11 {
                window: self.win,
                visual: None,
            }
        }

        fn device(&self) -> GlDevice {
            GlDevice::on_window_system(self.window_system()).unwrap_or_else(|e| {
                aterm_spec::host::could_not_run(format!("no GL device on $DISPLAY's X server: {e}"))
            })
        }

        fn sync(&self) {
            // SAFETY: a live display.
            unsafe { (self.x.sync)(self.dpy, 0) };
        }

        fn drain(&self) -> Seen {
            let mut seen = Seen::default();
            // SAFETY: a live display.
            while unsafe { (self.x.pending)(self.dpy) } > 0 {
                let mut ev = xlib::XEvent::zeroed();
                // SAFETY: a live display with an event queued; room for any event.
                unsafe { (self.x.next_event)(self.dpy, &raw mut ev) };
                seen.mapped |= ev.kind == xlib::MAP_NOTIFY;
                seen.exposed |= ev.kind == xlib::EXPOSE;
            }
            seen
        }

        /// Read the window's RGB pixel at (`px`, `py`) from the X server.
        fn pixel(&self, px: c_int, py: c_int) -> Option<[u8; 3]> {
            // SAFETY: a live display; the window is this display's.
            let img = unsafe {
                (self.x.sync)(self.dpy, 0);
                (self.x.get_image)(self.dpy, self.win, px, py, 1, 1, !0, xlib::Z_PIXMAP)
            };
            if img.is_null() {
                return None;
            }
            // SAFETY: a non-null image `XGetImage` returned, freed below.
            let i = unsafe { &*img };
            assert_eq!(
                i.bits_per_pixel, 32,
                "a TrueColor window's pixels are 32-bit"
            );
            // SAFETY: one 32-bit pixel at `data`.
            let raw: [u8; 4] = unsafe { std::slice::from_raw_parts(i.data, 4) }
                .try_into()
                .expect("four bytes");
            let v = if i.byte_order == xlib::LSB_FIRST {
                u32::from_le_bytes(raw)
            } else {
                u32::from_be_bytes(raw)
            };
            let channel = |mask: std::ffi::c_ulong| {
                let mask = u32::try_from(mask).expect("a 32-bit pixel's mask");
                u8::try_from((v & mask) >> mask.trailing_zeros()).expect("an 8-bit channel")
            };
            let rgb = [
                channel(i.red_mask),
                channel(i.green_mask),
                channel(i.blue_mask),
            ];
            // SAFETY: the image's data and the image, each freed once, as `XDestroyImage` frees them.
            unsafe {
                (self.x.free)(i.data.cast());
                (self.x.free)(img.cast());
            }
            Some(rgb)
        }
    }

    impl Drop for Desk<'_> {
        fn drop(&mut self) {
            // SAFETY: the window and the display this test opened, each closed once, then the handler Xlib had before.
            unsafe {
                (self.x.destroy_window)(self.dpy, self.win);
                (self.x.close_display)(self.dpy);
                (self.x.set_error_handler)(self.previous);
            }
        }
    }

    const CHILD: &str = "ATERM_GPU_GLX_CHILD";

    /// Run `body` as the test `name` in a child process and return its stdout after verifying `done`.
    fn in_child(name: &str, done: &str, body: fn()) -> Option<String> {
        use std::io::Read as _;

        use aterm_spec::wait::{MUST_HAPPEN, within};

        if std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty()) {
            eprintln!("SKIP: no DISPLAY");
            return None;
        }
        if std::env::var_os(CHILD).is_some() {
            body();
            return None;
        }
        let module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, path)| path);
        let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
        let out = scratch.path().join("out");
        let err = scratch.path().join("err");
        let mut child =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    &format!("{module}::{name}"),
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .stdout(std::fs::File::create(&out).expect("the child's stdout"))
                .stderr(std::fs::File::create(&err).expect("the child's stderr"))
                .spawn()
                .expect("the test binary runs again");
        let hang = 2 * MUST_HAPPEN;
        let status = within(hang, std::time::Duration::from_millis(20), || {
            child.try_wait().expect("the child's status")
        });
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let read = |path: &std::path::Path| {
            let mut text = String::new();
            let _ = std::fs::File::open(path).and_then(|mut f| f.read_to_string(&mut text));
            text
        };
        let (stdout, stderr) = (read(&out), read(&err));
        let said = format!("its stdout:\n{stdout}\nits stderr:\n{stderr}");
        let status = status.unwrap_or_else(|| panic!("the child still ran after {hang:?}; {said}"));
        if let Some(why) = stderr
            .lines()
            .find(|l| l.contains(aterm_spec::host::COULD_NOT_RUN))
        {
            aterm_spec::host::could_not_run(format!("the child: {why}"));
        }
        assert!(
            !stderr.contains("panicked at"),
            "the child panicked; {said}"
        );
        assert!(status.success(), "the child exited {status}; {said}");
        assert!(
            stdout.contains(done),
            "the child never said {done:?}; {said}"
        );
        for line in stdout.lines().filter_map(|l| l.find(done).map(|i| &l[i..])) {
            eprintln!("{line}");
        }
        Some(stdout)
    }

    const POSTED: &str = "GL-X11-POST: the window holds the posted colour, grown too";
    const FILL: u32 = 0x30_50_70;

    /// A frame posted to an X11 window becomes its `XGetImage` content, including across resizes.
    #[test]
    fn a_frame_posted_to_an_x11_window_is_its_content() {
        let Some(stdout) = in_child(
            "a_frame_posted_to_an_x11_window_is_its_content",
            POSTED,
            post_and_read_back,
        ) else {
            return;
        };
        for line in stdout
            .lines()
            .filter_map(|l| l.find("posting through").map(|i| &l[i..]))
        {
            eprintln!("{line}");
        }
    }

    fn post_and_read_back() {
        use std::sync::atomic::Ordering;

        use aterm_spec::wait::{MUST_HAPPEN, within};

        let x = xlib::Xlib::open();
        let (w, h) = (64_u32, 48_u32);
        let desk = Desk::open(&x, w, h);
        let dev = desk.device();
        println!(
            "posting through {} ({})",
            if dev.shared().glx().is_some() {
                "GLX"
            } else {
                "EGL"
            },
            dev.caps().renderer
        );
        let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()), None)
            .expect("a window surface");
        let name = std::ffi::CString::new(format!("_NET_WM_CM_S{}", desk.screen)).expect("a name");
        // SAFETY: a live display and a NUL-terminated name.
        let composited = unsafe {
            let selection = (x.intern_atom)(desk.dpy, name.as_ptr(), 0);
            (x.get_selection_owner)(desk.dpy, selection) != 0
        };
        if std::env::var_os("ATERM_GPU_PRESENT_MODE").is_none() {
            assert_eq!(
                surface.synced(),
                !composited,
                "a post waits for the refresh only where no compositing manager runs \
                 (composited: {composited})"
            );
        }
        let colour = [0.2_f32, 0.6, 1.0, 1.0];
        let want = [51_u8, 153, 255];
        {
            let standin = surface.standin(&dev).expect("the stand-in");
            let cur = dev.enter().expect("enter");
            let fbo = standin.fbo(&cur).expect("the stand-in's framebuffer");
            let gl = cur.gl();
            // SAFETY: the context is current; the stand-in's complete framebuffer; four floats for one colour attachment.
            unsafe {
                (gl.bind_framebuffer)(ffi::GL_FRAMEBUFFER, fbo);
                (gl.clear_bufferfv)(ffi::GL_COLOR, 0, colour.as_ptr());
            }
            cur.check("clear the stand-in").expect("no GL error");
        }
        surface.present(&dev, FILL, None).expect("the first post");
        let mut posts = 1_u32;
        let mut last = None;
        let held = within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
            if desk.drain().exposed {
                surface
                    .present(&dev, FILL, None)
                    .expect("the post on an expose");
                posts += 1;
            }
            last = desk.pixel(32, 24);
            (last == Some(want)).then_some(())
        });
        let errors = xlib::ERRORS.load(Ordering::Relaxed);
        assert!(
            held.is_some(),
            "the window read {last:?}, not {want:?}, {MUST_HAPPEN:?} after {posts} posts \
             ({errors} X errors)"
        );

        // SAFETY: a live display and its window.
        unsafe { (x.resize_window)(desk.dpy, desk.win, 2 * w, 2 * h) };
        surface
            .present(&dev, FILL, None)
            .expect("the post past the stand-in");
        posts += 1;
        let past = [(100, 10), (10, 70), (100, 70)];
        let mut read = Vec::new();
        let covered = within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
            if desk.drain().exposed {
                surface
                    .present(&dev, FILL, None)
                    .expect("the post on an expose");
                posts += 1;
            }
            read = past.iter().map(|&(px, py)| desk.pixel(px, py)).collect();
            read.iter().all(|p| *p == Some(want)).then_some(())
        });
        let errors = xlib::ERRORS.load(Ordering::Relaxed);
        assert!(
            covered.is_some(),
            "past the stand-in the grown window read {read:?} at {past:?}, not {want:?}, \
             {MUST_HAPPEN:?} after {posts} posts ({errors} X errors)"
        );

        surface.asked_size = Some((128, 96));
        // SAFETY: a live display and its window.
        unsafe { (x.resize_window)(desk.dpy, desk.win, 3 * w, 3 * h) };
        surface
            .present(&dev, FILL, None)
            .expect("the post past the answered size");
        posts += 1;
        let fill = FILL.to_be_bytes();
        let fill = [fill[1], fill[2], fill[3]];
        let points = [
            (10, 10, fill),
            (148, 134, fill),
            (148, 10, fill),
            (10, 134, want),
        ];
        let mut read = Vec::new();
        let filled = within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
            if desk.drain().exposed {
                surface
                    .present(&dev, FILL, None)
                    .expect("the post on an expose");
                posts += 1;
            }
            read = points
                .iter()
                .map(|&(px, py, _)| desk.pixel(px, py))
                .collect();
            read.iter()
                .zip(&points)
                .all(|(p, &(_, _, colour))| *p == Some(colour))
                .then_some(())
        });
        let errors = xlib::ERRORS.load(Ordering::Relaxed);
        assert!(
            filled.is_some(),
            "past an answered size that lags the buffer the window read {read:?}, not \
             {points:?}, {MUST_HAPPEN:?} after {posts} posts ({errors} X errors)"
        );

        surface.asked_size = None;
        surface.asked_once.set(Some((128, 96)));
        surface
            .present(&dev, FILL, None)
            .expect("the post whose answer lags once");
        posts += 1;
        let points = [(10, 10), (148, 134), (148, 10), (10, 134)];
        let mut read = Vec::new();
        let reposted = within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
            desk.drain();
            read = points.iter().map(|&(px, py)| desk.pixel(px, py)).collect();
            read.iter().all(|p| *p == Some(want)).then_some(())
        });
        let errors = xlib::ERRORS.load(Ordering::Relaxed);
        assert!(
            reposted.is_some(),
            "after an answer that lagged the swap the window read {read:?} at {points:?}, not \
             {want:?}, {MUST_HAPPEN:?} after {posts} posts ({errors} X errors)"
        );
        println!("{POSTED} ({posts} posts, {errors} X errors)");
    }

    const READ_BACK: &str =
        "GL-X11-READBACK: the presenter got the stand-in's words, top row first";

    type KeptFrame = (u32, u32, [u32; 4], Vec<u32>);

    /// Frame sink that records every posted frame over a copy of the previous frame.
    struct Kept {
        attach: Result<(), String>,
        frames: std::sync::Arc<std::sync::Mutex<Vec<KeptFrame>>>,
    }

    impl crate::FrameSink for Kept {
        fn attach(&mut self) -> Result<(), String> {
            self.attach.clone()
        }

        fn post(
            &mut self,
            width: u32,
            height: u32,
            damage: Option<[u32; 4]>,
            draw: &mut crate::FrameDraw<'_>,
        ) -> Result<(), String> {
            let mut frames = self.frames.lock().expect("the kept frames");
            let held = frames
                .last()
                .filter(|(w, h, ..)| (*w, *h) == (width, height))
                .map(|(.., words)| words.clone());
            let rect = damage
                .filter(|_| held.is_some())
                .unwrap_or([0, 0, width, height]);
            let mut words = held.unwrap_or_else(|| vec![0; width as usize * height as usize]);
            draw(&mut words, rect)?;
            frames.push((width, height, rect, words));
            Ok(())
        }
    }

    /// A window posted by readback delivers top-row-first `0x00RRGGBB` words and damage rectangles to its presenter.
    #[test]
    fn a_frame_posted_by_readback_is_the_stand_in_s_words() {
        in_child(
            "a_frame_posted_by_readback_is_the_stand_in_s_words",
            READ_BACK,
            read_back_post,
        );
    }

    fn read_back_post() {
        let x = xlib::Xlib::open();
        let (w, h) = (64_u32, 48_u32);
        let desk = Desk::open(&x, w, h);
        let dev = desk.device();
        let frames = std::sync::Arc::default();
        let kept = |attach| -> Option<Box<dyn crate::FrameSink>> {
            Some(Box::new(Kept {
                attach,
                frames: std::sync::Arc::clone(&frames),
            }))
        };
        let refused = GlWindowSurface::create(
            &dev,
            desk.native(),
            w,
            h,
            Box::new(()),
            kept(Err("no server to post to".to_owned())),
        )
        .expect("a window surface whose presenter refused");
        assert!(
            refused.glx_window().is_some(),
            "a presenter that cannot attach leaves the window its GLXWindow"
        );
        drop(refused);
        let mut surface =
            GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()), kept(Ok(())))
                .expect("a window surface posted by readback");
        assert!(
            surface.glx_window().is_none(),
            "a window posted by readback has no GLXWindow"
        );
        assert!(!surface.synced(), "a post by readback swaps nothing");
        let top = [0.2_f32, 0.6, 1.0, 1.0];
        let bottom = [1.0_f32, 0.4, 0.0, 1.0];
        let half = c_int::try_from(h / 2).expect("a small height");
        let width = c_int::try_from(w).expect("a small width");
        {
            let standin = surface.standin(&dev).expect("the stand-in");
            let cur = dev.enter().expect("enter");
            let fbo = standin.fbo(&cur).expect("the stand-in's framebuffer");
            let gl = cur.gl();
            // SAFETY: the context is current; the stand-in's complete framebuffer; four floats for one colour attachment.
            unsafe {
                (gl.bind_framebuffer)(ffi::GL_FRAMEBUFFER, fbo);
                (gl.enable)(ffi::GL_SCISSOR_TEST);
                (gl.scissor)(0, 0, width, half);
                (gl.clear_bufferfv)(ffi::GL_COLOR, 0, top.as_ptr());
                (gl.scissor)(0, half, width, half);
                (gl.clear_bufferfv)(ffi::GL_COLOR, 0, bottom.as_ptr());
                (gl.disable)(ffi::GL_SCISSOR_TEST);
            }
            cur.check("clear the stand-in").expect("no GL error");
        }
        surface
            .present(&dev, FILL, None)
            .expect("the post by readback");
        surface.read_rgba = true;
        surface
            .present(&dev, FILL, None)
            .expect("the post by readback read as GL_RGBA");
        {
            let standin = surface.standin(&dev).expect("the stand-in");
            let cur = dev.enter().expect("enter");
            let fbo = standin.fbo(&cur).expect("the stand-in's framebuffer");
            let gl = cur.gl();
            // SAFETY: as above, unscissored.
            unsafe {
                (gl.bind_framebuffer)(ffi::GL_FRAMEBUFFER, fbo);
                (gl.clear_bufferfv)(ffi::GL_COLOR, 0, [1.0_f32, 0.8, 0.0, 1.0].as_ptr());
            }
            cur.check("clear the stand-in").expect("no GL error");
        }
        let middle = [w / 4, h / 2, 3 * w / 4, h];
        let corner = [0, 0, w / 4, h / 2];
        surface.read_rgba = false;
        surface
            .present(&dev, FILL, Some(middle))
            .expect("a damaged post");
        surface.read_rgba = true;
        surface
            .present(&dev, FILL, Some(corner))
            .expect("a damaged post read as GL_RGBA");
        let frames = frames.lock().expect("the kept frames");
        assert_eq!(frames.len(), 4, "the presenter got one frame a post");
        let rgb = |word: &u32| word & 0x00ff_ffff;
        let at = |words: &[u32], x: u32, y: u32| rgb(&words[(y * w + x) as usize]);
        let (_, _, rect, words) = &frames[2];
        assert_eq!(*rect, middle, "the post drew the rectangle it was handed");
        assert_eq!(
            [
                at(words, w / 4, h / 2),
                at(words, 3 * w / 4 - 1, h - 1),
                at(words, w / 2, h / 2 + 1),
            ],
            [0x00ff_cc00; 3],
            "the damaged rectangle is the stand-in's"
        );
        assert_eq!(
            [
                at(words, 0, h - 1),
                at(words, 3 * w / 4, h / 2),
                at(words, w / 2, h / 2 - 1),
                at(words, 0, 0),
            ],
            [0x00ff_6600, 0x00ff_6600, 0x0033_99ff, 0x0033_99ff],
            "past the damage the frame is the last post's"
        );
        let (_, _, rect, words) = &frames[3];
        assert_eq!(*rect, corner, "the post drew the rectangle it was handed");
        assert_eq!(
            [at(words, 0, 0), at(words, w / 4 - 1, h / 2 - 1)],
            [0x00ff_cc00; 2],
            "the damaged corner is the stand-in's, swapped once"
        );
        assert_eq!(
            [
                at(words, w / 4, 0),
                at(words, w - 1, h / 2 - 1),
                at(words, w / 2, h - 1)
            ],
            [0x0033_99ff, 0x0033_99ff, 0x00ff_cc00],
            "the swap touched the corner alone"
        );
        for ((fw, fh, _, words), read) in frames.iter().take(2).zip([
            if dev.caps().bgra_read {
                "GL_BGRA_EXT"
            } else {
                "GL_RGBA, swapped (no GL_BGRA_EXT)"
            },
            "GL_RGBA, swapped",
        ]) {
            assert_eq!((*fw, *fh), (w, h), "the frame is the stand-in's size");
            assert_eq!(
                (words.first().map(rgb), words.last().map(rgb)),
                (Some(0x0033_99ff), Some(0x00ff_6600)),
                "the first word is the top half's colour and the last the bottom half's, as \
                 0x00RRGGBB (read through {read})"
            );
        }
        println!("{READ_BACK} (GL_BGRA_EXT: {})", dev.caps().bgra_read);
    }

    const ROBUST: &str = "GL-X11-ROBUST: each context's reset strategy is the one it was made with";

    /// A GLX context's `GL_RESET_NOTIFICATION_STRATEGY` matches the robustness it was created with.
    #[test]
    fn a_glx_context_reports_resets_where_it_was_made_to() {
        let _ = in_child(
            "a_glx_context_reports_resets_where_it_was_made_to",
            ROBUST,
            robust_contexts,
        );
    }

    fn robust_contexts() {
        use std::sync::atomic::Ordering;

        use aterm_spec::host::could_not_run;

        let x = xlib::Xlib::open();
        let desk = Desk::open(&x, 16, 16);
        let glx = Glx::get().unwrap_or_else(|e| could_not_run(format!("no GLX: {e}")));
        // SAFETY: a live display and one of its screens; the string is GLX's.
        let exts =
            unsafe { ffi::c_string((glx.fns.query_extensions_string)(desk.dpy, desk.screen)) };
        let offered = has_ext(&exts, "GLX_ARB_create_context_robustness");
        let own = GlxContext::new(desk.dpy, desk.screen)
            .unwrap_or_else(|e| could_not_run(format!("no GLX ES 3.0 context: {e}")));
        if !offered {
            assert_eq!(
                own.robustness(),
                Robustness::Plain,
                "a GLX without robustness makes a plain context"
            );
        }
        let strategy = reset_strategy(&own);
        assert_eq!(
            strategy == Some(ffi::GL_LOSE_CONTEXT_ON_RESET),
            own.robustness().reports_resets(),
            "the driver's strategy {strategy:x?} is the one {:?} asks for",
            own.robustness()
        );
        drop(own);
        let forced = GlxContext::offering(
            desk.dpy,
            desk.screen,
            "GLX_ARB_create_context_robustness GLX_NV_robustness_video_memory_purge",
        )
        .expect("a context, robust or plain, where the driver refuses the robust attributes");
        let forced_strategy = reset_strategy(&forced);
        assert_eq!(
            forced_strategy == Some(ffi::GL_LOSE_CONTEXT_ON_RESET),
            forced.robustness().reports_resets(),
            "the driver's strategy {forced_strategy:x?} is the one {:?} asks for",
            forced.robustness()
        );
        desk.sync();
        let errors = xlib::ERRORS.load(Ordering::Relaxed);
        println!(
            "{ROBUST} (GLX offers robustness: {offered}, the arm's context: {strategy:x?}; \
             offered by the test: {:?}, {forced_strategy:x?}; {errors} X errors)",
            forced.robustness()
        );
    }

    /// Query `ctx`'s `GL_RESET_NOTIFICATION_STRATEGY` on its pbuffer.
    fn reset_strategy(ctx: &GlxContext) -> Option<ffi::GLint> {
        // SAFETY: GLX's entry points for the two core GLES functions, whose registry signatures the casts name.
        let (get_error, get_integerv) = unsafe {
            (
                std::mem::transmute::<*const c_void, unsafe extern "C" fn() -> ffi::GLenum>(
                    ctx.proc_address(c"glGetError"),
                ),
                std::mem::transmute::<
                    *const c_void,
                    unsafe extern "C" fn(ffi::GLenum, *mut ffi::GLint),
                >(ctx.proc_address(c"glGetIntegerv")),
            )
        };
        ctx.make_current().expect("the context binds its pbuffer");
        let mut strategy = 0;
        // SAFETY: the context is current; a live out-pointer.
        let answered = unsafe {
            while get_error() != ffi::GL_NO_ERROR {}
            get_integerv(ffi::GL_RESET_NOTIFICATION_STRATEGY, &raw mut strategy);
            get_error() == ffi::GL_NO_ERROR
        };
        ctx.release();
        answered.then_some(strategy)
    }

    const NO_EGL: &str = "GL-X11-NO-EGL: a refused GLX mint makes no EGL device";

    /// A GLX mint failure on X11 falls back to the CPU renderer rather than EGL's X11 platform.
    #[test]
    fn a_glx_mint_failure_on_x11_draws_on_the_cpu_not_egl() {
        let _ = in_child(
            "a_glx_mint_failure_on_x11_draws_on_the_cpu_not_egl",
            NO_EGL,
            no_egl_after_glx,
        );
    }

    fn no_egl_after_glx() {
        let x = xlib::Xlib::open();
        let desk = Desk::open(&x, 16, 16);
        let ws = desk.window_system();
        let refused = GlDevice::on_window_system_via(ws, |_, _, _| {
            Err("gl: GLX refused (the test's refusal)".to_owned())
        })
        .expect_err("a refused GLX mint makes no device");
        assert!(
            refused.contains("the test's refusal") && refused.contains("CPU renderer"),
            "{refused}"
        );
        let glx = desk.device();
        assert!(
            glx.shared().glx().is_some(),
            "GLX's own mint makes a GLX device"
        );
        drop(glx);
        let egl = match GlDevice::on_egl_window_system(ws) {
            Ok(dev) => format!("would have served ({})", dev.caps().renderer),
            Err(e) => format!("refuses too ({e})"),
        };
        println!("{NO_EGL}; this server's EGL {egl}");
    }

    const DRILL: &str = "GL-X11-DRILL: the drill latched at the present's reset check";

    /// Injecting context loss on a GLX device latches the device lost at the next present's reset check.
    #[test]
    fn the_loss_drill_on_glx_latches_at_the_present_s_reset_check() {
        let _ = in_child(
            "the_loss_drill_on_glx_latches_at_the_present_s_reset_check",
            DRILL,
            drill_on_glx,
        );
    }

    fn drill_on_glx() {
        let x = xlib::Xlib::open();
        let (w, h) = (32, 24);
        let desk = Desk::open(&x, w, h);
        let own = desk.device();
        let robust = GlDevice::on_window_system_via(desk.window_system(), |d, s, ws| {
            let glx = GlxContext::offering(d as XDisplay, s, "GLX_ARB_create_context_robustness")?;
            GlDevice::on_glx_context(glx, ws)
        })
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no robust GLX device: {e}")));
        let mut said = Vec::new();
        for (dev, which) in [(own, "the arm's own"), (robust, "the robust")] {
            let glx = dev.shared().glx().expect("an X11 device is GLX's");
            let asks = dev.shared().asks_the_driver_for_resets();
            assert_eq!(
                asks,
                glx.robustness().reports_resets(),
                "{which} device asks the driver for resets exactly where its context reports them"
            );
            let mut surface =
                GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()), None)
                    .expect("a window surface");
            surface.standin(&dev).expect("the stand-in");
            for _ in 0..2 {
                surface
                    .present(&dev, FILL, None)
                    .expect("an undrilled present posts");
            }
            assert!(!dev.shared().lost(), "undrilled presents latch nothing");
            let what = dev.shared().inject_context_loss();
            assert!(what.contains("GL_UNKNOWN_CONTEXT_RESET"), "{what}");
            let err = surface
                .present(&dev, FILL, None)
                .expect_err("the drilled present fails")
                .to_string();
            assert!(
                err.contains("present: the context was reset (GL_UNKNOWN_CONTEXT_RESET)"),
                "{which}: {err}"
            );
            assert!(dev.shared().lost(), "the reset latched the device lost");
            assert_eq!(dev.shared().loss_reason(), Some(err.as_str()));
            let refused = dev
                .enter()
                .err()
                .expect("a lost device refuses every guard");
            assert!(refused.contains("GL_UNKNOWN_CONTEXT_RESET"), "{refused}");
            said.push(format!(
                "{which} ({:?}, asks the driver: {asks})",
                glx.robustness()
            ));
        }
        println!("{DRILL}: {}", said.join("; "));
    }

    const REFUSED: &str = "GL-X11-BIND: a window GLX keeps refusing latches the device lost";

    /// Three consecutive failed `glXMakeContextCurrent` window binds latch the device lost.
    #[test]
    fn a_window_glx_keeps_refusing_latches_the_device_lost() {
        let _ = in_child(
            "a_window_glx_keeps_refusing_latches_the_device_lost",
            REFUSED,
            refused_binds,
        );
    }

    fn refused_binds() {
        use std::sync::atomic::Ordering;

        let x = xlib::Xlib::open();
        let (w, h) = (32, 24);
        let desk = Desk::open(&x, w, h);
        let dev = desk.device();
        let glx = dev.shared().glx().expect("an X11 device is GLX's");
        let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()), None)
            .expect("a window surface");
        surface.standin(&dev).expect("the stand-in");
        surface
            .present(&dev, FILL, None)
            .expect("the window's first present posts");
        glx.destroy_window(surface.glx_window().expect("a GLX surface"));
        desk.sync();
        let mut seen = Vec::new();
        for n in 1..=3 {
            let posted = surface.present(&dev, FILL, None);
            seen.push((posted.is_ok(), dev.shared().lost()));
            if let Err(e) = posted {
                assert_eq!(
                    e.kind,
                    if n < 3 {
                        crate::SurfacePresentFailure::Unavailable
                    } else {
                        crate::SurfacePresentFailure::Validation
                    },
                    "only pre-threshold binds ask the frontend to retry"
                );
                println!("present: {e}");
            }
        }
        assert_eq!(
            seen,
            [(false, false), (false, false), (false, true)],
            "(posted, lost) at each present after the window's GLXWindow was destroyed"
        );
        let reason = dev.shared().loss_reason().unwrap_or_default();
        assert!(
            reason.contains("glXMakeContextCurrent failed on its window (3 in a row"),
            "{reason}"
        );
        desk.sync();
        let errors = xlib::ERRORS.load(Ordering::Relaxed);
        println!("{REFUSED} ({errors} X errors)");
    }

    /// What the child prints once a present whose window answered no size
    /// bound nothing and latched nothing.
    const UNANSWERED: &str =
        "GL-X11-UNANSWERED: a window that answers no size is not bound and latches nothing";

    /// The window under a surface that has posted is destroyed (by another
    /// client, as a crashed server's window goes): the next present's ask of
    /// its size goes unanswered (`BadDrawable`), so the present binds
    /// nothing, fails as an unavailable surface a retry may recover, and
    /// latches nothing. Binding it would have Mesa's software GLX (drisw)
    /// ask the same question after the swap, unchecked, and draw into a
    /// buffer of the uninitialized size it read: the Linux lane's display
    /// gone mid-resize, x86_64, 2026-10-22, all eight llvmpipe threads dead
    /// of SIGSEGV with the server killed under that ask. The control: the
    /// first present posts. It runs in a child ([`in_child`]).
    #[test]
    fn a_window_that_answers_no_size_is_not_bound() {
        let _ = in_child(
            "a_window_that_answers_no_size_is_not_bound",
            UNANSWERED,
            presents_with_no_size,
        );
    }

    /// The child's work.
    fn presents_with_no_size() {
        let x = xlib::Xlib::open();
        let (w, h) = (32, 24);
        let desk = Desk::open(&x, w, h);
        let dev = desk.device();
        let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()), None)
            .expect("a window surface");
        surface.standin(&dev).expect("the stand-in");
        surface
            .present(&dev, FILL, None)
            .expect("a present on the live window posts");
        // SAFETY: the window this test made, on its live display.
        unsafe { (x.destroy_window)(desk.dpy, desk.win) };
        desk.sync();
        let failed = surface
            .present(&dev, FILL, None)
            .expect_err("a present on a window that answers no size posted");
        assert_eq!(
            failed.kind,
            crate::SurfacePresentFailure::Unavailable,
            "the unanswered present's kind: {}",
            failed.detail
        );
        assert!(
            failed.detail.contains("answered no size"),
            "the unanswered present says why: {}",
            failed.detail
        );
        assert!(
            !dev.shared().lost(),
            "an unanswered size latches nothing: {:?}",
            dev.shared().loss_reason()
        );
        drop(surface);
        drop(dev);
        // The window is gone already: the desk's close alone.
        // SAFETY: the display this test opened, closed once.
        unsafe { (x.close_display)(desk.dpy) };
        std::mem::forget(desk);
        println!("{UNANSWERED}");
    }

    /// What the child prints once its presents on a hung-up connection all
    /// failed without a GLX call and the device latched lost.
    const HUNG_UP: &str =
        "GL-X11-GONE: a hung-up connection takes no GLX call and latches the device lost";

    /// The X server hangs up under a device that has posted (a crash or a
    /// restart): every present after fails, the first latching the device
    /// lost with the hang-up as the cause, and the process lives to say so.
    /// The hang-up is the test's `shutdown` of the connection's socket, both
    /// ways, as aterm-window's `x11/gone_tests.rs` makes it, under IO error
    /// handlers that return, as aterm-window installs them (Xlib's own end
    /// the process). The control: the first present, before the hang-up,
    /// posts. The negative control: with `hung_up` answering `false`, the
    /// first present after the hang-up reports a post (its swap is queued
    /// on the dead socket and nothing latches), and the GLX calls go on
    /// into a connection Xlib has given up on (the Linux lane's
    /// display-gone rows died of that on 2026-10-22, by SIGSEGV after Xlib's
    /// `request … would exceed buffer size` and by an abort in
    /// `glXQueryDrawable`'s unchecked reply). It runs in a child
    /// ([`in_child`]).
    #[test]
    fn a_hung_up_connection_takes_no_glx_call_and_latches_the_device_lost() {
        let _ = in_child(
            "a_hung_up_connection_takes_no_glx_call_and_latches_the_device_lost",
            HUNG_UP,
            presents_after_a_hang_up,
        );
    }

    /// The child's work.
    fn presents_after_a_hang_up() {
        let x = xlib::Xlib::open();
        let (w, h) = (32, 24);
        let desk = Desk::open(&x, w, h);
        let dev = desk.device();
        let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()), None)
            .expect("a window surface");
        surface.standin(&dev).expect("the stand-in");
        surface
            .present(&dev, FILL, None)
            .expect("a present on the live connection posts");
        assert!(!dev.shared().lost(), "a post latches nothing");
        // SAFETY: a live display; both handlers have Xlib's signatures and
        // touch nothing.
        let fd = unsafe {
            (x.set_io_error_handler)(Some(xlib::io_error_returns));
            (x.set_io_error_exit_handler)(
                desk.dpy,
                Some(xlib::io_error_exit_returns),
                std::ptr::null_mut(),
            );
            (x.connection_number)(desk.dpy)
        };
        // SAFETY: the display's own socket, shut both ways and left open
        // (Xlib still holds it).
        assert_eq!(
            unsafe { libc::shutdown(fd, libc::SHUT_RDWR) },
            0,
            "the hang-up"
        );
        for n in 1..=64 {
            let failed = surface.present(&dev, FILL, None);
            assert!(failed.is_err(), "present {n} after the hang-up posted");
        }
        assert!(dev.shared().lost(), "the hang-up latched the device lost");
        let why = dev.shared().loss_reason().unwrap_or_default().to_owned();
        assert!(why.contains(GONE), "the loss names the hang-up: {why}");
        // The surface's and the device's drops make no GLX call either; the
        // display is left to the process's end (its close would sync).
        drop(surface);
        drop(dev);
        std::mem::forget(desk);
        println!("{HUNG_UP}: {why}");
    }
}
