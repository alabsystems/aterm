// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (The X11 types are
// first-party aterm-x11's, and the event loops answer no `AsFd`: the X11 loop waits in a
// poll(2) over two descriptors, not one epoll descriptor an embedder could watch, and no
// aterm crate embeds the loop: docs/AUDIT-third-party-2026-10-15.md, LW.7.)

#![cfg(free_unix)]

#[cfg(all(not(x11_platform), not(wayland_platform)))]
compile_error!("Please select a feature to build for unix: `x11`, `wayland`");

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use std::{env, fmt};
#[cfg(x11_platform)]
use std::{ffi::CStr, mem::MaybeUninit, os::raw::*, sync::Mutex};

use crate::keyboard::SmolStr;
#[cfg(x11_platform)]
use crate::utils::Lazy;

#[cfg(x11_platform)]
use self::x11::{X11Error, XConnection, XError, XNotSupported};
use crate::dpi::{PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{EventLoopError, ExternalError, NotSupportedError, OsError as RootOsError};
use crate::event_loop::{ActiveEventLoop as RootELW, ControlFlow, EventLoopClosed};
use crate::keyboard::Key;
use crate::platform::pump_events::PumpStatus;
use crate::window::{
    Cursor, ImePurpose, ResizeDirection, Theme, UserAttentionType, WindowAttributes,
};

pub(crate) use crate::icon::RgbaIcon as PlatformIcon;
pub(crate) use crate::platform_impl::Fullscreen;

pub(crate) mod common;
pub(crate) mod headless;
#[cfg(wayland_platform)]
pub(crate) mod wayland;
#[cfg(x11_platform)]
pub(crate) mod x11;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Backend {
    #[cfg(x11_platform)]
    X,
    #[cfg(wayland_platform)]
    Wayland,
    /// No display server at all — see [`headless`]. Only ever selected by an explicit
    /// `EventLoopBuilderExtHeadless::with_headless`, never by probing the environment:
    /// a windowed run that has lost its display must still FAIL rather than silently
    /// come up invisible.
    Headless,
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {
    /// Only ever `Some(Backend::Headless)` (`EventLoopBuilderExtHeadless::with_headless`):
    /// upstream's `with_x11` and `with_wayland`, and its `with_any_thread`, had no aterm caller
    /// (WC.8).
    pub(crate) forced_backend: Option<Backend>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationName {
    pub general: String,
    pub instance: String,
}

impl ApplicationName {
    // Only `WindowAttributesExtWayland::with_name` names the application (WC.8 cut the X11
    // extension's `with_name`), so an X11-only build leaves this out.
    #[cfg(wayland_platform)]
    pub fn new(general: String, instance: String) -> Self {
        Self { general, instance }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlatformSpecificWindowAttributes {
    pub name: Option<ApplicationName>,
    /// The launcher's startup-notification token (`DESKTOP_STARTUP_ID` on X11,
    /// `XDG_ACTIVATION_TOKEN` on Wayland): X11 names it on the window and ends the
    /// launch sequence with it once the window maps; Wayland activates the surface
    /// with it. An aterm addition back onto the fork's cut of upstream's
    /// `startup_notify` (only this consuming half).
    pub activation_token: Option<String>,
    /// The X11 window's background (`0x00RRGGBB`) and top-left gravity
    /// (`platform::x11::WindowAttributesExtX11`). An aterm addition.
    pub x11_background: Option<u32>,
}

#[cfg(x11_platform)]
pub(crate) static X11_BACKEND: Lazy<Mutex<Result<Arc<XConnection>, XNotSupported>>> =
    Lazy::new(|| Mutex::new(XConnection::new(Some(x_error_callback)).map(Arc::new)));

#[derive(Debug, Clone)]
pub enum OsError {
    Misc(&'static str),
    #[cfg(x11_platform)]
    XNotSupported(XNotSupported),
    #[cfg(x11_platform)]
    XError(Arc<X11Error>),
    #[cfg(wayland_platform)]
    WaylandError(Arc<wayland::WaylandError>),
}

impl fmt::Display for OsError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match *self {
            OsError::Misc(e) => _f.pad(e),
            #[cfg(x11_platform)]
            OsError::XNotSupported(ref e) => fmt::Display::fmt(e, _f),
            #[cfg(x11_platform)]
            OsError::XError(ref e) => fmt::Display::fmt(e, _f),
            #[cfg(wayland_platform)]
            OsError::WaylandError(ref e) => fmt::Display::fmt(e, _f),
        }
    }
}

pub(crate) enum Window {
    #[cfg(x11_platform)]
    X(x11::Window),
    #[cfg(wayland_platform)]
    Wayland(wayland::Window),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u64);

impl From<WindowId> for u64 {
    fn from(window_id: WindowId) -> Self {
        window_id.0
    }
}

impl From<u64> for WindowId {
    fn from(raw_id: u64) -> Self {
        Self(raw_id)
    }
}

impl WindowId {
    pub const fn dummy() -> Self {
        Self(0)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceId {
    #[cfg(x11_platform)]
    X(x11::DeviceId),
    #[cfg(wayland_platform)]
    Wayland(wayland::DeviceId),
}

impl DeviceId {
    pub const fn dummy() -> Self {
        #[cfg(wayland_platform)]
        return DeviceId::Wayland(wayland::DeviceId::dummy());
        #[cfg(all(not(wayland_platform), x11_platform))]
        return DeviceId::X(x11::DeviceId::dummy());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MonitorHandle {
    #[cfg(x11_platform)]
    X(x11::MonitorHandle),
    #[cfg(wayland_platform)]
    Wayland(wayland::MonitorHandle),
}

/// Dispatch over the three event-loop backends.
/// `any_backend!(match expr; Enum(foo) => foo.something())` expands to this
/// pseudocode, with X11 and Wayland arms included only when their backend is enabled:
/// ```text
/// match expr {
///    Enum::X(foo) => foo.something(),
///    Enum::Wayland(foo) => foo.something(),
///    Enum::Headless(foo) => foo.something(),
/// }
/// ```
/// The result can be converted to another enum by adding `; as AnotherEnum`.
/// `x11_or_wayland!` stays as it is for the
/// window/monitor/device types, which a headless run never produces and which therefore
/// have no headless variant to dispatch to; only the loop trio (`EventLoop`,
/// `EventLoopProxy`, `ActiveEventLoop`) gained one, and only these sites may use this.
macro_rules! any_backend {
    (match $what:expr; $enum:ident ( $($c1:tt)* ) => $x:expr; as $enum2:ident ) => {
        match $what {
            #[cfg(x11_platform)]
            $enum::X($($c1)*) => $enum2::X($x),
            #[cfg(wayland_platform)]
            $enum::Wayland($($c1)*) => $enum2::Wayland($x),
            $enum::Headless($($c1)*) => $enum2::Headless($x),
        }
    };
    (match $what:expr; $enum:ident ( $($c1:tt)* ) => $x:expr) => {
        match $what {
            #[cfg(x11_platform)]
            $enum::X($($c1)*) => $x,
            #[cfg(wayland_platform)]
            $enum::Wayland($($c1)*) => $x,
            $enum::Headless($($c1)*) => $x,
        }
    };
}

macro_rules! x11_or_wayland {
    (match $what:expr; $enum:ident ( $($c1:tt)* ) => $x:expr; as $enum2:ident ) => {
        match $what {
            #[cfg(x11_platform)]
            $enum::X($($c1)*) => $enum2::X($x),
            #[cfg(wayland_platform)]
            $enum::Wayland($($c1)*) => $enum2::Wayland($x),
        }
    };
    (match $what:expr; $enum:ident ( $($c1:tt)* ) => $x:expr) => {
        match $what {
            #[cfg(x11_platform)]
            $enum::X($($c1)*) => $x,
            #[cfg(wayland_platform)]
            $enum::Wayland($($c1)*) => $x,
        }
    };
}

impl MonitorHandle {
    #[inline]
    pub fn size(&self) -> PhysicalSize<u32> {
        x11_or_wayland!(match self; MonitorHandle(m) => m.size())
    }

    #[inline]
    pub fn position(&self) -> PhysicalPosition<i32> {
        x11_or_wayland!(match self; MonitorHandle(m) => m.position())
    }

    #[inline]
    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        x11_or_wayland!(match self; MonitorHandle(m) => m.refresh_rate_millihertz())
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        x11_or_wayland!(match self; MonitorHandle(m) => m.scale_factor() as _)
    }
}

impl Window {
    #[inline]
    pub(crate) fn new(
        window_target: &ActiveEventLoop,
        attribs: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        match *window_target {
            // A window needs a surface, and a surface needs a display. Refusing is the
            // whole contract of the headless backend: better a caller-visible error here
            // than a window handle that can never be shown.
            ActiveEventLoop::Headless(_) => Err(os_error!(OsError::Misc(
                "cannot create a window on a headless event loop: it has no display"
            ))),
            #[cfg(wayland_platform)]
            ActiveEventLoop::Wayland(ref window_target) => {
                wayland::Window::new(window_target, attribs).map(Window::Wayland)
            },
            #[cfg(x11_platform)]
            ActiveEventLoop::X(ref window_target) => {
                x11::Window::new(window_target, attribs).map(Window::X)
            },
        }
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static) {
        f(self)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R {
        f(self)
    }

    #[inline]
    pub fn id(&self) -> WindowId {
        x11_or_wayland!(match self; Window(w) => w.id())
    }

    #[inline]
    pub fn set_title(&self, title: &str) {
        x11_or_wayland!(match self; Window(w) => w.set_title(title));
    }

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        x11_or_wayland!(match self; Window(w) => w.set_visible(visible))
    }

    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        x11_or_wayland!(match self; Window(w) => w.outer_position())
    }

    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        x11_or_wayland!(match self; Window(w) => w.inner_position())
    }

    #[inline]
    pub fn set_outer_position(&self, position: Position) {
        x11_or_wayland!(match self; Window(w) => w.set_outer_position(position))
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        x11_or_wayland!(match self; Window(w) => w.inner_size())
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        x11_or_wayland!(match self; Window(w) => w.outer_size())
    }

    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        x11_or_wayland!(match self; Window(w) => w.request_inner_size(size))
    }

    #[inline]
    pub fn set_min_inner_size(&self, dimensions: Option<Size>) {
        x11_or_wayland!(match self; Window(w) => w.set_min_inner_size(dimensions))
    }

    #[inline]
    pub fn set_resize_increments(&self, increments: Option<Size>) {
        x11_or_wayland!(match self; Window(w) => w.set_resize_increments(increments))
    }

    #[inline]
    pub fn set_cursor(&self, cursor: Cursor) {
        x11_or_wayland!(match self; Window(w) => w.set_cursor(cursor))
    }

    #[inline]
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        x11_or_wayland!(match self; Window(window) => window.drag_window())
    }

    #[inline]
    pub fn drag_resize_window(&self, direction: ResizeDirection) -> Result<(), ExternalError> {
        x11_or_wayland!(match self; Window(window) => window.drag_resize_window(direction))
    }

    #[inline]
    pub fn show_window_menu(&self, position: Position) {
        x11_or_wayland!(match self; Window(w) => w.show_window_menu(position))
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        x11_or_wayland!(match self; Window(w) => w.scale_factor())
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        x11_or_wayland!(match self; Window(w) => w.set_maximized(maximized))
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        x11_or_wayland!(match self; Window(w) => w.is_maximized())
    }

    #[inline]
    pub fn is_tiled(&self) -> bool {
        x11_or_wayland!(match self; Window(w) => w.is_tiled())
    }

    #[inline]
    pub fn is_shown_fullscreen(&self) -> bool {
        x11_or_wayland!(match self; Window(w) => w.is_shown_fullscreen())
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        x11_or_wayland!(match self; Window(w) => w.set_minimized(minimized))
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        x11_or_wayland!(match self; Window(w) => w.is_minimized())
    }

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        x11_or_wayland!(match self; Window(w) => w.fullscreen())
    }

    #[inline]
    pub(crate) fn set_fullscreen(&self, monitor: Option<Fullscreen>) {
        x11_or_wayland!(match self; Window(w) => w.set_fullscreen(monitor))
    }

    #[inline]
    pub fn is_decorated(&self) -> bool {
        x11_or_wayland!(match self; Window(w) => w.is_decorated())
    }

    #[inline]
    pub fn set_ime_cursor_area(&self, position: Position, size: Size) {
        x11_or_wayland!(match self; Window(w) => w.set_ime_cursor_area(position, size))
    }

    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        x11_or_wayland!(match self; Window(w) => w.set_ime_allowed(allowed))
    }

    #[inline]
    pub fn set_ime_purpose(&self, purpose: ImePurpose) {
        x11_or_wayland!(match self; Window(w) => w.set_ime_purpose(purpose))
    }

    #[inline]
    pub fn focus_window(&self) {
        x11_or_wayland!(match self; Window(w) => w.focus_window())
    }

    #[inline]
    pub fn focus_window_with_activation_token(&self, token: &str) {
        x11_or_wayland!(match self; Window(w) => w.focus_window_with_activation_token(token))
    }

    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        x11_or_wayland!(match self; Window(w) => w.request_user_attention(request_type))
    }

    #[inline]
    pub fn request_redraw(&self) {
        x11_or_wayland!(match self; Window(w) => w.request_redraw())
    }

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        Some(x11_or_wayland!(match self; Window(w) => w.current_monitor()?; as MonitorHandle))
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        match self {
            #[cfg(x11_platform)]
            Window::X(ref window) => {
                window.available_monitors().into_iter().map(MonitorHandle::X).collect()
            },
            #[cfg(wayland_platform)]
            Window::Wayland(ref window) => {
                window.available_monitors().into_iter().map(MonitorHandle::Wayland).collect()
            },
        }
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        Some(x11_or_wayland!(match self; Window(w) => w.primary_monitor()?; as MonitorHandle))
    }

    /// LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 6,
    /// the Linux half): the window's X11 or Wayland handles, which `window::Window` hands out
    /// through first-party `aterm_types::native_window::HasNativeWindow`.
    #[inline]
    pub fn native_window(&self) -> aterm_types::native_window::NativeWindow {
        x11_or_wayland!(match self; Window(window) => window.native_window())
    }

    /// The opt-in `rwh_06` window handle: [`Self::native_window`]'s answer in raw-window-handle's
    /// words, so the two cannot name different windows.
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        use aterm_types::native_window::NativeWindow;
        Ok(match self.native_window() {
            NativeWindow::Xlib { window, visual, .. } => {
                let mut handle = rwh_06::XlibWindowHandle::new(window.into());
                handle.visual_id = visual.into();
                handle.into()
            },
            NativeWindow::Wayland { surface, .. } => {
                rwh_06::WaylandWindowHandle::new(surface).into()
            },
        })
    }

    /// The opt-in `rwh_06` display handle, off [`Self::native_window`] as above.
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        use aterm_types::native_window::NativeWindow;
        Ok(match self.native_window() {
            NativeWindow::Xlib { display, screen, .. } => {
                rwh_06::XlibDisplayHandle::new(Some(display), screen).into()
            },
            NativeWindow::Wayland { display, .. } => {
                rwh_06::WaylandDisplayHandle::new(display).into()
            },
        })
    }

    #[inline]
    pub fn set_theme(&self, theme: Option<Theme>) {
        x11_or_wayland!(match self; Window(window) => window.set_theme(theme))
    }

    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        x11_or_wayland!(match self; Window(window) => window.theme())
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        x11_or_wayland!(match self; Window(window) => window.has_focus())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {
    pub text_with_all_modifiers: Option<SmolStr>,
    pub key_without_modifiers: Key,
    /// The press's text with Caps Lock off
    /// (`KeyEventExtModifierSupplement::text_ignoring_lock`).
    pub text_ignoring_lock: Option<SmolStr>,
}

