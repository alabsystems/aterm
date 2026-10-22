// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's X11 binding: a device on an X11 display makes its OpenGL ES 3
//! context with GLX, and its windows post with `glXSwapBuffers`.
//!
//! EGL's X11 platform is a vendor library, and NVIDIA's
//! (`libnvidia-egl-xlib`, the driver's X11 EGL since 560) posts nothing on an
//! Xorg server the NVIDIA X driver runs: every `eglSwapBuffers` succeeds and
//! the window stays black. GLX on the same server posts (driver 580.159.03,
//! Xorg 21.1.11, 2026-10-21). GLX is the GL API every X server's driver
//! serves, so an X11 device opens a GLX OpenGL ES 3.0 context
//! (`GLX_EXT_create_context_es_profile`, which Mesa and NVIDIA both offer;
//! its `es2` forerunner promises only 2.0) and EGL stays the binding
//! everywhere else: Wayland and headless. An X server whose GLX makes no ES
//! 3.0 context gets no EGL device instead (its windows draw on the CPU
//! renderer, `GlDevice::on_window_system`): an EGL swap that posts nothing
//! still answers success, so the arm could not tell a working X11 EGL from
//! NVIDIA's. A GLX refusal also raises an X error, which the windowing
//! crate's handler logs and keeps out of its own error slot: a GLX request's
//! error is the GL arm's, which reads its failures from GLX's answers.
//!
//! * The context reports a GPU reset where GLX can make it so
//!   (`GLX_ARB_create_context_robustness`: robust access, and the context
//!   lost on a reset), and NVIDIA's purge of its video memory as one too
//!   (`GLX_NV_robustness_video_memory_purge`: a VT switch, a mode switch,
//!   suspend and resume can erase every texture). Without it a reset or a
//!   purge left the window drawing from erased atlases, or frozen, and never
//!   reached the frontend's CPU fallback. A driver that refuses the robust
//!   attributes gets the plain context (see [`context_attempts`]).
//! * The config is the screen's default visual's exactly-RGB888,
//!   double-buffered, single-sampled one: the visual every aterm window has
//!   (the windowing crate makes each window on its parent's visual), so every
//!   window's `GLXWindow` shares the context's config.
//! * Between posts the context is current on a 1x1 pbuffer, as an EGL
//!   context is on its pbuffer where the display has no surfaceless binding.
//! * The calls share the windowing crate's Xlib connection, which ran
//!   `XInitThreads` first, so the render thread may make them.

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

/// How a GLX context hears of a GPU reset: what it was made with, by what
/// GLX offered and the driver accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Robustness {
    /// No reset notification: GLX offers no
    /// `GLX_ARB_create_context_robustness`, or the driver refused it.
    Plain,
    /// Robust access, and the context lost on a reset, which
    /// `glGetGraphicsResetStatus` and `GL_CONTEXT_LOST` then report.
    LoseOnReset,
    /// As [`Self::LoseOnReset`], and a purge of the context's video memory
    /// reported as a reset (`GL_PURGED_CONTEXT_RESET_NV`).
    LoseOnResetOrPurge,
}

impl Robustness {
    /// Whether `glGetGraphicsResetStatus` can answer anything but
    /// `GL_NO_ERROR` on such a context.
    #[inline]
    pub(crate) const fn reports_resets(self) -> bool {
        !matches!(self, Self::Plain)
    }
}

/// A GLX OpenGL ES 3 context on an X11 display, the config its windows share
/// and the pbuffer it is current on between posts.
pub(crate) struct GlxContext {
    glx: &'static Glx,
    display: XDisplay,
    config: GLXFBConfig,
    /// The config's visual: the one a window must have to post here.
    visual: u32,
    context: GLXContext,
    robustness: Robustness,
    pbuffer: GLXDrawable,
    swap: SwapControl,
}

/// What the config choice reads of one config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConfigFacts {
    rgb: [c_int; 3],
    alpha: c_int,
    sample_buffers: c_int,
    visual: u32,
}

