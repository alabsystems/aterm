// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native-Wayland clipboard route: the [`winit::platform::wayland::WaylandClipboard`]
//! handle the (vendored, patched) winit event loop hands out, installed once at
//! launch and consulted by every Linux arm in [`crate::clipboard`] BEFORE the X11
//! backend. On a Wayland session the window's own seat is what the compositor
//! trusts to set a selection; the X11 backend only ever reached the clipboard
//! through XWayland's bridge, and on a session without XWayland it reached
//! nothing while the GUI reported success — data loss plus a lie (glass hunt,
//! m17-tower, 2026-09-01, #1; closed 2026-09-15).
//!
//! Absent on X11 and headless launches, where [`install`] is never called and
//! every arm falls through to the X11 backend exactly as before.

use std::sync::OnceLock;

pub(crate) use winit::platform::wayland::{WaylandClipboard, WaylandSelection};

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
