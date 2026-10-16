// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `softbuffer`-backed CPU presenter, for every cell that is not macOS.
//!
//! A thin adapter. Its only work is translating [`DamageRect`] into
//! `softbuffer::Rect` through a presenter-owned scratch, so a damage-bounded
//! present stays allocation-free in steady state.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use winit::window::Window;

use super::{CpuFrameBuffer, CpuPresenter, DamageRect};

/// A `softbuffer` surface plus the display context that must outlive it: on
/// X11/Wayland the context owns the shared display connection. Nothing reads it.
pub(crate) struct SoftbufferPresenter {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    /// Scratch for the [`DamageRect`] -> `softbuffer::Rect` translation, owned
    /// by the presenter so a steady dirty-row frame reuses one allocation.
    damage_scratch: Vec<softbuffer::Rect>,
    _context: softbuffer::Context<Arc<Window>>,
}

/// One acquired `softbuffer` buffer plus the presenter's damage scratch (a
/// disjoint field borrow).
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
        // Split the borrow by field so the buffer and the scratch are live at once.
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
