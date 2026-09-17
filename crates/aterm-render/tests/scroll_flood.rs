// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! BOTTOM-PINNED OUTPUT FLOOD: the shift-aware damage diff.
//!
//! A flood keeps `display_offset` at 0 while every output line advances
//! `base_y`, so `compute_dirty_rows` admits the frame on its EQUAL-OFFSET arm
//! and then compares destination row `r` against SOURCE row `r`. The screen has
//! slid, so that comparison finds every row different: 49 of 50 rows repainted
//! per frame at EVERY output rate, from one line a frame to sixteen — the whole
//! screen re-rasterized to show one new line. The rigid E7 planner could not
//! rescue it either: the row the writer touched changes every frame, and the
//! rigid contract refuses the entire plan over a single changed retained row.
//!
//! `scroll_shift_plan(RetainedRows::Diff)` compares row `r` against row `r + da`
//! and marks what really changed, and the CPU path blits the rest.
//!
//! Two halves, because either alone is worthless:
//!   * the COUNT pin — a flood must repaint a handful of rows, not the screen
//!     (the defect this file exists for);
//!   * the BYTE-IDENTITY oracle — every rescued frame must equal a fresh full
//!     repaint, because a row marked clean is a row whose pixels were MOVED, and
//!     a shift-aware diff without a working pixel move is stale glass.

use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu};

fn renderer_px(px: f32) -> Option<Renderer> {
    Renderer::from_system(px, Theme::default()).map(|mut r| {
        r.debug_block_on_lazy_fallbacks();
        r
    })
}

/// Render through the warm damage cache, assert byte-identity against a fresh
/// full repaint of the same input, and return `(outcome, repainted_rows)`.
fn render_both(
    warm: &mut Renderer,
    wc: &mut WindowCpu,
    term: &mut Terminal,
    rows: usize,
    cols: usize,
    px: f32,
    label: &str,
) -> (DamageOutcome, usize) {
    let input = term.cell_frame(rows, cols);
    let (pixels, w, h) = {
        let view = warm.render_input_cached(wc, &input);
        (view.pixels().to_vec(), view.width(), view.height())
    };
    let outcome = wc.last_damage();
    let painted = match outcome {
        DamageOutcome::Full => rows,
        DamageOutcome::GateHit => 0,
        _ => wc.dirty_rows().iter().filter(|&&d| d).count(),
    };
    let mut fresh = renderer_px(px).expect("font (checked by caller)");
    let full = fresh.render_input(&input);
    assert_eq!((w, h), (full.width, full.height), "dims @ {label}");
    assert_eq!(pixels, full.pixels, "flood frame != full repaint @ {label}");
    (outcome, painted)
}

/// THE DEFECT, pinned as a count. A bottom-pinned flood at 1/2/4/8/16 lines per
/// frame must repaint the rows the flood actually exposed (plus the overshoot
/// apron), never the whole screen — and every frame must still be byte-identical
/// to a full repaint. Swept over font sizes because the seam geometry
/// (`cell_h` parity, glyph overshoot) is what makes the shift unsound when it is
/// unsound; 13/14 px are the sizes the E7 sweep found byte-diverging.
#[test]
fn bottom_pinned_flood_repaints_the_exposed_strip_not_the_screen() {
    for px in [13.0f32, 14.0, 16.0] {
        let Some(mut warm) = renderer_px(px) else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
        for rate in [1usize, 2, 4, 8, 16] {
            let mut wc = WindowCpu::new();
            let (rows, cols) = (50usize, 60usize);
            let mut term = Terminal::new(rows as u16, cols as u16);
            // DISTINCT lines: a uniform flood is the easy case (every row
            // compares equal and the frame gate-hits). Distinct lines are what
            // made the row-against-itself diff report the whole screen.
            for i in 0..200 {
                term.process(format!("line {i} \u{2502} output text here\r\n").as_bytes());
            }
            render_both(&mut warm, &mut wc, &mut term, rows, cols, px, "warmup");

            let mut painted_total = 0usize;
            let mut rescued = 0usize;
            let frames = 12usize;
            for f in 0..frames {
                for k in 0..rate {
                    term.process(format!("flood {f}.{k} \u{2502} more output\r\n").as_bytes());
                }
                let label = format!("px{px} rate{rate} f{f}");
                let (outcome, painted) =
                    render_both(&mut warm, &mut wc, &mut term, rows, cols, px, &label);
                painted_total += painted;
                if outcome
                    == (DamageOutcome::Scroll {
                        delta_rows: rate as i32,
                    })
                {
                    rescued += 1;
                }
            }
            // The ceiling, asserted FIRST because it is the defect: the exposed
            // strip (`rate`), the row the writer was still filling, and the
            // two-row overshoot apron. The pre-fix diff reported `rows - 1` = 49
            // at every rate, so this bound is what fails loudly if the
            // row-against-itself comparison ever comes back.
            let avg = painted_total as f64 / frames as f64;
            let ceiling = (rate + 4) as f64;
            assert!(
                avg <= ceiling,
                "flood repainted {avg} rows/frame @ px{px} rate{rate} (ceiling {ceiling} \
                 of {rows}) — the diff is comparing row r against row r again"
            );
            assert_eq!(
                rescued, frames,
                "every flood frame must take the shift-aware rescue @ px{px} rate{rate}"
            );
        }
    }
}

