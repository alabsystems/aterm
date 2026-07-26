// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Peeking-cat sprites (Sparkle Words v2, `RenderInput.cat_quads` + `cat_atlas`)
// on the CPU renderer. The contract under test:
//   * empty cat fields (quads empty / atlas absent — and quads empty WITH an
//     atlas set) are byte-identical to the pre-cat path, also after
//     `clear_overlays` (the `image plain` contract);
//   * pass 1c z-order: cat sprites draw UNDER the row's glyphs and UNDER inline
//     images (matching the GPU's `emit_base_pre` stream order);
//   * the cat stamp is NEAREST 1:1 and endpoint-exact (tint 0xFFFFFF + alpha
//     255 + opaque texels land the EXACT atlas bytes; a checker atlas yields NO
//     intermediate colours);
//   * damaged path: a moved cat re-renders with no ghosting (cached == fresh);
//   * dirty gate: settled (non-empty but EQUAL) cat quads at the same atlas
//     version gate-hit with zero rows marked; a change marks exactly the
//     prev∪cur cat rows; an atlas-version bump alone un-gates.

use std::sync::Arc;

use aterm_core::render::{SceneAtlas, SpriteQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{
    DirtyDecision, Frame, Renderer, Theme, WindowCpu, compute_dirty_rows, rgb_to_u32,
};

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
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

/// A deterministic, fully-opaque patterned RGBA atlas (per-texel distinct
/// colours so a wrong NEAREST index or an accidental filter shows up).
fn patterned_atlas(w: u32, h: u32, version: u64) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
            ]);
        }
    }
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

/// A 2×2 opaque black/white checker atlas: NEAREST sampling can only ever
/// produce pure black or pure white; any intermediate grey proves filtering.
fn checker_atlas(version: u64) -> SceneAtlas {
    #[rustfmt::skip]
    let rgba = vec![
        255, 255, 255, 255,   0, 0, 0, 255,
        0, 0, 0, 255,         255, 255, 255, 255,
    ];
    SceneAtlas {
        width: 2,
        height: 2,
        rgba,
        version,
    }
}

/// Opaque untinted quad: `dest = [x, y, w, h]`, `src = [ax, ay, aw, ah]`.
fn quad(row: u16, dest: [u16; 4], src: [u16; 4]) -> SpriteQuad {
    SpriteQuad {
        row,
        x: dest[0],
        y: dest[1],
        w: dest[2],
        h: dest[3],
        ax: src[0],
        ay: src[1],
        aw: src[2],
        ah: src[3],
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }
}

#[test]
fn empty_cat_fields_are_byte_identical_also_after_clear_overlays() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (_, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lkitty cat");

    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // Empty quads + NO atlas (the common off state).
    let mut input = term.cell_frame(3, 12);
    assert!(input.cat_quads.is_empty() && input.cat_atlas.is_none());
    let again = rend.render_input(&input).pixels.clone();
    assert_eq!(base, again, "empty cat fields must not change any pixel");

    // Empty quads WITH an atlas set: the atlas alone draws nothing.
    input.cat_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
    let atlas_only = rend.render_input(&input).pixels.clone();
    assert_eq!(
        base, atlas_only,
        "a cat atlas with no quads must draw nothing"
    );

    // `clear_overlays` (the `image plain` capture) strips the cat like every
    // other bling layer: quads cleared AND the atlas Arc nulled.
    let mut with_cat = term.cell_frame(3, 12);
    with_cat.cat_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
    with_cat.cat_quads = vec![quad(
        1,
        [0, ch as u16, 16, 16.min(ch as u16)],
        [0, 0, 16, 16.min(ch as u16)],
    )];
    let painted = rend.render_input(&with_cat).pixels.clone();
    assert_ne!(base, painted, "a non-empty cat must paint something");
    with_cat.clear_overlays();
    assert!(
        with_cat.cat_quads.is_empty(),
        "clear_overlays must strip cat quads"
    );
    assert!(
        with_cat.cat_atlas.is_none(),
        "clear_overlays must null the cat atlas Arc"
    );
    let stripped = rend.render_input(&with_cat).pixels.clone();
    assert_eq!(base, stripped, "clear_overlays must restore the bare frame");
}

