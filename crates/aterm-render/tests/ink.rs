// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Animated-ink fg overrides (Sparkle Words v2, `RenderInput.ink`) on the CPU
// renderer. The channel contract under test:
//   * empty ink is byte-identical to the pre-ink path (also after
//     `clear_overlays`, the `image plain` contract);
//   * ink substitutes for the cell fg at EVERY fg consult site — glyph blit,
//     combining marks, underline / strike / overline — so an inked cell renders
//     byte-identically to the same cell recoloured via SGR truecolor fg;
//   * ordering: ink FIRST, then the min-contrast floor, then (on selected
//     cells) the selection fg floor — the floors see the FINAL ink colour;
//   * an explicit SGR 58 underline colour still wins over ink;
//   * a wide glyph is governed by its LEAD cell's InkCell (a continuation-only
//     entry is inert);
//   * dirty gate: settled (non-empty but EQUAL) ink gate-hits with zero rows
//     marked; changed ink marks exactly the prev∪cur ink rows.

use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;
use aterm_render::{
    DirtyDecision, Frame, Renderer, Theme, compute_dirty_rows, floor_min_contrast_fg,
    floor_selection_fg, rgb_to_u32,
};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default()).map(|mut r| {
        // Deterministic pixels: block on the lazy fallback parses so a parse
        // landing between two renders can't recolour a "must not change" frame.
        r.debug_block_on_lazy_fallbacks();
        r
    })
}

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

#[test]
fn empty_ink_is_byte_identical_also_after_clear_overlays() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lultra think");

    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // Explicitly empty ink (feature on, nothing matched / everything truncated).
    let mut input = term.cell_frame(3, 12);
    assert!(input.ink.is_empty());
    input.ink.clear();
    let again = rend.render_input(&input).pixels.clone();
    assert_eq!(base, again, "empty ink must not change any pixel");

    // `clear_overlays` (the `image plain` capture) strips ink like every other
    // bling layer: a previously-inked input renders the bare frame afterwards.
    let mut inked = term.cell_frame(3, 12);
    inked.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: [0xFF, 0x00, 0xFF],
    }];
    let with_ink = rend.render_input(&inked).pixels.clone();
    assert_ne!(base, with_ink, "non-empty ink must recolour something");
    inked.clear_overlays();
    assert!(inked.ink.is_empty(), "clear_overlays must strip ink");
    let stripped = rend.render_input(&inked).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// The definitive substitution proof: inking a cell is byte-identical to
/// recolouring the SAME text via SGR truecolor fg — across a plain glyph, a
/// combining-mark cell (é as e + U+0301), a wide CJK cell (lead-governed), an
/// underlined cell, a struck cell and a CURLY-underlined cell. That is all five
/// CPU consult sites: glyph blit, combining blit, pass 3's underline colour,
/// pass 3's strike/overline colour, and pass 3b's AA undercurl.
///
/// The curl is called out because it is a SEPARATE pass with its OWN colour
/// derivation (a fresh `InkWalk`/`CharFgWalk`, not pass 3's advanced ones), and
/// it drifted: until ed9f774b the CPU curl kept the cell's own fg after ink.
/// The straight-underline cell does not cover it — `\x1b[4m` never reaches
/// pass 3b. The GPU twin (`ink_gpu_matches_cpu`) gained its curly cell in that
/// same fix; this CPU-only proof did not, and it is the only one that runs on a
/// host without a working wgpu device.
#[test]
fn ink_renders_byte_identically_to_sgr_fg_recolor() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let ink: [u8; 3] = [0x7C, 0xC8, 0xFF];
    let (rows, cols) = (2usize, 16usize);

    // Plain text, default fg, ink overrides on every lead cell.
    // Row 0: "e<combining acute> x<SGR4 underline> s<SGR9 strike> w<SGR 4:3 curl>";
    // row 1: 猫 (wide).
    let mut term_a = Terminal::new(rows as u16, cols as u16);
    term_a.process(
        "\x1b[?25le\u{0301} \x1b[4mx\x1b[24m \x1b[9ms\x1b[29m \x1b[4:3mw\x1b[24m\r\n猫".as_bytes(),
    );
    let mut input_a = term_a.cell_frame(rows, cols);
    input_a.ink = vec![
        InkCell {
            row: 0,
            col: 0,
            color: ink,
        },
        InkCell {
            row: 0,
            col: 2,
            color: ink,
        },
        InkCell {
            row: 0,
            col: 4,
            color: ink,
        },
        InkCell {
            row: 0,
            col: 6,
            color: ink,
        }, // curly-underlined w: pass 3b, the fifth consult site
        InkCell {
            row: 1,
            col: 0,
            color: ink,
        }, // 猫's LEAD cell
    ];

    // The same text recoloured with SGR 38;2 truecolor fg — no ink.
    let mut term_b = Terminal::new(rows as u16, cols as u16);
    term_b.process(
        "\x1b[?25l\x1b[38;2;124;200;255me\u{0301}\x1b[39m \x1b[38;2;124;200;255m\x1b[4mx\x1b[24m\
\x1b[39m \x1b[38;2;124;200;255m\x1b[9ms\x1b[29m\x1b[39m \x1b[38;2;124;200;255m\x1b[4:3mw\x1b[24m\
\x1b[39m\r\n\x1b[38;2;124;200;255m猫\x1b[39m"
            .as_bytes(),
    );
    let input_b = term_b.cell_frame(rows, cols);

    let fa = rend.render_input(&input_a).pixels.clone();
    let fb = rend.render_input(&input_b).pixels.clone();
    assert_eq!(
        fa, fb,
        "ink must substitute for the cell fg at every consult site (glyph, \
         combining, straight underline, strike, AA undercurl, wide lead)"
    );

    // Non-vacuous: the recolour actually changed pixels vs the un-inked frame.
    let mut plain = term_a.cell_frame(rows, cols);
    plain.ink.clear();
    assert_ne!(fa, rend.render_input(&plain).pixels);
}

