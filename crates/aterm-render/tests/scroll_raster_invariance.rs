// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raster invariance of smooth scrolling at `frac == 0`: a viewport reached by
//! incremental row steps (the wheel glide's `scroll_motion::decompose` row
//! deltas via `scroll_display`) is byte-identical to the same viewport reached
//! by one row-jump, on the stateless renderer (`render_input`) and on the
//! damage-cached path (`render_input_cached`), where one cache sees every
//! intermediate frame and a fresh one jumps straight there.
//!
//! The decomposition law `rows*cell_h + frac == px`, `frac ∈ [0, cell_h)` is
//! proven in aterm-gui's `scroll_motion` and the `ScrollGlide` model; this pins
//! the render half at the `frac == 0` anchor every glide passes through.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};

const ROWS: usize = 6;
const COLS: usize = 20;
/// How deep into history the test scrolls (well past one page).
const TARGET_OFFSET: i32 = 9;

/// A terminal with distinctive numbered + styled lines deep enough that every
/// offset in `0..=TARGET_OFFSET` shows a different viewport.
fn seeded_terminal() -> Terminal {
    let mut t = Terminal::new(ROWS as u16, COLS as u16);
    for i in 0..40 {
        // Alternate colour/attribute so intermediate frames differ visibly.
        let line = if i % 2 == 0 {
            format!("\x1b[3{}mline {:02} abc\x1b[0m\r\n", (i % 6) + 1, i)
        } else {
            format!("\x1b[1mLINE {i:02} XYZ\x1b[0m\r\n")
        };
        t.process(line.as_bytes());
    }
    t
}

#[test]
fn stepped_scroll_matches_row_jump_stateless() {
    let mut r = Renderer::from_system(16.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");

    // Path A: one row at a time (the glide's ticks), rendering every frame.
    let mut stepped = seeded_terminal();
    let mut last = None;
    for _ in 0..TARGET_OFFSET {
        stepped.scroll_display(1);
        last = Some(r.render_input(&stepped.cell_frame(ROWS, COLS)));
    }
    let stepped_frame = last.expect("TARGET_OFFSET > 0 renders at least one frame");

    // Path B: one row-jump straight to the destination offset.
    let mut jumped = seeded_terminal();
    jumped.scroll_display(TARGET_OFFSET);
    let jumped_frame = r.render_input(&jumped.cell_frame(ROWS, COLS));

    assert_eq!(
        (stepped_frame.width, stepped_frame.height),
        (jumped_frame.width, jumped_frame.height),
        "stepped and jumped frames must agree on dimensions"
    );
    assert_eq!(
        stepped_frame.pixels, jumped_frame.pixels,
        "a viewport reached by row-steps must be byte-identical to the row-jump"
    );
    // Non-vacuity: the scrolled viewport differs from offset 0.
    let live = r.render_input(&seeded_terminal().cell_frame(ROWS, COLS));
    assert_ne!(
        live.pixels, jumped_frame.pixels,
        "control: the scrolled viewport must differ from the live bottom"
    );
}

#[test]
fn stepped_scroll_matches_row_jump_damage_cached() {
    let mut r = Renderer::from_system(16.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");

    // Path A: one persistent cache sees every intermediate frame, so stale
    // rows surviving a scroll would leave residue.
    let mut wc_stepped = WindowCpu::new();
    let mut stepped = seeded_terminal();
    // Seed the cache with the live frame first, as a real present does.
    let _ = r.render_input_cached(&mut wc_stepped, &stepped.cell_frame(ROWS, COLS));
    for _ in 0..TARGET_OFFSET {
        stepped.scroll_display(1);
        let _ = r.render_input_cached(&mut wc_stepped, &stepped.cell_frame(ROWS, COLS));
    }
    let a: Vec<u32> = {
        let v = r.render_input_cached(&mut wc_stepped, &stepped.cell_frame(ROWS, COLS));
        v.pixels().to_vec()
    };

    // Path B: a fresh cache jumps straight to the destination.
    let mut wc_jumped = WindowCpu::new();
    let mut jumped = seeded_terminal();
    jumped.scroll_display(TARGET_OFFSET);
    let b: Vec<u32> = {
        let v = r.render_input_cached(&mut wc_jumped, &jumped.cell_frame(ROWS, COLS));
        v.pixels().to_vec()
    };

    assert_eq!(
        a, b,
        "the damage-cached present of a stepped scroll must match the row-jump \
         byte-for-byte (no residue from intermediate animation frames)"
    );
    assert!(!a.is_empty(), "rendered framebuffer must be non-empty");
}
