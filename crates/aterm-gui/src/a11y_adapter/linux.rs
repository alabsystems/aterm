// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_winit 0.33.1 (Apache-2.0), adapted to the
// first-party AT-SPI publisher; split from a11y_adapter.rs in 2026.

use super::*;
use aterm_a11y::model::Rect;

/// One window's adapter on the process's publisher.
pub(crate) struct Adapter {
    inner: aterm_a11y::atspi::Adapter,
}

impl Adapter {
    pub(crate) fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Option<Self> {
        let window_id = window.id();
        let handler = || Handler {
            window_id,
            proxy: proxy.clone(),
        };
        Some(Self {
            inner: aterm_a11y::atspi::Publisher::global().adapter(handler(), handler(), handler()),
        })
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