/// At full glyph coverage the blit lands the EXACT host-resolved ink bytes
/// (blend at cov 255 is the identity): the endpoint-exactness bar. U+2588 FULL
/// BLOCK is procedurally drawn at coverage 255 across the whole cell.
#[test]
fn ink_endpoint_exact_at_full_coverage() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(1, 4);
    term.process("\x1b[?25l█".as_bytes());
    let ink: [u8; 3] = [0x12, 0x34, 0xAB];
    let mut input = term.cell_frame(1, 4);
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink,
    }];
    let f = rend.render_input(&input);
    let px = cell_pixels(&f, cw, ch, 0, 0);
    assert!(
        px.contains(&rgb_to_u32(ink)),
        "cov==255 must land the exact ink bytes (no colour math in the renderer)"
    );
}

/// Ordering: ink FIRST, then the selection fg floor on selected cells — the
/// floor sees (and fixes) the FINAL ink colour, so selection legibility wins
/// over shimmer. Pinned with an ink colour deliberately illegible against the
/// selection band: the drawn colour is `floor_selection_fg(ink, sel_bg)`
/// exactly, and the raw ink bytes never reach the frame.
#[test]
fn ink_then_selection_floor_applies_in_that_order() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(1, 4);
    term.process("\x1b[?25l█".as_bytes());
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 0, SelectionSide::Right);
        sel.complete_selection();
    }
    // Near the default selection band colour (0x33415E): guaranteed under the
    // 4.5:1 selection floor, so the floor MUST move it.
    let ink: [u8; 3] = [0x33, 0x41, 0x5E];
    let sel_bg = rend.effective_selection_bg();
    let expected = floor_selection_fg(rgb_to_u32(ink), sel_bg);
    assert_ne!(
        expected,
        rgb_to_u32(ink),
        "test premise: the floor must actually move this ink colour"
    );

    let mut input = term.cell_frame(1, 4);
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink,
    }];
    let f = rend.render_input(&input);
    let px = cell_pixels(&f, cw, ch, 0, 0);
    assert!(
        px.contains(&expected),
        "the selected inked cell must carry the selection-floored ink colour"
    );
    assert!(
        !px.contains(&rgb_to_u32(ink)),
        "the raw (unfloored) ink bytes must never reach a selected cell"
    );
}