/// Pass-1c z-order on the CPU: an opaque cat sprite spanning two cells of a row
/// where the FIRST cell holds a full-block glyph. The glyph draws OVER the
/// sprite (cell 0 is the theme fg, exactly); the sprite shows where no glyph
/// covers it (cell 1 carries sprite colours). (The inline-image-over-cat case
/// is pinned on BOTH backends by `cat_under_opaque_sixel_is_hidden_on_both_
/// backends` in aterm-gpu's cat_parity.rs.)
#[test]
fn glyph_draws_over_cat_sprite() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process("\x1b[?25l█".as_bytes());

    // Solid-red atlas so "sprite pixels" are unmistakable.
    let red_atlas = SceneAtlas {
        width: 8,
        height: 8,
        rgba: (0..8 * 8).flat_map(|_| [0xC8u8, 0x20, 0x20, 255]).collect(),
        version: 1,
    };
    let mut input = term.cell_frame(2, 8);
    input.cat_atlas = Some(Arc::new(red_atlas));
    // One row-0 band quad across cells 0..2 (bake==dest is the cat contract, but
    // z-order is independent of scale; a flat atlas keeps the stamp exact).
    input.cat_quads = vec![SpriteQuad {
        row: 0,
        x: 0,
        y: 0,
        w: (2 * cw) as u16,
        h: ch as u16,
        ax: 0,
        ay: 0,
        aw: 8,
        ah: 8,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }];

    let f = rend.render_input(&input);
    let red = 0x00C8_2020;
    let fg = rgb_to_u32(input.cells[0][0].fg);
    let block_cell = cell_pixels(&f, cw, ch, 0, 0);
    assert!(
        block_cell.iter().all(|&p| p == fg),
        "the full-block glyph must draw OVER the cat sprite (cell 0 is pure fg)"
    );
    let bare_cell = cell_pixels(&f, cw, ch, 0, 1);
    assert!(
        bare_cell.iter().all(|&p| p == red),
        "the uncovered sprite half must be the exact sprite colour (cell 1 pure red)"
    );
}

/// NEAREST 1:1 endpoint exactness: with tint 0xFFFFFF, alpha 255 and opaque
/// texels, every dest pixel is the EXACT atlas texel byte (blend at cov 255 is
/// the identity) — the CPU stamp does zero colour math beyond the multiply
/// identities. And NEAREST discrimination: a 2×2 checker scaled up yields ONLY
/// pure black/white (any grey would mean the cat regime picked up a filter).
#[test]
fn cat_stamp_is_nearest_and_endpoint_exact_at_one_to_one() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (_, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l");

    // 1:1: dest == source rect, patterned atlas.
    let (aw, ahh) = (24u16, (ch as u16).min(24));
    let atlas = patterned_atlas(32, 32, 7);
    let mut input = term.cell_frame(3, 12);
    input.cat_atlas = Some(Arc::new(patterned_atlas(32, 32, 7)));
    input.cat_quads = vec![quad(1, [4, ch as u16, aw, ahh], [3, 5, aw, ahh])];
    let f = rend.render_input(&input);
    for dy in 0..ahh as usize {
        for dx in 0..aw as usize {
            let i = (((5 + dy) * 32) + 3 + dx) * 4;
            let want = ((atlas.rgba[i] as u32) << 16)
                | ((atlas.rgba[i + 1] as u32) << 8)
                | atlas.rgba[i + 2] as u32;
            let got = f.pixels[(ch + dy) * f.width + 4 + dx];
            assert_eq!(
                got, want,
                "NEAREST 1:1 must land the exact atlas texel at ({dx},{dy})"
            );
        }
    }

    // NEAREST discrimination on a scaled checker: only pure black / pure white.
    let mut input = term.cell_frame(3, 12);
    input.cat_atlas = Some(Arc::new(checker_atlas(9)));
    input.cat_quads = vec![quad(2, [0, (2 * ch) as u16, 16, ch as u16], [0, 0, 2, 2])];
    let f = rend.render_input(&input);
    for dy in 0..ch {
        for dx in 0..16usize {
            let p = f.pixels[(2 * ch + dy) * f.width + dx];
            assert!(
                p == 0x00FF_FFFF || p == 0,
                "the cat stamp must be NEAREST: no intermediate colour, got #{p:06X} at ({dx},{dy})"
            );
        }
    }
}