/// The config a context and its windows share: on `visual`, exactly RGB888
/// (the sizes `glXChooseFBConfig` matches are minimums, so 10-bit configs are
/// listed too, whose back buffer would rescale every byte the stand-in
/// carries), alpha 0 or 8, and single-sampled (GLES 3 refuses a blit into a
/// multisampled back buffer); the first such in GLX's order.
fn pick_config(configs: &[ConfigFacts], visual: u32) -> Option<usize> {
    configs.iter().position(|c| {
        c.visual == visual
            && c.rgb == [8, 8, 8]
            && matches!(c.alpha, 0 | 8)
            && c.sample_buffers == 0
    })
}

/// Whether the space-separated extension list `list` names `ext`.
fn has_ext(list: &str, ext: &str) -> bool {
    list.split_ascii_whitespace().any(|e| e == ext)
}

/// The OpenGL ES 3.0 context's attribute lists (each 0-terminated) to try in
/// order, with the robustness each makes: robust access and the context lost
/// on a reset where GLX offers `GLX_ARB_create_context_robustness`, first
/// with a video memory purge reported as a reset where it also offers
/// `GLX_NV_robustness_video_memory_purge`; the plain context last, which a
/// driver refusing the robust attributes (a robust ES profile is the
/// driver's to grant) still makes.
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
    /// A GLX OpenGL ES 3.0 context on `display` (an Xlib `Display*` the caller
    /// keeps open for the context's life) for windows on screen `screen`'s
    /// default visual, robust where GLX offers it ([`context_attempts`]).
    /// `Err` says why GLX cannot serve: no library, no ES context, no matching
    /// config, or a refused context or pbuffer.
    pub(crate) fn new(display: XDisplay, screen: c_int) -> Result<Self, String> {
        Self::offering(display, screen, "")
    }

    /// [`Self::new`], as if GLX's extension list also named `also` (a test
    /// offers the robustness extensions to a GLX that lists none, whose
    /// driver may grant or refuse the attributes).
    fn offering(display: XDisplay, screen: c_int, also: &str) -> Result<Self, String> {
        let glx = Glx::get()?;
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
        // SAFETY: GLX returned `n` configs at `list`; the configs outlive the
        // array, which is GLX's to free (`XFree`).
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
        // SAFETY: a live display and one of its screens; the visual is the
        // display's own.
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
        // SAFETY: `GLX_ARB_create_context` is offered (checked above), so the
        // name resolves to the function its registry signature declares.
        let create = unsafe {
            proc_address(glx, c"glXCreateContextAttribsARB")
                .map(|p| std::mem::transmute::<*const c_void, ffi::CreateContextAttribsFn>(p))
        }
        .ok_or("gl: GLX has no glXCreateContextAttribsARB")?;
        let mut refused = Vec::new();
        let mut made = None;
        for (robustness, attribs) in context_attempts(&exts) {
            // SAFETY: a config of this display, no share context, direct
            // rendering, a 0-terminated list.
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
            // GLX offered robustness and the driver refused some of it: the
            // device works, but what it was refused goes unheard. One note.
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
        // SAFETY: a pbuffer-capable config (`GLX_PBUFFER_BIT` above) and a
        // 0-terminated list.
        let pbuffer = unsafe { (fns.create_pbuffer)(display, config, pbuffer_attribs.as_ptr()) };
        if pbuffer == 0 {
            // SAFETY: the context made above, never current.
            unsafe { (fns.destroy_context)(display, context) };
            return Err("gl: GLX refused a 1x1 pbuffer".to_owned());
        }
        // SAFETY (each arm): the extension that defines the name is offered,
        // so it resolves to the function its alias type declares.
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
        Ok(Self {
            glx,
            display,
            config,
            visual,
            context,
            robustness,
            pbuffer,
            swap,
        })
    }

    /// How the context hears of a GPU reset.
    #[inline]
    pub(crate) const fn robustness(&self) -> Robustness {
        self.robustness
    }

    /// The GL (or GLX) function named `sym`, for the GL table; null when GLX
    /// has none.
    pub(crate) fn proc_address(&self, sym: &CStr) -> *const c_void {
        proc_address(self.glx, sym).unwrap_or(std::ptr::null())
    }

    /// Make the context current on its pbuffer (the binding between posts).
    pub(crate) fn make_current(&self) -> Result<(), String> {
        self.bind(self.pbuffer)
    }

    /// Make the context current on `window`, a `GLXWindow` of this context's
    /// ([`Self::create_window`]): the default framebuffer is then its back
    /// buffer.
    pub(crate) fn bind_window(&self, window: GLXDrawable) -> Result<(), String> {
        self.bind(window)
    }

    fn bind(&self, drawable: GLXDrawable) -> Result<(), String> {
        // SAFETY: a drawable made on this display with the context's config;
        // the device lock keeps every other thread off the context.
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
        // SAFETY: unbinding with no drawables and no context is always legal.
        unsafe {
            (self.glx.fns.make_context_current)(self.display, 0, 0, std::ptr::null_mut());
        }
    }

    /// A `GLXWindow` on the X11 window `window`, whose visual (`None`:
    /// unknown) must be the context's.
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
        // SAFETY: a live window of this display on the config's visual, and a
        // 0-terminated (empty) list.
        let drawable = unsafe {
            (self.glx.fns.create_window)(self.display, self.config, window, attribs.as_ptr())
        };
        if drawable == 0 {
            Err("gl: glXCreateWindow failed".to_owned())
        } else {
            Ok(drawable)
        }
    }

    /// Destroy a `GLXWindow` this context made, which no thread holds current.
    pub(crate) fn destroy_window(&self, window: GLXDrawable) {
        // SAFETY: as documented; the X11 window under it is still alive.
        unsafe { (self.glx.fns.destroy_window)(self.display, window) };
    }

    /// `window`'s width and height now (it follows the X11 window's size by
    /// itself).
    pub(crate) fn size(&self, window: GLXDrawable) -> (u32, u32) {
        let (mut w, mut h): (c_uint, c_uint) = (0, 0);
        // SAFETY (both): a drawable of this display and a live out-pointer.
        unsafe { (self.glx.fns.query_drawable)(self.display, window, ffi::GLX_WIDTH, &raw mut w) };
        unsafe { (self.glx.fns.query_drawable)(self.display, window, ffi::GLX_HEIGHT, &raw mut h) };
        (w, h)
    }

    /// Set `window`'s swap interval (0: post at once; 1: wait for the
    /// refresh). The window is current on the calling thread.
    pub(crate) fn swap_interval(&self, window: GLXDrawable, interval: i32) -> Result<(), String> {
        // SAFETY (each call): the function the offered extension defines;
        // the window is current, which the MESA and SGI forms act on.
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

    /// Post `window`'s back buffer. The window is current on the calling
    /// thread.
    pub(crate) fn swap(&self, window: GLXDrawable) {
        // SAFETY: a `GLXWindow` of this display, current here.
        unsafe { (self.glx.fns.swap_buffers)(self.display, window) };
    }
}

