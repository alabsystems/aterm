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
//! whose attributes both backends read.

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
