// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party X11 for aterm: the wire codec, a libxcb transport, ICCCM properties and the resource
//! database.

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