/// The same flood carrying the content that REFUTES a naive shift: accented
/// capitals / box glyphs on every line (upward overshoot into the row above,
/// which the blit cannot carry across a re-rastered seam) and a shade-dither
/// progress bar every fifth line (absolute-Y-parity rasters that a memmove by an
/// ODD pixel count lands at the wrong phase, so the planner re-rasters those rows
/// instead of blitting them). Byte-identity is the assertion; the rescue must
/// still fire, or it proves nothing.
///
/// The bar is periodic rather than on every line ON PURPOSE: a screen where EVERY
/// row is phase-sensitive and the shift is odd has nothing left to blit, the
/// planner's row set is no smaller than the ordinary diff's, and `render_core`
/// correctly declines the memmove. That is the planner working, not failing — but
/// it would make this test vacuous, so the fixture keeps blittable rows in play.
#[test]
fn flood_with_shade_and_overshoot_content_is_byte_exact() {
    for px in [13.0f32, 14.0, 16.0] {
        let Some(mut warm) = renderer_px(px) else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
        for rate in [1usize, 3] {
            let mut wc = WindowCpu::new();
            let (rows, cols) = (16usize, 30usize);
            let mut term = Terminal::new(rows as u16, cols as u16);
            let bar = "\u{2593}\u{2593}\u{2592}\u{2591}";
            let line = |i: usize, tag: String| {
                if i.is_multiple_of(5) {
                    format!("{bar} {tag} \u{2502}\u{2588}\r\n")
                } else {
                    format!("\u{c9}\u{c1}\u{d1}\u{c5} {tag} \u{2502}\u{2588}\r\n")
                }
            };
            for i in 0..80 {
                term.process(line(i, format!("r{i}")).as_bytes());
            }
            render_both(&mut warm, &mut wc, &mut term, rows, cols, px, "warmup");
            let mut rescued = 0usize;
            let frames = 12usize;
            for f in 0..frames {
                for k in 0..rate {
                    term.process(line(80 + f * rate + k, format!("f{f}.{k}")).as_bytes());
                }
                let (outcome, _) = render_both(
                    &mut warm,
                    &mut wc,
                    &mut term,
                    rows,
                    cols,
                    px,
                    &format!("shade px{px} rate{rate} f{f}"),
                );
                if matches!(outcome, DamageOutcome::Scroll { .. }) {
                    rescued += 1;
                }
            }
            assert!(
                rescued >= frames - 1,
                "adversarial flood must ride the rescue ({rescued}/{frames}) @ px{px} rate{rate}"
            );
        }
    }
}

