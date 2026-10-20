// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// One real window's AppKit publisher. There is no adapter on an offscreen
/// WindowState; creating this wrapper never creates an application or a window.
pub(crate) struct Adapter {
    inner: aterm_a11y::macos::Adapter,
}

impl Adapter {
    pub(crate) fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Option<Self> {
        let view = crate::platform::ns_view_of(window);
        let handler = || Handler {
            window_id: window.id(),
            proxy: proxy.clone(),
        };
        // SAFETY: the native window owns its content NSView, and window creation
        // runs on the event-loop main thread. The publisher stores it weakly.
        let mut inner = unsafe {
            aterm_a11y::macos::Adapter::new(
                Some(view),
                window.scale_factor(),
                handler(),
                handler(),
            )?
        };
        inner.update_window_focus_state(window.has_focus());
        Some(Self { inner })
    }

    pub(crate) fn process_event(&mut self, _window: &Window, event: &WinitWindowEvent) {
        match event {
            WinitWindowEvent::Moved(_) | WinitWindowEvent::Resized(_) => self.inner.window_moved(),
            WinitWindowEvent::Focused(focused) => self.inner.update_window_focus_state(*focused),
            WinitWindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.inner.set_scale_factor(*scale_factor)
            }
            _ => {}
        }
    }

    pub(crate) fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        self.inner.update_if_active(updater);
    }
}
