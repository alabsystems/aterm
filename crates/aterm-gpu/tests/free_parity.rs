// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// FREE-floating sprite layer, Phase 2 (FREE_OVERLAY_LAYER_DESIGN.md §5.1/§5.2/
// §5.5): CPU==GPU parity now that BOTH backends consume `free_sprites`. The
// free layer is the cat NEAREST-1:1 regime through the same src-over pipeline,
// so its effect-only delta is pinned at the cat bar — target <= 1, hard <= 2
// (float UV normalization can round a boundary texel one ULP on the GPU; the
// CPU stamp is integer-exact). Covered:
//   * effect-only parity for multi-row free rects over blank bg and over text:
//     opaque texels, partial-alpha texels, a sprite-level alpha multiplier,
//     `flip_x`, tint, and the `FreeZ::OverText` slot (§5.1/§5.6);
//   * damaged-path cross-band equality (§5.2): a moved multi-row rect vs the
//     legacy per-row slices of the same art — byte-exact on the CPU, <= 2 on
//     the GPU (the full-path GPU twin lives in free_sprite_gpu.rs);
//   * damaged-path no-ghosting + the dirty gate on BOTH backends (§5.5):
//     cached == fresh per backend after a move; settled ⇒ GPU `gate_hits`.
//
// Gated: no GPU or no font -> the test no-ops (returns), like the other parity gates.

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_render::{SceneAtlas, SpriteQuad, Theme, WindowCpu};

mod common;
use common::{backends, bb, gg, max_channel_delta, rr};

