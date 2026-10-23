// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sub-row scroll translation tests on `render_input` and `render_input_cached`.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

const ROWS: usize = 8;
const COLS: usize = 20;

fn seeded_terminal() -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..40 {
        let line = if i % 2 == 0 {
            format!("\x1b[3{}mrow {:02} abcdef\x1b[0m\r\n", (i % 6) + 1, i)
        } else {
            format!("\x1b[1mROW {i:02} XYZWUV\x1b[0m\r\n")
        };
        t.process(line.as_bytes());
    }
    t.scroll_display(5);
    t
}

fn frame_with_partition(t: &mut Terminal, frac: i32) -> aterm_core::render::RenderInput {
    let mut input = t.cell_frame(ROWS, COLS);
    input.grid_top_row = 1; // row 0 = tab strip (chrome)
    input.grid_bot_row = ROWS - 1; // last row = bottom chrome
    input.scroll_frac_px = frac;
    input
}

#[test]
fn frac_zero_is_byte_identical_to_untranslated() {
    let mut r = Renderer::from_system(16.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let mut t = seeded_terminal();
    let zero = r.render_input(&frame_with_partition(&mut t, 0));
    let plain = {
        let input = t.cell_frame(ROWS, COLS);
        r.render_input(&input)
    };
    assert_eq!(
        (zero.width, zero.height),
        (plain.width, plain.height),
        "dims must match"
    );
    assert_eq!(
        zero.pixels, plain.pixels,
        "PROVE (1): the translate at scroll_frac_px==0 is a literal identity — \
         byte-identical to the untranslated whole-row frame"
    );
}

#[test]
fn chrome_pixels_are_invariant_under_any_frac() {
    let mut r = Renderer::from_system(16.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let (_cw, cell_h) = r.cell_size();
    let pad = r.pad();
    let y0 = pad + cell_h; // grid_top_row = 1
    let y1 = pad + (ROWS - 1) * cell_h; // grid_bot_row = ROWS-1

    let mut t = seeded_terminal();
    let base = r.render_input(&frame_with_partition(&mut t, 0));
    let (w, h) = (base.width, base.height);

    let mut any_moved = false;
    let mut any_down = false;
    for frac in (-(cell_h as i32 - 1)..cell_h as i32).filter(|&f| f != 0) {
        let mut t = seeded_terminal();
        let shifted = r.render_input(&frame_with_partition(&mut t, frac));
        assert_eq!(
            (shifted.width, shifted.height),
            (w, h),
            "dims stable across frac"
        );
        for y in 0..h {
            let row_a = &base.pixels[y * w..y * w + w];
            let row_b = &shifted.pixels[y * w..y * w + w];
            if y < y0 || y >= y1 {
                assert_eq!(
                    row_a, row_b,
                    "PROVE (2): chrome pixel row {y} (outside the grid band \
                     [{y0},{y1})) must be byte-identical under frac={frac}"
                );
            } else if row_a != row_b {
                any_moved = true;
                if frac < 0 {
                    any_down = true;
                }
            }
        }
    }
    assert!(
        any_moved,
        "non-vacuity: some fractional shift genuinely moves grid-band pixels"
    );
    assert!(
        any_down,
        "non-vacuity: a NEGATIVE frac (overscroll bounce) genuinely shifts pixels DOWN"
    );
}
