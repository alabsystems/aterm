// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CPU present seam — the boundary the frontend's fail-soft renderer
//! presents through.
//!
//! # Why this module exists
//!
//! Until this module landed, `aterm-gui` named `softbuffer` types directly in
//! two of its own structs ([`crate::PresentTarget::Cpu`] and
//! `WindowState::cpu_damage_rect_scratch`) and in one pure helper. That is a
//! third-party type on the frontend's data model, and it is the last thing
//! standing between the macOS cell and a `softbuffer`-free build.
//!
//! The seam is deliberately the SMALLEST possible restatement of what the
//! frontend actually asks a CPU surface for — measured at 9 `softbuffer` API
//! items over 19 source lines in 3 files
//! (`docs/measured/gpu-seam-2026-08-30.md` §4.1):
//!
//! * create a presenter for one window,
//! * resize its backing store to the raw window size in physical pixels,
//! * acquire a `[u32]` frame buffer (0x00RRGGBB, the `aterm-render`
//!   framebuffer's own word layout),
//! * ask how old the acquired buffer's contents are, and
//! * commit it, whole or damage-bounded.
//!
//! `softbuffer`'s `Context`/`Surface` split does NOT appear here. That split
//! exists because X11 and Wayland need one shared display connection per
//! process; `aterm-gui` only ever built the pair together at the same two sites
//! and stored the context to keep it alive (`_context`). Folding it into
//! [`CpuPresenter::new`] removes one type from the boundary; the
//! softbuffer-backed implementation keeps the pair privately.
//!
//! # Which backend a cell gets
//!
//! | cell | backend | notes |
//! |---|---|---|
//! | macOS | [`mac::MacCpuPresenter`] | first-party CoreGraphics + CoreAnimation, no `softbuffer` |
//! | everything else | [`softbuffer_surface::SoftbufferPresenter`] | `softbuffer`, unchanged behaviour |
//!
//! Nothing dispatches dynamically. A `cfg`-selected alias
//! ([`CpuSurface`]) resolves to exactly one concrete type per cell, so the
//! present hot path keeps its direct, inlinable calls and there is no vtable on
//! a per-frame path. The traits exist so the two backends cannot drift: a
//! method added to one and not the other is a compile error at its `impl`.
//!
//! # This is the GPU-failure path
//!
//! `aterm-gpu` documents the downgrade to the CPU renderer as the fail-soft arm
//! for GPU device loss and for surfaces that will not create
//! (`crates/aterm-gpu/src/lib.rs:168`, `:240`). Every error here therefore
//! travels as a typed failure the caller turns into a DROPPED frame plus a
//! re-arm — never a panic, and never a silently-skipped commit that would leave
//! a user whose GPU has already failed looking at a black window.

use std::num::NonZeroU32;
use std::ops::DerefMut;
use std::sync::Arc;

use winit::window::Window;

#[cfg(target_os = "macos")]
pub(crate) mod mac;
#[cfg(not(target_os = "macos"))]
pub(crate) mod softbuffer_surface;

/// The CPU present backend for THIS cell. macOS gets the first-party
/// CoreGraphics presenter; every other cell keeps `softbuffer`.
#[cfg(target_os = "macos")]
pub(crate) type CpuSurface = mac::MacCpuPresenter;
/// The CPU present backend for THIS cell — see the macOS twin above.
#[cfg(not(target_os = "macos"))]
pub(crate) type CpuSurface = softbuffer_surface::SoftbufferPresenter;

/// One damaged region of a presented frame, in surface pixels with the origin
/// at the top-left.
///
/// First-party replacement for `softbuffer::Rect`, with the identical field
/// set and meaning, so `app_render::cpu_damage_rects_into` and the
/// window's persistent scratch stop naming a third-party crate. The non-zero
/// extents are the API's own contract, not a convenience: a zero-area damage
/// rectangle is not a well-specified no-op across backends, so the type makes
/// one unrepresentable.
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

/// One window's CPU presentation target: the thing that owns the platform
/// surface and hands out frame buffers to paint into.
///
/// Construction is fallible and the error is REPORTED, never swallowed — a
/// window whose CPU surface will not create is declined by the caller rather
/// than left on screen with nothing presenting into it.
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

    /// Set the backing store to `width` x `height` PHYSICAL pixels — the raw
    /// window size, never the frame size (the frame is placed into it at a
    /// centred band offset by the caller).
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error>;

    /// Acquire the next frame buffer. May block on compositor ownership, which
    /// is why the caller starts its copy timer only after this returns.
    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error>;
}

/// One acquired CPU frame: a `[u32]` of `width * height` words in
/// `0x00RRGGBB`, plus the two ways to commit it.
///
/// Committing CONSUMES the buffer, so a frame cannot be presented twice and a
/// buffer cannot outlive its present.
pub(crate) trait CpuFrameBuffer: DerefMut<Target = [u32]> + Sized {
    /// Backend-native failure; same type as the presenter's.
    type Error;

    /// How many presents ago this buffer's CURRENT contents were on screen.
    ///
    /// `1` — and ONLY `1` — means the buffer provably still holds the previous
    /// present, which is the single state in which the caller may copy just the
    /// dirty rows. `0` means new/unknown contents (first frame, post-resize, or
    /// a backend that does not retain buffers at all) and forces the full copy.
    fn age(&self) -> u8;

    /// Commit the whole buffer.
    fn present(self) -> Result<(), Self::Error>;

    /// Commit the buffer, promising that everything OUTSIDE `damage` is
    /// unchanged since the last present.
    ///
    /// Over-claiming damage is always safe; under-claiming is not. Callers must
    /// only reach this after [`CpuFrameBuffer::age`] returned `1`.
    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error>;
}
