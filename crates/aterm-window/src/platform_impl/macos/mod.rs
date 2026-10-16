// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.

#[macro_use]
mod util;

mod app;
mod app_state;
mod aterm_objc_seam;
mod cursor;
mod event;
mod event_handler;
mod event_loop;
mod ffi;
mod headless;
mod menu;
mod monitor;
mod observer;
mod view;
mod wake_queue;
mod window;
mod window_delegate;

use std::fmt;

pub(crate) use self::app_state::set_quit_confirm_hook;
pub(crate) use self::event::KeyEventExtra;
pub(crate) use self::event_loop::{
    ActiveEventLoop, EventLoop, EventLoopProxy, PlatformSpecificEventLoopAttributes,
};
pub(crate) use self::monitor::MonitorHandle;
pub(crate) use self::window::WindowId;
pub(crate) use self::window_delegate::PlatformSpecificWindowAttributes;
use crate::event::DeviceId as RootDeviceId;

pub(crate) use self::window::Window;
pub(crate) use crate::platform_impl::Fullscreen;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId;

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId
    }
}

// Constant device ID; to be removed when if backend is updated to report real device IDs.
pub(crate) const DEVICE_ID: RootDeviceId = RootDeviceId(DeviceId);

#[derive(Debug)]
pub enum OsError {
    CreationError(&'static str),
}

impl fmt::Display for OsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OsError::CreationError(e) => f.pad(e),
        }
    }
}