/// A deterministic patterned RGBA atlas: per-texel distinct colours, OPAQUE top
/// half (multi-row opaque rects), mixed alpha bottom half (real src-over).
/// Power-of-two dims keep the GPU's normalized-UV NEAREST math exact at texel
/// centres.
fn free_atlas(version: u64) -> SceneAtlas {
    let (w, h) = (64u32, 128u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if y < 64 {
                255u8
            } else {
                (60 + (x * 3 + y) % 180) as u8
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

/// A NEAREST-1:1 free sprite (`aw/ah == w/h`, bake == dest), under text by
/// default.
fn free_1to1(x: i32, y: i32, w: u16, h: u16, src_xy: [u16; 2], alpha: u8) -> FreeSprite {
    let [ax, ay] = src_xy;
    FreeSprite {
        x,
        y,
        w,
        h,
        ax,
        ay,
        aw: w,
        ah: h,
        tint: 0x00FF_FFFF,
        alpha,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }
}

/// THE free-layer parity pin (§5.1). The base frame is procedural full-block
/// glyphs + background — byte-exact CPU==GPU (delta 0) — so the measured delta
/// on the sprite frame is the FREE layer's alone (effect-only). Sprites cover:
/// a MULTI-ROW rect over blank bg (opaque texels), a multi-row rect over the
/// block TEXT, partial-alpha texels, a sprite-level alpha multiplier + flip_x,
/// a tinted rect, and an OverText rect over the text (§5.6's GPU/CPU slot
/// consistency; the OverText-vs-additive z-order is pinned separately by
/// `free_over_text_covers_wdeco_and_additive_light_both_backends`).
#[test]
fn free_effect_only_parity_pinned_over_bg_and_text() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l████████".as_bytes()); // procedural: byte-exact base

    // Base (no sprites): the effect-only premise — CPU==GPU exactly.
    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the free delta is effect-only"
    );

    let mh = (ch + ch / 2) as u16; // > one band: every rect crosses a boundary
    let mut input = term.cell_frame(rows, cols);
    input.free_atlas = Some(Arc::new(free_atlas(1)));
    input.free_sprites = vec![
        // Multi-row over blank bg, opaque texels (atlas top half).
        free_1to1(4, (2 * ch) as i32, 40, mh.min(60), [2, 0], 255),
        // Multi-row over the block TEXT, opaque texels, sub-cell origin.
        free_1to1(2, (ch / 3) as i32, 48, mh.min(60), [8, 0], 255),
        // Over blank bg, PARTIAL-alpha texels (atlas bottom half) — real blending.
        free_1to1(8, (4 * ch) as i32, 32, (ch as u16).min(56), [0, 65], 255),
        // Sprite-level alpha multiplier + horizontal flip.
        FreeSprite {
            flip_x: true,
            ..free_1to1(
                (cols * cw - 44) as i32,
                (5 * ch) as i32,
                40,
                mh.min(60),
                [12, 4],
                140,
            )
        },
        // Tinted (multiply 0x40C080) over blank bg.
        FreeSprite {
            tint: 0x0040_C080,
            ..free_1to1(
                (6 * cw) as i32,
                (6 * ch + 3) as i32,
                36,
                (ch as u16).min(56),
                [5, 2],
                255,
            )
        },
        // OverText: draws over the glyphs, immediately before the cursor slot.
        FreeSprite {
            z: FreeZ::OverText,
            ..free_1to1(
                (10 * cw) as i32,
                (ch / 2) as i32,
                36,
                mh.min(60),
                [20, 0],
                255,
            )
        },
    ];

    let cpu_free = cpu.render_input(&input);
    let gpu_free = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_free.pixels, cpu_base.pixels,
        "the free sprites must actually paint (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_free.pixels, &gpu_free.pixels);
    eprintln!("free effect-only GPU vs CPU max per-channel delta = {delta} (target <= 1)");
    assert!(
        delta <= 2,
        "free NEAREST-1:1 parity broke the pinned cat bar: max per-channel \
         delta {delta} > 2 (target <= 1)"
    );
}

/// §5.6 OVER-TEXT z-order pin, BOTH backends: over-text means over EVERYTHING
/// except the cursor. An OPAQUE OverText sprite covers a glyph, a wdeco `Over`
/// stamp, AND additive glow + nova light: on the CPU the covered cells are
/// byte-pure sprite colour (the stamp lands after `draw_decorations` and the
/// additive post-passes, right before `draw_cursor`); on the GPU (`FreeOver`
/// after the wdeco streams, before the cursor) the same cells match within the
/// cat bar (<= 2, float UV rounding only). The UnderText slot is pinned by the
/// other §5.6 cases and by free_composite.rs (unchanged).
#[test]
fn free_over_text_covers_wdeco_and_additive_light_both_backends() {
    use aterm_core::render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration};
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    // The GPU-only comet BLOOM halo re-adds the aurora light over the WHOLE
    // readback (deliberately above every stream, cursor included) — disable it
    // so this pin measures the shared FreeOver slot, like nova_parity does.
    // Ditto the heat shimmer (same parity class, wall-clock at present).
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l██".as_bytes());

    // A solid RED atlas: covered cells must come out exactly this colour.
    let red = 0x00C8_2020u32;
    let atlas = aterm_render::SceneAtlas {
        width: 8,
        height: 8,
        rgba: (0..8 * 8).flat_map(|_| [0xC8u8, 0x20, 0x20, 255]).collect(),
        version: 11,
    };

    let mut input = term.cell_frame(rows, cols);
    input.free_atlas = Some(Arc::new(atlas));
    // Opaque OverText sprite covering cells (0,0)..=(1,1).
    input.free_sprites = vec![FreeSprite {
        z: FreeZ::OverText,
        aw: 8,
        ah: 8,
        ..free_1to1(0, 0, (2 * cw) as u16, (2 * ch) as u16, [0, 0], 255)
    }];
    // A wdeco Over stamp + aurora and nova light UNDER the sprite's footprint.
    input.word_decorations.push(WordDecoration {
        row: 0,
        col: 1,
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Paw,
        blend: DecoBlend::Over,
        color: 0x0020_C020,
        alpha: 255,
    });
    input.cursor_glow_add.push(GlowQuad {
        row: 1,
        x: 0,
        y: ch as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: aterm_render::premul_rgb(0x0040_80FF, 200),
    });
    input.nova_add.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: cw as u16,
        h: ch as u16,
        color: aterm_render::premul_rgb(0x00FF_C040, 200),
    });

    // Non-vacuous premise: without the sprite the stamp + light paint.
    let mut bare = input.clone();
    bare.free_sprites.clear();
    bare.free_atlas = None;
    let cpu_bare = cpu.render_input(&bare);
    let cpu_f = cpu.render_input(&input);
    assert_ne!(
        cpu_bare.pixels, cpu_f.pixels,
        "the wdeco stamp + additive light must actually paint under the sprite"
    );
    let gpu_f = gpu.render_input(&mut win, &input, None);

    // Every pixel of the sprite's 2×2-cell footprint: CPU byte-pure red, GPU
    // within the cat bar of pure red.
    let mut gpu_max = 0i32;
    for y in 0..2 * ch {
        for x in 0..2 * cw {
            let c = cpu_f.pixels[y * cpu_f.width + x];
            assert_eq!(
                c, red,
                "CPU OverText must cover glyph/wdeco/glow/nova at ({x},{y})"
            );
            let g = gpu_f.pixels[y * gpu_f.width + x];
            gpu_max = gpu_max
                .max((rr(g) - rr(red)).abs())
                .max((gg(g) - gg(red)).abs())
                .max((bb(g) - bb(red)).abs());
        }
    }
    eprintln!("GPU OverText coverage max per-channel delta vs pure sprite colour = {gpu_max}");
    assert!(
        gpu_max <= 2,
        "GPU FreeOver must cover glyph/wdeco/glow/nova: max delta {gpu_max} > 2"
    );
}

