// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/windows.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! # Windows
//!
//! LOCAL PATCH (aterm): only what aterm calls. Upstream's other window controls (`set_enable`,
//! `set_skip_taskbar`, the undecorated shadow, the system backdrop, the border, caption and title
//! colours, the corner preference), `MonitorHandleExtWindows::native_id`,
//! `DeviceIdExtWindows::persistent_identifier` and the icon loaders by path and by resource name
//! had no caller, and are gone with their types.

/// aterm: recorded Windows key messages, replayed through this backend's real key path, for the tests of the crates above it (`aterm-gui`'s byte-level layout tests).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub use crate::platform_impl::recorded;

use crate::dpi::PhysicalSize;
use crate::monitor::MonitorHandle;
use crate::window::{BadIcon, Icon, Window, WindowAttributes};

/// Monitor Handle type used by Win32 API
pub type HMONITOR = isize;

/// Additional methods on `Window` that are specific to Windows.
pub trait WindowExtWindows {
    /// This sets `ICON_BIG`.
    fn set_taskbar_icon(&self, taskbar_icon: Option<Icon>);
}

impl WindowExtWindows for Window {
    #[inline]
    fn set_taskbar_icon(&self, taskbar_icon: Option<Icon>) {
        self.window.set_taskbar_icon(taskbar_icon)
    }
}

/// Additional methods on `WindowAttributes` that are specific to Windows.
///
/// LOCAL PATCH (aterm): only the three aterm-gui calls. Upstream's owner window, menu,
/// drag-and-drop switch, taskbar skip, undecorated shadow, system backdrop, clip-children switch,
/// colours and corner preference had no caller, and their defaults are folded into the backend.
pub trait WindowAttributesExtWindows {
    /// This sets `ICON_BIG`.
    fn with_taskbar_icon(self, taskbar_icon: Option<Icon>) -> Self;

    /// This sets `WS_EX_NOREDIRECTIONBITMAP`.
    fn with_no_redirection_bitmap(self, flag: bool) -> Self;

    /// Customize the window class name.
    fn with_class_name<S: Into<String>>(self, class_name: S) -> Self;
}

impl WindowAttributesExtWindows for WindowAttributes {
    #[inline]
    fn with_taskbar_icon(mut self, taskbar_icon: Option<Icon>) -> Self {
        self.platform_specific.taskbar_icon = taskbar_icon;
        self
    }

    #[inline]
    fn with_no_redirection_bitmap(mut self, flag: bool) -> Self {
        self.platform_specific.no_redirection_bitmap = flag;
        self
    }

    #[inline]
    fn with_class_name<S: Into<String>>(mut self, class_name: S) -> Self {
        self.platform_specific.class_name = class_name.into();
        self
    }
}

/// Additional methods on `MonitorHandle` that are specific to Windows.
pub trait MonitorHandleExtWindows {
    /// Returns the handle of the monitor - `HMONITOR`.
    fn hmonitor(&self) -> HMONITOR;
}

impl MonitorHandleExtWindows for MonitorHandle {
    #[inline]
    fn hmonitor(&self) -> HMONITOR {
        crate::platform_impl::handle_to_isize(self.inner.hmonitor())
    }
}

/// Additional methods on `Icon` that are specific to Windows.
pub trait IconExtWindows: Sized {
    /// Create an icon from a resource embedded in this executable or library by its ordinal id.
    fn from_resource(ordinal: u16, size: Option<PhysicalSize<u32>>) -> Result<Self, BadIcon>;
}

impl IconExtWindows for Icon {
    fn from_resource(ordinal: u16, size: Option<PhysicalSize<u32>>) -> Result<Self, BadIcon> {
        let win_icon = crate::platform_impl::WinIcon::from_resource(ordinal, size)?;
        Ok(Icon { inner: win_icon })
    }
}
