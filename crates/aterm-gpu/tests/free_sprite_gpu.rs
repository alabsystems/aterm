// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// FREE-floating sprite layer (`free_sprites` + `free_atlas`), Phase 1: GPU-only
// consumption (the CPU consumes free sprites in Phase 2, so every assertion here
// is GPU-vs-GPU). A single MULTI-ROW free rect — no host row-splitting — must:
//   * render exactly like the legacy per-row-sliced `cat_quads` emission of the
//     same art (both NEAREST 1:1 through the same src-over pipeline; a boundary
//     texel may round one ULP differently in float UV, so the bar is the cat
//     hard bar <= 2, target <= 1);
//   * repaint with no ghosting on the damaged/cached path when moved (the
//     row-union in `compute_dirty_rows` marks every band the rect overlaps,
//     prev-union-cur, so the dirty-band scissor spans the full Y-extent and
//     cached == fresh byte-for-byte): moved => `gate_misses` increments;
//   * take the dirty gate when settled (equal sprites + same atlas version):
//     settled => `gate_hits` increments.
//
// Gated: no GPU or no font -> the test no-ops (returns), like the other parity gates.

mod rain_common;

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::{Renderer, SceneAtlas, SpriteQuad, Theme};
use rain_common::RainScene;

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

fn max_channel_delta(a: &[u32], b: &[u32]) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.iter().zip(b.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

fn backends(px: f32, theme: Theme) -> Option<(Renderer, aterm_gpu::GpuRenderer)> {
    let gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return None;
        }
    };
    let Some(cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return None;
    };
    Some((cpu, gpu))
}