/// Ordering: ink FIRST, then the per-cell minimum-contrast floor on unselected
/// cells — the host-configured floor guarantees the FINAL ink colour's
/// readability against the cell's own bg. Same endpoint-exact pin as above.
#[test]
fn ink_then_min_contrast_floor_applies_in_that_order() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(1, 4);
    term.process("\x1b[?25l█".as_bytes());
    // Dark ink on the dark default bg (0x111318): illegible until floored.
    let ink: [u8; 3] = [0x16, 0x18, 0x1D];
    let bg = Theme::default().bg;
    let expected = floor_min_contrast_fg(rgb_to_u32(ink), bg, 4.5);
    assert_ne!(
        expected,
        rgb_to_u32(ink),
        "test premise: the floor must actually move this ink colour"
    );

    rend.set_minimum_contrast(4.5);
    let mut input = term.cell_frame(1, 4);
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink,
    }];
    let f = rend.render_input(&input);
    let px = cell_pixels(&f, cw, ch, 0, 0);
    assert!(
        px.contains(&expected),
        "the floored ink colour must land on the low-contrast inked cell"
    );
    assert!(
        !px.contains(&rgb_to_u32(ink)),
        "the raw (unfloored) ink bytes must not reach the frame with the floor on"
    );
}

/// An explicit SGR 58 underline colour wins over ink: with SGR 58 set, two
/// frames differing ONLY in ink colour are byte-identical on an underlined
/// SPACE cell (the underline is the sole fg consumer there); without SGR 58
/// the underline follows ink exactly (solid fill == exact ink bytes).
#[test]
fn sgr58_explicit_underline_color_wins_over_ink() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let ink_a: [u8; 3] = [0xFF, 0x00, 0x00];
    let ink_b: [u8; 3] = [0x00, 0xFF, 0x00];

    // SGR 58 explicit underline colour on an underlined space.
    let mut term = Terminal::new(1, 4);
    term.process(b"\x1b[?25l\x1b[4m\x1b[58;2;10;20;30m \x1b[0m");
    let mut input = term.cell_frame(1, 4);
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink_a,
    }];
    let fa = rend.render_input(&input).pixels.clone();
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink_b,
    }];
    let fb = rend.render_input(&input).pixels.clone();
    assert_eq!(fa, fb, "SGR 58 must win over ink: ink colour is irrelevant");

    // Control (non-vacuous): WITHOUT SGR 58 the underline follows ink.
    let mut term = Terminal::new(1, 4);
    term.process(b"\x1b[?25l\x1b[4m \x1b[0m");
    let mut input = term.cell_frame(1, 4);
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink_a,
    }];
    let fa = rend.render_input(&input);
    assert!(
        cell_pixels(&fa, cw, ch, 0, 0).contains(&rgb_to_u32(ink_a)),
        "without SGR 58 the underline fill must be the exact ink bytes"
    );
    input.ink = vec![InkCell {
        row: 0,
        col: 0,
        color: ink_b,
    }];
    let fb = rend.render_input(&input);
    assert_ne!(
        fa.pixels, fb.pixels,
        "without SGR 58 the underline must follow the ink colour"
    );
}

/// The LEAD cell's InkCell governs a wide glyph; an entry on the continuation
/// column alone is inert (that column carries no glyph and no decoration).
#[test]
fn wide_cjk_continuation_only_ink_is_inert() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut term = Terminal::new(1, 6);
    term.process("\x1b[?25l猫".as_bytes());
    let base = rend.render_input(&term.cell_frame(1, 6)).pixels.clone();
    let mut input = term.cell_frame(1, 6);
    input.ink = vec![InkCell {
        row: 0,
        col: 1, // the continuation column, NOT the lead
        color: [0xFF, 0x00, 0xFF],
    }];
    let f = rend.render_input(&input).pixels.clone();
    assert_eq!(
        base, f,
        "a continuation-column InkCell must be inert (lead cell governs)"
    );
}

