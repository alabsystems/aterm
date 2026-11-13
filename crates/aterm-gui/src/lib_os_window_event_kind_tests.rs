// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::os_window_event_kind;
use crate::metrics::OsWindowEvent;
use aterm_window::event::WindowEvent;

#[test]
fn a_redraw_is_not_counted_and_a_focus_change_counts_as_focus() {
    assert_eq!(os_window_event_kind(&WindowEvent::RedrawRequested), None);
    assert_eq!(
        os_window_event_kind(&WindowEvent::FrameTick {
            target: std::time::Instant::now(),
            period: std::time::Duration::from_micros(8_333),
        }),
        None,
        "a display link's tick is the window's own frame, as a redraw is"
    );
    // CONTROL: the focus change a pet take refuses on is counted, under its kind.
    assert_eq!(
        os_window_event_kind(&WindowEvent::Focused(false)),
        Some(OsWindowEvent::Focus)
    );
}
