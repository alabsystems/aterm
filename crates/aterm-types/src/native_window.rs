// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! [`HasNativeWindow`]: a window's X11 or Wayland handles, handed from crate to crate.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

/// A window's handles on its window system, borrowed from the window that owns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWindow {
    /// An X11 window, on the Xlib connection that made it.
    Xlib {
        /// The connection's `Display *`.
        display: NonNull<c_void>,
        /// The number of the screen the window was made on.
        screen: i32,
        /// The window's XID.
        window: u32,
        /// The window's visual id; 0 when it is not known.
        visual: u32,
    },
    /// A Wayland window.
    Wayland {
        /// The connection's `wl_display *`.
        display: NonNull<c_void>,
        /// The window's `wl_surface *`.
        surface: NonNull<c_void>,
    },
}

/// A value that owns or borrows an X11 or Wayland window: a window, or a reference or shared
/// pointer to one.
/// # Safety
/// [`native_window`](Self::native_window) answers `None` or handles that stay live for as long as
/// `self` does: the display connection stays open, and the X11 window or `wl_surface` is not
/// destroyed. A caller that keeps a handle past `self` keeps `self` alive with it.
pub unsafe trait HasNativeWindow {
    /// The window's handles, or `None` when there are none to hand out.
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
