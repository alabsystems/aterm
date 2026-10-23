// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bottom-pinned output flood: the shift-aware damage diff.

use aterm_core::render::{DecoBlend, DecoGlyph, InkCell, WordDecoration};
use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu};

fn renderer_px(px: f32) -> Renderer {
    Renderer::from_system(px, Theme::default())
        .map(|mut r| {
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
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
    let mut fresh = renderer_px(px);
    let full = fresh.render_input(&input);
    assert_eq!((w, h), (full.width, full.height), "dims @ {label}");
    assert_eq!(pixels, full.pixels, "flood frame != full repaint @ {label}");
    (outcome, painted)
}

/// A bottom-pinned flood at 1/2/4/8/16 lines per frame repaints the exposed
/// rows plus the overshoot apron, never the screen, and every frame is
/// byte-identical to a full repaint. Swept over px because the seam geometry
/// (`cell_h` parity, glyph overshoot) decides when a shift is unsound (13/14 px
/// are the seam-hazard sizes, as in `scroll_blit.rs`).
#[test]
fn bottom_pinned_flood_repaints_the_exposed_strip_not_the_screen() {
    for px in [13.0f32, 14.0, 16.0] {
        let mut warm = renderer_px(px);
        for rate in [1usize, 2, 4, 8, 16] {
            let mut wc = WindowCpu::new();
            let (rows, cols) = (50usize, 60usize);
            let mut term = Terminal::new(rows as u16, cols as u16);
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

/// The bar is periodic on purpose: with every row phase-sensitive and an odd
/// shift nothing is left to blit, `render_core` declines the memmove, and the
/// test would be vacuous.
#[test]
fn flood_with_shade_and_overshoot_content_is_byte_exact() {
    for px in [13.0f32, 14.0, 16.0] {
        let mut warm = renderer_px(px);
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

/// A status line rewritten in place at viewport row 3 while the bottom floods:
/// the shift-aware plan marks that row and the two above it (whose blitted
/// copies carry its old upward overshoot), byte-identical to a full repaint.
#[test]
fn a_changed_row_mid_screen_rides_the_flood_byte_exact() {
    for px in [13.0f32, 16.0] {
        let mut warm = renderer_px(px);
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

/// Why the GPU keeps Rigid is measured at `RetainedRows::Rigid`.
#[test]
fn rigid_and_diff_agree_except_on_a_changed_retained_row() {
    use aterm_render::{RetainedRows, scroll_blit_plan, scroll_shift_plan};

    let (rows, cols) = (24usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..200 {
        term.process(format!("line {i} some output text\r\n").as_bytes());
    }

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
        aterm_render::RowBands::new(0, 16),
        &mut rigid_rows,
    );
    let diff = scroll_shift_plan(
        &before,
        &after,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        RetainedRows::Diff,
        &mut diff_rows,
    );
    assert_eq!(rigid, Some(-3), "the history scroll must plan rigidly");
    assert_eq!(diff, rigid, "Diff must accept every frame Rigid accepts");
    assert_eq!(
        diff_rows, rigid_rows,
        "Diff must mark exactly Rigid's rows when no retained row changed"
    );

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
            aterm_render::RowBands::new(0, 16),
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
        aterm_render::RowBands::new(0, 16),
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

/// How a case lays the card line's ink and star for the frame `k` lines into
/// the flood: the line's row then, and the row and column the overlays sit on.
type Overlay = fn(usize, usize) -> (Vec<InkCell>, Vec<WordDecoration>);

/// The ink of an 8-cell word starting at column 2 of `row` (a gradient), in
/// colour family `tone`.
fn word_ink(row: usize, tone: u8) -> Vec<InkCell> {
    (0..8)
        .map(|i| InkCell {
            row: row as u16,
            col: 2 + i,
            color: [0x40 + tone, 0x30 + 0x18 * i as u8, 0xf0 - tone],
        })
        .collect()
}

/// The star leading the line on `row`, its sprite in the row's own band
/// (`dy` 0) unless `dy` says otherwise.
fn star(row: usize, col: u16, dy: i8) -> WordDecoration {
    WordDecoration {
        row: row as u16,
        col,
        dx: 0,
        dy,
        glyph: DecoGlyph::Star4,
        blend: DecoBlend::Add,
        color: 0x0060_5030,
        alpha: 220,
    }
}

/// The negative controls are mutations (run by hand, recorded in the lane
/// report): dropping `row_overlays_differ_shifted` from the `Diff` arm of
/// `scroll_shift_plan` fails "held" (the slid ink where the frame draws none),
/// and dropping its decoration half fails "star moves".
#[test]
fn ink_and_an_in_band_star_ride_the_flood() {
    let cases: [(&str, bool, Overlay); 6] = [
        ("rides", true, |line, _| {
            (word_ink(line, 0), vec![star(line, 0, 0)])
        }),
        ("held", false, |_, _| {
            (word_ink(12, 0), vec![star(12, 0, 0)])
        }),
        ("recoloured", false, |line, k| {
            (word_ink(line, 8 * k as u8), vec![star(line, 0, 0)])
        }),
        ("gone", false, |line, k| {
            if k % 2 == 0 {
                (word_ink(line, 0), vec![star(line, 0, 0)])
            } else {
                (Vec::new(), Vec::new())
            }
        }),
        ("star moves", false, |line, k| {
            (word_ink(line, 0), vec![star(line, k as u16 % 3, 0)])
        }),
        ("jittered", false, |line, _| {
            (word_ink(line, 0), vec![star(line, 0, -3)])
        }),
    ];
    for px in [13.0f32, 14.0, 16.0] {
        let mut warm = renderer_px(px);
        let mut fresh = renderer_px(px);
        for (name, slides, lay) in cases {
            let (rows, cols) = (20usize, 30usize);
            let mut term = Terminal::new(rows as u16, cols as u16);
            for i in 0..60 {
                term.process(format!("line {i} \u{c9}\u{c1} text\r\n").as_bytes());
            }
            term.process(b"\xe2\x9c\xa6 the card line \xc3\x89\r\n");
            for i in 0..4 {
                term.process(format!("after {i} \u{c5} text\r\n").as_bytes());
            }
            let mut wc = WindowCpu::new();
            let frame = |term: &mut Terminal, k: usize| {
                let mut input = term.cell_frame(rows, cols);
                let (ink, decos) = lay(rows - 6 - k, k);
                input.ink = ink;
                input.word_decorations = decos;
                input
            };
            let first = frame(&mut term, 0);
            let _ = warm.render_input_cached(&mut wc, &first);
            for k in 1..=6 {
                term.process(format!("flood {k} \u{c9} text\r\n").as_bytes());
                let input = frame(&mut term, k);
                let cached = warm.render_input_cached(&mut wc, &input).pixels().to_vec();
                let damage = wc.last_damage();
                let what = format!("px {px}, {name}, line {k}");
                assert_eq!(
                    cached,
                    fresh.render_input(&input).pixels,
                    "{what}: the frame equals a full repaint ({damage:?})"
                );
                if name == "jittered" {
                    assert!(
                        !matches!(damage, DamageOutcome::Scroll { .. }),
                        "{what}: a jittered star refuses the rescue ({damage:?})"
                    );
                    continue;
                }
                assert_eq!(
                    damage,
                    DamageOutcome::Scroll { delta_rows: 1 },
                    "{what}: the overlays keep the rescue"
                );
                let line = rows - 6 - k;
                let dirty = wc.dirty_rows();
                if slides {
                    assert!(!dirty[line], "{what}: the line's row is slid: {dirty:?}");
                } else {
                    assert!(
                        dirty[line] || dirty[12] || dirty[line + 1],
                        "{what}: the overlays' rows are rebuilt: {dirty:?}"
                    );
                }
            }
        }
    }
}

/// The negative control is a mutation (run by hand, recorded in the lane
/// report): dropping `row_overlays_differ_shifted` from the `Rigid` check of
/// `scroll_shift_plan` plans the held case.
#[test]
fn the_rigid_plan_refuses_overlays_that_stay_on_their_row() {
    use aterm_render::{RowBands, scroll_blit_plan};

    let (rows, cols) = (24usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..200 {
        term.process(format!("line {i} some output text\r\n").as_bytes());
    }
    let mut before = term.cell_frame(rows, cols);
    before.ink = word_ink(10, 0);
    before.word_decorations = vec![star(10, 0, 0)];
    term.scroll_display(3);
    let after = term.cell_frame(rows, cols);
    let plan = |ink: Vec<InkCell>, decos: Vec<WordDecoration>| {
        let mut cur = after.clone();
        cur.ink = ink;
        cur.word_decorations = decos;
        scroll_blit_plan(
            &before,
            &cur,
            false,
            None,
            false,
            None,
            RowBands::new(0, 16),
            &mut Vec::new(),
        )
    };
    assert_eq!(
        plan(word_ink(13, 0), vec![star(13, 0, 0)]),
        Some(-3),
        "overlays that rode the scroll keep the rigid plan"
    );
    assert_eq!(
        plan(word_ink(10, 0), vec![star(13, 0, 0)]),
        None,
        "ink left on its row refuses it"
    );
    assert_eq!(
        plan(word_ink(13, 0), vec![star(10, 0, 0)]),
        None,
        "a star left on its row refuses it"
    );
}