/// §5.2 on the DAMAGED path, both backends: prime the caches with the rect at
/// bands 1..=3, move it to 2..=4, and compare the cached repaint against the
/// SAME move expressed as legacy per-row `cat_quads` slices — byte-exact on
/// the CPU, <= 2 on the GPU. (The full-path GPU twin is
/// free_sprite_gpu.rs::free_multirow_rect_matches_legacy_perrow_slices_on_gpu;
/// the full-path CPU twin is in aterm-render's free_composite.rs.)
#[test]
fn free_multirow_rect_matches_legacy_slices_on_damaged_path_both_backends() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(2));

    let (x, w) = (4i32, 40u16);
    let h = (2 * ch + ch / 2) as u16;
    let (y_a, y_b) = ((ch + ch / 2) as i32, (2 * ch + ch / 2) as i32);
    let src = [2u16, 3u16];

    let make_free = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![free_1to1(x, y, w, h, src, 255)];
        input
    };
    let make_legacy = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.cat_atlas = Some(atlas.clone());
        let (y0, y1) = (y as usize, y as usize + h as usize);
        for r in y0 / ch..=(y1 - 1) / ch {
            let band_y0 = y0.max(r * ch);
            let band_y1 = y1.min((r + 1) * ch);
            input.cat_quads.push(SpriteQuad {
                row: r as u16,
                x: x as u16,
                y: band_y0 as u16,
                w,
                h: (band_y1 - band_y0) as u16,
                ax: src[0],
                ay: src[1] + (band_y0 - y0) as u16,
                aw: w,
                ah: (band_y1 - band_y0) as u16,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x: false,
            });
        }
        assert!(input.cat_quads.len() >= 3, "multi-row premise");
        input
    };

    // CPU: byte-exact.
    let mut wc_free = WindowCpu::new();
    let mut wc_legacy = WindowCpu::new();
    let _ = cpu.render_input_cached(&mut wc_free, &make_free(&mut term, y_a));
    let cpu_free = cpu
        .render_input_cached(&mut wc_free, &make_free(&mut term, y_b))
        .pixels()
        .to_vec();
    let _ = cpu.render_input_cached(&mut wc_legacy, &make_legacy(&mut term, y_a));
    let cpu_legacy = cpu
        .render_input_cached(&mut wc_legacy, &make_legacy(&mut term, y_b))
        .pixels()
        .to_vec();
    assert_eq!(
        cpu_free, cpu_legacy,
        "CPU damaged path: a moved multi-row free rect must equal its moved \
         legacy per-row slices byte-for-byte"
    );

    // GPU: within the cat bar (float UV boundary-texel rounding only).
    let mut win_free = aterm_gpu::WindowGpu::new();
    let _ = gpu.render_input_cached(&mut win_free, &make_free(&mut term, y_a));
    let gpu_free = gpu
        .render_input_cached(&mut win_free, &make_free(&mut term, y_b))
        .pixels()
        .to_vec();
    let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
    let mut win_legacy = aterm_gpu::WindowGpu::new();
    let _ = gpu2.render_input_cached(&mut win_legacy, &make_legacy(&mut term, y_a));
    let gpu_legacy = gpu2
        .render_input_cached(&mut win_legacy, &make_legacy(&mut term, y_b))
        .pixels()
        .to_vec();
    let delta = max_channel_delta(&gpu_free, &gpu_legacy);
    eprintln!("free vs legacy damaged-path GPU max per-channel delta = {delta} (target <= 1)");
    assert!(
        delta <= 2,
        "GPU damaged path: moved free rect vs moved legacy slices delta {delta} > 2"
    );
}

