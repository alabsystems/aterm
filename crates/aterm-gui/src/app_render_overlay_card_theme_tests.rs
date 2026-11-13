// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};

#[test]
fn overlay_card_tracks_live_background_and_theme_fallback() {
    let mut app = App::headless_for_test();
    app.theme.bg = 0x00FF_FFFF; // a light user theme
    let wid0 = WindowId(0);
    if let Some(ws) = app.windows.get_mut(&wid0) {
        ws.input_scratch.default_bg = 0x0012_3456;
    }
    assert_eq!(
        app.overlay_card_theme(wid0).bg,
        0x0012_3456,
        "a terminal window's overlay still tracks the live OSC-11 bg"
    );
    if let Some(ws) = app.windows.get_mut(&wid0) {
        ws.input_scratch.default_bg = aterm_core::render::COLOR_UNSET;
    }
    assert_eq!(app.overlay_card_theme(wid0).bg, 0x00FF_FFFF);
}
