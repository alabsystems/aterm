// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn renderer_state(app: &App) -> (bool, Option<CursorStyle>, bool) {
    let Backend::Cpu(renderer) = app.backend.ready() else {
        panic!("headless test backend is CPU")
    };
    (
        renderer.cursor_blink_phase(),
        renderer.cursor_style_override(),
        renderer.selection_inactive(),
    )
}

#[test]
fn opposite_two_window_cursor_state_is_rebound_before_each_encode() {
    let mut app = App::headless_for_test();
    let first = WindowId(0);
    let session = app.next_session_id;
    let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
    app.render_knobs.selection_inactive = true;
    {
        let first = app.windows.get_mut(&first).unwrap();
        first.blink_phase = false;
        first.focused = false;
    }
    {
        let second = app.windows.get_mut(&second).unwrap();
        second.blink_phase = true;
        second.focused = true;
    }

    app.backend.set_cursor_blink_phase(true);
    app.backend
        .set_cursor_style_override(Some(CursorStyle::HollowBlock));
    app.backend.set_selection_inactive(false);

    app.bind_window_renderer_state(first);
    assert_eq!(
        renderer_state(&app),
        (false, None, true),
        "glass-less A owns its blink/inactive state and clears a stale OS-only style"
    );
    app.bind_window_renderer_state(second);
    assert_eq!(
        renderer_state(&app),
        (true, None, false),
        "B cannot inherit A's blink or inactive-selection state"
    );
}
