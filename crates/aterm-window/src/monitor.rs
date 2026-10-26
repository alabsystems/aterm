// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/monitor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Types for querying monitor resolution, position, scale factor, and refresh rate.
use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::platform_impl;

// LOCAL PATCH (aterm): no video modes: they served only exclusive fullscreen, which no aterm crate
// asks for.
/// Handle to a monitor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonitorHandle {
    pub(crate) inner: platform_impl::MonitorHandle,
}

impl MonitorHandle {
    /// Returns a human-readable name of the monitor, or `None` if unavailable.
    #[cfg(macos_platform)]
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
}
