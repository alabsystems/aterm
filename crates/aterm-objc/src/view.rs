// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`HasNsView`]: a window's AppKit content view, handed from crate to crate.
//!
//! The windowing crate (`crates/aterm-window`) owns every window's `NSView`.
//! `aterm-gpu`'s Metal presenter attaches its `CAMetalLayer` under that view's
//! layer and reads the screen the view sits on; `aterm-gui`'s chrome walks
//! from it to the `NSWindow`. Those crates used to meet at
//! `raw-window-handle`'s `RawWindowHandle::AppKit`, a third-party enum the
//! macOS build carried for this one pointer
//! (docs/AUDIT-third-party-2026-10-15.md, winit-core step 6). This crate is
//! the one all three already depend on there.

use std::rc::Rc;
use std::sync::Arc;

use crate::Id;

/// A value that owns or borrows an AppKit `NSView`: a window, or a reference
/// or shared pointer to one.
///
/// # Safety
///
/// [`ns_view`](Self::ns_view) answers [`Id::NIL`] or an `NSView` that stays
/// live for as long as `self` does. The answer is borrowed (+0): a caller that
/// keeps the view past `self` retains it. AppKit objects are main-thread-only,
/// so callers message the view on the main thread alone.
pub unsafe trait HasNsView {
    /// The view, or nil when there is none: a display-free window, or a read
    /// off the main thread.
    fn ns_view(&self) -> Id;
}

// SAFETY: a borrow of `T` lives no longer than `T`, so `T`'s view outlives it.
unsafe impl<T: HasNsView + ?Sized> HasNsView for &T {
    #[inline]
    fn ns_view(&self) -> Id {
        (**self).ns_view()
    }
}

// SAFETY: the box owns `T`; `T`'s view lives as long as the box does.
unsafe impl<T: HasNsView + ?Sized> HasNsView for Box<T> {
    #[inline]
    fn ns_view(&self) -> Id {
        (**self).ns_view()
    }
}

// SAFETY: each `Rc` keeps `T` alive, so `T`'s view lives as long as it does.
unsafe impl<T: HasNsView + ?Sized> HasNsView for Rc<T> {
    #[inline]
    fn ns_view(&self) -> Id {
        (**self).ns_view()
    }
}

// SAFETY: each `Arc` keeps `T` alive, so `T`'s view lives as long as it does.
unsafe impl<T: HasNsView + ?Sized> HasNsView for Arc<T> {
    #[inline]
    fn ns_view(&self) -> Id {
        (**self).ns_view()
    }
}
