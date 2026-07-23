// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Sparkle-word decoration compositing for the CPU renderer: an empty
// `word_decorations` list is byte-identical to the pre-feature render, and a
// non-empty list stamps sprites over exactly the targeted cell (and leaves the
// rest of the frame untouched).

use aterm_core::terminal::Terminal;
use aterm_render::{
    DecoBlend, DecoGlyph, DirtyDecision, Frame, Renderer, Theme, WindowCpu, WordDecoration,
    compute_dirty_rows,
};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A settled (frame-over-frame identical) LIFTED additive sparkle: `dy != 0`
/// makes its stamp spill into the neighbouring row's pixel band — the v2 nova
/// ember regime (`dy = -cell_h/3`).
fn lifted_add_deco(row: u16, col: u16, dy: i8) -> WordDecoration {
    WordDecoration {
        row,
        col,
        dx: 0,
        dy,
        glyph: DecoGlyph::Star4,
        blend: DecoBlend::Add,
        color: 0x0060_5030,
        alpha: 220,
    }
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
fn empty_decorations_are_byte_identical() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut term = Terminal::new(3, 12);
    term.process(b"i love cats");

    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    let mut input = term.cell_frame(3, 12);
    assert!(input.word_decorations.is_empty());
    let again = rend.render_input(&input).pixels.clone();
    assert_eq!(base, again, "empty decorations must not change any pixel");

    // Explicitly empty list (host feature on, no match) is also identical.
    input.word_decorations.clear();
    let still = rend.render_input(&input).pixels.clone();
    assert_eq!(base, still);
}

#[test]
fn paw_over_blend_paints_the_target_cell_only() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"i love cats");

    let base = rend.render_input(&term.cell_frame(3, 12));
    let base_target = cell_pixels(&base, cw, ch, 0, 7);
    let base_far = cell_pixels(&base, cw, ch, 2, 0);

    let mut input = term.cell_frame(3, 12);
    input.word_decorations.push(WordDecoration {
        row: 0,
        col: 7,
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Paw,
        blend: DecoBlend::Over,
        color: 0x00FF_00FF, // bright magenta — distinct from text/bg
        alpha: 255,
    });
    let f = rend.render_input(&input);
    let now_target = cell_pixels(&f, cw, ch, 0, 7);
    let now_far = cell_pixels(&f, cw, ch, 2, 0);

    assert_ne!(
        base_target, now_target,
        "the decorated cell must have changed pixels"
    );
    assert_eq!(
        base_far, now_far,
        "a cell far from the decoration must be untouched"
    );
    // The paw's solid core lands the exact stamp colour somewhere in the cell.
    assert!(
        now_target.contains(&0x00FF_00FF),
        "Over-blend at alpha 255 must paint the stamp colour at the paw core"
    );
}

#[test]
fn add_blend_only_brightens() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"fuck this");

    let base = rend.render_input(&term.cell_frame(3, 12));
    let base_target = cell_pixels(&base, cw, ch, 0, 0);

    let mut input = term.cell_frame(3, 12);
    input.word_decorations.push(WordDecoration {
        row: 0,
        col: 0,
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Star4,
        blend: DecoBlend::Add,
        color: 0x0040_4040,
        alpha: 255,
    });
    let f = rend.render_input(&input);
    let now_target = cell_pixels(&f, cw, ch, 0, 0);

    // Additive light never darkens any channel of any pixel.
    for (b, n) in base_target.iter().zip(now_target.iter()) {
        for sh in [16, 8, 0] {
            let bc = (b >> sh) & 0xff;
            let nc = (n >> sh) & 0xff;
            assert!(nc >= bc, "additive blend darkened a channel");
        }
    }
    assert_ne!(base_target, now_target, "additive sparkle must brighten");
}

