// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/cookie.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::ffi::c_int;
use std::sync::Arc;

use super::ffi::{self, XEvent, XGenericEventCookie};

use crate::platform_impl::x11::XConnection;

/// Wrapper that extracts and releases `XGenericEventCookie` data from a `GenericEvent`.
pub struct GenericEventCookie {
    cookie: XGenericEventCookie,
    xconn: Arc<XConnection>,
}

impl GenericEventCookie {
    pub fn from_event(xconn: Arc<XConnection>, event: XEvent) -> Option<GenericEventCookie> {
        unsafe {
            let mut cookie: XGenericEventCookie = event.generic_event_cookie;
            if (xconn.xlib.XGetEventData)(xconn.display, &mut cookie) == ffi::TRUE {
                Some(GenericEventCookie { cookie, xconn })
            } else {
                None
            }
        }
    }

    #[inline]
    pub fn extension(&self) -> u8 {
        self.cookie.extension as u8
    }

    #[inline]
    pub fn evtype(&self) -> c_int {
        self.cookie.evtype
    }

    /// Borrow inner event data as `&T`.
    ///
    /// ## SAFETY
    ///
    /// The caller must ensure that the event has the `T` inside of it.
    #[inline]
    pub unsafe fn as_event<T>(&self) -> &T {
        unsafe { &*(self.cookie.data as *const _) }
    }
}

impl Drop for GenericEventCookie {
    fn drop(&mut self) {
        unsafe {
            (self.xconn.xlib.XFreeEventData)(self.xconn.display, &mut self.cookie);
        }
    }
}
