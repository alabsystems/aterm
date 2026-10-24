// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Window X11 or Wayland handles ([`HasNativeWindow`]).

use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

/// Borrowed X11 or Wayland window handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWindow {
    /// X11 window on an Xlib connection.
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
    Wayland {
        /// Connection `wl_display *`.
        display: NonNull<c_void>,
        /// Window `wl_surface *`.
        surface: NonNull<c_void>,
    },
}

/// A value that owns or borrows an X11 or Wayland window.
/// # Safety
/// [`native_window`](Self::native_window) answers `None` or handles that stay live for as long as
/// `self` does: the display connection stays open, and the X11 window or `wl_surface` is not
/// destroyed. A caller that keeps a handle past `self` keeps `self` alive with it.
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
