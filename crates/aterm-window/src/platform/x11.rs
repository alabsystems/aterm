// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/x11.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! LOCAL PATCH (aterm): upstream's `ActiveEventLoopExtX11`, `EventLoopExtX11`,
//! `EventLoopBuilderExtX11` (`with_x11`, `with_any_thread`), the `WindowExtX11` and
//! `WindowAttributesExtX11` settings (a visual, a screen, override-redirect,
//! `_NET_WM_WINDOW_TYPE`, a base size, an embedding parent), `MonitorHandleExtX11` and the Xlib
//! error hooks are gone: no aterm crate called them. The window's `WM_CLASS` and startup token
//! are set through `platform::wayland::WindowAttributesExtWayland`, whose attributes both
//! backends read. aterm adds the key event's server time ([`key_event_server_time`]), the newest
//! press's ([`user_time`]), the window's background ([`WindowAttributesExtX11`],
//! [`WindowExtX11`]), whether a redraw is the server's ([`redraw_is_exposure`]), the
//! application's word that a frame is on the window ([`WindowExtX11::x11_frame_presented`]),
//! which the window manager's resize sync request waits for, and its bell
//! ([`WindowExtX11::ring_x11_bell`]).

/// The X server timestamp (milliseconds) of the key event currently being dispatched.
pub fn key_event_server_time() -> Option<u32> {
    let stamped = KEY_EVENT_SERVER_TIME.load(std::sync::atomic::Ordering::Relaxed);
    (stamped != 0).then_some(stamped as u32)
}

/// The event's server `time` with bit 32 set (so a `0` stamp still reads as present)
/// while [`dispatch_with_key_event_server_time`] runs; 0 otherwise.
static KEY_EVENT_SERVER_TIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs one `KeyboardInput` dispatch with `time` published through [`key_event_server_time`].
#[doc(hidden)]
pub fn dispatch_with_key_event_server_time<R>(time: u32, dispatch: impl FnOnce() -> R) -> R {
    struct Withdraw;
    impl Drop for Withdraw {
        fn drop(&mut self) {
            KEY_EVENT_SERVER_TIME.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }
    KEY_EVENT_SERVER_TIME.store(u64::from(time) | (1 << 32), std::sync::atomic::Ordering::Relaxed);
    let _withdraw = Withdraw;
    dispatch()
}

/// The X server time of the newest key or button press the event loop delivered.
pub fn user_time() -> Option<u32> {
    let time = USER_TIME.load(std::sync::atomic::Ordering::Relaxed);
    (time != 0).then_some(time)
}

/// The newest press's server time for [`user_time`]; 0 before the first.
static USER_TIME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Notes a delivered press's server time for [`user_time`].
pub(crate) fn note_user_time(time: u32) {
    USER_TIME.store(time, std::sync::atomic::Ordering::Relaxed);
}

/// [`note_user_time`] for the application's tests.
#[doc(hidden)]
#[cfg(any(test, debug_assertions))]
pub fn note_user_time_for_test(time: u32) {
    note_user_time(time);
}

/// Whether the current `WindowEvent::RedrawRequested` dispatch is from an X server `Expose`.
pub fn redraw_is_exposure() -> bool {
    EXPOSURE.with(std::cell::Cell::get)
}

thread_local! {
    /// Set while [`dispatch_exposure`] runs.
    static EXPOSURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs one `RedrawRequested` dispatch with [`redraw_is_exposure`] `true`.
#[doc(hidden)]
pub fn dispatch_exposure<R>(dispatch: impl FnOnce() -> R) -> R {
    struct Withdraw;
    impl Drop for Withdraw {
        fn drop(&mut self) {
            EXPOSURE.with(|exposure| exposure.set(false));
        }
    }
    EXPOSURE.with(|exposure| exposure.set(true));
    let _withdraw = Withdraw;
    dispatch()
}

use crate::window::{Window, WindowAttributes};

/// Additional methods on [`WindowAttributes`] that are specific to X11.
pub trait WindowAttributesExtX11 {
    /// Build the window with a background of `rgb` (`0x00RRGGBB`) and `NorthWestGravity`.
    fn with_x11_background(self, rgb: u32) -> Self;
}

impl WindowAttributesExtX11 for WindowAttributes {
    #[inline]
    fn with_x11_background(mut self, rgb: u32) -> Self {
        self.platform_specific.x11_background = Some(rgb);
        self
    }
}

/// Additional methods on [`Window`] that are specific to X11.
pub trait WindowExtX11 {
    /// Change the window's X11 background to `rgb` (`0x00RRGGBB`).
    fn set_x11_background(&self, rgb: u32);

    /// Ring the bell for this window via XKB or core `Bell`.
    fn ring_x11_bell(&self);

    /// Notify the window manager's `_NET_WM_SYNC_REQUEST` that a frame was presented.
    fn x11_frame_presented(&self);
}

impl WindowExtX11 for Window {
    #[inline]
    fn set_x11_background(&self, rgb: u32) {
        match &self.window {
            crate::platform_impl::Window::X(window) => window.set_background(rgb),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(_) => {},
        }
    }

    #[inline]
    fn ring_x11_bell(&self) {
        match &self.window {
            crate::platform_impl::Window::X(window) => window.ring_bell(),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(_) => {},
        }
    }

    #[inline]
    fn x11_frame_presented(&self) {
        match &self.window {
            crate::platform_impl::Window::X(window) => window.frame_presented(),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(_) => {},
        }
    }
}
