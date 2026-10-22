// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CPU present seam: the boundary the frontend's fail-soft renderer presents
//! through.
//!
//! It restates only what the frontend asks of a CPU surface: create a presenter
//! for one window, resize its backing store to the raw window size in physical
//! pixels, acquire a `[u32]` frame buffer (0x00RRGGBB, the `aterm-render`
//! framebuffer's word layout), ask how old the buffer's contents are, and commit
//! it whole or damage-bounded.
//!
//! macOS gets [`mac::MacCpuPresenter`] (CoreGraphics + CoreAnimation); Linux
//! gets [`linux::LinuxPresenter`] (MIT-SHM on X11, `wl_shm` on Wayland,
//! first-party since LW.6 of docs/AUDIT-third-party-2026-10-15.md); Windows gets
//! [`win32::Win32Presenter`] (GDI, first-party since LW.3).
//! The `cfg`-selected [`CpuSurface`] alias resolves to one concrete type per
//! cell, so the per-frame path has no vtable; the traits make a method added to
//! one backend and not the other a compile error.
//!
//! This is the fail-soft arm for GPU device loss and for surfaces that will not
//! create: every error here is a typed failure the caller turns into a dropped
//! frame plus a re-arm, never a panic or a silently skipped commit that would
//! leave a black window.

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

/// The CPU present backend for this cell: the first-party CoreGraphics presenter.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) type CpuSurface = mac::MacCpuPresenter;
/// The CPU present backend for this cell: X11 or Wayland, by the window's server.
#[cfg(all(target_os = "linux", not(test)))]
pub(crate) type CpuSurface = linux::LinuxPresenter;
/// The CPU present backend for this cell: GDI.
#[cfg(all(windows, not(test)))]
pub(crate) type CpuSurface = win32::Win32Presenter;
/// The test build's CPU surface: the platform presenter, or a scripted one a
/// headless test installs to drive the real present path ([`scripted`]).
#[cfg(test)]
pub(crate) type CpuSurface = scripted::TestCpuSurface;

/// One damaged region of a presented frame, in surface pixels with the origin
/// at the top-left; the fields and meaning of softbuffer's `Rect`. The extents
/// are non-zero because a zero-area damage rectangle is not a well-specified
/// no-op across backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DamageRect {
    /// Left edge, in surface pixels.
    pub x: u32,
    /// Top edge, in surface pixels.
    pub y: u32,
    /// Width in surface pixels; never zero.
    pub width: NonZeroU32,
    /// Height in surface pixels; never zero.
    pub height: NonZeroU32,
}

/// One window's CPU presentation target: owns the platform surface and hands
/// out frame buffers. A construction error is reported, and the caller declines
/// the window rather than leave it on screen with nothing presenting into it.
pub(crate) trait CpuPresenter: Sized {
    /// Backend-native failure. `Display` because the two construction sites
    /// print it verbatim; the present path collapses it to a typed drop reason.
    type Error: std::fmt::Display;

    /// The acquired, paintable frame for one present. Borrows the presenter, so
    /// acquire-paint-commit is one transaction the borrow checker enforces.
    type Buffer<'a>: CpuFrameBuffer<Error = Self::Error>
    where
        Self: 'a;

    /// Attach a CPU presentation target to `window`.
    fn new(window: Arc<Window>) -> Result<Self, Self::Error>;

    /// Set the backing store to `width` x `height` physical pixels: the raw
    /// window size, never the frame size (the caller centres the frame in it).
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error>;

    /// Acquire the next frame buffer. May block on compositor ownership, which
    /// is why the caller starts its copy timer only after this returns.
    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error>;

    /// Whether the window system asks for part of the window to be drawn again
    /// (Windows' update region: rows the window gained after its last present,
    /// say), which the next present owes even when the frame is unchanged.
    /// `false` where the compositor keeps the window's last frame.
    fn owes_repaint(&self) -> bool {
        false
    }
}

/// One acquired CPU frame: a `[u32]` of `width * height` words in
/// `0x00RRGGBB`. Committing consumes the buffer, so a frame is presented once.
pub(crate) trait CpuFrameBuffer: DerefMut<Target = [u32]> + Sized {
    /// Backend-native failure; same type as the presenter's.
    type Error;

    /// How many presents ago this buffer's current contents were on screen.
    /// Only `1` proves the buffer still holds the previous present, so only then
    /// may the caller copy just the dirty rows; `0` (first frame, post-resize, or
    /// a backend that does not retain buffers) forces the full copy.
    fn age(&self) -> u8;

    /// Commit the whole buffer.
    fn present(self) -> Result<(), Self::Error>;

    /// Commit the buffer, promising that everything outside `damage` is
    /// unchanged since the last present. Over-claiming damage is safe;
    /// under-claiming is not. Call only after [`CpuFrameBuffer::age`] returned `1`.
    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error>;
}
