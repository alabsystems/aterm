// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU presentation seam for the fail-soft software renderer.

use std::num::NonZeroU32;
use std::ops::DerefMut;
use std::sync::Arc;

use aterm_window::window::Window;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod mac;
#[cfg(test)]
pub(crate) mod scripted;
#[cfg(target_os = "linux")]
mod shm_mem;
#[cfg(target_os = "linux")]
pub(crate) mod wayland;
#[cfg(windows)]
pub(crate) mod win32;
#[cfg(target_os = "linux")]
pub(crate) mod x11;

/// CPU presentation backend for macOS (CoreGraphics).
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) type CpuSurface = mac::MacCpuPresenter;
/// CPU presentation backend for Linux (X11 or Wayland).
#[cfg(all(target_os = "linux", not(test)))]
pub(crate) type CpuSurface = linux::LinuxPresenter;
/// CPU presentation backend for Windows (GDI).
#[cfg(all(windows, not(test)))]
pub(crate) type CpuSurface = win32::Win32Presenter;
/// CPU presentation backend for test builds.
#[cfg(test)]
pub(crate) type CpuSurface = scripted::TestCpuSurface;

/// Non-empty damaged region of a presented frame in surface pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DamageRect {
    /// Left edge in surface pixels.
    pub x: u32,
    /// Top edge in surface pixels.
    pub y: u32,
    /// Non-zero width in surface pixels.
    pub width: NonZeroU32,
    /// Non-zero height in surface pixels.
    pub height: NonZeroU32,
}

/// Per-window CPU presentation target that owns the platform surface and hands out frame buffers.
pub(crate) trait CpuPresenter: Sized {
    /// Backend-native presentation error.
    type Error: std::fmt::Display;

    /// Acquired paintable frame buffer for one presentation pass.
    type Buffer<'a>: CpuFrameBuffer<Error = Self::Error>
    where
        Self: 'a;

    /// Attach a CPU presentation target to `window`.
    fn new(window: Arc<Window>) -> Result<Self, Self::Error>;

    /// Resize the backing store to `width` by `height` physical pixels.
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error>;

    /// Acquire the next paintable frame buffer.
    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error>;

    /// Whether the window system requires a repaint on the next presentation pass.
    fn owes_repaint(&self) -> bool {
        false
    }
}

/// Acquired CPU frame buffer of `0x00RRGGBB` pixel words.
pub(crate) trait CpuFrameBuffer: DerefMut<Target = [u32]> + Sized {
    /// Backend-native presentation error.
    type Error;

    /// Presents since this buffer's contents were on screen: only `1` (the previous present) lets
    /// the caller copy just the dirty rows, and `0` (none) forces the full copy.
    fn age(&self) -> u8;

    /// Commit the entire frame buffer.
    fn present(self) -> Result<(), Self::Error>;

    /// Commit the frame buffer for the given `damage` rectangles when [`Self::age`] is `1`.
    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error>;
}