/// §5.5 damaged/cached-path no-ghosting + the dirty gate, on BOTH backends:
/// frame A primes the caches with a multi-row rect at bands 1..=3; frame B
/// moves it down one band (GPU gate must MISS; both cached repaints must equal
/// a fresh full render byte-for-byte — the row-union marked every vacated +
/// occupied band, so Phase A re-cleared them); frame C repeats B (settled:
/// equal sprites, same atlas version) and must take the GPU dirty gate.
#[test]
fn damaged_path_free_no_ghosting_and_settled_gate_hit_both_backends() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
    // Glyph-free terminal (all background), like the cat twin: a glyph AA
    // overhang across a band boundary is a pre-existing damaged-vs-full note
    // unrelated to the free layer.
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(3));

    let mut make = |y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![free_1to1(2, y, 40, (2 * ch + ch / 2) as u16, [2, 0], 255)];
        input
    };
    let in_a = make((ch + ch / 2) as i32); // bands 1..=3
    let in_b = make((2 * ch + ch / 2) as i32); // bands 2..=4

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
        "a moved multi-row free rect must MISS the GPU dirty gate (real re-render)"
    );

    // Fresh ground truths (throwaway caches / a fresh GPU renderer).
    let cpu_b_fresh = cpu.render_input(&in_b).pixels;
    assert_eq!(
        cpu_b_cached, cpu_b_fresh,
        "CPU damaged path must repaint the moved multi-row free rect with no ghosting"
    );
    let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
    let mut win2 = aterm_gpu::WindowGpu::new();
    let gpu_b_fresh = gpu2.render_input(&mut win2, &in_b, None).pixels;
    assert_eq!(
        gpu_b_cached, gpu_b_fresh,
        "GPU dirty-row path must repaint the moved multi-row free rect with no ghosting"
    );

    // Settled: byte-equal sprites + same atlas version ⇒ the GPU gate HITS.
    let in_c = make((2 * ch + ch / 2) as i32);
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_c);
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled free sprite (equal sprites, same atlas version) must take the dirty gate"
    );
}

/// A SETTLED translucent multi-row sprite straddled by the dirty BOUNDING BAND
/// on the SCISSORED present path: the sprite sits on rows 3..=6, and every
/// damaged frame edits text on rows 2 and 10 only — so the GPU's scissor band
/// (rows 2..=10) covers the sprite's Load-preserved rows while the free stream
/// is rebuilt unconditionally. The dirty set must cover every band row the
/// sprite overlaps (the scissor-band fill in `compute_dirty_rows`), else the
/// translucent texels re-blend over their own cached pixels and the AA edges
/// accumulate darker every frame. N damaged frames, each asserted BYTE-STABLE
/// against a fresh full render on the GPU (and on the CPU damaged path, whose
/// per-band stamp clipping is the twin invariant).
#[test]
fn settled_translucent_sprite_inside_dirty_band_stays_byte_stable() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (12usize, 20usize);
    // Glyph-free rows under the sprite (the free layer's own invariant); the
    // dirt lives on rows 2 and 10, straddling the sprite's bands 3..=6.
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(7));

    // Translucent texels (atlas bottom half) + a sprite-level alpha: any
    // double-blend is visible. Rows 3..=6: y in [3ch + ch/4, 6ch + 3ch/4).
    let sprite = free_1to1(
        6,
        (3 * ch + ch / 4) as i32,
        48,
        (3 * ch + ch / 2) as u16,
        [0, 65],
        140,
    );
    let make = |term: &mut Terminal, n: usize| {
        term.process(format!("\x1b[3;1Htop dirt {n}\x1b[11;1Hbottom dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![sprite];
        input
    };

    // Prime the CPU damage cache and the GPU present-path offscreen, then N
    // damaged frames with the sprite SETTLED (equal sprites, same version).
    let in_0 = make(&mut term, 0);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_0);
    let _ = gpu.present_input_readback(&mut win_gpu, &in_0);
    for n in 1..=4usize {
        let input = make(&mut term, n);
        let scissors_before = gpu.scissor_taken();
        let cpu_cached = cpu
            .render_input_cached(&mut win_cpu, &input)
            .pixels()
            .to_vec();
        let gpu_scissored = gpu.present_input_readback(&mut win_gpu, &input).pixels;
        assert!(
            gpu.scissor_taken() > scissors_before,
            "frame {n}: the two-row text edit must take the SCISSORED path \
             (else this pin is vacuous)"
        );
        let cpu_fresh = cpu.render_input(&input).pixels;
        assert_eq!(
            cpu_cached, cpu_fresh,
            "frame {n}: CPU cached repaint must be byte-stable (no re-blend of \
             the settled translucent sprite)"
        );
        let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
        let mut win2 = aterm_gpu::WindowGpu::new();
        let gpu_fresh = gpu2.render_input(&mut win2, &input, None).pixels;
        assert_eq!(
            gpu_scissored, gpu_fresh,
            "frame {n}: GPU scissored repaint must be byte-stable — a settled \
             sprite row inside the dirty bounding band must be fully rebuilt, \
             not Load-preserved under an unconditional sprite redraw"
        );
    }
}

