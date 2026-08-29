// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Peeking-cat sprite (Sparkle Words v2 `cat_quads` + `cat_atlas`) CPU/GPU parity.
// The cat is a NEAREST 1:1 regime (bake == dest size; shared RGBA texels; CPU
// linear-light `blend` vs GPU ALPHA_BLENDING on the sRGB view), so its
// effect-only delta is pinned at the tightest measured bar — target <= 1, hard
// bar <= 2 — so the suite-wide <= 8 can never mask decay. Also covered:
//   * an opaque SIXEL inline image hides the sprite on BOTH backends (the
//     pass-1c / emit_base_pre under-image slot);
//   * empty cat fields are byte-identical on the GPU, also after clear_overlays;
//   * the damaged/cached path repaints a moved cat with no ghosting (cached ==
//     fresh, byte-for-byte per backend) and a SETTLED cat (equal quads + same
//     atlas version) takes the dirty gate.
//
// Gated: no GPU or no font -> the test no-ops (returns), like the other parity gates.

use std::sync::Arc;

use aterm_core::terminal::Terminal;
use aterm_render::{SceneAtlas, SpriteQuad, Theme, WindowCpu};

mod common;
use common::{backends, max_channel_delta};

/// A deterministic patterned RGBA cat atlas: per-texel distinct colours (a
/// wrong NEAREST index shows up), mixed alpha in the bottom half (real src-over
/// blending happens, not just opaque replacement). Power-of-two dims keep the
/// GPU's normalized-UV NEAREST math exact at texel centres.
fn cat_atlas(version: u64) -> SceneAtlas {
    let (w, h) = (64u32, 64u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if y < 32 {
                255u8
            } else {
                (60 + (x * 3) % 180) as u8
            };
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                a,
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

/// 1:1 quad — `dest = [x, y, w, h]`, source origin `[ax, ay]`; `aw/ah == w/h`
/// (bake == dest: the NEAREST 1:1 contract).
fn quad_1to1(row: u16, dest: [u16; 4], src_xy: [u16; 2], alpha: u8) -> SpriteQuad {
    let [x, y, w, h] = dest;
    let [ax, ay] = src_xy;
    SpriteQuad {
        row,
        x,
        y,
        w,
        h,
        ax,
        ay,
        aw: w, // bake == dest: the NEAREST 1:1 contract
        ah: h,
        tint: 0x00FF_FFFF,
        alpha,
        flip_x: false,
    }
}

/// THE cat parity pin. The base frame is procedural full-block glyphs +
/// background — byte-exact CPU==GPU (delta 0) — so the measured delta on the
/// cat frame is the CAT's alone (effect-only). Quads cover: 1:1 over blank bg
/// (opaque + partial-alpha texels), 1:1 over text, a quad-level alpha
/// multiplier, and a flipped quad. Target <= 1, hard bar <= 2.
#[test]
fn cat_effect_only_parity_pinned_over_bg_and_text() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l████████".as_bytes()); // procedural: byte-exact base

    // Base (no cat): the effect-only premise — CPU==GPU exactly.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    let base_delta = max_channel_delta(&cpu_base.pixels, &gpu_base.pixels);
    assert_eq!(
        base_delta, 0,
        "procedural-block base must be byte-exact so the cat delta is effect-only"
    );

    let hh = (ch as u16).min(32); // stay inside one row band AND the opaque atlas half
    let mut input = term.cell_frame(rows, cols);
    input.cat_atlas = Some(Arc::new(cat_atlas(1)));
    input.cat_quads = vec![
        // Over blank bg, opaque texels (atlas top half).
        quad_1to1(3, [4, (3 * ch) as u16, 40, hh], [2, 0], 255),
        // Over the block TEXT, opaque texels.
        quad_1to1(0, [2, 0, 48, hh], [8, 0], 255),
        // Over blank bg, PARTIAL-alpha texels (atlas bottom half) — real blending.
        quad_1to1(
            4,
            [8, (4 * ch) as u16, 32, (ch as u16).min(30)],
            [0, 33],
            255,
        ),
        // Quad-level alpha multiplier + horizontal flip.
        SpriteQuad {
            flip_x: true,
            ..quad_1to1(
                5,
                [(cols * cw - 44) as u16, (5 * ch) as u16, 40, hh],
                [12, 4],
                140,
            )
        },
    ];

    let cpu_cat = cpu.render_input(&input);
    let gpu_cat = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_cat.pixels, cpu_base.pixels,
        "the cat must actually paint (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_cat.pixels, &gpu_cat.pixels);
    eprintln!("cat effect-only GPU vs CPU max per-channel delta = {delta} (target <= 1)");
    assert!(
        delta <= 2,
        "cat NEAREST-1:1 parity broke its pinned bar: max per-channel delta \
         {delta} > 2 (target <= 1)"
    );
}

/// An OPAQUE sixel inline image hides the cat sprite on BOTH backends: the CPU
/// stamps sprites in pass 1c BEFORE the inline-image stamp; the GPU draws the
/// cat stream in `emit_base_pre` before the inline-image stream — so with-cat
/// and without-cat frames are byte-identical per backend when the image fully
/// covers the quad.
///
/// "WHEN THE IMAGE FULLY COVERS THE QUAD" is now a checked precondition, not a
/// sentence. It stopped being true under the fixture (see the DCS below) and
/// the test went red on a claim it was not making; and had it gone the other
/// way — image and cat both absent — the equality would have passed while
/// nothing was drawn at all.
#[test]
fn cat_under_opaque_sixel_is_hidden_on_both_backends() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    // A REAL sixel DCS (the aterm-gpu test build enables the engine's `sixel`
    // feature): raster 2*cw px wide x ch px TALL, every pixel opaque red — a
    // decoded RawRgba8 image that fills a 2x1-cell footprint edge to edge.
    //
    // IT USED TO BE ONE SIXEL BAND — six pixel rows — and the comment said the
    // decode "scales to a fully-opaque 2x1-cell footprint". That was true while
    // the renderer STRETCHED an inline image to its cell footprint, and
    // `6c6f6a94` ("an inline image survives a resize, reports its pixels, and
    // stops being stretched") deliberately ended that: an image now paints at
    // its own pixel size inside the footprint. So the fixture's 22x6 raster
    // covered 6 of the cell's 21 rows, the 21-px-tall cat quad stuck out of the
    // top of it, and the CPU arm went red — not because the z-order this test
    // is about had moved, but because the premise it never checked had.
    // MEASURED at that point: 132 red pixels in the base frame (22 x 6), not
    // the 2*cw*ch a cover owes. The raster is now built to the LIVE cell height
    // so the premise holds on any font metric, and `red_px` below CHECKS it
    // rather than assuming it.
    //
    // Sixel writes six pixel rows per data char, so `ch` rows is `ch / 6` full
    // bands (`~` = all six bits) plus, when `ch` is not a multiple of six, one
    // partial band whose low `ch % 6` bits are set (`?` + (2^rem - 1)).
    let mut dcs: Vec<u8> = Vec::new();
    dcs.extend_from_slice(format!("\x1bP0;0;8q\"1;1;{};{ch}#1;2;100;0;0#1", 2 * cw).as_bytes());
    for _ in 0..ch / 6 {
        dcs.extend(std::iter::repeat_n(b'~', 2 * cw));
        dcs.extend_from_slice(b"$-");
    }
    if !ch.is_multiple_of(6) {
        let partial = b'?' + u8::try_from((1u32 << (ch % 6)) - 1).expect("rem < 6 ⇒ value < 64");
        dcs.extend(std::iter::repeat_n(partial, 2 * cw));
        dcs.extend_from_slice(b"$-");
    }
    dcs.extend_from_slice(b"\x1b\\");
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\x1b[?25l");
    term.process(&dcs);
    assert!(
        !term.images_row(0).is_empty(),
        "sixel DCS must place an inline image on row 0"
    );

    let base_input = term.cell_frame(rows, cols);
    let mut input = term.cell_frame(rows, cols);
    input.cat_atlas = Some(Arc::new(cat_atlas(1)));
    // The quad sits fully INSIDE the image's 2-cell footprint (opaque cover).
    let hh = (ch as u16).min(32);
    input.cat_quads = vec![quad_1to1(0, [2, 0, (cw as u16) * 2 - 4, hh], [0, 0], 255)];

    let cpu_base = cpu.render_input(&base_input);
    let cpu_cat = cpu.render_input(&input);
    // THE PREMISE, CHECKED. `assert_eq!(base, cat)` is satisfied just as well by
    // a renderer that draws neither the image nor the cat, so the cover has to
    // be measured or the whole test can pass while painting nothing. The
    // opaque red must fill exactly the 2x1-cell footprint.
    let red_px = |f: &aterm_render::Frame| {
        f.pixels
            .iter()
            .filter(|&&p| ((p >> 16) & 0xFF) > 150 && ((p >> 8) & 0xFF) < 90 && (p & 0xFF) < 90)
            .count()
    };
    assert_eq!(
        red_px(&cpu_base),
        2 * cw * ch,
        "CPU: the sixel must cover its whole 2x1-cell footprint — a partial \
         cover makes the comparison below true for the wrong reason (the cat \
         painting where no image reaches)"
    );
    assert_eq!(
        cpu_base.pixels, cpu_cat.pixels,
        "CPU: an opaque sixel must hide the cat sprite (the pass-1c sprite stamp \
         is under the inline-image stamp — pass 2b for this image's z >= 0)"
    );
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    let gpu_cat = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        gpu_base.pixels, gpu_cat.pixels,
        "GPU: an opaque sixel must hide the cat sprite (cat draws before images)"
    );

    // Control (non-vacuous): the SAME quad moved outside the image footprint
    // paints on both backends.
    let mut moved = term.cell_frame(rows, cols);
    moved.cat_atlas = Some(Arc::new(cat_atlas(1)));
    moved.cat_quads = vec![quad_1to1(
        2,
        [2, (2 * ch) as u16, (cw as u16) * 2 - 4, hh],
        [0, 0],
        255,
    )];
    assert_ne!(
        cpu.render_input(&moved).pixels,
        cpu_base.pixels,
        "control: the uncovered cat must paint on the CPU"
    );
    assert_ne!(
        gpu.render_input(&mut win, &moved, None).pixels,
        gpu_base.pixels,
        "control: the uncovered cat must paint on the GPU"
    );
}