/// A retained row that changes FAR from the exposed strip: a status line rewritten
/// in place at row 3 of the viewport while the bottom floods. The rigid planner
/// refuses this frame outright; the shift-aware one must mark that row — and the
/// two rows above it, whose blitted copies carry its old upward overshoot — and
/// still land byte-identical to a full repaint.
#[test]
fn a_changed_row_mid_screen_rides_the_flood_byte_exact() {
    for px in [13.0f32, 16.0] {
        let Some(mut warm) = renderer_px(px) else {
            eprintln!("SKIP: no system monospace font");
            return;
        };
        let mut wc = WindowCpu::new();
        let (rows, cols) = (24usize, 40usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        for i in 0..100 {
            term.process(format!("line {i} \u{c9} text\r\n").as_bytes());
        }
        render_both(&mut warm, &mut wc, &mut term, rows, cols, px, "warmup");
        let mut rescued = 0usize;
        let frames = 10usize;
        for f in 0..frames {
            // Rewrite the in-viewport status line, then park the cursor back at
            // the bottom-left (where the flood left it) so the frame is still a
            // static-cursor slide, then emit the flood line.
            term.process(format!("\x1b[4;1H\u{2593} status {f} \u{c5}\u{2588}").as_bytes());
            term.process(format!("\x1b[{rows};1H").as_bytes());
            term.process(format!("flood {f} \u{c9} text\r\n").as_bytes());
            let (outcome, _) = render_both(
                &mut warm,
                &mut wc,
                &mut term,
                rows,
                cols,
                px,
                &format!("status px{px} f{f}"),
            );
            if matches!(outcome, DamageOutcome::Scroll { .. }) {
                rescued += 1;
            }
        }
        assert!(
            rescued >= frames - 1,
            "a mid-screen rewrite must not sink the flood rescue ({rescued}/{frames}) @ px{px}"
        );
    }
}

/// THE BACKEND SPLIT, pinned as a relationship rather than a comment.
///
/// `RetainedRows::Rigid` is what the GPU present path consults and `Diff` is what
/// the CPU path consults, so the two must differ in exactly ONE way: a retained
/// row whose content changed. Everywhere Rigid says yes, Diff must say yes with a
/// byte-identical row set (or the refactor that introduced the policy silently
/// moved the GPU); and on a flood — one changed retained row per frame — Rigid
/// must refuse while Diff rescues, which is the whole reason the knob exists.
///
/// The GPU keeps Rigid because the choice was MEASURED, not assumed: on the
/// first-party Metal arm (M4 Pro, 50x200) its pixel move is a staged texture band
/// copy, and `gpu_present_flood_50x200` reads 343 us/frame with the 49-row
/// scissored encode against 690 us/frame with the shift plan and a 328-instance
/// encode — 47% slower for 16x fewer instances. The same copy against a FULL
/// repaint (`gpu_present_scrollback_50x200`) is 40% FASTER, which is why the rigid
/// rescue stays on the GPU and the flood rescue does not.
#[test]
fn rigid_and_diff_agree_except_on_a_changed_retained_row() {
    use aterm_render::{RetainedRows, scroll_blit_plan, scroll_shift_plan};

    let (rows, cols) = (24usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..200 {
        term.process(format!("line {i} some output text\r\n").as_bytes());
    }

    // (a) A HISTORY SCROLL with no content change: both policies must agree,
    // set for set — this is the GPU's arm, and it must not have moved.
    let before = term.cell_frame(rows, cols);
    term.scroll_display(3);
    let after = term.cell_frame(rows, cols);
    let (mut rigid_rows, mut diff_rows) = (Vec::new(), Vec::new());
    let rigid = scroll_blit_plan(
        &before,
        &after,
        false,
        None,
        false,
        None,
        16,
        &mut rigid_rows,
    );
    let diff = scroll_shift_plan(
        &before,
        &after,
        false,
        None,
        false,
        None,
        16,
        RetainedRows::Diff,
        &mut diff_rows,
    );
    assert_eq!(rigid, Some(-3), "the history scroll must plan rigidly");
    assert_eq!(diff, rigid, "Diff must accept every frame Rigid accepts");
    assert_eq!(
        diff_rows, rigid_rows,
        "Diff must mark exactly Rigid's rows when no retained row changed"
    );

    // (b) A FLOOD frame: one retained row (the line the writer was filling)
    // changes, so Rigid refuses the whole plan and Diff rescues it.
    term.scroll_display(-3); // back to the bottom
    let pinned = term.cell_frame(rows, cols);
    term.process(b"a flooded line of output\r\n");
    let flooded = term.cell_frame(rows, cols);
    assert_eq!(pinned.display_offset, 0, "precondition: bottom-pinned");
    assert_eq!(flooded.display_offset, 0, "precondition: bottom-pinned");
    assert!(
        flooded.base_y > pinned.base_y,
        "precondition: the flood advanced the anchor"
    );
    let (mut rigid_rows, mut diff_rows) = (Vec::new(), Vec::new());
    assert_eq!(
        scroll_blit_plan(
            &pinned,
            &flooded,
            false,
            None,
            false,
            None,
            16,
            &mut rigid_rows,
        ),
        None,
        "the rigid contract must refuse a flood (a retained row changed)"
    );
    let delta = scroll_shift_plan(
        &pinned,
        &flooded,
        false,
        None,
        false,
        None,
        16,
        RetainedRows::Diff,
        &mut diff_rows,
    );
    assert_eq!(delta, Some(1), "the shift-aware plan must rescue the flood");
    let marked = diff_rows.iter().filter(|&&d| d).count();
    assert!(
        marked <= 5,
        "a one-line flood must mark the exposed strip + apron, not {marked} of {rows} rows"
    );
}
