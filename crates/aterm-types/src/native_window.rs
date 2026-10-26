// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A window's X11, Wayland or Win32 handles ([`HasNativeWindow`]).

use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

/// Borrowed X11 or Wayland window handles; on Windows, a Win32 window's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWindow {
    /// X11 window on an Xlib connection.
    #[cfg(not(windows))]
    Xlib {
        /// Connection `Display *`.
        display: NonNull<c_void>,
        /// Screen number.
        screen: i32,
        /// Window XID.
        window: u32,
        /// Visual ID, or 0 when unknown.
        visual: u32,
    },
    /// Wayland window.
    #[cfg(not(windows))]
    Wayland {
        /// Connection `wl_display *`.
        display: NonNull<c_void>,
        /// Window `wl_surface *`.
        surface: NonNull<c_void>,
    },
    /// Win32 window.
    #[cfg(windows)]
    Win32 {
        /// The window's `HWND`.
        hwnd: NonNull<c_void>,
        /// The `HINSTANCE` of the module whose class the window has, or `None` when unknown.
        hinstance: Option<NonNull<c_void>>,
    },
}

/// A value that owns or borrows an X11, Wayland or Win32 window.
/// # Safety
/// [`native_window`](Self::native_window) answers `None` or handles that stay live for as long as
/// `self` does: the display connection stays open, and the X11 window, `wl_surface` or `HWND` is
/// not destroyed. A caller that keeps a handle past `self` keeps `self` alive with it.
pub unsafe trait HasNativeWindow {
    /// The window's handles, or `None` when unavailable.
    fn native_window(&self) -> Option<NativeWindow>;
}

// SAFETY: a borrow of `T` lives no longer than `T`, so `T`'s handles outlive it.
unsafe impl<T: HasNativeWindow + ?Sized> HasNativeWindow for &T {
    #[inline]
    fn native_window(&self) -> Option<NativeWindow> {
        (**self).native_window()
    }
}

// SAFETY: the box owns `T`; `T`'s handles live as long as the box does.
unsafe impl<T: HasNativeWindow + ?Sized> HasNativeWindow for Box<T> {
    #[inline]
    fn native_window(&self) -> Option<NativeWindow> {
        (**self).native_window()
    }
}

// SAFETY: each `Rc` keeps `T` alive, so `T`'s handles live as long as it does.
unsafe impl<T: HasNativeWindow + ?Sized> HasNativeWindow for Rc<T> {
    #[inline]
    fn native_window(&self) -> Option<NativeWindow> {
        (**self).native_window()
    }
}

// SAFETY: each `Arc` keeps `T` alive, so `T`'s handles live as long as it does.
unsafe impl<T: HasNativeWindow + ?Sized> HasNativeWindow for Arc<T> {
    #[inline]
    fn native_window(&self) -> Option<NativeWindow> {
        (**self).native_window()
    }
}