/// Empty cat fields are byte-identical on the GPU — including an atlas with no
/// quads (uploads but draws nothing) and a populated input after
/// `clear_overlays` (the `image plain` contract).
#[test]
fn empty_cat_fields_byte_identical_on_gpu() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    // Cell metrics for quad geometry (the GPU shares the CPU face's metrics).
    let (_, ch) = cpu.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25lkitty");

    let base = gpu
        .render_input(&mut win, &term.cell_frame(3, 10), None)
        .pixels;

    let mut atlas_only = term.cell_frame(3, 10);
    atlas_only.cat_atlas = Some(Arc::new(cat_atlas(1)));
    assert!(atlas_only.cat_quads.is_empty());
    let atlas_only_px = gpu.render_input(&mut win, &atlas_only, None).pixels;
    assert_eq!(
        base, atlas_only_px,
        "a cat atlas with no quads must be byte-identical on the GPU"
    );

    let mut cleared = term.cell_frame(3, 10);
    cleared.cat_atlas = Some(Arc::new(cat_atlas(1)));
    cleared.cat_quads = vec![quad_1to1(
        1,
        [0, ch as u16, 24, (ch as u16).min(32)],
        [0, 0],
        255,
    )];
    let painted = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_ne!(base, painted, "a non-empty cat must paint on the GPU");
    cleared.clear_overlays();
    let stripped = gpu.render_input(&mut win, &cleared, None).pixels;
    assert_eq!(
        base, stripped,
        "clear_overlays must restore the bare GPU frame (quads cleared, atlas nulled)"
    );
}

