// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party X11 for aterm: the wire codec, a libxcb transport, ICCCM
//! properties and the resource database.
//!
//! It replaces x11rb, x11rb-protocol, as-raw-xcb-connection, tiny-xlib and
//! gethostname on Linux (docs/AUDIT-third-party-2026-10-15.md,
//! linux-windowing LW.6; the ruling's §3 item 5 scopes it). The C libraries
//! stay: wgpu's Vulkan WSI and aterm's EGL surfaces take a real
//! `xcb_connection_t *` / `Display *`, so the transport is libxcb's own,
//! opened at run time through `aterm-dl`, and on a window's connection it is
//! the one under Xlib's `Display`.
//!
//! * [`proto`]: requests encoded to bytes, replies and events parsed from
//!   them, with no connection: core, MIT-SHM, the setup. Pure, so it is
//!   held on any host to x11rb-protocol 0.13.2's bytes, recorded once by
//!   `tests/recorder` (a crate outside the workspace) into `tests/recorded/`.
//! * `conn` (Linux): [`Connection`] over libxcb, raw requests, checked
//!   errors, descriptor passing (MIT-SHM AttachFd), events.
//! * [`icccm`]: `WM_CLASS`, `WM_SIZE_HINTS`, `WM_HINTS`.
//! * [`xrm`]: the resource database (`Xft.dpi` from `RESOURCE_MANAGER`).
//! * `xlib` (Linux): the Xlib, XKB, XIM, XInput 2 and Xcursor declarations
//!   the window system's X11 backend calls, opened at run time, their struct
//!   layouts held to the C headers on each architecture.
//!
//! Every parser reads bytes another process wrote and returns an error
//! rather than panic.

#[cfg(target_os = "linux")]
mod conn;
pub mod icccm;
pub mod proto;
#[cfg(target_os = "linux")]
mod sys;
#[cfg(target_os = "linux")]
pub mod xlib;
pub mod xrm;

#[cfg(target_os = "linux")]
pub use conn::{Connection, Cookie, Error, VoidCookie};