/// A deterministic patterned RGBA atlas, tall enough for a rect spanning
/// several cell-row bands at any realistic cell height: per-texel distinct
/// colours (a wrong NEAREST index shows up), mixed alpha below the top strip
/// (real src-over blending happens, not just opaque replacement).
fn free_atlas(version: u64) -> SceneAtlas {
    let (w, h) = (64u32, 128u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if y < 16 {
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

/// A NEAREST-1:1 free sprite (`aw/ah == w/h`, the cat bake==dest contract) at
/// an on-grid pixel origin, under text (the default z).
fn free_1to1(x: i32, y: i32, w: u16, h: u16, src_xy: [u16; 2]) -> FreeSprite {
    let [ax, ay] = src_xy;
    FreeSprite {
        x,
        y,
        w,
        h,
        ax,
        ay,
        aw: w, // bake == dest: the NEAREST 1:1 contract
        ah: h,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }
}

/// One MULTI-ROW free rect == the legacy per-row `cat_quads` slices of the same
/// art, on the GPU: proves the host head/chin split is no longer needed (no seam,
/// no clobber) at the cat parity bar (hard <= 2, target <= 1). The terminal is
/// glyph-free so the delta is effect-only.
#[test]
fn free_multirow_rect_matches_legacy_perrow_slices_on_gpu() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(1));

    // A sub-cell origin mid-band of row 1, spanning into band 3 (>= 2 band
    // crossings): y in [ch + ch/2, ch + ch/2 + 2*ch).
    let (x, y) = (4i32, (ch + ch / 2) as i32);
    let (w, h) = (40u16, (2 * ch + ch / 2) as u16);
    let (ax, ay) = (2u16, 3u16);
    assert!(
        (h as u32) <= atlas.height - ay as u32,
        "atlas must cover the rect 1:1"
    );

    let base = gpu
        .render_input(&mut win, &term.cell_frame(rows, cols), None)
        .pixels;

    let mut free_input = term.cell_frame(rows, cols);
    free_input.free_atlas = Some(atlas.clone());
    free_input.free_sprites = vec![free_1to1(x, y, w, h, [ax, ay])];
    let free_px = gpu.render_input(&mut win, &free_input, None).pixels;
    assert_ne!(
        free_px, base,
        "the multi-row free rect must actually paint (non-vacuous)"
    );

    // The SAME art as legacy single-band cat slices: one SpriteQuad per cell-row
    // band the rect overlaps, each sub-windowing the same atlas region.
    let mut slices = Vec::new();
    let (y0, y1) = (y as usize, y as usize + h as usize);
    for r in y0 / ch..=(y1 - 1) / ch {
        let band_y0 = y0.max(r * ch);
        let band_y1 = y1.min((r + 1) * ch);
        slices.push(SpriteQuad {
            row: r as u16,
            x: x as u16,
            y: band_y0 as u16,
            w,
            h: (band_y1 - band_y0) as u16,
            ax,
            ay: ay + (band_y0 - y0) as u16,
            aw: w,
            ah: (band_y1 - band_y0) as u16,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x: false,
        });
    }
    assert!(
        slices.len() >= 3,
        "the rect must span >= 3 bands (multi-row premise)"
    );
    let mut legacy_input = term.cell_frame(rows, cols);
    legacy_input.cat_atlas = Some(atlas.clone());
    legacy_input.cat_quads = slices;
    let legacy_px = gpu.render_input(&mut win, &legacy_input, None).pixels;

    let delta = max_channel_delta(&free_px, &legacy_px);
    eprintln!("free multi-row rect vs legacy slices max per-channel delta = {delta} (target <= 1)");
    assert!(
        delta <= 2,
        "a multi-row free rect must match its legacy per-row slices: max \
         per-channel delta {delta} > 2 (target <= 1)"
    );
}

/// Damaged/cached-path gating for a MULTI-ROW free rect (the Phase-1 exit
/// test): frame A primes the caches with a rect spanning bands 1..=3; frame B
/// moves it down one band (a real change — `gate_misses` must increment, and
/// the cached repaint must equal a fresh full render byte-for-byte: the
/// row-union marked every vacated + occupied band, so no ghost survives);
/// frame C repeats B unchanged (equal sprites, same atlas version) and must
/// take the dirty gate (`gate_hits` increments).
#[test]
fn damaged_path_free_sprite_no_ghosting_and_settled_gate_hit() {
    let theme = Theme::default();
    let Some((cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
    // Glyph-free terminal (all background), like the cat damaged-path test: a
    // glyph AA overhang across a band boundary is a pre-existing damaged-vs-full
    // divergence unrelated to the free layer.
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(3));

    let mut make = |y: i32| {
        let mut input = term.cell_frame(rows, cols);
        input.free_atlas = Some(atlas.clone());
        input.free_sprites = vec![free_1to1(2, y, 40, (2 * ch + ch / 2) as u16, [2, 0])];
        input
    };
    let in_a = make((ch + ch / 2) as i32); // bands 1..=3
    let in_b = make((2 * ch + ch / 2) as i32); // bands 2..=4

    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let misses_before = gpu.gate_misses();
    let gpu_b_cached = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved multi-row free rect must MISS the GPU dirty gate (real re-render)"
    );

    // Fresh ground truth (a fresh GPU renderer, throwaway caches).
    let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
    let mut win2 = aterm_gpu::WindowGpu::new();
    let gpu_b_fresh = gpu2.render_input(&mut win2, &in_b, None).pixels;
    assert_eq!(
        gpu_b_cached, gpu_b_fresh,
        "GPU dirty-row path must repaint the moved multi-row free rect with no \
         ghosting (row-union covers prev-union-cur bands)"
    );

    // Settled: byte-equal sprites + same atlas version => the GPU gate HITS.
    let in_c = make((2 * ch + ch / 2) as i32);
    let hits_before = gpu.gate_hits();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_c);
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled free sprite (equal sprites, same atlas version) must take the dirty gate"
    );
}

/// The shipping CatBaker atlas is `4 * cell_h` pixels wide, so its RGBA row
/// pitch is usually NOT wgpu's 256-byte copy alignment (for example, a 21 px
/// cell produces an 84 px / 336 byte row). Keep that real geometry covered:
/// the synthetic 64 px atlas above has an accidentally aligned 256-byte row
/// and cannot detect a backend that silently drops ordinary kitty atlases.
#[test]
fn free_sprite_upload_accepts_real_catbaker_row_pitch() {
    let theme = Theme::default();
    let Some((_, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");

    let (aw, ah) = (84u32, 48u32);
    let mut rgba = vec![0u8; (aw * ah * 4) as usize];
    for px in rgba.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&[0xF7, 0xA8, 0xB8, 0xFF]);
    }
    let atlas = Arc::new(aterm_render::SceneAtlas {
        width: aw,
        height: ah,
        rgba,
        version: 41,
    });

    let base_input = term.cell_frame(rows, cols);
    let base = gpu.render_input(&mut win, &base_input, None).pixels;
    let mut cat_input = term.cell_frame(rows, cols);
    cat_input.free_atlas = Some(atlas);
    cat_input.free_sprites = vec![free_1to1(6, 6, 32, 32, [0, 0])];
    let cat = gpu.render_input(&mut win, &cat_input, None).pixels;

    assert_ne!(
        cat, base,
        "a non-256-byte-row CatBaker atlas must paint its free sprite"
    );
}

