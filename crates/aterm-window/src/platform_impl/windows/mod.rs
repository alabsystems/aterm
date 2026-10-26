// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/mod.rs,
// modified by Andrew Yates for the aterm project in 2026 (see the repository NOTICE).

use crate::keyboard::SmolStr;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::WINDOW_LONG_PTR_INDEX;

pub(crate) use self::event_loop::{
    ActiveEventLoop, EventLoop, EventLoopProxy, PlatformSpecificEventLoopAttributes,
};
pub(crate) use self::icon::WinIcon;
pub(crate) use self::keyboard::scancode_to_physicalkey;
pub(crate) use self::monitor::MonitorHandle;
pub(crate) use self::window::Window;

pub use self::icon::WinIcon as PlatformIcon;
use crate::platform_impl::Fullscreen;

use crate::event::DeviceId as RootDeviceId;
use crate::icon::Icon;
use crate::keyboard::Key;

/// LOCAL PATCH (aterm): only what `WindowAttributesExtWindows` still sets. Upstream's owner window,
/// menu, drag-and-drop switch, taskbar skip, undecorated shadow, system backdrop, clip-children
/// switch, border, caption and title colours and corner preference had no aterm caller; their
/// defaults are folded into `window::init` (no owner or menu, an OLE drop target, on the taskbar,
/// `WS_CLIPCHILDREN`, and DWM's own backdrop, colours and corners).
#[derive(Clone, Debug)]
pub struct PlatformSpecificWindowAttributes {
    pub taskbar_icon: Option<Icon>,
    pub no_redirection_bitmap: bool,
    pub class_name: String,
}

impl Default for PlatformSpecificWindowAttributes {
    fn default() -> Self {
        Self {
            taskbar_icon: None,
            no_redirection_bitmap: false,
            class_name: "Window Class".to_string(),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(u32);

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId(0)
    }
}

// Constant device ID, to be removed when this backend is updated to report real device IDs.
//
// LOCAL PATCH (aterm): the only one: the raw-input devices that had their own ids reported device
// events, which no aterm crate reads, and the backend registers for raw input no more.
const DEVICE_ID: RootDeviceId = RootDeviceId(DeviceId(0));

pub type OsError = std::io::Error;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {
    pub text_with_all_modifiers: Option<SmolStr>,
    pub key_without_modifiers: Key,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(HWND);
unsafe impl Send for WindowId {}
unsafe impl Sync for WindowId {}

impl WindowId {
    pub const fn dummy() -> Self {
        WindowId(std::ptr::null_mut())
    }
}

impl From<WindowId> for u64 {
    fn from(window_id: WindowId) -> Self {
        handle::to_usize(window_id.0) as u64
    }
}

impl From<WindowId> for HWND {
    fn from(window_id: WindowId) -> Self {
        window_id.0
    }
}

impl From<u64> for WindowId {
    fn from(raw_id: u64) -> Self {
        Self(handle::from_usize(raw_id as usize))
    }
}

#[inline(always)]
const fn get_xbutton_wparam(x: u32) -> u16 {
    hiword(x)
}

#[inline(always)]
const fn get_x_lparam(x: u32) -> i16 {
    loword(x) as _
}

#[inline(always)]
const fn get_y_lparam(x: u32) -> i16 {
    hiword(x) as _
}

#[inline(always)]
pub(crate) const fn primarylangid(lgid: u16) -> u16 {
    lgid & 0x3ff
}

#[inline(always)]
pub(crate) const fn loword(x: u32) -> u16 {
    (x & 0xffff) as u16
}

#[inline(always)]
const fn hiword(x: u32) -> u16 {
    ((x >> 16) & 0xffff) as u16
}

#[inline(always)]
unsafe fn get_window_long(hwnd: HWND, nindex: WINDOW_LONG_PTR_INDEX) -> isize {
    #[cfg(target_pointer_width = "64")]
    return unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, nindex) };
    #[cfg(target_pointer_width = "32")]
    return unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongW(hwnd, nindex) as isize
    };
}

#[inline(always)]
unsafe fn set_window_long(hwnd: HWND, nindex: WINDOW_LONG_PTR_INDEX, dwnewlong: isize) -> isize {
    #[cfg(target_pointer_width = "64")]
    return unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(hwnd, nindex, dwnewlong)
    };
    #[cfg(target_pointer_width = "32")]
    return unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongW(hwnd, nindex, dwnewlong as i32)
            as isize
    };
}

#[macro_use]
mod util;
mod dark_mode;
mod definitions;
mod dpi;
mod drop_handler;
mod handle;

pub(crate) use self::handle::to_isize as handle_to_isize;
mod event_loop;
mod icon;
mod ime;
mod keyboard;
mod keyboard_layout;
mod monitor;
#[cfg(any(test, debug_assertions))]
pub mod recorded;
mod window;
mod window_state;
