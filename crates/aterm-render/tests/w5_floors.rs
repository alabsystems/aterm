// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_render::{
    DefaultBgSpan, Renderer, Theme, floor_cursor_fill, floor_min_contrast_fg, underline_rects_into,
};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

#[test]
fn block_cursor_near_bg_is_floored_visible() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(2, 4);
    term.process(b"\x1b[2 q\x1b[48;2;32;32;32m \x1b[0m\x1b[H");
    let mut input = term.cell_frame(2, 4);
    let bg = input.cells[0][0].bg;
    let bg_u32 = (u32::from(bg[0]) << 16) | (u32::from(bg[1]) << 8) | u32::from(bg[2]);
    assert_eq!(
        bg_u32, 0x0020_2020,
        "fixture: the cursor cell carries the SGR bg"
    );
    input.cursor_color = bg_u32;

    let f = r.render_input(&input);
    assert_eq!(
        f.pixels[0], bg_u32,
        "knob off: cursor == cell bg paints invisibly (pre-W5 behavior preserved)"
    );

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

#[test]
fn sparse_block_cursor_uses_its_pane_default_for_contrast() {
    let mut renderer = renderer();
    let (cw, ch) = renderer.cell_size();
    let (rows, cols) = (2usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[2 q\x1b[1;8H");
    let mut input = term.cell_frame(rows, cols);
    assert!(
        input.cells[0].get(7).is_none(),
        "fixture requires an unmaterialized cursor cell"
    );

    let pane_bg = 0x0024_4668;
    input.default_bg = 0x0012_3456;
    input.default_bg_spans = vec![vec![DefaultBgSpan::new(4, cols, pane_bg)], Vec::new()];
    input.cursor_color = pane_bg;
    renderer.set_minimum_contrast(4.5);

    let expected = floor_cursor_fill(pane_bg, pane_bg, 4.5);
    let frame = renderer.render_input(&input);
    for y in 0..ch {
        for x in 7 * cw..8 * cw {
            assert_eq!(
                frame.pixels[y * frame.width + x],
                expected,
                "sparse cursor must be floored against its pane default at ({x},{y})"
            );
        }
    }
}

#[test]
fn underline_routes_through_the_min_contrast_floor() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process(b"\x1b[48;2;32;32;32m\x1b[38;2;48;48;48m\x1b[4m \x1b[0m");
    let input = term.cell_frame(2, 8);
    let fg = 0x0030_3030u32;
    let bg = 0x0020_2020u32;

    let mut rects = Vec::new();
    underline_rects_into(
        &mut rects,
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

    let f = r.render_input(&input);
    let [rx, ry, ..] = rects[0];
    assert_eq!(f.pixels[ry * f.width + rx], fg, "knob off: raw underline");

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
