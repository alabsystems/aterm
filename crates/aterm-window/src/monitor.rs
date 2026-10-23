// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/monitor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Types for querying monitor resolution, position, scale factor, and refresh rate.
use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::platform_impl;

/// Describes a fullscreen video mode of a monitor.
#[cfg(windows_platform)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct VideoModeHandle {
    pub(crate) video_mode: platform_impl::VideoModeHandle,
}

#[cfg(windows_platform)]
impl std::fmt::Debug for VideoModeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.video_mode.fmt(f)
    }
}

#[cfg(windows_platform)]
impl PartialOrd for VideoModeHandle {
    fn partial_cmp(&self, other: &VideoModeHandle) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(windows_platform)]
impl Ord for VideoModeHandle {
    fn cmp(&self, other: &VideoModeHandle) -> std::cmp::Ordering {
        self.monitor().cmp(&other.monitor()).then(
            self.size()
                .cmp(&other.size())
                .then(
                    self.refresh_rate_millihertz()
                        .cmp(&other.refresh_rate_millihertz())
                        .then(self.bit_depth().cmp(&other.bit_depth())),
                )
                .reverse(),
        )
    }
}

#[cfg(windows_platform)]
impl VideoModeHandle {
    /// Returns the resolution of this video mode.
    #[inline]
    pub fn size(&self) -> PhysicalSize<u32> {
        self.video_mode.size()
    }

    /// Returns the bit depth of this video mode.
    #[inline]
    pub fn bit_depth(&self) -> u16 {
        self.video_mode.bit_depth()
    }

    /// Returns the refresh rate of this video mode in mHz.
    #[inline]
    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.video_mode.refresh_rate_millihertz()
    }

    /// Returns the monitor that this video mode is valid for.
    #[inline]
    pub fn monitor(&self) -> MonitorHandle {
        MonitorHandle { inner: self.video_mode.monitor() }
    }
}

#[cfg(windows_platform)]
impl std::fmt::Display for VideoModeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}x{} @ {} mHz ({} bpp)",
            self.size().width,
            self.size().height,
            self.refresh_rate_millihertz(),
            self.bit_depth()
        )
    }
}

/// Handle to a monitor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonitorHandle {
    pub(crate) inner: platform_impl::MonitorHandle,
}

impl MonitorHandle {
    /// Returns a human-readable name of the monitor, or `None` if unavailable.
    #[cfg(any(macos_platform, windows_platform))]
    #[inline]
    pub fn name(&self) -> Option<String> {
        self.inner.name()
    }

    /// Returns the monitor's resolution.
    #[inline]
    pub fn size(&self) -> PhysicalSize<u32> {
        self.inner.size()
    }

    /// Returns the top-left corner position of the monitor in physical coordinates.
    #[inline]
    pub fn position(&self) -> PhysicalPosition<i32> {
        self.inner.position()
    }

    /// Returns the monitor refresh rate in millihertz, if available.
    #[inline]
    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        self.inner.refresh_rate_millihertz()
    }

    /// Returns the scale factor of the underlying monitor.
    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.inner.scale_factor()
    }

    /// Returns all fullscreen video modes supported by this monitor.
    #[cfg(windows_platform)]
    #[inline]
    pub fn video_modes(&self) -> impl Iterator<Item = VideoModeHandle> {
        self.inner.video_modes().map(|video_mode| VideoModeHandle { video_mode })
    }
}