/// Damaged/cached-path no-ghosting + the dirty gate, on BOTH backends: frame A
/// primes the caches with a cat at row 1; frame B moves it to row 2 (a real
/// change — the GPU gate must MISS and both cached repaints must equal a fresh
/// full render byte-for-byte, proving the vacated band re-cleared); frame C
/// repeats B unchanged (settled cat: equal quads, same atlas version) and must
/// take the GPU dirty gate.
#[test]
fn damaged_path_cat_no_ghosting_and_settled_gate_hit() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 12usize);
    // Glyph-free terminal (all background) like damaged_path_glow_parity: a
    // glyph AA overhang across a band boundary is a pre-existing damaged-vs-full
    // divergence unrelated to the cat (see cat_sprites.rs for the note).
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(cat_atlas(3));

    let mut make = |row: u16| {
        let mut input = term.cell_frame(rows, cols);
        input.cat_atlas = Some(atlas.clone());
        input.cat_quads = vec![quad_1to1(
            row,
            [2, row * ch as u16, 40, (ch as u16).min(32)],
            [2, 0],
            255,
        )];
        input
    };
    let in_a = make(1);
    let in_b = make(2);

    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let misses_before = gpu.gate_misses();
    let cpu_b_cached = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b_cached = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved cat must MISS the GPU dirty gate (real re-render)"
    );

    // Fresh ground truths (throwaway caches / a fresh GPU renderer).
    let cpu_b_fresh = cpu.render_input(&in_b).pixels;
    assert_eq!(
        cpu_b_cached, cpu_b_fresh,
        "CPU damaged path must repaint the moved cat with no ghosting"
    );
    let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
    let mut win2 = aterm_gpu::WindowGpu::new();
    let gpu_b_fresh = gpu2.render_input(&mut win2, &in_b, None).pixels;
    assert_eq!(
        gpu_b_cached, gpu_b_fresh,
        "GPU dirty-row path must repaint the moved cat with no ghosting"
    );

    // Settled: byte-equal quads + same atlas version ⇒ the GPU gate HITS.
    let in_c = make(2);
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_c);
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled cat (equal quads, same atlas version) must take the dirty gate"
    );
}
