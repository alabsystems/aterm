// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The companions' sprites, as the effects pipeline publishes them, on the GPU.

use crate::common;

use crate::rain_common::RainScene;
use aterm_core::terminal::Terminal;
use aterm_effects::pipeline::EffectsPipeline;
use aterm_render::Theme;
use aterm_render::sprites::{FreeSprite, SpriteLayers, published};

use crate::common::sprites::{put, solid, under};
use crate::common::{backends, bb, gg, max_channel_delta, rr};

const SPRITE_BAR: i32 = SpriteLayers::PARITY as i32;
const GLYPH_BAR: i32 = 8;

/// Worst channel delta between CPU and GPU frames `(over sprites, over rest)`.
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

/// A CatBaker tile's row is `84 × 4` bytes, no multiple of 256: the region
/// upload into the fx atlas must take any pitch.
#[test]
fn an_art_whose_row_pitch_is_no_multiple_of_256_uploads_and_paints() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");

    let art = solid([0xF7, 0xA8, 0xB8, 0xFF], 84, 48);
    let base_input = term.cell_frame(rows, cols);
    let base = gpu.render_input(&mut win, &base_input, None).pixels;
    let mut cat_input = term.cell_frame(rows, cols);
    put(&mut cat_input, &[under(6, 6, art)]);
    let cat = gpu.render_input(&mut win, &cat_input, None).pixels;

    assert_ne!(
        cat, base,
        "an art of a non-256-byte row pitch must paint its sprite"
    );
    assert_eq!(
        max_channel_delta(&cat, &cpu.render_input(&cat_input).pixels),
        0,
        "…and land the CPU's bytes (an opaque art under an opaque white ink)"
    );
}

#[test]
fn real_catbaker_sprite_is_visible_on_gpu_present_path() {
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in common::arms(18.0, theme) {
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
            if !published(&input.fx_layers).is_empty() {
                cat_input = Some(input.clone());
                break;
            }
        }
        let cat_input = cat_input.expect("the real feline pipeline must publish a sprite layer");
        assert!(
            published(&cat_input.fx_layers).iter().any(|s| s
                .art
                .data()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] != 0)),
            "a real CatBaker art must contain visible authored pixels"
        );

        let mut bare = cat_input.clone();
        bare.fx_layers.retain(|l| FreeSprite::of_layer(l).is_none());
        let base = gpu.present_input_readback(&mut win, &bare).pixels;
        let gpu_cat = gpu.present_input_readback(&mut win, &cat_input).pixels;
        assert_ne!(
            gpu_cat, base,
            "the CatBaker sprite layer must paint on the GPU present path"
        );

        let cpu_base = cpu.render_input(&bare).pixels;
        let cpu_cat = cpu.render_input(&cat_input).pixels;
        let (delta, text) = sprite_and_rest_delta(&cpu_cat, &gpu_cat, &cpu_base, &base);
        assert!(
            delta <= SPRITE_BAR,
            "{arm}: real CatBaker CPU/GPU parity exceeded the Linear bar: {delta} > {SPRITE_BAR}"
        );
        assert!(
            text <= GLYPH_BAR,
            "the text beside the cat exceeded the glyph bar: {text} > {GLYPH_BAR}"
        );

        let mut rain_term = Terminal::new(rows as u16, cols as u16);
        rain_term.process("\x1b[?25l████████".as_bytes());
        let rain_base = rain_term.cell_frame(rows, cols);
        let mut rain = RainScene::new(rows, cols, (cw, ch), &rain_base);
        rain.drive_until_raining();
        let mut rain_only = bare.clone();
        rain.apply(&mut rain_only);
        let mut rain_and_cat = cat_input.clone();
        rain.apply(&mut rain_and_cat);
        assert!(rain.drawn().next().is_some(), "the rain must draw cells");

        let _ = gpu.present_input_readback(&mut win, &rain_only);
        let gpu_rain_cat = gpu.present_input_readback(&mut win, &rain_and_cat).pixels;
        let mut fresh = common::independent_gpu(18.0, theme);
        fresh.set_pad(14);
        let mut fresh_win = aterm_gpu::WindowGpu::new();
        let gpu_rain_only = fresh.render_input(&mut fresh_win, &rain_only, None).pixels;
        assert_ne!(
            gpu_rain_cat, gpu_rain_only,
            "a real feline sprite must stay visible beside the rain's tiles in the one atlas"
        );

        let cpu_rain_cat = cpu.render_input(&rain_and_cat).pixels;
        let (combined_delta, text) =
            sprite_and_rest_delta(&cpu_rain_cat, &gpu_rain_cat, &cpu_base, &base);
        assert!(
            combined_delta <= SPRITE_BAR,
            "{arm}: rain + real CatBaker CPU/GPU parity exceeded the Linear bar: \
         {combined_delta} > {SPRITE_BAR}"
        );
        assert!(
            text <= GLYPH_BAR,
            "the text beside rain and the cat exceeded the glyph bar: {text} > {GLYPH_BAR}"
        );
        common::assert_arm(arm, &gpu);
    }
}