/// Damaged-path no-ghosting: prime a persistent damage cache with a cat at row
/// 1, then move it to row 2. The cached repaint must equal a FRESH full render
/// of frame B byte-for-byte — the vacated row-1 band is re-cleared and
/// re-stamped by pass 1c inside `render_row` (no post-pass ghost).
#[test]
fn damaged_path_no_ghosting_when_cat_moves() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (_, ch) = rend.cell_size();
    // Glyph-free terminal (all background), like the damaged-path glow parity
    // test: a glyph whose AA overhangs its band by 1 px is erased by a
    // neighbouring band's refill but survives a full render — a PRE-EXISTING
    // engine-snapshot quirk (blank rows carry empty cell vecs, so their pass-1
    // fill is a no-op) that exists with the v1 glow overlay too and is not what
    // this test pins. The cat contract is the row bands it owns.
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(patterned_atlas(32, 32, 4));

    let mut make = |row: u16| {
        let mut input = term.cell_frame(4, 12);
        input.cat_atlas = Some(atlas.clone());
        input.cat_quads = vec![quad(
            row,
            [2, row * ch as u16, 24, (ch as u16).min(24)],
            [0, 0, 24, (ch as u16).min(24)],
        )];
        input
    };
    let in_a = make(1);
    let in_b = make(2);

    let mut wc = WindowCpu::new();
    let a_cached = rend.render_input_cached(&mut wc, &in_a).pixels().to_vec();
    let b_cached = rend.render_input_cached(&mut wc, &in_b).pixels().to_vec();
    assert_ne!(a_cached, b_cached, "the cat actually moved between frames");

    // Ground truth: a fresh full render of frame B (throwaway cache).
    let b_fresh = rend.render_input(&in_b).pixels.clone();
    assert_eq!(
        b_cached, b_fresh,
        "damaged-path repaint after a cat move must equal a fresh full render \
         (no ghost at the vacated row, no missing stamp at the new row)"
    );
}

/// Dirty gate: settled cat quads — non-empty but EQUAL, same atlas version —
/// gate-hit with zero rows marked (the 0% steady state); a moved quad marks
/// exactly the prev∪cur cat rows; an atlas-version bump alone (equal quads)
/// sets `cat_changed` and marks the quad rows.
#[test]
fn cat_dirty_gate_settled_hits_changed_marks_prev_union_cur_rows() {
    let mut term = Terminal::new(5, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let atlas_v1 = Arc::new(patterned_atlas(16, 16, 1));
    let settled = vec![quad(1, [0, 20, 16, 16], [0, 0, 16, 16])];

    // Settled: equal non-empty quads + same atlas ⇒ gate hit, nothing marked.
    let mut prev = term.cell_frame(5, 8);
    let mut cur = term.cell_frame(5, 8);
    prev.cat_atlas = Some(atlas_v1.clone());
    prev.cat_quads = settled.clone();
    cur.cat_atlas = Some(atlas_v1.clone());
    cur.cat_quads = settled.clone();
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.cat_changed,
        "equal quads + same atlas must not set cat_changed"
    );
    assert!(
        d.is_gate_hit(),
        "settled (non-empty but equal) cat must gate-hit: steady state is free"
    );
    assert!(dirty.iter().all(|&b| !b), "settled cat must mark no rows");

    // Changed: the cat moves row 1 → row 3 ⇒ exactly rows {1, 3}, no gate.
    cur.cat_quads = vec![quad(3, [0, 60, 16, 16], [0, 0, 16, 16])];
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.cat_changed, "moved quads must set cat_changed");
    assert!(!d.is_gate_hit(), "changed cat must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(
        marked,
        vec![1, 3],
        "changed cat must mark exactly the prev∪cur cat rows"
    );

    // Atlas-version bump with byte-equal quads: a rebake must repaint.
    cur.cat_quads = settled;
    cur.cat_atlas = Some(Arc::new(patterned_atlas(16, 16, 2)));
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, 16, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.cat_changed,
        "an atlas-version bump alone must set cat_changed (a rebake repaints)"
    );
    assert!(!d.is_gate_hit(), "a rebaked atlas must NOT gate-hit");
    let marked: Vec<usize> = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    assert_eq!(marked, vec![1], "the rebake marks the (unmoved) cat's row");
}

