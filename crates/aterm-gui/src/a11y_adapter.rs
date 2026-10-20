// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_winit 0.33.1 (Apache-2.0) src/lib.rs and
// src/platform_impl/unix.rs, and rewritten by the aterm project in 2026 over
// the first-party AT-SPI publisher (aterm-a11y) for aterm's own event type.

//! One real window's native accessibility adapter. Publisher callbacks only post
//! typed events; the main loop owns the shared semantic tree and action routes.

use aterm_a11y::model::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate,
};
use aterm_window::event::WindowEvent as WinitWindowEvent;
use aterm_window::event_loop::EventLoopProxy;
use aterm_window::window::{Window, WindowId};

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

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
pub(crate) use linux::Adapter;
#[cfg(target_os = "macos")]
pub(crate) use macos::Adapter;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::Adapter;
