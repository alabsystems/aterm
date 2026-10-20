// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native-Wayland clipboard route: the [`aterm_window::platform::wayland::WaylandClipboard`] handle
//! the (vendored, patched) winit event loop hands out, installed once at launch and consulted by
//! every Linux arm in [`crate::clipboard`] before the X11 backend. On Wayland the window's own
//! seat is what the compositor trusts to set a selection; the X11 backend reaches the clipboard
//! only through XWayland, and without XWayland not at all.
//!
//! Absent on X11 and headless launches, where [`install`] is never called and every arm falls
//! through to the X11 backend.

use std::sync::OnceLock;

pub(crate) use aterm_window::platform::wayland::{WaylandClipboard, WaylandSelection};

static HANDLE: OnceLock<WaylandClipboard> = OnceLock::new();

/// Install the event loop's clipboard handle. First call wins; a launch has one
/// event loop, so a second call is a programming error worth a log line, not a
/// panic.
pub(crate) fn install(handle: WaylandClipboard) {
    if HANDLE.set(handle).is_err() {
        aterm_log::warn!("wayland clipboard: installed twice; keeping the first handle");
    }
}

/// The installed handle — `Some` only for a window running on Wayland.
pub(crate) fn handle() -> Option<&'static WaylandClipboard> {
    HANDLE.get()
}
