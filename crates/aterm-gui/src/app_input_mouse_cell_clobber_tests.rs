// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::WindowId;
use crate::input::{InputEvent, PixelOffset, Source};
use aterm_core::selection::SelectionSide;

#[test]
fn mouse_move_does_not_clobber_pane_local_cell() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().last_mouse_cell = (5, 7);
    let _ = app.input(
        wid,
        InputEvent::MouseMove {
            buttons: 3,
            row: 30,
            col: 30,
            mods: 0,
            side: SelectionSide::Left,
            px_off: PixelOffset::CELL_ORIGIN,
        },
        Source::Human,
    );
    assert_eq!(
        app.windows.get(&wid).unwrap().last_mouse_cell,
        (5, 7),
        "the seam must not overwrite the pane-local last_mouse_cell"
    );
}