#[cfg(x11_platform)]
unsafe extern "C" fn x_error_callback(
    display: *mut x11::ffi::Display,
    event: *mut x11::ffi::XErrorEvent,
) -> c_int {
    let xconn_lock = X11_BACKEND.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(ref xconn) = *xconn_lock {
        // `assume_init` is safe here because the array consists of `MaybeUninit` values,
        // which do not require initialization.
        let mut buf: [MaybeUninit<c_char>; 1024] = unsafe { MaybeUninit::uninit().assume_init() };
        unsafe {
            (xconn.xlib.XGetErrorText)(
                display,
                (*event).error_code as c_int,
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
            )
        };
        let description =
            unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) }.to_string_lossy();

        let error = unsafe {
            XError {
                description: description.into_owned(),
                error_code: (*event).error_code,
                request_code: (*event).request_code,
                minor_code: (*event).minor_code,
            }
        };

        // A GLX request's error is the GL renderer's (`XConnection::glx_major`).
        if xconn.glx_major == Some(error.request_code) {
            tracing::warn!("X11 error of a GLX request: {:#?}", error);
            return 0;
        }
        tracing::error!("X11 error: {:#?}", error);
        *xconn.latest_error.lock().unwrap() = Some(error);
    }
    // Fun fact: this return value is completely ignored.
    0
}

