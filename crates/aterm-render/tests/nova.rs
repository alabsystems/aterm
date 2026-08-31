// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Supernova additive light (Sparkle Words v2, `RenderInput.nova_add`) on the
// CPU renderer. The channel contract under test:
//   * empty nova is byte-identical to the pre-nova path (also after
//     `clear_overlays`, the `image plain` contract);
//   * a nova quad saturating-adds its PREMULTIPLIED colour over the frame —
//     exactly `add_sat` per pixel, `min(255, bg + premul)` per channel over a
//     flat background (the byte-exact additive primitive);
//   * dirty gate: a settled nova (both frames empty — Settled emits NOTHING)
//     gate-hits; a changed nova marks exactly the prev∪cur quad rows and must
//     not gate-hit — including a MULTI-ROW ring emitted as per-row quads,
//     whose every band is covered by rows-only marking;
//   * the damaged/cached path never lets additive light accumulate or ghost:
//     a nova that appears then clears returns the frame to the exact base
//     bytes.

use aterm_core::terminal::Terminal;
use aterm_render::{
    DirtyDecision, GlowQuad, Renderer, Theme, WindowCpu, compute_dirty_rows, premul_rgb,
};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A single-row nova quad covering cell `(row, col)` — the emitter's row-band
/// invariant (ring chords / ray slabs are split at row boundaries).
fn quad_at(cw: usize, ch: usize, row: u16, col: usize, color: u32) -> GlowQuad {
    GlowQuad {
        row,
        x: (col * cw) as u16,
        y: (row as usize * ch) as u16,
        w: cw as u16,
        h: ch as u16,
        color,

        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
    }
}

#[test]
fn empty_nova_is_byte_identical_also_after_clear_overlays() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lbuild: fuck");

    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // Explicitly empty nova (feature on, every nova Settled — the steady state
    // emits nothing).
    let mut input = term.cell_frame(3, 12);
    assert!(input.nova_add.is_empty());
    input.nova_add.clear();
    let again = rend.render_input(&input).pixels.clone();
    assert_eq!(base, again, "empty nova_add must not change any pixel");

    // `clear_overlays` (the `image plain` capture) strips the nova like every
    // other bling layer: a previously-lit input renders the bare frame after.
    let mut lit = term.cell_frame(3, 12);
    lit.nova_add
        .push(quad_at(cw, ch, 1, 2, premul_rgb(0x00FF_9A3C, 220)));
    let with_nova = rend.render_input(&lit).pixels.clone();
    assert_ne!(base, with_nova, "a non-empty nova must brighten something");
    lit.clear_overlays();
    assert!(
        lit.nova_add.is_empty(),
        "clear_overlays must strip nova_add"
    );
    let stripped = rend.render_input(&lit).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// The additive math is exact on the CPU: a premultiplied quad over a flat
/// background lands `min(255, bg + premul)` per channel — the same `add_sat`
/// the LUMEN aurora is proven byte-exact against the GPU with.
#[test]
fn nova_adds_exact_premultiplied_light_over_background() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 8);
    term.process(b"\x1b[?25l"); // pure background, no glyphs, no cursor
    let mut input = term.cell_frame(3, 8);

    let base = 0x00FF_9A3C; // solar-palette fringe
    let premul = premul_rgb(base, 160);
    input.nova_add.push(quad_at(cw, ch, 1, 2, premul));
    let f = rend.render_input(&input);

    let bg = Theme::default().bg;
    let want = |shift: u32| (((bg >> shift) & 0xff) + ((premul >> shift) & 0xff)).min(255);
    let expected = (want(16) << 16) | (want(8) << 8) | want(0);
    let got = f.pixels[(ch + ch / 2) * f.width + (2 * cw + cw / 2)];
    assert_eq!(
        got & 0x00FF_FFFF,
        expected,
        "nova light over flat bg must be exactly min(255, bg + premul) per channel"
    );
}

