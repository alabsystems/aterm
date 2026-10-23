// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-row scroll-blit differential oracle.

use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu};

fn renderer() -> Renderer {
    renderer_px(16.0)
}

fn renderer_px(px: f32) -> Renderer {
    Renderer::from_system(px, Theme::default())
        .map(|mut r| {
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

fn render_both(
    warm: &mut Renderer,
    wc: &mut WindowCpu,
    term: &mut Terminal,
    rows: usize,
    cols: usize,
    label: &str,
) -> DamageOutcome {
    let input = term.cell_frame(rows, cols);
    let (pixels, w, h) = {
        let view = warm.render_input_cached(wc, &input);
        (view.pixels().to_vec(), view.width(), view.height())
    };
    let outcome = wc.last_damage();
    let mut fresh = renderer();
    let full = fresh.render_input(&input);
    assert_eq!((w, h), (full.width, full.height), "dims @ {label}");
    assert_eq!(pixels, full.pixels, "scroll-blit != full repaint @ {label}");
    outcome
}

#[test]
fn single_row_scroll_is_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (5usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..50 {
        term.process(format!("l{i}\r\n").as_bytes());
    }
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "warmup");

    term.scroll_display(1);
    let entering = render_both(
        &mut warm,
        &mut wc,
        &mut term,
        rows,
        cols,
        "one[live-to-history]",
    );
    assert_eq!(
        entering,
        DamageOutcome::Full,
        "hiding the live cursor must reject scroll rescue"
    );

    for step in 1..10 {
        term.scroll_display(1);
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("one[{step}]"),
        );
        assert!(
            matches!(outcome, DamageOutcome::Scroll { delta_rows: -1 }),
            "a one-row scroll must take the blit @ step {step} (got {outcome:?})"
        );
    }

    for step in 1..10 {
        term.scroll_display(-1);
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("one-back[{step}]"),
        );
        assert!(
            matches!(outcome, DamageOutcome::Scroll { delta_rows: 1 }),
            "a one-row history-to-history scroll must blit @ step {step} (got {outcome:?})"
        );
    }

    term.scroll_display(-1);
    let leaving = render_both(
        &mut warm,
        &mut wc,
        &mut term,
        rows,
        cols,
        "one[history-to-live]",
    );
    assert_eq!(
        leaving,
        DamageOutcome::Full,
        "revealing the live cursor must reject scroll rescue"
    );
}

fn class_line(class: &str, i: usize) -> String {
    match class {
        "plain" => format!("line {i} content"),
        "accent" => format!("\u{c9}\u{c1}\u{d1}\u{c5}\u{ca}\u{cd} r{i}"),
        "shade" => format!("\u{2591}\u{2592}\u{2593}\u{2588} b{i}"),
        "box" => format!("\u{2502}\u{2500}\u{253c}\u{256c}\u{2551} x{i}"),
        "wide" => format!("\u{65e5}\u{672c}\u{8a9e}\u{1f525} w{i}"),
        "mixed" => {
            if i.is_multiple_of(2) {
                format!("\u{c9}\u{c1}\u{d1}\u{c5}\u{ca} m{i}")
            } else {
                format!("\u{2593}\u{2592}\u{2591}\u{2588} m{i}")
            }
        }
        other => panic!("unknown class {other}"),
    }
}

fn assert_blit_equals_full(
    warm: &mut Renderer,
    wc: &mut WindowCpu,
    fresh: &mut Renderer,
    term: &mut Terminal,
    rows: usize,
    cols: usize,
    label: &str,
) -> DamageOutcome {
    let input = term.cell_frame(rows, cols);
    let (pixels, w, h) = {
        let view = warm.render_input_cached(wc, &input);
        (view.pixels().to_vec(), view.width(), view.height())
    };
    let outcome = wc.last_damage();
    let full = fresh.render_input(&input);
    assert_eq!((w, h), (full.width, full.height), "dims @ {label}");
    if pixels != full.pixels {
        let mut diffs = 0usize;
        let mut first: Vec<String> = Vec::new();
        for (idx, (&a, &b)) in pixels.iter().zip(&full.pixels).enumerate() {
            if a != b {
                diffs += 1;
                if first.len() < 6 {
                    first.push(format!(
                        "({},{}) blit {a:#08X} != full {b:#08X}",
                        idx % w,
                        idx / w
                    ));
                }
            }
        }
        panic!(
            "scroll-blit != full repaint @ {label}: {diffs} diverging px; first: [{}]",
            first.join("; ")
        );
    }
    outcome
}

#[test]
fn e7_scroll_blit_geometry_direction_glyph_sweep_is_byte_exact() {
    let px_sweep = [11.0f32, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0];
    let classes = ["plain", "accent", "shade", "box", "wide", "mixed"];
    let deltas = [1i32, 2, 3];
    let (rows, cols) = (8usize, 22usize);

    let mut combos = 0usize; // px × class × dir × delta
    let mut notch_renders = 0usize; // byte-identity assertions
    let mut blits = 0usize; // notches that took the blit fast path

    for &px in &px_sweep {
        let mut warm = renderer_px(px);
        let mut fresh = renderer_px(px);

        for class in classes {
            for &delta in &deltas {
                {
                    let mut wc = WindowCpu::new();
                    let mut term = Terminal::new(rows as u16, cols as u16);
                    for i in 0..90 {
                        term.process(class_line(class, i).as_bytes());
                        term.process(b"\r\n");
                    }
                    assert_blit_equals_full(
                        &mut warm, &mut wc, &mut fresh, &mut term, rows, cols, "warm",
                    );
                    combos += 1;
                    for step in 0..8 {
                        term.scroll_display(delta); // back into history
                        let o = assert_blit_equals_full(
                            &mut warm,
                            &mut wc,
                            &mut fresh,
                            &mut term,
                            rows,
                            cols,
                            &format!("px{px}/{class}/hist/d{delta}[{step}]"),
                        );
                        notch_renders += 1;
                        if matches!(o, DamageOutcome::Scroll { delta_rows } if delta_rows == -delta)
                        {
                            blits += 1;
                        }
                    }
                }
                {
                    let mut wc = WindowCpu::new();
                    let mut term = Terminal::new(rows as u16, cols as u16);
                    for i in 0..90 {
                        term.process(class_line(class, i).as_bytes());
                        term.process(b"\r\n");
                    }
                    term.scroll_display(40);
                    assert_blit_equals_full(
                        &mut warm,
                        &mut wc,
                        &mut fresh,
                        &mut term,
                        rows,
                        cols,
                        "deep-warm",
                    );
                    combos += 1;
                    for step in 0..8 {
                        term.scroll_display(-delta); // toward the bottom
                        let o = assert_blit_equals_full(
                            &mut warm,
                            &mut wc,
                            &mut fresh,
                            &mut term,
                            rows,
                            cols,
                            &format!("px{px}/{class}/botm/d{delta}[{step}]"),
                        );
                        notch_renders += 1;
                        if matches!(o, DamageOutcome::Scroll { delta_rows } if delta_rows == delta)
                        {
                            blits += 1;
                        }
                    }
                }
            }
        }
    }

    assert!(
        blits * 2 >= notch_renders,
        "blit fast path took only {blits}/{notch_renders} notches — identity is \
         near-vacuous; the sweep must actually exercise the blit"
    );
    eprintln!(
        "E7 sweep: {combos} scenarios (px×class×dir×delta), {notch_renders} notch \
         frames byte-identical to full repaint, {blits} via the blit fast path"
    );
}