impl Drop for GlxContext {
    fn drop(&mut self) {
        // SAFETY: the device owning this context is gone, so no thread holds
        // it current; the pbuffer and the context were made on this display.
        unsafe {
            (self.glx.fns.destroy_pbuffer)(self.display, self.pbuffer);
            (self.glx.fns.destroy_context)(self.display, self.context);
        }
    }
}

/// Whether a compositing manager runs on `display`'s screen `screen` (it owns
/// the `_NET_WM_CM_S<screen>` selection, EWMH): it then composites every frame
/// at its own refresh. `false` where libX11 cannot be read.
pub(crate) fn composited(display: XDisplay, screen: c_int) -> bool {
    let Ok(glx) = Glx::get() else {
        return false;
    };
    let Ok(name) = std::ffi::CString::new(format!("_NET_WM_CM_S{screen}")) else {
        return false;
    };
    // SAFETY: a live display and a NUL-terminated name; `only_if_exists`
    // false, so the atom is made if no client named it yet.
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

    /// The shared config is the first exactly-RGB888, single-sampled one on
    /// the window's visual: a 10-bit config, a multisampled one and one on
    /// another visual are each passed over even when GLX lists them first,
    /// and a visual no config serves has none.
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

    /// The context's attribute lists, tried in order: where GLX offers
    /// `GLX_ARB_create_context_robustness`, the first asks for robust access
    /// (`GLX_CONTEXT_FLAGS_ARB` with the robust-access bit) and the context
    /// lost on a reset (`GLX_LOSE_CONTEXT_ON_RESET_ARB`), with a video memory
    /// purge reported as a reset ahead of it where GLX also offers
    /// `GLX_NV_robustness_video_memory_purge` (NVIDIA's libGLX_nvidia
    /// 580.159.03 offers both); the plain ES 3.0 list always comes last, and
    /// alone where GLX offers no robustness (Mesa's GLX on the private
    /// Xwayland test bed), or only the purge, which asks nothing without it.
    /// Every list is attribute pairs and a 0.
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

    /// The test's own Xlib: a window to post to and a pixel read back from it
    /// (`XGetImage`), which is what the X server holds as the window's
    /// content, the content a compositor shows.
    mod xlib {
        use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub(super) type Display = *mut c_void;
        pub(super) type ErrorHandler = unsafe extern "C" fn(Display, *mut c_void) -> c_int;

        crate::gl::ffi::fn_table! {
            /// The calls the test makes (Xlib.h).
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
            }
        }

        /// An `XEvent`: its type, then the union's room (24 longs).
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

        /// `XSetWindowAttributes` (Xlib.h); all zero is a `None` background.
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

        /// The head of `XImage` (Xlib.h), up to the colour masks.
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
                // SAFETY: `dlsym` on a live handle; the table declares each
                // function's Xlib.h signature.
                unsafe { Xlib::load(&mut |sym: &CStr| libc::dlsym(h, sym.as_ptr()).cast_const()) }
                    .expect("libX11 has the calls the test makes")
            }
        }

        /// The X errors the connection raised.
        pub(super) static ERRORS: AtomicUsize = AtomicUsize::new(0);

        /// An X error handler that counts the error and returns, as
        /// aterm-window's does (Xlib's default handler exits the process).
        pub(super) unsafe extern "C" fn count_errors(_: Display, _: *mut c_void) -> c_int {
            ERRORS.fetch_add(1, Ordering::Relaxed);
            0
        }
    }

    /// The child's display and window: both closed when it drops, after the
    /// surface and the device declared later, and the error handler it
    /// replaced put back.
    struct Desk<'x> {
        x: &'x xlib::Xlib,
        dpy: xlib::Display,
        screen: c_int,
        win: std::ffi::c_ulong,
        previous: Option<xlib::ErrorHandler>,
    }

    /// What a drain of the window's events saw.
    #[derive(Default)]
    struct Seen {
        mapped: bool,
        exposed: bool,
    }

    impl<'x> Desk<'x> {
        /// `$DISPLAY`'s X server, opened with [`xlib::count_errors`] as the
        /// process's error handler (the first Xlib calls the process makes),
        /// and a mapped `w` x `h` window on it: override-redirect (no window
        /// manager frames or moves it) with no background (the server never
        /// paints over a post). The wait for the map is a one-minute hang
        /// detector.
        fn open(x: &'x xlib::Xlib, w: u32, h: u32) -> Self {
            use aterm_spec::host::could_not_run;
            use aterm_spec::wait::{MUST_HAPPEN, until_within};

            // SAFETY: the first Xlib calls this process makes; the handler
            // only counts.
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
            // SAFETY: a live display; a top-level window on its default
            // screen with the root's depth and visual, its attributes read
            // from `attributes`, then mapped.
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

        /// The window system a device on this display opens on.
        fn window_system(&self) -> WindowSystem {
            WindowSystem::X11 {
                display: self.dpy as usize,
                screen: self.screen,
            }
        }

        /// The window, as a window surface is made on it.
        fn native(&self) -> NativeWindow {
            NativeWindow::X11 {
                window: self.win,
                visual: None,
            }
        }

        /// The GL arm's device on this display; a server it cannot open one
        /// on could not run the test.
        fn device(&self) -> GlDevice {
            GlDevice::on_window_system(self.window_system()).unwrap_or_else(|e| {
                aterm_spec::host::could_not_run(format!("no GL device on $DISPLAY's X server: {e}"))
            })
        }

        /// Wait for the server to answer every request sent so far.
        fn sync(&self) {
            // SAFETY: a live display.
            unsafe { (self.x.sync)(self.dpy, 0) };
        }

        /// Read every queued event.
        fn drain(&self) -> Seen {
            let mut seen = Seen::default();
            // SAFETY: a live display.
            while unsafe { (self.x.pending)(self.dpy) } > 0 {
                let mut ev = xlib::XEvent::zeroed();
                // SAFETY: a live display with an event queued; room for any
                // event.
                unsafe { (self.x.next_event)(self.dpy, &raw mut ev) };
                seen.mapped |= ev.kind == xlib::MAP_NOTIFY;
                seen.exposed |= ev.kind == xlib::EXPOSE;
            }
            seen
        }

        /// The window's pixel at (`px`, `py`) as the X server holds it, or
        /// `None` when the server answers no image.
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
            // SAFETY: the image's data and the image, each freed once, as
            // `XDestroyImage` frees them.
            unsafe {
                (self.x.free)(i.data.cast());
                (self.x.free)(img.cast());
            }
            Some(rgb)
        }
    }

    impl Drop for Desk<'_> {
        fn drop(&mut self) {
            // SAFETY: the window and the display this test opened, each closed
            // once, then the handler Xlib had before.
            unsafe {
                (self.x.destroy_window)(self.dpy, self.win);
                (self.x.close_display)(self.dpy);
                (self.x.set_error_handler)(self.previous);
            }
        }
    }

    /// Set in the child's environment: run the test's body.
    const CHILD: &str = "ATERM_GPU_GLX_CHILD";

    /// Run `body` as the test `name` in a child, and its stdout, which must
    /// hold `done`. Xlib's error and IO handlers are process-wide, so each X11
    /// test's work runs in a child: this test binary run again on that one
    /// test, with [`CHILD`] set (`None` there, once `body` ran). It runs where
    /// `$DISPLAY` names an X server; elsewhere it says SKIP (`None`). The
    /// child's own one-minute detectors run inside this one's, so a wait
    /// that never ends fails in the child, with what it read.
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
        // libtest prints the child's own `test … ` prefix on the same line.
        for line in stdout.lines().filter_map(|l| l.find(done).map(|i| &l[i..])) {
            eprintln!("{line}");
        }
        Some(stdout)
    }

    /// What the child prints once its window held the posted colour, grown
    /// too.
    const POSTED: &str = "GL-X11-POST: the window holds the posted colour, grown too";

    /// A frame the arm posts to an X11 window becomes that window's content:
    /// the X server answers `XGetImage` with the colour the stand-in held.
    /// On an Xorg server the NVIDIA X driver runs, the EGL binding this
    /// device replaces posted nothing (the negative control: with
    /// `on_window_system`'s GLX attempt removed the window reads black on
    /// such a desk, driver 580.159.03, 2026-10-21); elsewhere both bindings
    /// post, and the test holds the GLX device the arm picks to it.
    ///
    /// The surface's present mode is checked too: it waits for the refresh
    /// only where no compositing manager runs (`_NET_WM_CM_S<screen>`).
    ///
    /// Then the window grows behind the surface's back, as a window manager's
    /// resize does before the frontend hears of it, and the next post still
    /// covers all of it: the area past the stand-in reads the stand-in's edge
    /// colour. The negative control: without `present`'s edge stretch, that
    /// area reads [0, 0, 0] on this desk's NVIDIA GLX (2026-10-22), the back
    /// buffer's bytes.
    ///
    /// The arm posts once the window maps ([`Desk::open`]), and again on each
    /// `Expose`, as a terminal does; the wait for the colour is a one-minute
    /// hang detector. It runs in a child ([`in_child`]).
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

    /// The child's work: an override-redirect window on `$DISPLAY`, a device
    /// and a surface on it, a colour cleared into the stand-in and posted, and
    /// the window's pixel read until it is that colour; then the window
    /// doubled, posted again, and read past the stand-in.
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
        let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()))
            .expect("a window surface");
        // The present mode: synchronized only where no compositing manager
        // owns `_NET_WM_CM_S<screen>`, asked here on the test's own terms.
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
        // 0.2, 0.6, 1.0 in the stand-in's linear RGBA8: 51, 153, 255.
        let colour = [0.2_f32, 0.6, 1.0, 1.0];
        let want = [51_u8, 153, 255];
        {
            let standin = surface.standin(&dev).expect("the stand-in");
            let cur = dev.enter().expect("enter");
            let fbo = standin.fbo(&cur).expect("the stand-in's framebuffer");
            let gl = cur.gl();
            // SAFETY: the context is current; the stand-in's complete
            // framebuffer; four floats for one colour attachment.
            unsafe {
                (gl.bind_framebuffer)(ffi::GL_FRAMEBUFFER, fbo);
                (gl.clear_bufferfv)(ffi::GL_COLOR, 0, colour.as_ptr());
            }
            cur.check("clear the stand-in").expect("no GL error");
        }
        surface.present(&dev).expect("the first post");
        let mut posts = 1_u32;
        let mut last = None;
        let held = within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
            if desk.drain().exposed {
                surface.present(&dev).expect("the post on an expose");
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

        // The window doubles; the surface is not told. Points past the
        // stand-in's right edge, past its bottom edge, and past both.
        // SAFETY: a live display and its window.
        unsafe { (x.resize_window)(desk.dpy, desk.win, 2 * w, 2 * h) };
        surface.present(&dev).expect("the post past the stand-in");
        posts += 1;
        let past = [(100, 10), (10, 70), (100, 70)];
        let mut read = Vec::new();
        let covered = within(MUST_HAPPEN, std::time::Duration::from_millis(10), || {
            if desk.drain().exposed {
                surface.present(&dev).expect("the post on an expose");
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
        println!("{POSTED} ({posts} posts, {errors} X errors)");
    }

    /// What the child prints once each context's reset strategy matched the
    /// robustness it was made with.
    const ROBUST: &str = "GL-X11-ROBUST: each context's reset strategy is the one it was made with";

    /// A GLX context reports resets exactly where it was made to: the
    /// driver's own answer (`GL_RESET_NOTIFICATION_STRATEGY`) is
    /// `GL_LOSE_CONTEXT_ON_RESET` on a context the arm made robust, and not
    /// on a plain one. Where GLX offers no `GLX_ARB_create_context_robustness`
    /// the context is plain and the robust attributes are never sent (Mesa's
    /// GLX on the private Xwayland test bed, whose arm context answers
    /// `GL_NO_RESET_NOTIFICATION`). Offered both extensions by the test, the
    /// attributes are sent, and a refused attempt falls to the next: the
    /// test bed's llvmpipe refuses the purge attribute (one X error) and
    /// makes the `LoseOnReset` context, which answers
    /// `GL_LOSE_CONTEXT_ON_RESET` (the negative control: with only the first
    /// attempt tried, the forced mint fails there). NVIDIA's GLX offers both
    /// extensions, and the arm asks it for `LoseOnResetOrPurge` first. It
    /// runs in a child ([`in_child`]).
    #[test]
    fn a_glx_context_reports_resets_where_it_was_made_to() {
        let _ = in_child(
            "a_glx_context_reports_resets_where_it_was_made_to",
            ROBUST,
            robust_contexts,
        );
    }

    /// The child's work: the arm's own context and one offered robustness
    /// by the test, each held to the driver's reset strategy.
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

    /// `ctx`'s `GL_RESET_NOTIFICATION_STRATEGY`, as the driver answers it on
    /// its pbuffer; `None` where the context has no such query.
    fn reset_strategy(ctx: &GlxContext) -> Option<ffi::GLint> {
        // SAFETY: GLX's entry points for the two core GLES functions, whose
        // registry signatures the casts name.
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

    /// What the child prints once a refused GLX mint left the window system
    /// with no device.
    const NO_EGL: &str = "GL-X11-NO-EGL: a refused GLX mint makes no EGL device";

    /// A GLX mint failure on an X11 window system makes no device, which
    /// sends its windows to the CPU renderer, never to EGL's X11 platform:
    /// NVIDIA's answers every swap with success and posts nothing on an Xorg
    /// server its own driver runs (the black window of 2026-10-21). The test
    /// refuses the mint itself, and the refusal comes back naming the CPU
    /// renderer. The controls: GLX's own mint on the same display makes a
    /// GLX device; and this server's EGL does make a device (Mesa's on the
    /// private Xwayland test bed), so the fallback the arm had would have
    /// chosen it (the negative control: with `on_window_system_via` trying
    /// `on_egl_window_system` after the refusal, as it did, the test fails on
    /// the test bed). It runs in a child ([`in_child`]).
    #[test]
    fn a_glx_mint_failure_on_x11_draws_on_the_cpu_not_egl() {
        let _ = in_child(
            "a_glx_mint_failure_on_x11_draws_on_the_cpu_not_egl",
            NO_EGL,
            no_egl_after_glx,
        );
    }

    /// The child's work.
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

    /// What the child prints once the drill latched at a present.
    const DRILL: &str = "GL-X11-DRILL: the drill latched at the present's reset check";

    /// The loss drill on a GLX device (`ATERM_GL_INJECT_LOSS`) runs the path
    /// a real reset takes: its present binds the window through GLX, then
    /// reads the reset status, where the drill's `GL_UNKNOWN_CONTEXT_RESET`
    /// stands in for the driver's answer; the present fails naming it, the
    /// device latches lost with it as the cause (what the frontend's
    /// downgrade line prints), and refuses every later guard. Twice: on the
    /// arm's own device, and on one whose context the test made robust
    /// (`LoseOnReset` on the test bed's llvmpipe), whose reset check asks
    /// the driver's real `glGetGraphicsResetStatus` at every present. The
    /// control: each surface's undrilled presents post and latch nothing,
    /// the robust one's answered by the driver. The negative control: with
    /// `poll_reset` dropped from the GLX present, the drilled present posts
    /// and the device stays live. It runs in a child ([`in_child`]).
    #[test]
    fn the_loss_drill_on_glx_latches_at_the_present_s_reset_check() {
        let _ = in_child(
            "the_loss_drill_on_glx_latches_at_the_present_s_reset_check",
            DRILL,
            drill_on_glx,
        );
    }

    /// The child's work.
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
            let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()))
                .expect("a window surface");
            surface.standin(&dev).expect("the stand-in");
            for _ in 0..2 {
                surface.present(&dev).expect("an undrilled present posts");
            }
            assert!(!dev.shared().lost(), "undrilled presents latch nothing");
            let what = dev.shared().inject_context_loss();
            assert!(what.contains("GL_UNKNOWN_CONTEXT_RESET"), "{what}");
            let err = surface
                .present(&dev)
                .expect_err("the drilled present fails");
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

    /// What the child prints once a window GLX kept refusing latched.
    const REFUSED: &str = "GL-X11-BIND: a window GLX keeps refusing latches the device lost";

    /// A GLX window whose bind keeps failing latches the device lost at the
    /// third present in a row, and not before: the window's `GLXWindow` is
    /// destroyed behind the surface's back, so every later
    /// `glXMakeContextCurrent` on it fails while the context still binds its
    /// own pbuffer. The first two presents fail and latch nothing (a window
    /// in teardown can be refused once); the third latches, with the bind's
    /// failure as the cause. The control: the window's first present, before
    /// the destroy, posts. The negative control: before this rule a window
    /// GLX refused dropped every present and never latched (every present
    /// here fails, and the device stays live through all of them). It runs
    /// in a child ([`in_child`]).
    #[test]
    fn a_window_glx_keeps_refusing_latches_the_device_lost() {
        let _ = in_child(
            "a_window_glx_keeps_refusing_latches_the_device_lost",
            REFUSED,
            refused_binds,
        );
    }

    /// The child's work.
    fn refused_binds() {
        use std::sync::atomic::Ordering;

        let x = xlib::Xlib::open();
        let (w, h) = (32, 24);
        let desk = Desk::open(&x, w, h);
        let dev = desk.device();
        let glx = dev.shared().glx().expect("an X11 device is GLX's");
        let mut surface = GlWindowSurface::create(&dev, desk.native(), w, h, Box::new(()))
            .expect("a window surface");
        surface.standin(&dev).expect("the stand-in");
        surface
            .present(&dev)
            .expect("the window's first present posts");
        glx.destroy_window(surface.glx_window().expect("a GLX surface"));
        desk.sync();
        let mut seen = Vec::new();
        for _ in 0..3 {
            let posted = surface.present(&dev);
            seen.push((posted.is_ok(), dev.shared().lost()));
            if let Err(e) = posted {
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
}
