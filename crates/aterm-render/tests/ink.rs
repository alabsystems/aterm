// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Animated-ink fg overrides (Sparkle Words v2, `RenderInput.ink`) on the CPU renderer. The
// empty-ink no-op is the `ink` row of `tests/empty_channels.rs`. Pinned here: ink
// substitutes for the cell fg at every consult site; it applies before the min-contrast and
// selection floors; an explicit SGR 58 underline colour wins; a wide glyph follows its lead
// cell's InkCell; the dirty gate.

use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;
use aterm_render::{
    DirtyDecision, Frame, Renderer, Theme, compute_dirty_rows, floor_min_contrast_fg,
    floor_selection_fg, rgb_to_u32,
};

pub(crate) fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .map(|mut r| {
            // A lazy fallback parse landing between two renders would break the
            // byte comparisons.
            r.debug_block_on_lazy_fallbacks();
            r
        })
        .expect("no font: build with aterm-render's default embedded-font feature")
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

/// Inking a cell is byte-identical to recolouring the same text via SGR truecolor fg, at
/// all five CPU consult sites: glyph blit, combining blit (é as e + U+0301), pass 3's
/// underline colour, pass 3's strike/overline colour and pass 3b's AA undercurl, plus a
/// lead-governed wide CJK cell. The curly cell is needed because `\x1b[4m` never reaches
/// pass 3b. The GPU twin is `ink_gpu_matches_cpu`; this one runs without a wgpu device.
#[test]
fn ink_renders_byte_identically_to_sgr_fg_recolor() {
    let mut rend = renderer();
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

/// Ink applies before the selection fg floor on selected cells: with an ink colour
/// illegible on the selection band, the drawn colour is exactly
/// `floor_selection_fg(ink, sel_bg)` and the raw ink bytes never reach the frame.
#[test]
fn ink_then_selection_floor_applies_in_that_order() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(1, 4);
    term.process("\x1b[?25l█".as_bytes());
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 0, SelectionSide::Right);
        sel.complete_selection();
    }
    // The default selection band colour (0x33415E): under the 4.5:1 floor, so it moves.
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

/// Ink applies before the per-cell minimum-contrast floor on unselected cells, so the floor
/// sees the final ink colour.
#[test]
fn ink_then_min_contrast_floor_applies_in_that_order() {
    let mut rend = renderer();
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

/// An explicit SGR 58 underline colour wins over ink: on an underlined space (the underline
/// is its only fg consumer) two frames differing only in ink are byte-identical; without
/// SGR 58 the underline is the exact ink bytes.
#[test]
fn sgr58_explicit_underline_color_wins_over_ink() {
    let mut rend = renderer();
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

    // Control (non-vacuous): without SGR 58 the underline follows ink.
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

/// The lead cell's InkCell governs a wide glyph; an entry on the continuation
/// column alone is inert (that column carries no glyph and no decoration).
#[test]
fn wide_cjk_continuation_only_ink_is_inert() {
    let mut rend = renderer();
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

/// Settled ink (non-empty but equal between frames) gate-hits; changed ink marks exactly
/// the prev∪cur ink rows and does not gate-hit.
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
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
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
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
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
