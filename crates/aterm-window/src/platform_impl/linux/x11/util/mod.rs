// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::mem::{self, MaybeUninit};
use std::ops::BitAnd;
use std::os::raw::*;

mod client_msg;
pub mod cookie;
mod cursor;
mod geometry;
mod hint;
mod icon;
mod input;
pub mod keys;
pub(crate) mod memory;
mod randr;
mod window_property;
mod wm;
mod xmodmap;

pub(crate) use self::cursor::CursorFollow;
#[cfg(test)]
pub(crate) use self::cursor::CURSOR_LOADS;
pub use self::geometry::*;
pub use self::hint::*;
pub use self::input::*;
pub use self::window_property::*;
pub use self::wm::*;
pub use self::xmodmap::ModifierKeymap;

use super::atoms::*;
use super::{ffi, IgnoreError as _, VoidCookie, X11Error, XConnection, XError};
use aterm_x11::proto::xproto;

pub fn maybe_change<T: PartialEq>(field: &mut Option<T>, value: T) -> bool {
    let wrapped = Some(value);
    if *field != wrapped {
        *field = wrapped;
        true
    } else {
        false
    }
}

pub fn has_flag<T>(bitset: T, flag: T) -> bool
where
    T: Copy + PartialEq + BitAnd<T, Output = T>,
{
    bitset & flag == flag
}

impl XConnection {
    pub fn flush_requests(&self) -> Result<(), XError> {
        unsafe { (self.xlib.XFlush)(self.display) };
        self.check_errors()
    }

    pub fn sync_with_server(&self) -> Result<(), XError> {
        unsafe { (self.xlib.XSync)(self.display, ffi::FALSE) };
        self.check_errors()
    }
}
