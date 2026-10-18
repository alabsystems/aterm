// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_winit 0.33.1 (Apache-2.0) src/lib.rs and
// src/platform_impl/unix.rs, and rewritten by the aterm project in 2026 over
// the first-party AT-SPI publisher (aterm-a11y) for aterm's own event type.

//! A window's accessibility adapter on Linux: the window side of
//! [`aterm_a11y::atspi`], which publishes the window's accessibility tree to
//! screen readers over AT-SPI (docs/AUDIT-third-party-2026-10-15.md, unit
//! a11y, step A11Y.4; it replaced accesskit_winit).
//!
//! The publisher calls a window's handlers on its own thread, so the handlers
//! here only post a [`Wake::Accessibility`] to the event loop:
//! [`WindowEvent::InitialTreeRequested`] when a screen reader starts
//! listening (the window answers with its full tree,
//! `App::push_a11y_tree`), [`WindowEvent::ActionRequested`] for its actions,
//! and [`WindowEvent::AccessibilityDeactivated`] when it stops or the bus is
//! lost. The window feeds the adapter its moves, resizes and focus changes
//! ([`Adapter::process_event`]) so extents and the active state stay true.

use aterm_a11y::model::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Rect, TreeUpdate,
};
use winit::event::WindowEvent as WinitWindowEvent;
use winit::event_loop::EventLoopProxy;
use winit::window::{Window, WindowId};

use crate::Wake;

/// An accessibility event for the window `window_id`, delivered as
/// [`Wake::Accessibility`].
#[derive(Debug)]
pub(crate) struct Event {
    pub(crate) window_id: WindowId,
    pub(crate) window_event: WindowEvent,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WindowEvent {
    /// A screen reader is listening: publish the full tree.
    InitialTreeRequested,
    /// A screen reader asked for an action.
    ActionRequested(ActionRequest),
    /// Nobody is listening any more: stop building trees.
    AccessibilityDeactivated,
}

/// The window's three handlers, each a post to the event loop.
struct Handler {
    window_id: WindowId,
    proxy: EventLoopProxy<Wake>,
}

impl Handler {
    fn post(&self, window_event: WindowEvent) {
        // A loop that has exited has no window left to tell.
        let _ = self.proxy.send_event(Wake::Accessibility(Event {
            window_id: self.window_id,
            window_event,
        }));
    }
}

impl ActivationHandler for Handler {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.post(WindowEvent::InitialTreeRequested);
        None
    }
}

impl ActionHandler for Handler {
    fn do_action(&mut self, request: ActionRequest) {
        self.post(WindowEvent::ActionRequested(request));
    }
}

impl DeactivationHandler for Handler {
    fn deactivate_accessibility(&mut self) {
        self.post(WindowEvent::AccessibilityDeactivated);
    }
}

/// One window's adapter on the process's publisher.
pub(crate) struct Adapter {
    inner: aterm_a11y::atspi::Adapter,
}

impl Adapter {
    pub(crate) fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Self {
        let window_id = window.id();
        let handler = || Handler {
            window_id,
            proxy: proxy.clone(),
        };
        Self {
            inner: aterm_a11y::atspi::Publisher::global().adapter(handler(), handler(), handler()),
        }
    }

    /// Keeps the window's screen bounds and focus current; called with every
    /// window event, before aterm handles it.
    pub(crate) fn process_event(&mut self, window: &Window, event: &WinitWindowEvent) {
        match event {
            WinitWindowEvent::Moved(_) | WinitWindowEvent::Resized(_) => {
                let outer_position: (f64, f64) = match event {
                    WinitWindowEvent::Moved(position) => position.cast::<f64>().into(),
                    _ => window
                        .outer_position()
                        .unwrap_or_default()
                        .cast::<f64>()
                        .into(),
                };
                let inner_size: (f64, f64) = match event {
                    WinitWindowEvent::Resized(size) => size.cast::<f64>().into(),
                    _ => window.inner_size().cast::<f64>().into(),
                };
                let outer_size: (f64, f64) = window.outer_size().cast::<f64>().into();
                let inner_position: (f64, f64) = window
                    .inner_position()
                    .unwrap_or_default()
                    .cast::<f64>()
                    .into();
                self.inner.set_root_window_bounds(
                    Rect::from_origin_size(outer_position, outer_size),
                    Rect::from_origin_size(inner_position, inner_size),
                );
            }
            WinitWindowEvent::Focused(focused) => self.inner.update_window_focus_state(*focused),
            _ => {}
        }
    }

    /// Applies the update `updater` builds while a screen reader listens.
    pub(crate) fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        self.inner.update_if_active(updater);
    }
}
