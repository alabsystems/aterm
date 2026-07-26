// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M1b PROVE bullets bound to the SHIPPING renderer (`render_input` /
//! `render_input_cached`): the sub-row scroll translate is (1) the identity at
//! `scroll_frac_px == 0` — a fractional frame at frac 0 is byte-identical to the
//! whole-row-jump frame — and (2) chrome-invariant: for ANY `scroll_frac_px`,
//! every pixel OUTSIDE the terminal-content grid band `[grid_top_row,
//! grid_bot_row)` is byte-identical to the frac-0 frame (the KEY theorem). The
//! pure geometry carries its own always-on lattice proofs + a `ty` twin
//! (`aterm_render::scroll_translate` / `grid_translate_model`); this test binds
//! that behaviour to the code the frontend actually presents.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};

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
    // Scroll into history so the grid shows scrollback (a real smooth-scroll frame).
    t.scroll_display(5);
    t
}

/// Build a frame snapshot with a chrome partition: row 0 is designated tab-strip
/// chrome, the last row is designated bottom chrome, and the middle is the terminal
/// grid — the exact `[grid_top_row, grid_bot_row)` layout the compose path emits.
fn frame_with_partition(t: &mut Terminal, frac: i32) -> aterm_core::render::RenderInput {
    let mut input = t.cell_frame(ROWS, COLS);
    input.grid_top_row = 1; // row 0 = tab strip (chrome)
    input.grid_bot_row = ROWS - 1; // last row = bottom chrome
    input.scroll_frac_px = frac;
    input
}

#[test]
fn frac_zero_is_byte_identical_to_untranslated() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found (headless without fonts)");
        return;
    };
    let mut t = seeded_terminal();
    // A frame with a grid partition but frac 0.
    let zero = r.render_input(&frame_with_partition(&mut t, 0));
    // The same viewport with NO partition set at all (the pre-M1b default path).
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
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found (headless without fonts)");
        return;
    };
    let (_cw, cell_h) = r.cell_size();
    let pad = r.pad();
    // Grid band in device pixels (the renderer's own row→px mapping).
    let y0 = pad + cell_h; // grid_top_row = 1
    let y1 = pad + (ROWS - 1) * cell_h; // grid_bot_row = ROWS-1

    let mut t = seeded_terminal();
    let base = r.render_input(&frame_with_partition(&mut t, 0));
    let (w, h) = (base.width, base.height);

    let mut any_moved = false;
    let mut any_down = false;
    // BIDIRECTIONAL: sweep sub-cell residuals across BOTH signs — a positive frac
    // shifts the grid band UP (glide), a negative frac shifts it DOWN (the elastic
    // overscroll bounce at a history end). Chrome stays pinned for either sign.
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

#[test]
fn damage_cached_present_translates_without_perturbing_the_cache() {
    // The frac translate must present a shifted frame while the UNTRANSLATED
    // damage cache keeps driving the row diff: after a fractional frame, a
    // subsequent frac-0 frame of the SAME viewport returns to the byte-identical
    // untranslated pixels (no residue banked into the cache).
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found (headless without fonts)");
        return;
    };
    let mut wc = WindowCpu::new();
    let mut t = seeded_terminal();

    // Frame 1: frac 0 (seeds the cache).
    let zero_a: Vec<u32> = r
        .render_input_cached(&mut wc, &frame_with_partition(&mut t, 0))
        .pixels()
        .to_vec();
    // Frame 2: a fractional present (shifts the grid band).
    let shifted: Vec<u32> = r
        .render_input_cached(&mut wc, &frame_with_partition(&mut t, 4))
        .pixels()
        .to_vec();
    // Frame 3: back to frac 0, same viewport — must equal frame 1 exactly.
    let zero_b: Vec<u32> = r
        .render_input_cached(&mut wc, &frame_with_partition(&mut t, 0))
        .pixels()
        .to_vec();

    assert_eq!(
        zero_a, zero_b,
        "a frac-0 present after a fractional one returns the pristine untranslated \
         frame — the damage cache was never perturbed by the translate"
    );
    assert_ne!(
        shifted, zero_a,
        "non-vacuity: the fractional present genuinely differs from frac 0"
    );
}