#[allow(clippy::large_enum_variant)]
pub enum EventLoop<T: 'static> {
    #[cfg(wayland_platform)]
    Wayland(Box<wayland::EventLoop<T>>),
    #[cfg(x11_platform)]
    X(x11::EventLoop<T>),
    Headless(headless::EventLoop<T>),
}

pub enum EventLoopProxy<T: 'static> {
    #[cfg(x11_platform)]
    X(x11::EventLoopProxy<T>),
    #[cfg(wayland_platform)]
    Wayland(wayland::EventLoopProxy<T>),
    Headless(headless::EventLoopProxy<T>),
}

impl<T: 'static> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        any_backend!(match self; EventLoopProxy(proxy) => proxy.clone(); as EventLoopProxy)
    }
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new(
        attributes: &PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        if !is_main_thread() {
            panic!(
                "Initializing the event loop outside of the main thread is a significant \
                 cross-platform compatibility hazard."
            );
        }

        // NOTE: Wayland first because of X11 could be present under Wayland as well. Empty
        // variables are also treated as not set.
        let backend = match (
            attributes.forced_backend,
            env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|var| !var.is_empty())
                .or_else(|| env::var("WAYLAND_SOCKET").ok())
                .filter(|var| !var.is_empty())
                .is_some(),
            env::var("DISPLAY").map(|var| !var.is_empty()).unwrap_or(false),
        ) {
            // User is forcing a backend.
            (Some(backend), ..) => backend,
            // Wayland is present.
            #[cfg(wayland_platform)]
            (None, true, _) => Backend::Wayland,
            // X11 is present.
            #[cfg(x11_platform)]
            (None, _, true) => Backend::X,
            // No backend is present.
            (_, wayland_display, x11_display) => {
                let msg = if wayland_display && !cfg!(wayland_platform) {
                    "DISPLAY is not set; note: enable the `aterm-window/wayland` feature to support \
                     Wayland"
                } else if x11_display && !cfg!(x11_platform) {
                    "neither WAYLAND_DISPLAY nor WAYLAND_SOCKET is set; note: enable the \
                     `aterm-window/x11` feature to support X11"
                } else {
                    "neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set."
                };
                return Err(EventLoopError::Os(os_error!(OsError::Misc(msg))));
            },
        };

        // Create the display based on the backend.
        match backend {
            Backend::Headless => Ok(EventLoop::Headless(headless::EventLoop::new()?)),
            #[cfg(wayland_platform)]
            Backend::Wayland => EventLoop::new_wayland_any_thread(),
            #[cfg(x11_platform)]
            Backend::X => EventLoop::new_x11_any_thread(),
        }
    }

    #[cfg(wayland_platform)]
    fn new_wayland_any_thread() -> Result<EventLoop<T>, EventLoopError> {
        wayland::EventLoop::new().map(|evlp| EventLoop::Wayland(Box::new(evlp)))
    }

    #[cfg(x11_platform)]
    fn new_x11_any_thread() -> Result<EventLoop<T>, EventLoopError> {
        let xconn = match X11_BACKEND.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Ok(xconn) => xconn.clone(),
            Err(err) => {
                return Err(EventLoopError::Os(os_error!(OsError::XNotSupported(err.clone()))))
            },
        };

        x11::EventLoop::new(xconn).map(EventLoop::X)
    }

    /// The Wayland clipboard handle; `None` on any other backend.
    #[cfg(wayland_platform)]
    pub fn wayland_clipboard(&self) -> Option<wayland::WaylandClipboard> {
        match self {
            EventLoop::Wayland(evlp) => Some(evlp.clipboard()),
            _ => None,
        }
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        any_backend!(match self; EventLoop(evlp) => evlp.create_proxy(); as EventLoopProxy)
    }

    pub fn run<F>(mut self, callback: F) -> Result<(), EventLoopError>
    where
        F: FnMut(crate::event::Event<T>, &RootELW),
    {
        self.run_on_demand(callback)
    }

    pub fn run_on_demand<F>(&mut self, callback: F) -> Result<(), EventLoopError>
    where
        F: FnMut(crate::event::Event<T>, &RootELW),
    {
        any_backend!(match self; EventLoop(evlp) => evlp.run_on_demand(callback))
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, callback: F) -> PumpStatus
    where
        F: FnMut(crate::event::Event<T>, &RootELW),
    {
        any_backend!(match self; EventLoop(evlp) => evlp.pump_events(timeout, callback))
    }

    pub fn window_target(&self) -> &crate::event_loop::ActiveEventLoop {
        any_backend!(match self; EventLoop(evlp) => evlp.window_target())
    }
}

