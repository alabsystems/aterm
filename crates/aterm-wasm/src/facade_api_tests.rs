// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

/// A main-screen row cached at a key the alt screen comes to share is not served for the alt screen.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn display_row_cache_distinguishes_main_and_alt_screen() {
    let mut t = crate::system_font_terminal(5, 20, 16.0);
    for _ in 0..30 {
        t.process(b"\r\n");
    }
    t.process(b"\x1b[HM");
    let main_gen = t.term.grid().content_gen();
    assert_eq!(t.cell_text(0, 0), "M", "primed main cell at (0,0)");

    t.process(b"\x1b[?1049h");
    t.process(b"\x1b[HA");
    assert!(
        t.term.grid().content_gen() <= main_gen,
        "alt buffer starts below main_gen so we can drive it up to collide"
    );
    let mut guard = 0;
    while t.term.grid().content_gen() != main_gen && guard < 10_000 {
        let g: &[u8] = if guard % 2 == 0 {
            b"\x1b[2;1Hx"
        } else {
            b"\x1b[2;1Hy"
        };
        t.process(g);
        guard += 1;
    }
    assert_eq!(
        t.term.grid().content_gen(),
        main_gen,
        "manufactured the content_gen collision across the swap"
    );
    assert_eq!(
        t.cell_text(0, 0),
        "A",
        "alt-screen cell must not be served from the main-screen cache at a coinciding key"
    );
}

/// Extreme `i32` selection rows stay clipped to the viewport without wrapping.
#[test]
fn hostile_selection_rows_stay_offscreen_without_wrapping() {
    let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
    let mut t = WebTerminal::new(3, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
    for _ in 0..8 {
        t.process(b"line\r\n");
    }
    t.selection_start(i32::MAX, 0);
    t.selection_extend(i32::MAX, 7);
    t.scroll_lines(4);
    assert!(
        t.selection_range().is_none(),
        "a far-below row must stay below"
    );

    t.selection_start(i32::MIN, 0);
    t.selection_extend(i32::MAX, 7);
    t.selection_finish();
    let range = t.selection_range().expect("selection crosses the viewport");
    assert_eq!((range.start_y(), range.end_y()), (0, 2));
    assert_eq!((range.start_x(), range.end_x()), (0, 7));

    assert!(t.selection_line(i32::MIN, 0).is_none());
    assert!(t.selection_range().is_none());
    assert!(t.selection_word(i32::MIN, 0).is_none());
    assert!(t.selection_range().is_none());
}
