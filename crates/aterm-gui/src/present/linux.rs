// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux CPU presenter: whichever display server the window is on. X11
//! windows draw through [`super::x11`] (MIT-SHM), Wayland windows through
//! [`super::wayland`] (`wl_shm`); both first-party since LW.6 of
//! docs/AUDIT-third-party-2026-10-15.md, which took softbuffer off Linux.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use aterm_types::native_window::{HasNativeWindow, NativeWindow};
use aterm_window::window::Window;

use super::wayland::{WaylandFrame, WaylandPresentError, WaylandPresenter};
use super::x11::{X11Frame, X11PresentError, X11Presenter};
use super::{CpuFrameBuffer, CpuPresenter, DamageRect};

/// One window's presenter, by display server.
pub(crate) enum LinuxPresenter {
    X11(X11Presenter),
    Wayland(WaylandPresenter),
}

/// Either presenter's failure.
#[derive(Debug)]
pub(crate) enum LinuxPresentError {
    X11(X11PresentError),
    Wayland(WaylandPresentError),
    /// Neither an X11 nor a Wayland window.
    Unsupported,
}

impl std::fmt::Display for LinuxPresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::X11(e) => write!(f, "{e}"),
            Self::Wayland(e) => write!(f, "{e}"),
            Self::Unsupported => f.write_str("neither an X11 nor a Wayland window"),
        }
    }
}

/// One acquired frame of either presenter.
pub(crate) enum LinuxFrame<'a> {
    X11(X11Frame<'a>),
    Wayland(WaylandFrame<'a>),
}

impl CpuPresenter for LinuxPresenter {
    type Error = LinuxPresentError;
    type Buffer<'a>
        = LinuxFrame<'a>
    where
        Self: 'a;

    fn new(window: Arc<Window>) -> Result<Self, Self::Error> {
        match window.native_window() {
            Some(NativeWindow::Xlib { .. }) => X11Presenter::new(window)
                .map(Self::X11)
                .map_err(LinuxPresentError::X11),
            Some(NativeWindow::Wayland { .. }) => WaylandPresenter::new(window)
                .map(Self::Wayland)
                .map_err(LinuxPresentError::Wayland),
            None => Err(LinuxPresentError::Unsupported),
        }
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        match self {
            Self::X11(p) => p.resize(width, height).map_err(LinuxPresentError::X11),
            Self::Wayland(p) => p.resize(width, height).map_err(LinuxPresentError::Wayland),
        }
    }

    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error> {
        match self {
            Self::X11(p) => p
                .buffer_mut()
                .map(LinuxFrame::X11)
                .map_err(LinuxPresentError::X11),
            Self::Wayland(p) => p
                .buffer_mut()
                .map(LinuxFrame::Wayland)
                .map_err(LinuxPresentError::Wayland),
        }
    }
}

impl Deref for LinuxFrame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        match self {
            Self::X11(f) => f,
            Self::Wayland(f) => f,
        }
    }
}

impl DerefMut for LinuxFrame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        match self {
            Self::X11(f) => f,
            Self::Wayland(f) => f,
        }
    }
}

impl CpuFrameBuffer for LinuxFrame<'_> {
    type Error = LinuxPresentError;

    fn age(&self) -> u8 {
        match self {
            Self::X11(f) => f.age(),
            Self::Wayland(f) => f.age(),
        }
    }

    fn present(self) -> Result<(), Self::Error> {
        match self {
            Self::X11(f) => f.present().map_err(LinuxPresentError::X11),
            Self::Wayland(f) => f.present().map_err(LinuxPresentError::Wayland),
        }
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        match self {
            Self::X11(f) => f
                .present_with_damage(damage)
                .map_err(LinuxPresentError::X11),
            Self::Wayland(f) => f
                .present_with_damage(damage)
                .map_err(LinuxPresentError::Wayland),
        }
    }
}
