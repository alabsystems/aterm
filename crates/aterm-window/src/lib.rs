// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/lib.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Cross-platform window creation and event loop management library.
#![cfg_attr(
    any(windows_platform, macos_platform, x11_platform, wayland_platform),
    doc = "[`EventLoopExtPumpEvents::pump_app_events()`][platform::pump_events::EventLoopExtPumpEvents::pump_app_events()]"
)]
#![cfg_attr(
    not(any(windows_platform, macos_platform, x11_platform, wayland_platform)),
    doc = "`EventLoopExtPumpEvents::pump_app_events()`"
)]
#![warn(rust_2018_idioms)]
#![warn(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg), doc(auto_cfg(hide(doc, docsrs))))]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::uninlined_format_args)]

#[cfg(feature = "rwh_06")]
pub use rwh_06 as raw_window_handle;

pub mod dpi;

pub mod application;
#[macro_use]
pub mod error;
mod cursor;
mod cursor_icon;
pub mod event;
pub mod event_loop;
/// The shadow margin around a client-decorated Wayland window (pixels and geometry; tested on
/// every host, shown only by the Wayland backend).
#[cfg(any(test, wayland_platform))]
#[cfg_attr(not(wayland_platform), allow(dead_code))]
mod frame_shadow;
#[cfg(not(macos_platform))]
mod icon;
pub mod keyboard;
pub mod monitor;
mod platform_impl;
mod utils;
pub mod window;

pub mod platform;
