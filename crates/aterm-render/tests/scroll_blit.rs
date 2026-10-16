// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-row scroll-blit differential oracle.
//!
//! A rigid history scroll shifts the retained rows inside the cached
//! framebuffer and re-rasterizes only the exposed strip (and the cursor's old
//! and new rows). Every presented frame must be byte-identical to a fresh full
//! repaint of the same input, across directions, deltas, geometries and glyph
//! classes.

use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu};

fn renderer() -> Renderer {
    renderer_px(16.0)
}

/// A renderer at an explicit font px: the overshoot seam diverges only at some
/// cell_h/baseline geometries, so the gate sweeps px.
fn renderer_px(px: f32) -> Renderer {
    Renderer::from_system(px, Theme::default())
        .map(|mut r| {
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// Render through the warm damage cache, assert byte-identity with a fresh full
/// repaint, and return the warm path's damage outcome.
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

/// Scrolling deep into history notch by notch blits every retained row and
/// re-rasters only the exposed top strip; wide glyphs ride the retained rows.
#[test]
fn deep_history_scroll_blits_and_matches_full_repaint() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Wide (CJK) glyphs in every line, so retained rows carry double-width
    // cells across the shift.
    for i in 0..120 {
        term.process(format!("line {i} 日本語 tail\r\n").as_bytes());
    }
    // Warm the cache at the bottom (a full repaint — no prior frame).
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "warmup");

    let mut blits = 0usize;
    for step in 0..40 {
        term.scroll_display(3); // back into history: offset += 3
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("back[{step}]"),
        );
        if matches!(outcome, DamageOutcome::Scroll { delta_rows: -3 }) {
            blits += 1;
        }
    }
    assert!(
        blits >= 35,
        "the whole-row blit must carry the deep scroll (took it {blits}/40 notches)"
    );
}

/// Scrolling toward the bottom blits with a positive delta and exposes the
/// bottom strip.
#[test]
fn scroll_toward_bottom_blits_the_other_direction() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (6usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..80 {
        term.process(format!("row {i} content\r\n").as_bytes());
    }
    // Park deep in history, warm the cache there.
    term.scroll_display(40);
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "deep-warmup");

    let mut blits = 0usize;
    for step in 0..20 {
        term.scroll_display(-2); // toward the bottom: offset -= 2, anchor += 2
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("fwd[{step}]"),
        );
        if matches!(outcome, DamageOutcome::Scroll { delta_rows: 2 }) {
            blits += 1;
        }
    }
    assert!(
        blits >= 15,
        "the reverse-direction blit must carry the scroll ({blits}/20 notches)"
    );
}

/// A one-row history scroll blits with |delta| == 1 in both directions.
/// Entering and leaving history flips the cursor's visibility, so those two
/// frames repaint in full.
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

// ---- Shade parity and upward overshoot ----
//
// Shade dithers U+2591–2593 key on absolute framebuffer Y-parity, so a shift
// by an odd pixel count (`da·cell_h`) lands them at the wrong phase unless
// re-rastered; tall glyphs overshoot into the row above, which the exposed-strip
// seam must keep.

/// Shade rows (░▒▓) with accented capitals and box symbols, one notch at a time
/// (an odd `da·cell_h` when cell_h is odd).
#[test]
fn shade_and_overshoot_single_notch_is_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (8usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..120 {
        // ▓▓▒░ shade run + accented capitals (upward overshoot) + box glyphs.
        let bar = "\u{2593}\u{2593}\u{2592}\u{2591}";
        term.process(
            format!("{bar} \u{c9}\u{c1}\u{d1}\u{c5} r{i} \u{2502}\u{2588}\r\n").as_bytes(),
        );
    }
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "warmup");
    let mut blits = 0usize;
    for step in 0..30 {
        term.scroll_display(1); // one notch back → |da| == 1 (odd shift when cell_h odd)
        let outcome = render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("shade1[{step}]"),
        );
        if matches!(outcome, DamageOutcome::Scroll { delta_rows: -1 }) {
            blits += 1;
        }
    }
    // Shade rows ride the blit path rather than fall to a full repaint, which
    // would make byte-identity vacuous.
    assert!(
        blits >= 25,
        "shade rows must scroll via the blit ({blits}/30 notches took it)"
    );
}

/// The same content scrolled both directions by 1 and 2 rows (both parities of
/// `da·cell_h`): an even shift blits shade rows, an odd one re-rasters them.
#[test]
fn shade_and_overshoot_multi_row_both_directions_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (10usize, 30usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..200 {
        let bar = "\u{2591}\u{2592}\u{2593}\u{2588}";
        term.process(format!("{bar}\u{c0}g\u{ca}y{i} \u{2551}\u{256c}\r\n").as_bytes());
    }
    // Park deep, warm there.
    term.scroll_display(80);
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "deep-warmup");
    // Further back (top strip exposed), alternating odd/even deltas.
    for step in 0..20 {
        let delta = if step % 2 == 0 { 1 } else { 2 };
        term.scroll_display(delta);
        render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("back-multi[{step}]"),
        );
    }
    // Toward the bottom (bottom strip exposed), same alternation.
    for step in 0..20 {
        let delta = if step % 2 == 0 { -2 } else { -1 };
        term.scroll_display(delta);
        render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("fwd-multi[{step}]"),
        );
    }
}

