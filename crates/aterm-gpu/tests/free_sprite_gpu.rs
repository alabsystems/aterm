// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// FREE-floating sprite layer (`free_sprites` + `free_atlas`) on the GPU: the
// real CatBaker atlas geometry uploads (its row pitch is not wgpu's 256-byte
// copy alignment), and a real `EffectsPipeline` cat survives the GPU present
// path. The multi-row-rect-vs-legacy-slices and damaged-path no-ghosting laws
// are held on both backends by `free_parity.rs`.
//
// Gated: no GPU or no font -> the test no-ops (returns), like the other parity gates.

mod rain_common;

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::Theme;
use rain_common::RainScene;

mod common;
use common::{backends, max_channel_delta};

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

/// The shipping CatBaker atlas is `4 * cell_h` pixels wide, so its RGBA row
/// pitch is usually NOT wgpu's 256-byte copy alignment (for example, a 21 px
/// cell produces an 84 px / 336 byte row). Keep that real geometry covered:
/// a synthetic 64 px atlas has an accidentally aligned 256-byte row
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