/// End-to-end regression for the exact channel that the native sparkle-word
/// and cursor-companion cats share: a real `EffectsPipeline` must bake its
/// sparse CatBaker atlas, emit the arbitrary-rect `FreeSprite`, and have that
/// authored art survive the GPU present path. Synthetic solid/pattern atlases
/// cannot detect a UV window that lands in CatBaker's transparent slot area.
#[test]
fn real_catbaker_free_sprite_is_visible_on_gpu_present_path() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    // Native windows use a padded grid; exercise the same signed-origin
    // translation and padded offscreen/scissor dimensions as the GUI path.
    cpu.set_pad(14);
    gpu.set_pad(14);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[7;10Hkitty");

    let mut effects = EffectsPipeline::new();
    effects.set_sparkle_enabled(true);
    effects.set_sparkle_classes(true, true, false, true);
    effects.set_sparkle_feline("cat", None, 0.7, true, true, true, true, false);
    effects.set_sparkle_reduced_motion(true);

    let mut input = term.cell_frame(rows, cols);
    let mut cat_input = None;
    for _ in 0..12 {
        effects.advance(100.0);
        term.cell_frame_into(&mut input, rows, cols);
        effects.apply(&mut term, &mut input, cw, ch);
        if !input.free_sprites.is_empty() {
            cat_input = Some(input.clone());
            break;
        }
    }
    let cat_input = cat_input.expect("the real feline pipeline must emit a free sprite");
    let atlas = cat_input
        .free_atlas
        .as_ref()
        .expect("an emitted feline sprite carries its CatBaker atlas");
    assert!(
        atlas.rgba.as_chunks::<4>().0.iter().any(|px| px[3] != 0),
        "the real CatBaker atlas must contain visible authored pixels"
    );

    let mut bare = cat_input.clone();
    bare.free_sprites.clear();
    bare.free_atlas = None;
    let base = gpu.present_input_readback(&mut win, &bare).pixels;
    let gpu_cat = gpu.present_input_readback(&mut win, &cat_input).pixels;
    assert_ne!(
        gpu_cat, base,
        "the exact CatBaker/free-sprite stream must paint on the GPU present path"
    );

    let cpu_cat = cpu.render_input(&cat_input).pixels;
    let delta = max_channel_delta(&cpu_cat, &gpu_cat);
    assert!(
        delta <= 2,
        "real CatBaker CPU/GPU parity exceeded the cat bar: {delta} > 2"
    );

    // Native production composes both independent sprite atlases in one
    // frame: PHOSPHOR rain first, then the feline free sprite. Keep the exact
    // two-atlas transition non-vacuous on the incremental present path. The
    // rain-only frame primes a resident offscreen; the next frame changes
    // only by adding the real CatBaker sprite and must visibly differ.
    let mut rain_term = Terminal::new(rows as u16, cols as u16);
    rain_term.process("\x1b[?25l████████".as_bytes());
    let rain_base = rain_term.cell_frame(rows, cols);
    let mut rain = RainScene::new(rows, cols, (cw, ch), &rain_base);
    rain.drive_until_raining();
    assert!(rain.atlas().is_some());
    let mut rain_only = bare.clone();
    rain.apply(&mut rain_only);
    let mut rain_and_cat = cat_input.clone();
    rain.apply(&mut rain_and_cat);
    assert!(!rain_and_cat.rain_quads.is_empty());
    assert!(rain_and_cat.rain_atlas.is_some());

    let _ = gpu.present_input_readback(&mut win, &rain_only);
    let gpu_rain_cat = gpu.present_input_readback(&mut win, &rain_and_cat).pixels;
    let mut fresh = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
    fresh.set_pad(14);
    let mut fresh_win = aterm_gpu::WindowGpu::new();
    let gpu_rain_only = fresh.render_input(&mut fresh_win, &rain_only, None).pixels;
    assert_ne!(
        gpu_rain_cat, gpu_rain_only,
        "a real feline sprite must remain visible when the rain atlas is bound"
    );

    let cpu_rain_cat = cpu.render_input(&rain_and_cat).pixels;
    let combined_delta = max_channel_delta(&cpu_rain_cat, &gpu_rain_cat);
    assert!(
        combined_delta <= 2,
        "rain + real CatBaker CPU/GPU parity exceeded the sprite bar: \
         {combined_delta} > 2"
    );
}