/// A live, static cursor during a shade + overshoot scroll: the cursor's rows
/// re-stamp amid the shade and apron re-rasters, still byte-exact.
#[test]
fn shade_scroll_with_live_cursor_is_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (7usize, 26usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..90 {
        let bar = "\u{2592}\u{2593}\u{2591}";
        term.process(format!("{bar} \u{c9}tall{i} \u{2588}\r\n").as_bytes());
    }
    // Leave a visible cursor on the last row (no trailing newline).
    term.process("\u{2593}\u{2593} \u{c1}live".as_bytes());
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "cursor-warmup");
    for step in 0..24 {
        term.scroll_display(1);
        render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("cursor-shade[{step}]"),
        );
    }
}

/// A shade row above a tall-glyph row: a shade re-raster must neither lose the
/// row below's upward overshoot into the shade band nor double its own over the
/// blitted row above.
#[test]
fn shade_row_over_tall_glyph_row_is_byte_exact() {
    let mut warm = renderer();
    let mut wc = WindowCpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..100 {
        // Alternate shade and tall-accent rows: every shade band has an
        // overshooting neighbour above and below.
        if i % 2 == 0 {
            term.process(
                b"\xe2\x96\x93\xe2\x96\x93\xe2\x96\x93\xe2\x96\x93\xe2\x96\x93\xe2\x96\x93\r\n",
            );
        } else {
            term.process(format!("\u{c9}\u{c1}\u{d1}\u{c5}\u{ca}{i}\r\n").as_bytes());
        }
    }
    render_both(&mut warm, &mut wc, &mut term, rows, cols, "warmup");
    for step in 0..30 {
        term.scroll_display(1);
        render_both(
            &mut warm,
            &mut wc,
            &mut term,
            rows,
            cols,
            &format!("stack[{step}]"),
        );
    }
}

// ---- Geometry × direction × glyph-class sweep ----
//
// A row's band carries its below-neighbour's upward overshoot
// (`Scale::NORMAL.clip_y0 = i32::MIN`), and the blit seam diverges from a full
// repaint only at some cell_h/baseline geometries (px 13/14, not 16), so the
// gate sweeps px.

/// Line `i`'s text (no newline) for each adversarial glyph class. `mixed`
/// alternates classes per row, so adjacent rows carry different overshoot
/// profiles across the shift.
fn class_line(class: &str, i: usize) -> String {
    match class {
        // Plain ASCII — the overshoot-free control.
        "plain" => format!("line {i} content"),
        // Accented capitals ÉÁÑÅÊÍ: tall diacritics overshoot up into the row
        // above.
        "accent" => format!("\u{c9}\u{c1}\u{d1}\u{c5}\u{ca}\u{cd} r{i}"),
        // Procedural shade dithers ░▒▓ (+ full block) — absolute-Y-parity keyed.
        "shade" => format!("\u{2591}\u{2592}\u{2593}\u{2588} b{i}"),
        // Box drawing │─┼╬║ — cell-spanning strokes at the band edges.
        "box" => format!("\u{2502}\u{2500}\u{253c}\u{256c}\u{2551} x{i}"),
        // Wide (CJK) + emoji — double-width cells riding the shift.
        "wide" => format!("\u{65e5}\u{672c}\u{8a9e}\u{1f525} w{i}"),
        // Adjacent rows differ: even → tall accents, odd → shade dither.
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

/// Present `input` through the warm blit path and compare it with a fresh full
/// repaint, panicking with the first few diverging (x,y) and the total count.
/// Returns the warm damage outcome.
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

/// The gate: px × direction × delta (1/2/3 rows) × glyph class, each notch's
/// blit frame byte-identical to a fresh full repaint, with the blit carrying
/// most of the notches.
#[test]
fn e7_scroll_blit_geometry_direction_glyph_sweep_is_byte_exact() {
    // Odd and even cell_h (an odd `da·cell_h` re-rasters shade rows), including
    // px 13/14, where the overshoot seam diverges.
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
                // --- Into history: top strip exposed, delta_rows == -delta. ---
                {
                    let mut wc = WindowCpu::new();
                    let mut term = Terminal::new(rows as u16, cols as u16);
                    for i in 0..90 {
                        term.process(class_line(class, i).as_bytes());
                        term.process(b"\r\n");
                    }
                    // Warm the cache at the bottom (full repaint, no prior frame).
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
                // --- Toward bottom: bottom strip exposed, delta_rows == +delta;
                //     the retained row directly above the strip must re-raster. ---
                {
                    let mut wc = WindowCpu::new();
                    let mut term = Terminal::new(rows as u16, cols as u16);
                    for i in 0..90 {
                        term.process(class_line(class, i).as_bytes());
                        term.process(b"\r\n");
                    }
                    // Park deep in history, warm there.
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

    // Non-vacuity: the blit carries most notches; a silent full repaint would
    // pass byte-identity trivially.
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
