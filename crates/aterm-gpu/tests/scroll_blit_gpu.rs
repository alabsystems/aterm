// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Whole-row scroll blit on the GPU: byte-identity gate. `compute_dirty_rows`
// returns `FullRepaint` for scrollback navigation even though the grid slid by a
// known row delta; like the CPU, the GPU consults `scroll_blit_plan`, shifts the
// offscreen's grid band with a staged texture-to-texture copy and re-rasters
// only the exposed strip. A rescued frame must equal a fresh full repaint. The
// fixture targets upward glyph overshoot at the exposed/retained boundary
// (`OVERSHOOT_APRON_ROWS`; hence parentheses, pipes, underscores, descenders)
// and wide cells riding the shift, in both directions.
//
// No GPU or no font: COULD NOT RUN.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{RenderInput, Theme};

use crate::common;

const ROWS: usize = 10;
const COLS: usize = 32;

/// A fresh GpuRenderer (no GPU: COULD NOT RUN), with lazy fallback parses settled so
/// every renderer rasterizes CJK identically, and the wall-clock heat shimmer
/// off so two renders of one input can agree.
fn fresh_gpu() -> GpuRenderer {
    let mut g = common::gpu(18.0, Theme::default());
    g.debug_block_on_lazy_fallbacks();
    g.set_shimmer(false);
    g
}

/// The ground truth: a brand-new GpuRenderer renders `input` with a full repaint
/// (Clear + every row) and reads it back.
fn fresh_render(input: &RenderInput) -> Vec<u32> {
    let mut g = fresh_gpu();
    let mut win = WindowGpu::new();
    g.render_input(&mut win, input, None).pixels
}

/// Deep scrollback whose rows carry overshooting glyphs and wide cells.
/// `hide_cursor` does not decide rescuability (`RenderInput::cursor_row` does not
/// change with `display_offset`); hiding it only drops the cursor's re-stamp and
/// ghost rows from the dirty set.
fn scrollback_term(hide_cursor: bool) -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    if hide_cursor {
        term.process(b"\x1b[?25l");
    }
    for i in 0..140 {
        term.process(format!("(|_gy) line {i} 日本語 tail\r\n").as_bytes());
    }
    term
}

#[test]
fn history_scroll_is_rescued_and_stays_byte_identical_to_a_full_repaint() {
    let mut gpu = fresh_gpu();
    let mut term = scrollback_term(true);
    let mut win = WindowGpu::new();
    // Warm the window at the bottom: no prior frame ⇒ a full repaint.
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );

    let mut rescued = 0u64;
    let mut checked = 0usize;
    // Into history (delta < 0, top strip exposed), then back (delta > 0,
    // bottom strip exposed, the apron direction). Different magnitudes sweep
    // odd and even pixel shifts (the shade-phase parity clause).
    for (notches, step) in [(10, 3i32), (10, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let before = gpu.scroll_rescues();
            let got = gpu.present_input_readback(&mut win, &input);
            rescued += gpu.scroll_rescues() - before;
            checked += 1;
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }
    // Reach: the rescue must carry the sweep. Some notches legitimately refuse
    // (the first after a direction change, or identical content hitting the
    // gate), so the floor is generous but non-trivial.
    assert!(
        rescued >= 14,
        "the scroll-blit rescue barely fired ({rescued}/{checked} notches) — the \
         byte-identity gate above is not testing it"
    );
    eprintln!("gpu scroll-blit: {rescued}/{checked} notches rescued");
}

#[test]
fn a_cursor_that_really_moves_still_refuses_the_scroll_rescue() {
    let mut gpu = fresh_gpu();
    // Move the cursor for real (a CUP in the same frame that scrolls, as when
    // a scroll lands on the same tick as shell output). The frame must fall
    // back to a full repaint and still match one.
    let mut term = scrollback_term(false);
    let mut win = WindowGpu::new();
    let input = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &input);

    term.scroll_display(1);
    term.process(b"\x1b[3;7H");
    let input = term.cell_frame(ROWS, COLS);
    let before = gpu.scroll_rescues();
    let got = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        gpu.scroll_rescues(),
        before,
        "a scroll frame that also MOVES the cursor must not be rescued"
    );
    assert_eq!(
        got.pixels,
        fresh_render(&input),
        "the refused frame must still match a full repaint"
    );
}

#[test]
fn a_visible_cursor_rides_the_rescue_and_stays_byte_identical() {
    let mut gpu = fresh_gpu();
    // With the cursor shown, every rescued frame must erase the blitted ghost
    // at `cursor_row - da` and re-stamp the cursor at `cursor_row`.
    let mut term = scrollback_term(false);
    let mut win = WindowGpu::new();
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );

    let mut rescued = 0u64;
    for (notches, step) in [(6, 3i32), (6, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let before = gpu.scroll_rescues();
            let got = gpu.present_input_readback(&mut win, &input);
            rescued += gpu.scroll_rescues() - before;
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "cursor-visible scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }
    assert!(
        rescued >= 8,
        "the rescue barely fired with a visible cursor ({rescued}) — the ghost-row \
         handling above is not being tested"
    );
    eprintln!("gpu scroll-blit (cursor shown): {rescued} notches rescued");
}

/// On the armed Metal arm the rescue's band copies ride the armed encode's own
/// command buffer ahead of the scissored pass (Submit A), so a rescued present
/// never waits on a command buffer of its own: the inline-wait count stays flat
/// and `metal_encode_band_shifts` grows by exactly the rescues taken. The
/// byte-identity oracle catches a copy that landed after the pass.
#[cfg(target_os = "macos")]
#[test]
fn a_rescued_scroll_present_parks_on_no_command_buffer_of_its_own() {
    let mut gpu = fresh_gpu();
    let mut term = scrollback_term(true);
    let mut win = WindowGpu::new();
    // Warm at the bottom (a full repaint), then take the baselines after it:
    // the warm frame is not a rescue and must not be charged to the sweep.
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );
    let waits_before = gpu.metal_encode_inline_waits();
    let shifts_before = gpu.metal_encode_band_shifts();
    let rescues_before = gpu.scroll_rescues();

    for (notches, step) in [(6, 3i32), (6, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let got = gpu.present_input_readback(&mut win, &input);
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }

    let rescued = gpu.scroll_rescues() - rescues_before;
    assert!(
        rescued >= 8,
        "the rescue barely fired ({rescued} notches) — the no-wait contract \
         below is not being tested"
    );
    assert_eq!(
        gpu.metal_encode_band_shifts() - shifts_before,
        rescued,
        "every rescued present must record its band copies INTO Submit A"
    );
    assert_eq!(
        gpu.metal_encode_inline_waits(),
        waits_before,
        "a rescued scroll present parked the UI thread on a command buffer of \
         its own ({} inline waits across {rescued} rescues)",
        gpu.metal_encode_inline_waits() - waits_before
    );
}
