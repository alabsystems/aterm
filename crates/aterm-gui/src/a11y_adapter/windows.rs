// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_a11y::model::Rect;

/// One real HWND's UI Automation publisher. Offscreen windows have no adapter.
pub(crate) struct Adapter {
    inner: aterm_a11y::uia::Adapter,
}

impl Adapter {
    pub(crate) fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Option<Self> {
        let hwnd = crate::platform_win::hwnd_of(window)?;
        let (outer, inner) = bounds(window)?;
        let handler = || Handler {
            window_id: window.id(),
            proxy: proxy.clone(),
        };
        // SAFETY: the native Window owns this live HWND on its event-loop
        // thread. Window teardown retires the provider before handle reuse.
        let mut publisher = unsafe { aterm_a11y::uia::Adapter::new(hwnd, handler(), handler())? };
        publisher.set_root_window_bounds(outer, inner);
        publisher.update_window_focus_state(window.has_focus());
        Some(Self { inner: publisher })
    }

    pub(crate) fn process_event(&mut self, window: &Window, event: &WinitWindowEvent) {
        match event {
            WinitWindowEvent::Moved(_)
            | WinitWindowEvent::Resized(_)
            | WinitWindowEvent::ScaleFactorChanged { .. } => {
                if let Some((outer, inner)) = bounds(window) {
                    self.inner.set_root_window_bounds(outer, inner);
                }
            }
            WinitWindowEvent::Focused(focused) => self.inner.update_window_focus_state(*focused),
            _ => {}
        }
    }

    pub(crate) fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        self.inner.update_if_active(updater);
    }
}

/// UIA uses physical screen coordinates, while the shared nodes are relative
/// to the client area. An unavailable native position is not guessed as zero.
fn bounds(window: &Window) -> Option<(Rect, Rect)> {
    let outer_position: (f64, f64) = window.outer_position().ok()?.cast::<f64>().into();
    let inner_position: (f64, f64) = window.inner_position().ok()?.cast::<f64>().into();
    let outer_size: (f64, f64) = window.outer_size().cast::<f64>().into();
    let inner_size: (f64, f64) = window.inner_size().cast::<f64>().into();
    Some((
        Rect::from_origin_size(outer_position, outer_size),
        Rect::from_origin_size(inner_position, inner_size),
    ))
}
