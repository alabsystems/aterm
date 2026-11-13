// Copyright The winit contributors
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (Cut the Android, iOS,
// Orbital and Web backends: no aterm cell compiles them.)

use crate::monitor::MonitorHandle as RootMonitorHandle;
use crate::window::Fullscreen as RootFullscreen;

#[cfg(any(test, wayland_platform))]
mod clipboard_reply;

#[cfg(any(test, wayland_platform))]
mod clipboard_transfer;

#[cfg(any(x11_platform, wayland_platform))]
mod linux;
// Exercise the actual Linux attribute formatter on other test hosts without a display server.
#[cfg(all(test, not(any(x11_platform, wayland_platform))))]
#[path = "linux/attributes.rs"]
mod linux_attributes_tests;
#[cfg(macos_platform)]
mod macos;
#[cfg(windows_platform)]
mod windows;

#[cfg(any(x11_platform, wayland_platform))]
use linux as platform;
#[cfg(macos_platform)]
use macos as platform;
#[cfg(windows_platform)]
use windows as platform;

pub use self::platform::*;

/// Helper for converting between platform-specific and generic [`MonitorHandle`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fullscreen {
    Borderless(Option<MonitorHandle>),
}

impl From<RootFullscreen> for Fullscreen {
    fn from(f: RootFullscreen) -> Self {
        match f {
            RootFullscreen::Borderless(Some(handle)) => Self::Borderless(Some(handle.inner)),
            RootFullscreen::Borderless(None) => Self::Borderless(None),
        }
    }
}

impl From<Fullscreen> for RootFullscreen {
    fn from(f: Fullscreen) -> Self {
        match f {
            Fullscreen::Borderless(Some(inner)) => {
                Self::Borderless(Some(RootMonitorHandle { inner }))
            },
            Fullscreen::Borderless(None) => Self::Borderless(None),
        }
    }
}

/// LOCAL PATCH (aterm): the units of a `cap`-unit output buffer an OS call reports writing
/// (`UCKeyTranslate`, `ToUnicodeEx`): `len`, or the whole buffer where the call reports more than
/// it was given room for, so a slice by it stays inside the buffer.
#[cfg(any(macos_platform, windows_platform, test))]
pub(crate) const fn reported_len(len: usize, cap: usize) -> usize {
    if len < cap {
        len
    } else {
        cap
    }
}

#[cfg(test)]
#[path = "mod_reported_len_tests.rs"]
mod reported_len_tests;

#[cfg(all(not(windows_platform), not(macos_platform), not(x11_platform), not(wayland_platform),))]
compile_error!("The platform you're compiling for is not supported by winit");
