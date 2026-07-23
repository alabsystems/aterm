// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Correctness gate for the GPU **scene layer** (the "Scenes" animated HUD). The empty-scene
// case is covered elsewhere (byte-identical when there are no sprite quads); THIS test proves
// a NON-EMPTY scene renders the same on the GPU (`fs_scene_over` / `fs_scene_add`) as on the
// already-verified CPU scene path (pass-1c over-stamp + `draw_scene_add`), within the same small AA/rounding tolerance the
// glyph parity uses. It builds the scene layer directly from `SpriteQuad`s + a `SceneAtlas` (no
// dependency on the `aterm-scene` engine), so it isolates the RENDER path — src-over placement,
// the multiply tint, and premultiplied additive light — from any scene content.
//
// Gated: no GPU or no font -> the test no-ops (returns), like the other parity gates.

use aterm_render::{Frame, Renderer, SceneAtlas, SpriteQuad, Theme};
use std::sync::Arc;

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

fn max_channel_delta(a: &Frame, b: &Frame) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.pixels.iter().zip(b.pixels.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

/// A uniform 8×8 fully-opaque WHITE atlas: every sample is white, so a quad's colour comes
/// purely from its multiply `tint`. Using a flat texture means the LINEAR GPU sampler and the
/// CPU sampler agree exactly at every sub-texel (no gradient/edge to round differently) — the
/// test then isolates the BLEND math (src-over + premultiplied-additive), which is where a GPU
/// shader would diverge from the CPU. Sub-rect/row-slice geometry is covered by the aterm-scene
/// bridge test; this is the pixel-blend oracle.
fn white_atlas() -> SceneAtlas {
    SceneAtlas {
        width: 8,
        height: 8,
        rgba: vec![0xFF; 8 * 8 * 4],
        version: 1,
    }
}

fn over_quad(row: u16, x: u16, y: u16, w: u16, h: u16, tint: u32, alpha: u8) -> SpriteQuad {
    SpriteQuad {
        row,
        x,
        y,
        w,
        h,
        ax: 0,
        ay: 0,
        aw: 8,
        ah: 8,
        tint,
        alpha,
        flip_x: false,
    }
}

#[test]
fn gpu_scene_layer_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();

    let (rows, cols) = (6usize, 12usize);
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    term.process(b"scene\r\n");
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);

    // A non-empty scene band across the bottom two rows: an OVER world block (opaque, tinted
    // warm) with a translucent OVER block beside it, plus an ADDITIVE light quad overlapping
    // the world (a glow) — exercising src-over at α=255 and α<255 AND premultiplied add.
    let r0 = (rows - 2) as u16;
    let r1 = (rows - 1) as u16;
    let y0 = r0 * ch as u16;
    let y1 = r1 * ch as u16;
    input.scene_atlas = Some(Arc::new(white_atlas()));
    input.scene_over = vec![
        // opaque warm block, full first band width.
        over_quad(r0, 0, y0, (cols * cw) as u16, ch as u16, 0x00E0_9048, 255),
        // half-opacity cool block on the second band (tests α<255 src-over).
        over_quad(
            r1,
            0,
            y1,
            (cols * cw / 2) as u16,
            ch as u16,
            0x0040_70C0,
            140,
        ),
    ];
    input.scene_add = vec![
        // an additive light over the warm block (premultiplied One/One).
        over_quad(
            r0,
            (cw * 2) as u16,
            y0,
            (cw * 4) as u16,
            ch as u16,
            0x0060_C0FF,
            160,
        ),
    ];

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "scene-layer frames differ in size"
    );

    // Same blend math ⇒ only rounding differs (same ≤8 tolerance as the glyph parity gate).
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("scene GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU scene pixels diverge: max per-channel delta {delta} > 8"
    );

    // Sanity: the scene actually PAINTED (the warm over-block is present and warm), so the
    // tolerance isn't being met by two identically-blank frames.
    let mid = (y0 as usize + ch / 2) * gpu_frame.width + cw; // inside the warm block, col 1
    let p = gpu_frame.pixels[mid];
    assert!(
        rr(p) > gg(p) && gg(p) > bb(p) && rr(p) > 100,
        "scene over-block should be a warm colour on the GPU frame, got #{p:06X}"
    );

    // And where the additive light overlaps, the pixel is brighter than the bare warm block.
    let lit = (y0 as usize + ch / 2) * gpu_frame.width + cw * 3; // under the add quad
    let bare = (y0 as usize + ch / 2) * gpu_frame.width + cw / 2; // warm block, before the light
    let lit_sum = rr(gpu_frame.pixels[lit]) + gg(gpu_frame.pixels[lit]) + bb(gpu_frame.pixels[lit]);
    let bare_sum =
        rr(gpu_frame.pixels[bare]) + gg(gpu_frame.pixels[bare]) + bb(gpu_frame.pixels[bare]);
    assert!(
        lit_sum > bare_sum,
        "additive light must brighten the block (lit {lit_sum} vs bare {bare_sum})"
    );
}

