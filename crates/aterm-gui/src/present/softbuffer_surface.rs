// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `softbuffer`-backed CPU presenter: linux, windows, and every cell that
//! is not macOS.
//!
//! This is a THIN adapter, deliberately. It exists so those cells keep exactly
//! the behaviour they shipped with while the macOS cell moves to a first-party
//! presenter — same crate, same calls, same order, same errors. The only real
//! work it does is translate the seam's first-party [`DamageRect`] back into
//! `softbuffer::Rect`, and it does that through a scratch buffer owned by the
//! presenter so a damage-bounded present keeps its steady-state zero-allocation
//! property (the whole point of the frontend's own
//! `WindowState::cpu_damage_rect_scratch`).

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use winit::window::Window;

use super::{CpuFrameBuffer, CpuPresenter, DamageRect};

/// A `softbuffer` surface plus the display context that must outlive it.
///
/// The context is stored only to keep it alive: on X11/Wayland it owns the
/// shared display connection the surface was built against. Nothing reads it,
/// which is exactly why it does not appear on the seam.
pub(crate) struct SoftbufferPresenter {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    /// Scratch for the [`DamageRect`] -> `softbuffer::Rect` translation, owned
    /// by the presenter so a steady dirty-row frame reuses one allocation.
    damage_scratch: Vec<softbuffer::Rect>,
    _context: softbuffer::Context<Arc<Window>>,
}

/// One acquired `softbuffer` buffer, paired with the presenter's damage
/// scratch (a disjoint field borrow, so acquiring the buffer does not lock the
/// scratch away).
pub(crate) struct SoftbufferFrame<'a> {
    buffer: softbuffer::Buffer<'a, Arc<Window>, Arc<Window>>,
    damage_scratch: &'a mut Vec<softbuffer::Rect>,
}

impl CpuPresenter for SoftbufferPresenter {
    type Error = softbuffer::SoftBufferError;
    type Buffer<'a>
        = SoftbufferFrame<'a>
    where
        Self: 'a;

    fn new(window: Arc<Window>) -> Result<Self, Self::Error> {
        let context = softbuffer::Context::new(window.clone())?;
        let surface = softbuffer::Surface::new(&context, window)?;
        Ok(Self {
            surface,
            damage_scratch: Vec::new(),
            _context: context,
        })
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        self.surface.resize(width, height)
    }

    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error> {
        // Split the borrow by field: the buffer borrows `surface`, the
        // translation scratch is a sibling field, so both can be live at once.
        let Self {
            surface,
            damage_scratch,
            ..
        } = self;
        let buffer = surface.buffer_mut()?;
        Ok(SoftbufferFrame {
            buffer,
            damage_scratch,
        })
    }
}

impl Deref for SoftbufferFrame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        &self.buffer
    }
}

impl DerefMut for SoftbufferFrame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        &mut self.buffer
    }
}

impl CpuFrameBuffer for SoftbufferFrame<'_> {
    type Error = softbuffer::SoftBufferError;

    fn age(&self) -> u8 {
        self.buffer.age()
    }

    fn present(self) -> Result<(), Self::Error> {
        self.buffer.present()
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        let Self {
            buffer,
            damage_scratch,
        } = self;
        damage_scratch.clear();
        damage_scratch.reserve(damage.len());
        damage_scratch.extend(damage.iter().map(|rect| softbuffer::Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }));
        buffer.present_with_damage(damage_scratch)
    }
}
