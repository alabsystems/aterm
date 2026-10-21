// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Free-floating sprite layer (`free_sprites` + `free_atlas`) on the GPU: the real CatBaker atlas
// geometry uploads (its row pitch is not wgpu's 256-byte copy alignment), and a real
// `EffectsPipeline` cat survives the GPU present path. `free_parity.rs` holds the cross-backend
// laws.
//
// No GPU or no font: COULD NOT RUN.

use crate::common;
use std::sync::Arc;

use crate::rain_common::RainScene;
use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::Theme;

use crate::common::{backends, bb, gg, rr};

/// What the sprite streams may differ by, CPU against GPU, where they drew
/// (`cat_parity.rs`, `rain_parity.rs`: target <= 1).
const SPRITE_BAR: i32 = 2;

/// What text may differ by, CPU against GPU: the glyph bar every parity suite
/// holds text to (`gpu_matches_cpu.rs`).
const GLYPH_BAR: i32 = 8;

/// The worst channel delta between the CPU and GPU frames `(over the sprites,
/// over the rest)`. The sprites' pixels are those where either backend's frame
/// differs from its own frame `without` the sprites, so a sprite the GPU drew
/// elsewhere, or not at all, is measured where both should have drawn it.
///
/// The rest is the word that wakes the feline pipeline (`kitty`): text, and
/// held to the glyph bar, not the sprite bar. A GPU whose blender takes a
/// glyph's coverage alpha at the 8-bit target's precision before it blends in
/// linear light lands a faint fringe over a dark ground up to 5 off the CPU
/// (NVIDIA GB10, GL arm, 2026-10-20: 17 fringe pixels of `kitty`, each where an
/// 8-bit alpha puts it; wgpu's Vulkan frame there is byte-identical to the GL
/// arm's, and lavapipe's, blending in float, is 1 off the CPU). Unlike
/// `cat_parity.rs`'s block glyphs, this text is no byte-exact base.
fn sprite_and_rest_delta(
    cpu: &[u32],
    gpu: &[u32],
    cpu_without: &[u32],
    gpu_without: &[u32],
) -> (i32, i32) {
    let (mut sprite, mut rest) = (0, 0);
    for (((&c, &g), &c0), &g0) in cpu.iter().zip(gpu).zip(cpu_without).zip(gpu_without) {
        let d = (rr(c) - rr(g))
            .abs()
            .max((gg(c) - gg(g)).abs())
            .max((bb(c) - bb(g)).abs());
        let worst = if c != c0 || g != g0 {
            &mut sprite
        } else {
            &mut rest
        };
        *worst = (*worst).max(d);
    }
    (sprite, rest)
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

/// The CatBaker atlas is `4 * cell_h` pixels wide, so its row pitch is usually
/// not wgpu's 256-byte copy alignment (a 21 px cell gives 336-byte rows). A
/// synthetic 64 px atlas would be accidentally aligned and miss a backend that
/// drops ordinary kitty atlases.
#[test]
fn free_sprite_upload_accepts_real_catbaker_row_pitch() {
    let theme = Theme::default();
    let (_, mut gpu) = backends(18.0, theme);
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

/// A real `EffectsPipeline` bakes its sparse CatBaker atlas and emits the arbitrary-rect
/// `FreeSprite` (the channel sparkle-word and companion cats share), and the art survives the
/// GPU present path. A synthetic atlas cannot catch a UV window landing in CatBaker's
/// transparent area.
#[test]
fn real_catbaker_free_sprite_is_visible_on_gpu_present_path() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    // Native windows use a padded grid: the same signed-origin translation and padded
    // offscreen/scissor dimensions as the GUI path.
    cpu.set_pad(14);
    gpu.set_pad(14);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l\x1b[7;10Hkitty");

    let mut effects = EffectsPipeline::new();
    effects.set_sparkle_enabled(true);
    effects.set_sparkle_classes(true, true, true);
    effects.set_sparkle_feline("cat", true, true, false);
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

    let cpu_base = cpu.render_input(&bare).pixels;
    let cpu_cat = cpu.render_input(&cat_input).pixels;
    let (delta, text) = sprite_and_rest_delta(&cpu_cat, &gpu_cat, &cpu_base, &base);
    assert!(
        delta <= SPRITE_BAR,
        "real CatBaker CPU/GPU parity exceeded the cat bar: {delta} > {SPRITE_BAR}"
    );
    assert!(
        text <= GLYPH_BAR,
        "the text beside the cat exceeded the glyph bar: {text} > {GLYPH_BAR}"
    );

    // Production composes both sprite atlases in one frame: phosphor rain, then the feline
    // free sprite. A rain-only frame primes the resident offscreen; the next adds only the
    // CatBaker sprite and must differ on the incremental present path.
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
    let mut fresh = common::independent_gpu(18.0, theme);
    fresh.set_pad(14);
    let mut fresh_win = aterm_gpu::WindowGpu::new();
    let gpu_rain_only = fresh.render_input(&mut fresh_win, &rain_only, None).pixels;
    assert_ne!(
        gpu_rain_cat, gpu_rain_only,
        "a real feline sprite must remain visible when the rain atlas is bound"
    );

    let cpu_rain_cat = cpu.render_input(&rain_and_cat).pixels;
    let (combined_delta, text) =
        sprite_and_rest_delta(&cpu_rain_cat, &gpu_rain_cat, &cpu_base, &base);
    assert!(
        combined_delta <= SPRITE_BAR,
        "rain + real CatBaker CPU/GPU parity exceeded the sprite bar: \
         {combined_delta} > {SPRITE_BAR}"
    );
    assert!(
        text <= GLYPH_BAR,
        "the text beside rain and the cat exceeded the glyph bar: {text} > {GLYPH_BAR}"
    );
}