/// Dirty gate: settled ink — non-empty but EQUAL between frames — is a gate
/// hit (zero rendering; the steady state costs nothing); CHANGED ink marks
/// exactly the prev∪cur ink rows (rows only — ink never spills its row band)
/// and must not gate-hit.
#[test]
fn ink_dirty_gate_settled_hits_changed_marks_prev_union_cur_rows() {
    let mut term = Terminal::new(4, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let settled = vec![
        InkCell {
            row: 1,
            col: 2,
            color: [1, 2, 3],
        },
        InkCell {
            row: 1,
            col: 3,
            color: [4, 5, 6],
        },
    ];

    // Settled: equal non-empty ink on both frames ⇒ gate hit, nothing marked.
    let mut prev = term.cell_frame(4, 8);
    let mut cur = term.cell_frame(4, 8);
    prev.ink = settled.clone();
    cur.ink = settled;
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(!d.ink_changed, "equal ink must not set ink_changed");
    assert!(
        d.is_gate_hit(),
        "settled (non-empty but equal) ink must gate-hit: steady state is free"
    );
    assert!(dirty.iter().all(|&b| !b), "settled ink must mark no rows");

    // Changed: ink moves row 1 → row 3 ⇒ exactly rows {1, 3} marked, no gate.
    cur.ink = vec![InkCell {
        row: 3,
        col: 2,
        color: [1, 2, 3],
    }];
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.ink_changed, "changed ink must set ink_changed");
    assert!(!d.is_gate_hit(), "changed ink must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(
        marked,
        vec![1, 3],
        "changed ink must mark exactly the prev∪cur ink rows"
    );
}

/// §7.4/§14 P5 perf gate — `bench_ink_apply`: the §4.1 merge-walk overhead of
/// a FULL 512-InkCell load (the MAX_INK_CELLS cap) in a full-frame render vs a
/// no-ink baseline of the identical frame; both medians are reported. The
/// merge-walk is O(1)/cell — one pointer compare per column — so the overhead
/// must stay in the noise class, not the per-cell-binary-search class. Runs
/// alternate so drift hits both sides. The measured numbers land in
/// PROOF_CARRYING_PERFORMANCE.md ("Sparkle Words v2.1"). Timing-sensitive, so
/// it follows the repo's manual-timing idiom:
///
/// ```sh
/// cargo test -p aterm-render --release --test ink \
///   bench_ink_apply -- --ignored --nocapture
/// ```
#[test]
#[ignore = "perf gate (design §7.4): run manually in --release with --ignored --nocapture"]
fn bench_ink_apply() {
    use std::time::Instant;
    let Some(mut rend) = renderer() else {
        panic!("bench needs a system monospace font");
    };
    let (rows, cols) = (40usize, 120usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(3);
    for r in 0..rows {
        term.process(format!("\x1b[{};1H{}", r + 1, &line[..cols]).as_bytes());
    }

    let base_input = term.cell_frame(rows, cols);
    let mut inked_input = base_input.clone();
    // Exactly 512 InkCells (the cap), sorted (row, col), spread over every row
    // — 12–13 word-shaped cells per row, the §7.3 worst-case ink screen.
    let mut ink = Vec::with_capacity(512);
    'fill: for r in 0..rows as u16 {
        for c in 0..13u16 {
            if ink.len() == 512 {
                break 'fill;
            }
            let col = c * 9 + (r % 3); // staggered word-ish spread, still sorted
            ink.push(InkCell {
                row: r,
                col,
                color: [0x7C, 0xC8, 0xFF],
            });
        }
    }
    assert_eq!(ink.len(), 512, "the MAX_INK_CELLS worst case");
    inked_input.ink = ink;

    // Warm both paths.
    for _ in 0..4 {
        let _ = rend.render_input(&base_input);
        let _ = rend.render_input(&inked_input);
    }
    let iters = 60usize;
    let (mut t_base, mut t_ink) = (Vec::with_capacity(iters), Vec::with_capacity(iters));
    for _ in 0..iters {
        let s = Instant::now();
        let _ = rend.render_input(&base_input);
        t_base.push(s.elapsed());
        let s = Instant::now();
        let _ = rend.render_input(&inked_input);
        t_ink.push(s.elapsed());
    }
    t_base.sort();
    t_ink.sort();
    let (mb, mi) = (t_base[iters / 2], t_ink[iters / 2]);
    let overhead_us = (mi.as_nanos() as i128 - mb.as_nanos() as i128) as f64 / 1000.0;
    println!(
        "bench_ink_apply: no-ink full-frame median {mb:?}, 512-InkCell merge-walk \
         median {mi:?} — overhead {overhead_us:.1} us/frame ({:.1} ns/inked cell; \
         120x40 grid)",
        overhead_us * 1000.0 / 512.0
    );
    assert!(
        overhead_us < 500.0,
        "§7.4 gate: 512-cell ink apply overhead {overhead_us:.0} us >= 500 us/frame"
    );
}