/// Dirty-row closure for the settled-deco dy spill: a SETTLED lifted deco
/// (unchanged stream, so `deco_changed` is false) whose row goes dirty for an
/// UNRELATED reason must drag its spill-neighbour row into the dirty set —
/// else `draw_decorations` re-stamps it and the `dy` spill re-Adds onto the
/// neighbour's NOT-rebuilt pixels. Chains iterate to a fixpoint, and a fully
/// settled frame (no dirt at all) still gate-hits with zero rows marked.
#[test]
fn settled_deco_dy_spill_marks_neighbour_of_unrelated_dirt_to_fixpoint() {
    const CELL_H: usize = 16;
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };
    let mut term = Terminal::new(8, 12);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let mut dirty = Vec::new();

    // (a) Settled deco at row 4, dy < 0 (spill into row 3): unrelated content
    // dirt on row 4 must mark rows {3, 4}.
    let mut prev = term.cell_frame(8, 12);
    prev.word_decorations.push(lifted_add_deco(4, 9, -5));
    term.process(b"\x1b[5;1Hdirt");
    let mut cur = term.cell_frame(8, 12);
    cur.word_decorations.push(lifted_add_deco(4, 9, -5));
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.deco_changed,
        "a settled stream must not set deco_changed"
    );
    assert_eq!(
        marked(&dirty),
        vec![3, 4],
        "unrelated dirt on a lifted deco's row must mark the dy-spill neighbour"
    );

    // (b) Chain to a fixpoint: decos at rows 4 (dy > 0) and 5 (dy > 0); dirt
    // on row 4 marks 5, whose own spill then marks 6.
    let mut prev = term.cell_frame(8, 12);
    prev.word_decorations.push(lifted_add_deco(4, 9, 4));
    prev.word_decorations.push(lifted_add_deco(5, 2, 4));
    term.process(b"\x1b[5;1Hmore");
    let mut cur = term.cell_frame(8, 12);
    cur.word_decorations = prev.word_decorations.clone();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.deco_changed,
        "a settled stream must not set deco_changed"
    );
    assert_eq!(
        marked(&dirty),
        vec![4, 5, 6],
        "the spill closure must iterate: a newly marked neighbour that is \
         itself a lifted deco's row marks the next row"
    );

    // (c) Fully settled frame: no dirt anywhere ⇒ the closure marks nothing
    // and the frame still gate-hits (the 0% steady state stays free).
    let prev = cur.clone();
    let cur2 = cur.clone();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur2, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.is_gate_hit(),
        "a settled lifted deco with no dirt must still gate-hit"
    );
    assert!(
        dirty.iter().all(|&b| !b),
        "the spill closure must mark nothing on a clean frame"
    );
}

/// END-TO-END regression for the settled-deco dy-spill re-blend: a SETTLED
/// lifted `Add` sparkle (the v2 nova-ember regime, `dy = -cell_h/3`) with
/// UNRELATED text dirt on its own row, N damaged frames. Pre-fix,
/// `draw_decorations` re-stamped the deco every frame (its row is dirty) and
/// the upward spill re-Added onto row-above pixels the damaged path never
/// rebuilt — accumulating brighter each frame. Every cached frame must equal
/// a fresh full render byte-for-byte.
#[test]
fn settled_lifted_add_deco_with_dirt_on_its_row_stays_byte_stable() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (_, ch) = rend.cell_size();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    // Static text on the SPILL row (row 3) so the re-Add would also land over
    // glyph pixels; the row BELOW the deco row stays glyph-free (its upward
    // glyph-AA overshoot into a rebuilt row is a pre-existing damaged-path
    // note, not this regression's subject).
    term.process(b"\x1b[4;1Hstatic spill row");

    let deco = lifted_add_deco(4, 16, -((ch / 3).min(120) as i8));
    assert!(deco.dy != 0, "the lift must be non-zero (spill premise)");
    let make = |term: &mut Terminal, n: usize| {
        // Unrelated dirt on the deco's OWN row (row index 4 = 1-based row 5),
        // away from the decorated column.
        term.process(format!("\x1b[5;1Hdirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.word_decorations.push(deco);
        input
    };

    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &make(&mut term, 0));
    for n in 1..=4usize {
        let input = make(&mut term, n);
        let cached = rend.render_input_cached(&mut wc, &input).pixels().to_vec();
        let fresh = rend.render_input(&input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "frame {n}: cached repaint must be byte-stable — the settled \
             lifted Add deco's dy spill must land on a rebuilt neighbour row, \
             never re-Add over its own cached pixels"
        );
    }
}
