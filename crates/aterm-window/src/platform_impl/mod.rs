// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (Cut the Android, iOS,
// Orbital and Web backends: no aterm cell compiles them.)

use crate::monitor::MonitorHandle as RootMonitorHandle;
#[cfg(windows_platform)]
use crate::monitor::VideoModeHandle as RootVideoModeHandle;
use crate::window::Fullscreen as RootFullscreen;

#[cfg(any(x11_platform, wayland_platform))]
mod linux;
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

/// Helper for converting between platform-specific and generic
/// [`VideoModeHandle`]/[`MonitorHandle`]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fullscreen {
    // Windows only (winit-core step 5, WC.8): see `crate::window::Fullscreen`.
    #[cfg(windows_platform)]
    Exclusive(VideoModeHandle),
    Borderless(Option<MonitorHandle>),
}

impl From<RootFullscreen> for Fullscreen {
    fn from(f: RootFullscreen) -> Self {
        match f {
            #[cfg(windows_platform)]
            RootFullscreen::Exclusive(mode) => Self::Exclusive(mode.video_mode),
            RootFullscreen::Borderless(Some(handle)) => Self::Borderless(Some(handle.inner)),
            RootFullscreen::Borderless(None) => Self::Borderless(None),
        }
    }
}

impl From<Fullscreen> for RootFullscreen {
    fn from(f: Fullscreen) -> Self {
        match f {
            #[cfg(windows_platform)]
            Fullscreen::Exclusive(video_mode) => {
                Self::Exclusive(RootVideoModeHandle { video_mode })
            },
            Fullscreen::Borderless(Some(inner)) => {
                Self::Borderless(Some(RootMonitorHandle { inner }))
            },
            Fullscreen::Borderless(None) => Self::Borderless(None),
        }
    }
}

#[cfg(all(not(windows_platform), not(macos_platform), not(x11_platform), not(wayland_platform),))]
compile_error!("The platform you're compiling for is not supported by winit");
