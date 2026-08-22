// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! E7 WHOLE-ROW SCROLL-BLIT differential oracle.
//!
//! A rigid history scroll (offset AND anchor both shifted) used to `FullRepaint`
//! every visible row every notch, even though the grid merely slid by a known
//! integer row delta and those rows were already rasterized last frame. The
//! blit now shifts the retained rows inside the cached framebuffer and
//! re-rasterizes ONLY the newly-exposed strip (+ the cursor's old/new rows).
//!
//! GUARD: the presented frame must be BYTE-IDENTICAL to a fresh full repaint of
//! the same input — the blit is faster, never different. Exercised across scroll
//! directions and deltas, with a visible cursor and wide (CJK) glyphs in the
//! retained rows, each notch checked against a from-scratch `render_input`.

use aterm_core::terminal::Terminal;
use aterm_render::{DamageOutcome, Renderer, Theme, WindowCpu};

fn renderer() -> Option<Renderer> {
    renderer_px(16.0)
}

/// A warm/fresh renderer at an EXPLICIT font px — the E7 overshoot bugs only
/// manifest at specific cell_h/baseline geometries (the seam byte-diverges at
/// px 13/14 but not at 16), so the differential gate MUST sweep px, not pin one.
fn renderer_px(px: f32) -> Option<Renderer> {
    Renderer::from_system(px, Theme::default()).map(|mut r| {
        r.debug_block_on_lazy_fallbacks();
        r
    })
}

/// Render through the warm damage cache and assert byte-identity against a fresh
/// full repaint of the same input (the differential oracle); return the warm
/// path's damage outcome.
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
    let mut fresh = renderer().expect("font (checked by caller)");
    let full = fresh.render_input(&input);
    assert_eq!((w, h), (full.width, full.height), "dims @ {label}");
    assert_eq!(pixels, full.pixels, "scroll-blit != full repaint @ {label}");
    outcome
}

/// Scrolling DEEP into history, notch by notch: every retained row is blitted,
/// only the newly-exposed top strip is re-rasterized, and each frame is
/// byte-identical to a full repaint. Wide glyphs ride the retained rows; the
/// cursor is live-visible on the bottom row throughout.
#[test]
fn deep_history_scroll_blits_and_matches_full_repaint() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut wc = WindowCpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Deep history with wide (CJK) glyphs interleaved so retained rows carry
    // double-width cells across the shift.
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

/// Scrolling BACK toward the bottom (the opposite sign) blits with a positive
/// delta and exposes the BOTTOM strip; still byte-identical to a full repaint.
#[test]
fn scroll_toward_bottom_blits_the_other_direction() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

/// A single-row history scroll blits with |delta| == 1 (the tightest exposed
/// strip) and stays byte-exact in both directions. Entering/leaving history flips
/// the raw cursor's visibility, so those two boundary frames deliberately repaint
/// in full rather than shifting stale cursor pixels into/out of the viewport.
#[test]
fn single_row_scroll_is_byte_exact() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

// ---- FRAME-EQUALITY DIFFERENTIAL over the E7 review's REFUTING cases ----
//
// The blit REFUTED byte-identity in two ways the plain-ASCII oracles above never
// exercised: (1) SHADE DITHER PARITY — U+2591–2593 key on absolute framebuffer
// Y-parity, so a memmove by an ODD pixel count (`da·cell_h`, e.g. a single notch
// with cell_h == 19) lands them at the wrong phase; (2) UPWARD GLYPH OVERSHOOT —
// tall symbols / accented capitals overshoot into the row above, which the blit
// dropped at the exposed-strip seam. Each test below drives adversarial content
// and asserts the warm (blit) frame is byte-identical to a fresh full repaint,
// notch by notch, over both directions and multi-row/multi-notch deltas.

/// Progress-bar / htop-style shade rows (░▒▓) plus accented capitals and tall
/// box symbols, scrolled a single notch at a time (the ODD `da·cell_h` that trips
/// BOTH the shade-phase seam and the overshoot apron) — every frame byte-exact.
#[test]
fn shade_and_overshoot_single_notch_is_byte_exact() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // The shade-parity re-raster must ride the BLIT fast path, not silently fall to
    // a full repaint (which would make byte-identity vacuous and kill the win).
    assert!(
        blits >= 25,
        "shade rows must scroll via the blit ({blits}/30 notches took it)"
    );
}

/// The same adversarial content scrolled the OTHER direction and by MULTI-ROW
/// deltas (both parities of `da·cell_h`): the even shift keeps every shade row on
/// the pure blit fast path, the odd shift re-rasters the shade rows — both must be
/// byte-identical to a full repaint.
#[test]
fn shade_and_overshoot_multi_row_both_directions_byte_exact() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
    // Back further (top exposed strip), alternating odd/even multi-row deltas.
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
    // Forward toward the bottom (bottom exposed strip), same alternation.
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