/// §7.4/§14 P5 perf gate — `bench_render_row_under_sprites`: the pass-1c row
/// cost. A 120×40 text frame with ONE cat-scale sprite quad (80 px × cell)
/// under EVERY row is rendered against a no-sprite baseline of the same frame;
/// both full-frame medians are reported plus the per-row pass-1c delta. Runs
/// alternate so drift hits both sides. The measured numbers land in
/// PROOF_CARRYING_PERFORMANCE.md ("Sparkle Words v2.1"). Timing-sensitive, so
/// it follows the repo's manual-timing idiom:
///
/// ```sh
/// cargo test -p aterm-render --release --test cat_sprites \
///   bench_render_row_under_sprites -- --ignored --nocapture
/// ```
#[test]
#[ignore = "perf gate (design §7.4): run manually in --release with --ignored --nocapture"]
fn bench_render_row_under_sprites() {
    use std::time::Instant;
    let Some(mut rend) = renderer() else {
        panic!("bench needs a system monospace font");
    };
    let (cw, ch) = rend.cell_size();
    let (rows, cols) = (40usize, 120usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(3);
    for r in 0..rows {
        term.process(format!("\x1b[{};1H{}", r + 1, &line[..cols]).as_bytes());
    }

    let base_input = term.cell_frame(rows, cols);
    let mut sprite_input = base_input.clone();
    sprite_input.cat_atlas = Some(std::sync::Arc::new(patterned_atlas(128, ch as u32, 1)));
    // One cat-scale quad (≈ a settled HeadPeek body slice: 80 px wide, one full
    // row band) under EVERY row — 40 pass-1c stamps per frame.
    sprite_input.cat_quads = (0..rows as u16)
        .map(|r| {
            quad(
                r,
                [(4 * cw) as u16, r * ch as u16, 80, ch as u16],
                [0, 0, 80, ch as u16],
            )
        })
        .collect();

    // Warm both paths.
    for _ in 0..4 {
        let _ = rend.render_input(&base_input);
        let _ = rend.render_input(&sprite_input);
    }
    let iters = 60usize;
    let (mut t_base, mut t_sprite) = (Vec::with_capacity(iters), Vec::with_capacity(iters));
    for _ in 0..iters {
        let s = Instant::now();
        let _ = rend.render_input(&base_input);
        t_base.push(s.elapsed());
        let s = Instant::now();
        let _ = rend.render_input(&sprite_input);
        t_sprite.push(s.elapsed());
    }
    t_base.sort();
    t_sprite.sort();
    let (mb, ms) = (t_base[iters / 2], t_sprite[iters / 2]);
    let per_row_ns = (ms.as_nanos() as i128 - mb.as_nanos() as i128) as f64 / rows as f64;
    println!(
        "bench_render_row_under_sprites: baseline full-frame median {mb:?} \
         ({:.1} us/row), with a sprite under EVERY row median {ms:?} ({:.1} us/row) \
         — pass-1c cost {:.2} us/row (120x40, {}x{} px cells, 80 px quad/row)",
        mb.as_nanos() as f64 / rows as f64 / 1000.0,
        ms.as_nanos() as f64 / rows as f64 / 1000.0,
        per_row_ns / 1000.0,
        cw,
        ch
    );
    assert!(
        per_row_ns < 10_000.0,
        "§7.4 gate: pass-1c row cost {per_row_ns:.0} ns/row >= 10 us/row"
    );
}
