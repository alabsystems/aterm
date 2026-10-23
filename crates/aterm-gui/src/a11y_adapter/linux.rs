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
    /// The window moved or resized while nobody listened: its bounds are read
    /// when someone does ([`Self::listened`]).
    bounds_owed: bool,
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
            bounds_owed: true,
        })
    }

    /// Keeps the window's screen bounds and focus current; called with every
    /// window event, before aterm handles it. The bounds are X11 round trips,
    /// a handful a resize while the server is busy with it: read only while
    /// someone listens.
    pub(crate) fn process_event(&mut self, window: &Window, event: &WinitWindowEvent) {
        match event {
            WinitWindowEvent::Moved(_) | WinitWindowEvent::Resized(_) => {
                if self.inner.is_listened() {
                    self.read_bounds(window, Some(event));
                } else {
                    self.bounds_owed = true;
                }
            }
            WinitWindowEvent::Focused(focused) => self.inner.update_window_focus_state(*focused),
            _ => {}
        }
    }

    /// An assistive technology asked for the tree: the bounds a move or resize
    /// left unread go with it.
    pub(crate) fn listened(&mut self, window: &Window) {
        if self.bounds_owed {
            self.read_bounds(window, None);
        }
    }

    /// Reads the window's bounds, the position or size `event` reports as it
    /// does.
    fn read_bounds(&mut self, window: &Window, event: Option<&WinitWindowEvent>) {
        let outer_position: (f64, f64) = match event {
            Some(WinitWindowEvent::Moved(position)) => position.cast::<f64>().into(),
            _ => window
                .outer_position()
                .unwrap_or_default()
                .cast::<f64>()
                .into(),
        };
        let inner_size: (f64, f64) = match event {
            Some(WinitWindowEvent::Resized(size)) => size.cast::<f64>().into(),
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
        self.bounds_owed = false;
    }

    /// Applies the update `updater` builds while a screen reader listens.
    pub(crate) fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        self.inner.update_if_active(updater);
    }
}
