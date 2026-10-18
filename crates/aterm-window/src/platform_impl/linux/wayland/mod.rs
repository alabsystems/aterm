// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! Winit's Wayland backend.
//!
//! LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7 with winit-core
//! WC.8): the backend speaks through first-party `aterm-wayland` (its protocol tables, a runtime
//! over the system's libwayland-client, and its registry, output, seat, selection and cursor
//! helpers) and waits in `common::poll`, where upstream used smithay-client-toolkit,
//! wayland-client, wayland-backend, wayland-protocols and calloop. Every event the backend hears
//! is recorded in one ordered [`wire::Inbox`]: its own objects' by their listeners, and
//! aterm-wayland's seats', data devices' and selection sources' through the sinks the inbox
//! gives them. The loop applies the inbox to its state after each dispatch, in the order the
//! queue dispatched the events (the order the compositor sent them), as sctk's handlers ran
//! inside the dispatch.

use std::fmt::Display;
use std::sync::Arc;

use aterm_wayland::proxy::Proxy;

pub(super) use crate::cursor::OnlyCursorImage as CustomCursor;
use crate::dpi::{LogicalSize, PhysicalSize};
pub use crate::platform_impl::platform::{OsError, WindowId};
pub use clipboard::{WaylandClipboard, WaylandSelection};
pub use event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
pub use output::{MonitorHandle, VideoModeHandle};
pub use window::Window;

pub(crate) use state::Connection;

mod clipboard;
mod event_loop;
mod output;
mod seat;
mod state;
mod types;
mod window;
mod wire;

#[derive(Debug)]
pub enum WaylandError {
    /// The client library, the display or an object (aterm-wayland's error).
    Wire(aterm_wayland::proxy::Error),

    /// The compositor does not offer a global the backend cannot run without.
    Missing(&'static str),

    /// The event loop's own descriptors (its waker, its poll).
    Io(std::io::Error),
}

impl Display for WaylandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaylandError::Wire(error) => error.fmt(f),
            WaylandError::Missing(global) => write!(f, "the compositor offers no {global}"),
            WaylandError::Io(error) => error.fmt(f),
        }
    }
}

impl From<WaylandError> for OsError {
    fn from(value: WaylandError) -> Self {
        Self::WaylandError(Arc::new(value))
    }
}

impl From<aterm_wayland::proxy::Error> for OsError {
    fn from(value: aterm_wayland::proxy::Error) -> Self {
        WaylandError::Wire(value).into()
    }
}

/// Dummy device id, since Wayland doesn't have device events.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId;

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId
    }
}

/// Get the WindowId out of the surface: its `wl_proxy` address, as upstream's
/// `ObjectId::as_ptr` gave it.
#[inline]
fn make_wid(surface: Proxy) -> WindowId {
    WindowId(surface.as_ptr() as u64)
}

/// The default routine does floor, but we need round on Wayland.
fn logical_to_physical_rounded(size: LogicalSize<u32>, scale_factor: f64) -> PhysicalSize<u32> {
    let width = size.width as f64 * scale_factor;
    let height = size.height as f64 * scale_factor;
    (width.round(), height.round()).into()
}