impl<T: 'static> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        any_backend!(match self; EventLoopProxy(proxy) => proxy.send_event(event))
    }
}

#[allow(clippy::large_enum_variant)]
pub enum ActiveEventLoop {
    #[cfg(wayland_platform)]
    Wayland(wayland::ActiveEventLoop),
    #[cfg(x11_platform)]
    X(x11::ActiveEventLoop),
    Headless(headless::ActiveEventLoop),
}

impl ActiveEventLoop {
    // Only `ActiveEventLoopExtWayland::is_wayland` asks (WC.8 cut the X11 extensions' `is_x11`,
    // its other callers), so an X11-only build leaves this out.
    #[cfg(wayland_platform)]
    #[inline]
    pub fn is_wayland(&self) -> bool {
        match *self {
            #[cfg(wayland_platform)]
            ActiveEventLoop::Wayland(_) => true,
            _ => false,
        }
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        match *self {
            ActiveEventLoop::Headless(ref evlp) => evlp.available_monitors(),
            #[cfg(wayland_platform)]
            ActiveEventLoop::Wayland(ref evlp) => {
                evlp.available_monitors().map(MonitorHandle::Wayland).collect()
            },
            #[cfg(x11_platform)]
            ActiveEventLoop::X(ref evlp) => {
                evlp.available_monitors().map(MonitorHandle::X).collect()
            },
        }
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        match self {
            // A headless run enumerates no monitors, so it has no primary one either.
            ActiveEventLoop::Headless(evlp) => evlp.primary_monitor(),
            #[cfg(x11_platform)]
            ActiveEventLoop::X(evlp) => Some(MonitorHandle::X(evlp.primary_monitor()?)),
            #[cfg(wayland_platform)]
            ActiveEventLoop::Wayland(evlp) => Some(MonitorHandle::Wayland(evlp.primary_monitor()?)),
        }
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        any_backend!(match self; Self(evlp) => evlp.raw_display_handle_rwh_06())
    }

    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        any_backend!(match self; Self(evlp) => evlp.set_control_flow(control_flow))
    }

    /// The Wayland loop's own reader (WC.8: the public getter is Windows-only).
    #[cfg(wayland_platform)]
    pub(crate) fn control_flow(&self) -> ControlFlow {
        any_backend!(match self; Self(evlp) => evlp.control_flow())
    }

    pub(crate) fn exit(&self) {
        any_backend!(match self; Self(evlp) => evlp.exit())
    }

    pub(crate) fn exiting(&self) -> bool {
        any_backend!(match self; Self(evlp) => evlp.exiting())
    }

    // Only the Wayland loop sets and reads the exit code through the dispatcher.
    #[cfg(wayland_platform)]
    fn set_exit_code(&self, code: i32) {
        any_backend!(match self; Self(evlp) => evlp.set_exit_code(code))
    }

    #[cfg(wayland_platform)]
    fn exit_code(&self) -> Option<i32> {
        any_backend!(match self; Self(evlp) => evlp.exit_code())
    }
}

/// Returns the minimum `Option<Duration>`, taking into account that `None`
/// equates to an infinite timeout, not a zero timeout (so can't just use
/// `Option::min`)
fn min_timeout(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    a.map_or(b, |a_timeout| b.map_or(Some(a_timeout), |b_timeout| Some(a_timeout.min(b_timeout))))
}

#[cfg(target_os = "linux")]
fn is_main_thread() -> bool {
    // SAFETY: getpid(2) cannot fail, and SYS_gettid takes no arguments and cannot fail;
    // glibc's own `gettid` wrapper exists only since 2.30. A process's main thread is the
    // one whose thread id is the process id.
    unsafe { libc::syscall(libc::SYS_gettid) == libc::c_long::from(libc::getpid()) }
}

#[cfg(any(target_os = "dragonfly", target_os = "freebsd", target_os = "openbsd"))]
fn is_main_thread() -> bool {
    use libc::pthread_main_np;

    unsafe { pthread_main_np() == 1 }
}

#[cfg(target_os = "netbsd")]
fn is_main_thread() -> bool {
    std::thread::current().name() == Some("main")
}