/// THE scene z-order fix pin (Sparkle Words v2 pass 1c): `scene_over` sprites
/// draw UNDER the text on BOTH backends. Before the fix the GPU drew scene_over
/// in `emit_base_pre` (under text) while the CPU composited `draw_scene` after
/// all glyphs (over text) — a verified divergence; the CPU now stamps over-quads
/// inside `render_row` (pass 1c). Pinned with a full-block glyph over an opaque
/// warm sprite: the block cell stays pure fg on BOTH backends, the uncovered
/// neighbour cell carries the sprite. Includes the SCALED-sprite case (dest ≠
/// atlas size): the scene stays in the bilinear/LINEAR tolerance regime (the
/// flat atlas isolates the blend; delta ≤ 8, the suite bar) — scenes are NOT
/// folded into the cat's NEAREST 1:1 stamp.
#[test]
fn scene_under_matches_gpu() {
    let theme = Theme::default();
    let px = 18.0;

    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();

    let (rows, cols) = (6usize, 12usize);
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    // Row 0: procedural full blocks (cov-255 ⇒ endpoint-exact fg on both backends).
    term.process("\x1b[?25l████".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);

    input.scene_atlas = Some(Arc::new(white_atlas()));
    input.scene_over = vec![
        // Opaque warm sprite ACROSS the glyph row (cells 0..6) — under the text.
        over_quad(0, 0, 0, (6 * cw) as u16, ch as u16, 0x00E0_9048, 255),
        // SCALED dest (8×8 atlas → 5-cell-wide band) at half opacity on row 4:
        // the bilinear/LINEAR regime case (dest ≠ atlas size).
        over_quad(
            4,
            (2 * cw) as u16,
            (4 * ch) as u16,
            (5 * cw) as u16,
            ch as u16,
            0x0040_70C0,
            140,
        ),
    ];

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    // The z pin: the block glyph's cell is PURE fg on both backends — the sprite
    // is entirely underneath it.
    let fg = {
        let c = input.cells[0][0].fg;
        ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
    };
    let cell = |f: &Frame, row: usize, col: usize| -> Vec<u32> {
        let mut out = Vec::new();
        for y in row * ch..(row + 1) * ch {
            for x in col * cw..(col + 1) * cw {
                out.push(f.pixels[y * f.width + x] & 0x00FF_FFFF);
            }
        }
        out
    };
    assert!(
        cell(&cpu_frame, 0, 0).iter().all(|&p| p == fg),
        "CPU: glyph ink must sit ON TOP of the scene_over sprite (the z fix)"
    );
    assert!(
        cell(&gpu_frame, 0, 0).iter().all(|&p| p == fg),
        "GPU: glyph ink must sit ON TOP of the scene_over sprite"
    );
    // Non-vacuous: the sprite actually painted where no glyph covers it.
    let p = cpu_frame.pixels[(ch / 2) * cpu_frame.width + 5 * cw + cw / 2];
    assert!(
        rr(p) > 100 && rr(p) > bb(p),
        "the warm sprite must show on the glyph-free cell, got #{p:06X}"
    );

    // Whole-frame parity, scaled sprite included: the scene's tolerance regime.
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("scene under-text GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU under-text scene pixels diverge: max per-channel delta {delta} > 8"
    );
}