/// The OVER-TEXT twin of the settled-translucent scissor-band pin above: a
/// SETTLED translucent multi-row `FreeZ::OverText` sprite (rows 3..=6, static
/// text underneath it) with per-frame dirt straddling it on rows 2 and 10.
/// The CPU damaged path stamps OverText sprites as the LAST post-pass clipped
/// to the dirty bands; the GPU `FreeOver` stream is rebuilt unconditionally
/// and clipped by the scissor — on both, every band row the sprite overlaps
/// inside the dirty bounding band must be fully rebuilt (the scissor-band
/// fill is z-agnostic), else the translucent texels re-blend over their own
/// cached pixels. N damaged frames, each byte-stable against a fresh full
/// render per backend.
#[test]
fn settled_translucent_over_text_sprite_inside_dirty_band_stays_byte_stable() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (12usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    // STATIC text inside the sprite's bands: OverText must keep covering it
    // (the z premise), and a re-blend over the glyph pixels is visible too.
    term.process(b"\x1b[5;3Hcovered text");
    let atlas = Arc::new(free_atlas(9));

    // Translucent texels (atlas bottom half) + a sprite-level alpha, in the
    // OVER-TEXT slot. Rows 3..=6: y in [3ch + ch/4, 6ch + 3ch/4).
    let sprite = FreeSprite {
        z: FreeZ::OverText,
        ..free_1to1(
            6,
            (3 * ch + ch / 4) as i32,
            48,
            (3 * ch + ch / 2) as u16,
            [0, 65],
            140,
        )
    };
    let make = |term: &mut Terminal, n: usize| {
        term.process(format!("\x1b[3;1Htop dirt {n}\x1b[11;1Hbottom dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![sprite];
        input
    };

    let in_0 = make(&mut term, 0);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_0);
    let _ = gpu.present_input_readback(&mut win_gpu, &in_0);
    for n in 1..=4usize {
        let input = make(&mut term, n);
        let scissors_before = gpu.scissor_taken();
        let cpu_cached = cpu
            .render_input_cached(&mut win_cpu, &input)
            .pixels()
            .to_vec();
        let gpu_scissored = gpu.present_input_readback(&mut win_gpu, &input).pixels;
        assert!(
            gpu.scissor_taken() > scissors_before,
            "frame {n}: the two-row text edit must take the SCISSORED path \
             (else this pin is vacuous)"
        );
        let cpu_fresh = cpu.render_input(&input).pixels;
        assert_eq!(
            cpu_cached, cpu_fresh,
            "frame {n}: CPU damaged path must be byte-stable for a settled \
             translucent OverText sprite inside the dirty bounding band"
        );
        let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
        let mut win2 = aterm_gpu::WindowGpu::new();
        let gpu_fresh = gpu2.render_input(&mut win2, &input, None).pixels;
        assert_eq!(
            gpu_scissored, gpu_fresh,
            "frame {n}: GPU scissored path must be byte-stable — a settled \
             OverText sprite row inside the dirty bounding band must be fully \
             rebuilt, not Load-preserved under the unconditional FreeOver redraw"
        );
    }
}

/// Settled-deco dy-SPILL scissor-band pin, BOTH backends: a SETTLED lifted
/// `Add` sparkle (the v2 nova-ember regime, `dy = -cell_h/3`) on row 5, with
/// per-frame dirt on its OWN row and on row 2 (so the GPU's scissor band
/// [1..=5] covers the spill-neighbour row 4). The deco streams are row-gated
/// on the deco's OWN row, so both backends re-stamp the settled deco every
/// damaged frame — pre-fix its upward spill re-Added onto row 4's
/// NOT-rebuilt pixels (CPU: never re-cleared; GPU: Load-preserved inside the
/// scissor), accumulating brighter each frame. `compute_dirty_rows` must drag
/// the spill neighbour into the dirty set. N damaged frames, byte-stable
/// against a fresh full render per backend.
#[test]
fn settled_lifted_add_deco_spill_inside_dirty_band_stays_byte_stable() {
    use aterm_core::render::{DecoBlend, DecoGlyph, WordDecoration};
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    // Static text on the SPILL row (index 4) so the re-Add would also land
    // over glyph pixels; the row below the deco row stays glyph-free.
    term.process(b"\x1b[5;1Hstatic spill row");

    let deco = WordDecoration {
        row: 5,
        col: 16,
        dx: 0,
        dy: -((ch / 3).min(120) as i8),
        glyph: DecoGlyph::Star4,
        blend: DecoBlend::Add,
        color: 0x0060_5030,
        alpha: 220,
    };
    assert!(deco.dy != 0, "the lift must be non-zero (spill premise)");
    let make = |term: &mut Terminal, n: usize| {
        // Dirt on row 2 (widens the scissor band over the spill row) and on
        // the deco's OWN row (re-stamps the settled deco), settled deco.
        term.process(format!("\x1b[2;1Htop dirt {n}\x1b[6;1Hdeco row dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.word_decorations.push(deco);
        input
    };

    let in_0 = make(&mut term, 0);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_0);
    let _ = gpu.present_input_readback(&mut win_gpu, &in_0);
    for n in 1..=4usize {
        let input = make(&mut term, n);
        let scissors_before = gpu.scissor_taken();
        let cpu_cached = cpu
            .render_input_cached(&mut win_cpu, &input)
            .pixels()
            .to_vec();
        let gpu_scissored = gpu.present_input_readback(&mut win_gpu, &input).pixels;
        assert!(
            gpu.scissor_taken() > scissors_before,
            "frame {n}: the two-row text edit must take the SCISSORED path \
             (else this pin is vacuous)"
        );
        let cpu_fresh = cpu.render_input(&input).pixels;
        assert_eq!(
            cpu_cached, cpu_fresh,
            "frame {n}: CPU damaged path must be byte-stable — the settled \
             lifted Add deco's dy spill must land on a rebuilt neighbour row"
        );
        let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
        let mut win2 = aterm_gpu::WindowGpu::new();
        let gpu_fresh = gpu2.render_input(&mut win2, &input, None).pixels;
        assert_eq!(
            gpu_scissored, gpu_fresh,
            "frame {n}: GPU scissored path must be byte-stable — the spill \
             neighbour row inside the scissor band must be rebuilt, not \
             Load-preserved under the row-gated wdeco re-stamp"
        );
    }
}

/// The same scissor-band invariant for the LEGACY settled sprites (cat_quads —
/// the defect predates the free layer): a settled translucent cat quad on row 4
/// with per-frame dirt on rows 2 and 8 must stay byte-stable on the GPU's
/// scissored present path across damaged frames (its band row is inside the
/// scissor band, and the cat stream is built unconditionally).
#[test]
fn settled_translucent_cat_quad_inside_dirty_band_stays_byte_stable() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (10usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(8));

    let quad = SpriteQuad {
        row: 4,
        x: 6,
        y: (4 * ch + 2) as u16,
        w: 40,
        h: (ch / 2) as u16,
        ax: 0,
        ay: 70, // translucent texels
        aw: 40,
        ah: (ch / 2) as u16,
        tint: 0x00FF_FFFF,
        alpha: 140,
        flip_x: false,
    };
    let make = |term: &mut Terminal, n: usize| {
        term.process(format!("\x1b[3;1Htop dirt {n}\x1b[9;1Hbottom dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.cat_atlas = Some(atlas.clone());
        input.cat_quads = vec![quad];
        input
    };

    let in_0 = make(&mut term, 0);
    let _ = gpu.present_input_readback(&mut win_gpu, &in_0);
    for n in 1..=3usize {
        let input = make(&mut term, n);
        let scissors_before = gpu.scissor_taken();
        let gpu_scissored = gpu.present_input_readback(&mut win_gpu, &input).pixels;
        assert!(
            gpu.scissor_taken() > scissors_before,
            "frame {n}: the two-row text edit must take the SCISSORED path"
        );
        let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
        let mut win2 = aterm_gpu::WindowGpu::new();
        let gpu_fresh = gpu2.render_input(&mut win2, &input, None).pixels;
        assert_eq!(
            gpu_scissored, gpu_fresh,
            "frame {n}: a settled translucent cat quad inside the dirty \
             bounding band must be byte-stable on the GPU"
        );
    }
}