/// Dirty gate: a settled nova (both frames empty — the Ember/Settled phases
/// emit nothing) gate-hits; a changed nova marks exactly the prev∪cur quad
/// rows and never gate-hits.
#[test]
fn nova_dirty_gate_settled_hits_changed_marks_prev_union_cur_rows() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let (cw, ch) = (10usize, 20usize); // any consistent pixel geometry works here

    // Settled: both empty ⇒ gate hit, nothing marked (the 0% steady state).
    let prev = term.cell_frame(6, 8);
    let mut cur = term.cell_frame(6, 8);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.nova_changed,
        "empty==empty nova must not set nova_changed"
    );
    assert!(d.is_gate_hit(), "a settled nova (empty vec) must gate-hit");
    assert!(dirty.iter().all(|&b| !b), "settled nova must mark no rows");

    // Changed: the ring advances from row 2 to row 4 ⇒ exactly rows {2, 4}
    // marked (prev∪cur), no gate.
    let mut prev = term.cell_frame(6, 8);
    prev.nova_add
        .push(quad_at(cw, ch, 2, 1, premul_rgb(0x00FF_5C3C, 200)));
    cur.nova_add
        .push(quad_at(cw, ch, 4, 1, premul_rgb(0x00FF_5C3C, 150)));
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.nova_changed, "changed nova must set nova_changed");
    assert!(!d.is_gate_hit(), "changed nova must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(
        marked,
        vec![2, 4],
        "changed nova must mark exactly the prev∪cur nova rows"
    );
}

/// MULTI-ROW dirty coverage: a shockwave ring spans several row bands, emitted
/// as one quad per band (the GlowQuad invariant). When the whole ring vanishes
/// (Settled), EVERY previously-lit row must be marked — rows-only prev∪cur
/// marking covers the full vertical extent because each band quad carries its
/// own row tag.
#[test]
fn multi_row_nova_marks_every_quad_row_on_change() {
    let mut term = Terminal::new(8, 10);
    term.process(b"\x1b[?25l");
    let (cw, ch) = (10usize, 20usize);

    // A ring straddling rows 1..=4 (four per-row band quads) on the prev frame;
    // the nova settles (emits nothing) on the current frame.
    let mut prev = term.cell_frame(8, 10);
    for r in 1..=4u16 {
        prev.nova_add
            .push(quad_at(cw, ch, r, 3, premul_rgb(0x004C_C8FF, 120)));
    }
    let cur = term.cell_frame(8, 10);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.nova_changed, "a vanishing ring must set nova_changed");
    assert!(!d.is_gate_hit(), "a vanishing ring must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(
        marked,
        vec![1, 2, 3, 4],
        "every band of the multi-row ring must be marked so no stale light survives"
    );
}

/// The damaged/cached presentation path: additive nova light never accumulates
/// across frames and leaves no ghost — re-presenting the SAME nova gate-hits
/// (byte-stable, no double-add), and clearing it restores the exact base bytes.
#[test]
fn cached_path_nova_never_accumulates_and_clears_without_ghosting() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut win = WindowCpu::new();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(4, 16);
    term.process(b"\x1b[?25lit failed: fuck");
    let base_input = term.cell_frame(4, 16);

    // Frame 0: no nova (primes the damage cache with the base frame).
    let base = rend
        .render_input_cached(&mut win, &base_input)
        .pixels()
        .to_vec();

    // Frame 1: the ring appears across rows 1..=2.
    let mut lit = base_input.clone();
    for r in 1..=2u16 {
        lit.nova_add
            .push(quad_at(cw, ch, r, 5, premul_rgb(0x00FF_D8A0, 180)));
    }
    let f1 = rend.render_input_cached(&mut win, &lit).pixels().to_vec();
    assert_ne!(base, f1, "the nova must actually brighten the frame");

    // Frame 2: the SAME quads again (a re-present of an unchanged animating
    // frame) — additive light must NOT double-add on the cached path.
    let f2 = rend.render_input_cached(&mut win, &lit).pixels().to_vec();
    assert_eq!(f1, f2, "re-presenting equal nova quads must be byte-stable");

    // Frame 3: the nova settles (empty vec) — every previously-lit row is
    // rebuilt and the frame returns to the exact base bytes (no ghost light).
    let f3 = rend
        .render_input_cached(&mut win, &base_input)
        .pixels()
        .to_vec();
    assert_eq!(
        base, f3,
        "a settled nova must leave no residue on the damaged path"
    );
}
