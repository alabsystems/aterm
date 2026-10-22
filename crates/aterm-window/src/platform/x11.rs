// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/x11.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! # X11
//!
//! LOCAL PATCH (aterm), 2026-10-21 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 8,
//! WC.8): what is left of upstream's X11 extensions is the key event's server time, which
//! aterm-gui reads. Upstream's `ActiveEventLoopExtX11`, `EventLoopExtX11`,
//! `EventLoopBuilderExtX11` (`with_x11`, `with_any_thread`), `WindowExtX11`,
//! `WindowAttributesExtX11` (a visual, a screen, override-redirect, `_NET_WM_WINDOW_TYPE`, a
//! base size, an embedding parent), `MonitorHandleExtX11` and the Xlib error hooks had no
//! caller in any aterm crate, and left with the backend code only they reached. The window's
//! `WM_CLASS` and startup token are set through `platform::wayland::WindowAttributesExtWayland`,
//! whose attributes both backends read. aterm adds the window's background
//! ([`WindowAttributesExtX11`], [`WindowExtX11`]), whether a redraw is the server's
//! ([`redraw_is_exposure`]), and the application's word that a frame is on the window
//! ([`WindowExtX11::x11_frame_presented`]), which the window manager's resize sync request
//! waits for.

/// The X server timestamp (milliseconds on the server's clock) of the key event whose
/// `WindowEvent::KeyboardInput` is being dispatched right now.
///
/// `Some` only from inside that callback, so a caller can never read the stamp of an
/// earlier key. The server stamps the event when it processes the device's input, so
/// the difference to the current server time is how long the key took to reach the
/// application: the input-method round trip (`XFilterEvent` forwards every key to the
/// XIM server first) plus the time the event waited while the event loop was busy.
/// Whether the server's clock is comparable to a local one is the caller's question;
/// on a local Xorg it is `CLOCK_MONOTONIC` in milliseconds.
pub fn key_event_server_time() -> Option<u32> {
    let stamped = KEY_EVENT_SERVER_TIME.load(std::sync::atomic::Ordering::Relaxed);
    (stamped != 0).then_some(stamped as u32)
}

/// The event's server `time` with bit 32 set (so a `0` stamp still reads as present)
/// while [`dispatch_with_key_event_server_time`] runs; 0 otherwise.
static KEY_EVENT_SERVER_TIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs one non-synthetic `KeyboardInput` dispatch with `time` published through
/// [`key_event_server_time`], and withdraws it when `dispatch` returns or unwinds.
/// Public only so the application's tests can drive the seam (this crate's own
/// tests cannot run inside the application's workspace).
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

/// The X server time of the newest key or button press the event loop delivered; `None` before
/// the first. A program launched for that press carries it as its startup id
/// (`DESKTOP_STARTUP_ID=_TIME<t>`): GTK presents a window with the id's time
/// (`gtk_window_set_startup_id`), and Firefox's remote hand-off gives the running browser the
/// id, so mutter's focus-stealing prevention, which refuses an activation stamped before the
/// newest press it knows of, lets that window come forward instead of marking it "is ready".
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

/// [`note_user_time`] for the application's tests, which cannot deliver an X press.
#[doc(hidden)]
#[cfg(any(test, debug_assertions))]
pub fn note_user_time_for_test(time: u32) {
    note_user_time(time);
}

/// Whether the `WindowEvent::RedrawRequested` being dispatched right now on this thread is one
/// the X server asked for with an `Expose`: the server holds nothing of what the window last
/// drew in part of it (the window was mapped again, a VT switch, a driver's video-memory purge,
/// a compositor's unredirection), so the redraw owes the whole window though nothing in it
/// changed. The redraws the application asks for (`Window::request_redraw`) and the server's
/// are one event per window and loop turn; this is `true` when the server's is among them.
/// `false` outside that callback and on Wayland.
pub fn redraw_is_exposure() -> bool {
    EXPOSURE.with(std::cell::Cell::get)
}

thread_local! {
    /// Set while [`dispatch_exposure`] runs.
    static EXPOSURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs one `RedrawRequested` dispatch with [`redraw_is_exposure`] `true`, and withdraws it
/// when `dispatch` returns or unwinds. Public only so the application's tests can drive the
/// seam.
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
    /// Build the window with a background of `rgb` (`0x00RRGGBB`) and its contents kept at
    /// its top-left corner when it is resized (`NorthWestGravity`). The X server paints any
    /// area the window gains in `rgb`, from its first map until the application first draws
    /// and after every resize until the application draws the new size. Without one, a
    /// composited window shows whatever its new pixmap held, which can be other windows'
    /// pixels. No effect on Wayland, or on a screen whose default visual has a colormap.
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
    /// Change the background [`WindowAttributesExtX11::with_x11_background`] gave the window
    /// to `rgb` (`0x00RRGGBB`), for the areas the X server paints from now on; what is on
    /// screen stays. No effect on Wayland.
    fn set_x11_background(&self, rgb: u32);

    /// The application has presented a frame of the window (its swap or its image put returned).
    /// Call it after every present. A window manager that resizes the window with EWMH's
    /// `_NET_WM_SYNC_REQUEST` (mutter) keeps showing the window's last frame until the window
    /// answers, and a request that a resize changed the window's size is answered by the first
    /// present after it; the event loop answers one that no frame is coming for at the end of
    /// its turn. A no-op while nothing waits, and on Wayland, whose frame callbacks are the
    /// compositor's own pacing.
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
    fn x11_frame_presented(&self) {
        match &self.window {
            crate::platform_impl::Window::X(window) => window.frame_presented(),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(_) => {},
        }
    }
}
