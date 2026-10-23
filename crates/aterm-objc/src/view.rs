// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`HasNsView`] trait for borrowing an AppKit window content view.

use std::rc::Rc;
use std::sync::Arc;

use crate::Id;

/// A value that owns or borrows an AppKit `NSView`.
///
/// # Safety
/// [`ns_view`](Self::ns_view) returns [`Id::NIL`] or an `NSView` live for as long as `self`.
pub unsafe trait HasNsView {
    /// The view, or nil when there is none.
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
