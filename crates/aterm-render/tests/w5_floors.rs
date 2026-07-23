// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// W5b regression: the minimum-contrast knob floors MORE than the glyph fg —
// the block-cursor fill (an OSC-12 cursor colour near the cell bg used to
// vanish outright) and the line decorations (underline colour used to paint
// raw/unfloored on both backends). CPU fills are exact, so both are asserted
// as exact pixel values. GPU parity for these paths is carried by the shared
// pure functions (`floor_cursor_fill` / `effective_deco_color`) plus the
// gpu_matches_cpu suite.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, floor_cursor_fill, floor_min_contrast_fg, underline_rects};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// An OSC-12-style cursor colour EQUAL to the cell bg: invisible with the knob
/// off (byte-identical pre-W5 behavior), floored to a visible fill with it on.
#[test]
fn block_cursor_near_bg_is_floored_visible() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(2, 4);
    // Steady block; one space cell with bg #202020; cursor back on it (CUP).
    term.process(b"\x1b[2 q\x1b[48;2;32;32;32m \x1b[0m\x1b[H");
    let mut input = term.cell_frame(2, 4);
    // Host-resolved cursor colour == the cursor cell's own bg (worst case).
    let bg = input.cells[0][0].bg;
    let bg_u32 = (u32::from(bg[0]) << 16) | (u32::from(bg[1]) << 8) | u32::from(bg[2]);
    assert_eq!(
        bg_u32, 0x0020_2020,
        "fixture: the cursor cell carries the SGR bg"
    );
    input.cursor_color = bg_u32;

    // Knob off: the fill is exactly the (invisible) cursor colour.
    let f = r.render_input(&input);
    assert_eq!(
        f.pixels[0], bg_u32,
        "knob off: cursor == cell bg paints invisibly (pre-W5 behavior preserved)"
    );

    // Knob on: the whole cursor cell paints the FLOORED fill, which differs
    // from bg (the floor's delivery bound is proven in tests/contrast_floor.rs).
    r.set_minimum_contrast(4.5);
    let expected = floor_cursor_fill(bg_u32, bg_u32, 4.5);
    assert_ne!(expected, bg_u32, "the floor must move a bg-equal cursor");
    let f = r.render_input(&input);
    for y in 0..ch {
        for x in 0..cw {
            assert_eq!(
                f.pixels[y * f.width + x],
                expected,
                "floored cursor fill at ({x},{y})"
            );
        }
    }
}

/// A low-contrast underline (fg near bg, no explicit SGR 58 colour) paints
/// floored — through the SAME floor the glyph fg gets — when the knob is on,
/// and stays raw (byte-identical) when it is off.
#[test]
fn underline_routes_through_the_min_contrast_floor() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(2, 8);
    // Cell bg #202020 with an underlined fg only slightly lighter — legible to
    // nobody. (A space glyph so no AA ink lands on the underline rows.)
    term.process(b"\x1b[48;2;32;32;32m\x1b[38;2;48;48;48m\x1b[4m \x1b[0m");
    let input = term.cell_frame(2, 8);
    let fg = 0x0030_3030u32;
    let bg = 0x0020_2020u32;

    let rects = underline_rects(
        aterm_core::terminal::UnderlineStyle::Single,
        0,
        0,
        cw,
        cw,
        ch,
        r.deco_metrics(),
        false,
    );
    assert!(!rects.is_empty(), "single underline yields rects");

    // Knob off: raw fg (pre-W5 byte-identical).
    let f = r.render_input(&input);
    let [rx, ry, ..] = rects[0];
    assert_eq!(f.pixels[ry * f.width + rx], fg, "knob off: raw underline");

    // Knob on: the underline paints the floored colour.
    r.set_minimum_contrast(4.5);
    let expected = floor_min_contrast_fg(fg, bg, 4.5);
    assert_ne!(expected, fg, "fixture must actually floor");
    let f = r.render_input(&input);
    for &[x0, y0, w, h] in &rects {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                assert_eq!(
                    f.pixels[y * f.width + x],
                    expected,
                    "floored underline at ({x},{y})"
                );
            }
        }
    }
}