/// A LIVE, static cursor riding a shade+overshoot scroll: the cursor's re-stamp /
/// blitted-ghost rows land amid the shade-parity and apron re-rasters, and the
/// composed frame must still equal a full repaint every notch.
#[test]
fn shade_scroll_with_live_cursor_is_byte_exact() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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

/// A full row of shade with a tall/accented glyph in the row DIRECTLY BELOW each
/// shade row — the case a shade re-raster must NOT lose (the row-below's upward
/// overshoot into the shade band) nor double (its own overshoot over the blitted
/// row above). Byte-exact confirms the upper+lower apron pair.
#[test]
fn shade_row_over_tall_glyph_row_is_byte_exact() {
    let Some(mut warm) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut wc = WindowCpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..100 {
        // Alternate a pure-shade row and a tall-accent row so every shade band has
        // an overshooting neighbour above AND below across the shift.
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

// ---- THE GATE: full geometry × direction × glyph-class differential sweep ----
//
// The single-geometry oracles above are HOLLOW: they pin `from_system(16.0)`, and
// the E7 upward-overshoot seam (a row's band carries its BELOW-neighbour's
// overshoot, since `Scale::NORMAL.clip_y0 = i32::MIN`) byte-diverges from a full
// repaint only at particular cell_h/baseline geometries — px 13/14 refuted it
// where 16 passed. This sweep is the real gate: for EVERY (px × scroll direction
// × glyph class) it drives the warm blit path notch-by-notch and asserts the
// presented frame is byte-identical to a from-scratch full repaint, so the
// GENERAL overshoot invariant is proven geometry-independent, not patched per
// geometry.

/// The adversarial glyph classes. Each generator returns line `i`'s text (no
/// newline). MIXED alternates class per row so adjacent retained/exposed rows
/// carry DIFFERENT overshoot profiles across the shift — the case a per-row
/// re-raster must reconcile on both edges.
fn class_line(class: &str, i: usize) -> String {
    match class {
        // Plain ASCII — the overshoot-free control.
        "plain" => format!("line {i} content"),
        // Accented capitals ÉÁÑÅÊÍ — tall diacritics overshoot UPWARD into the
        // row above (the exact bug-3 repro content).
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

/// Present `input` through the warm blit path and, pixel-for-pixel, against a
/// fresh full repaint. On ANY divergence, panic with the first few (x,y) seams
/// and the total count (the E7 review's diagnostic form) rather than an
/// unreadable whole-Vec `assert_eq`. Returns the warm damage outcome so the
/// caller can prove the blit fast path was actually taken (non-vacuous identity).
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

/// THE E7 GATE. Sweep cell geometry (px) × scroll direction (into history AND
/// toward bottom) × delta (1/2/3-row, single- and multi-row exposed strips) ×
/// glyph class (plain, accented capitals, shade dither, box drawing, wide/emoji,
/// and MIXED adjacent rows), and for EVERY combination assert the warm blit frame
/// is byte-identical to a fresh full repaint at every notch. Non-hollow: the blit
/// fast path must actually carry a large majority of the notches (else identity is
/// vacuous — a silent full-repaint would also pass).
#[test]
fn e7_scroll_blit_geometry_direction_glyph_sweep_is_byte_exact() {
    // 8 geometries — including the 13/14 that refuted the seam and even/odd cell_h
    // (odd → odd `da·cell_h` shade-phase re-raster) — so the invariant is proven
    // geometry-independent, not tuned for one px.
    let px_sweep = [11.0f32, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0];
    let classes = ["plain", "accent", "shade", "box", "wide", "mixed"];
    let deltas = [1i32, 2, 3];
    let (rows, cols) = (8usize, 22usize);

    let mut combos = 0usize; // px × class × dir × delta
    let mut notch_renders = 0usize; // byte-identity assertions
    let mut blits = 0usize; // notches that took the blit fast path

    for &px in &px_sweep {
        let Some(mut warm) = renderer_px(px) else {
            eprintln!("SKIP px={px}: no system monospace font");
            continue;
        };
        let mut fresh = renderer_px(px).expect("font (warm built at same px)");

        for class in classes {
            for &delta in &deltas {
                // --- INTO HISTORY: top strip exposed, delta_rows == -delta. ---
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
                // --- TOWARD BOTTOM: bottom strip exposed, delta_rows == +delta
                //     (the bug-3 direction: the retained row directly ABOVE the
                //     exposed bottom strip is stale unless re-rastered). ---
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

    if combos == 0 {
        eprintln!("SKIP: no system monospace font at any swept px");
        return;
    }
    // Non-hollow: the blit fast path must carry the clear majority of notches —
    // a silent fall-through to full repaint would satisfy byte-identity vacuously
    // and forfeit the entire E7 win.
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
