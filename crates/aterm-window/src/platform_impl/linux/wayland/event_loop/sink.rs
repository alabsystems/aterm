// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/event_loop/sink.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! An event loop's sink to deliver events from the Wayland event callbacks.
//!
//! Modified by the aterm project in 2026; see the repository NOTICE. (`push_device_event`
//! left with the relative pointer, its one caller: docs/AUDIT-third-party-2026-10-15.md, LW.7.)

use std::vec::Drain;

use crate::event::{Event, WindowEvent};
use crate::window::WindowId as RootWindowId;

use crate::platform_impl::wayland::WindowId;

/// An event loop's sink to deliver events from the Wayland event callbacks
/// to the winit's user.
#[derive(Default)]
pub struct EventSink {
    pub window_events: Vec<Event<()>>,
}

impl EventSink {
    pub fn new() -> Self {
        Default::default()
    }

    /// Return `true` if there're pending events.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.window_events.is_empty()
    }

    /// Add new window event to a queue.
    #[inline]
    pub fn push_window_event(&mut self, event: WindowEvent, window_id: WindowId) {
        self.window_events.push(Event::WindowEvent { event, window_id: RootWindowId(window_id) });
    }

    #[inline]
    pub fn append(&mut self, other: &mut Self) {
        self.window_events.append(&mut other.window_events);
    }

    #[inline]
    pub fn drain(&mut self) -> Drain<'_, Event<()>> {
        self.window_events.drain(..)
    }
}
