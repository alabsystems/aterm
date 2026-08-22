// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// E7 WHOLE-ROW SCROLL BLIT — the GPU arm's byte-identity gate.
//
// `compute_dirty_rows` returns `FullRepaint` for ordinary scrollback navigation
// (display_offset AND the absolute anchor both shifted) even though the grid
// merely slid by a known integer row delta. The CPU backend has always rescued
// that verdict with `scroll_blit_plan` — memmove the retained rows, re-raster
// only the newly-exposed strip — while the GPU present path re-encoded every
// row of every scrolled frame. The GPU now consults the SAME planner and shifts
// the offscreen's grid band with a staged texture-to-texture copy.
//
// THE CONTRACT, and the only thing that makes the rescue admissible: a rescued
// frame must be BYTE-IDENTICAL to a fresh full GPU repaint of the same input.
// This is the same oracle pattern `scissor_repaint.rs` uses (a separate
// GpuRenderer renders the input with Clear + every row), applied to the frame
// class that oracle deliberately expected to fall back.
//
// The fixture is chosen to attack the two seams the CPU planner needed explicit
// machinery for:
//   * UPWARD GLYPH OVERSHOOT at the boundary between the re-rastered exposed
//     strip and the rigidly-shifted retained region (the OVERSHOOT_APRON_ROWS
//     reconciliation) — hence parentheses, pipes, underscores and descenders,
//     which are the glyphs that overshoot their cell band;
//   * WIDE (CJK) cells riding the retained rows across the shift.
// Both scroll directions are swept: into history (top strip exposed) and back
// toward the bottom (bottom strip exposed, the apron direction).
//
// Gated: no GPU or no system font => the test no-ops (returns).

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{RenderInput, Theme};

const ROWS: usize = 10;
const COLS: usize = 32;

/// A fresh GpuRenderer (or skip-marker). Blocks on the lazy fallback parses so
/// the reused renderer and every fresh oracle rasterize the CJK rows identically,
/// and disables the wall-clock heat shimmer (two renders of one input can never
/// byte-agree with it live) — the `scissor_repaint.rs` recipe.
fn fresh_gpu() -> Option<GpuRenderer> {
    match GpuRenderer::new(18.0, Theme::default()) {
        Ok(mut g) => {
            g.debug_block_on_lazy_fallbacks();
            g.set_shimmer(false);
            Some(g)
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

/// The ground truth: a brand-new GpuRenderer renders `input` with a FULL repaint
/// (Clear + every row) and reads it back.
fn fresh_render(input: &RenderInput) -> Vec<u32> {
    let mut g = fresh_gpu().expect("GPU was available a moment ago");
    let mut win = WindowGpu::new();
    g.render_input(&mut win, input, None).pixels
}

/// Deep scrollback whose rows carry overshooting glyphs and wide cells.
///
/// `hide_cursor` is NOT what decides rescuability, though it looks like it should
/// be. A history scroll does not move the cursor at all as the renderer sees it:
/// `RenderInput::cursor_row` is the LIVE grid's viewport row and is independent of
/// `display_offset` (measured — it stays put across `scroll_display`), so a plain
/// scroll leaves every field `scroll_blit_plan`'s cursor clause compares equal and
/// the frame is rescued with the cursor visible. The planner still handles the
/// cursor's PIXELS moving, because the band shift relocates them: it marks both
/// the re-stamp row and the blitted ghost row `input.cursor_row - da`. Hiding the
/// cursor therefore only removes those two rows from the exposed strip's dirty
/// set; the negative control lives in the test below, which moves the cursor for
/// real.
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
    let Some(mut gpu) = fresh_gpu() else { return };
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
    // INTO HISTORY (delta < 0, the TOP strip is exposed), then BACK TOWARD THE
    // BOTTOM (delta > 0, the BOTTOM strip is exposed — the overshoot-apron
    // direction). Different magnitudes so an odd pixel shift (the shade-phase
    // parity clause) and an even one are both swept.
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
    // REACH: the rescue must actually carry the sweep, or the byte-identity
    // assertions above proved nothing about it. A handful of notches legitimately
    // refuse (the first notch after a direction change re-exposes the cursor
    // clause, and a notch that lands on identical history content is a plain
    // gate hit), so the floor is generous but non-trivial.
    assert!(
        rescued >= 14,
        "the scroll-blit rescue barely fired ({rescued}/{checked} notches) — the \
         byte-identity gate above is not testing it"
    );
    eprintln!("gpu scroll-blit: {rescued}/{checked} notches rescued");
}

#[test]
fn a_cursor_that_really_moves_still_refuses_the_scroll_rescue() {
    let Some(mut gpu) = fresh_gpu() else { return };
    // The refusal clause is about the cursor's GRID position changing, not about
    // the viewport sliding under it. So move it for real — a CUP to a different
    // row/column in the same frame that scrolls — which is the shape of a scroll
    // arriving on the same tick as shell output. The frame must fall back to the
    // always-correct full repaint, and must still match one.
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
    let Some(mut gpu) = fresh_gpu() else { return };
    // The POSITIVE half of the same clause, and the one that actually exercises
    // the ghost-row machinery: with the cursor SHOWN, every rescued frame must
    // erase the blitted cursor ghost at `cursor_row - da` and re-stamp a crisp
    // cursor at `cursor_row`. A stale ghost or a missing stamp is a pixel
    // difference the oracle catches immediately.
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
