// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (Cut the Android, iOS,
//! Orbital and Web modules and the unused `run_on_demand`, `scancode` and `startup_notify`
//! extensions.)
//!
//! Contains traits with platform-specific methods in them.
//!
//! Only the modules corresponding to the platform you're compiling to will be available.

#[cfg(any(macos_platform, docsrs))]
pub mod macos;
#[cfg(any(wayland_platform, docsrs))]
pub mod wayland;
#[cfg(any(windows_platform, docsrs))]
pub mod windows;
#[cfg(any(x11_platform, docsrs))]
pub mod x11;

#[cfg(any(windows_platform, macos_platform, x11_platform, wayland_platform, docsrs))]
pub mod pump_events;

#[cfg(any(windows_platform, macos_platform, x11_platform, wayland_platform, docsrs))]
pub mod modifier_supplement;

#[cfg(any(free_unix, macos_platform))]
pub mod headless;
