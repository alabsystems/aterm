// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU/GPU parity and damage-cache tests for the free-floating sprite layer.

use crate::common;
use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_render::{SceneAtlas, SpriteQuad, Theme, WindowCpu};

use crate::common::{backends, bb, gg, max_channel_delta, rr};

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

#[test]
fn free_effect_only_parity_pinned_over_bg_and_text() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l████████".as_bytes()); // procedural: byte-exact base

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
        free_1to1(4, (2 * ch) as i32, 40, mh.min(60), [2, 0], 255),
        free_1to1(2, (ch / 3) as i32, 48, mh.min(60), [8, 0], 255),
        free_1to1(8, (4 * ch) as i32, 32, (ch as u16).min(56), [0, 65], 255),
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

#[test]
fn free_over_text_covers_wdeco_and_additive_light_both_backends() {
    use aterm_core::render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration};
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l██".as_bytes());

    let red = 0x00C8_2020u32;
    let atlas = aterm_render::SceneAtlas {
        width: 8,
        height: 8,
        rgba: (0..8 * 8).flat_map(|_| [0xC8u8, 0x20, 0x20, 255]).collect(),
        version: 11,
    };

    let mut input = term.cell_frame(rows, cols);
    input.free_atlas = Some(Arc::new(atlas));
    input.free_sprites = vec![FreeSprite {
        z: FreeZ::OverText,
        aw: 8,
        ah: 8,
        ..free_1to1(0, 0, (2 * cw) as u16, (2 * ch) as u16, [0, 0], 255)
    }];
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
        alpha: 0,
        color2: aterm_render::premul_rgb(0x0040_80FF, 200),
        alpha2: 0,
    });
    input.nova_add.push(GlowQuad {
        row: 0,
        x: 0,
        y: 0,
        w: cw as u16,
        h: ch as u16,
        color: aterm_render::premul_rgb(0x00FF_C040, 200),
        alpha: 0,
        color2: aterm_render::premul_rgb(0x00FF_C040, 200),
        alpha2: 0,
    });

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

#[test]
fn free_multirow_rect_matches_legacy_slices_on_damaged_path_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
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

    let mut win_free = aterm_gpu::WindowGpu::new();
    let _ = gpu.render_input_cached(&mut win_free, &make_free(&mut term, y_a));
    let gpu_free = gpu
        .render_input_cached(&mut win_free, &make_free(&mut term, y_b))
        .pixels()
        .to_vec();
    let mut gpu2 = common::independent_gpu(18.0, theme);
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

#[test]
fn damaged_path_free_no_ghosting_and_settled_gate_hit_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
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
    let in_c = make((2 * ch + ch / 2) as i32); // settled: B again
    common::assert_moved_then_settled(
        (&mut cpu, &mut gpu),
        (18.0, theme),
        [&in_a, &in_b, &in_c],
        "multi-row free rect",
        "free sprite (equal sprites, same atlas version)",
    );
}

fn assert_damaged_frames_byte_stable(
    (cpu, gpu): (&mut aterm_render::Renderer, &mut aterm_gpu::GpuRenderer),
    theme: Theme,
    mut term: Terminal,
    make: impl Fn(&mut Terminal, usize) -> aterm_render::RenderInput,
    cpu_why: &str,
    gpu_why: &str,
) {
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
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
        assert_eq!(cpu_cached, cpu_fresh, "frame {n}: {cpu_why}");
        let mut gpu2 = common::independent_gpu(18.0, theme);
        let mut win2 = aterm_gpu::WindowGpu::new();
        let gpu_fresh = gpu2.render_input(&mut win2, &input, None).pixels;
        assert_eq!(gpu_scissored, gpu_fresh, "frame {n}: {gpu_why}");
    }
}

#[test]
fn settled_translucent_sprite_inside_dirty_band_stays_byte_stable() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (12usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(free_atlas(7));

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

    assert_damaged_frames_byte_stable(
        (&mut cpu, &mut gpu),
        theme,
        term,
        make,
        "CPU cached repaint must be byte-stable (no re-blend of \
         the settled translucent sprite)",
        "GPU scissored repaint must be byte-stable — a settled \
         sprite row inside the dirty bounding band must be fully rebuilt, \
         not Load-preserved under an unconditional sprite redraw",
    );
}

#[test]
fn settled_translucent_over_text_sprite_inside_dirty_band_stays_byte_stable() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (12usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    term.process(b"\x1b[5;3Hcovered text");
    let atlas = Arc::new(free_atlas(9));

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

    assert_damaged_frames_byte_stable(
        (&mut cpu, &mut gpu),
        theme,
        term,
        make,
        "CPU damaged path must be byte-stable for a settled \
         translucent OverText sprite inside the dirty bounding band",
        "GPU scissored path must be byte-stable — a settled \
         OverText sprite row inside the dirty bounding band must be fully \
         rebuilt, not Load-preserved under the unconditional FreeOver redraw",
    );
}

#[test]
fn settled_lifted_add_deco_spill_inside_dirty_band_stays_byte_stable() {
    use aterm_core::render::{DecoBlend, DecoGlyph, WordDecoration};
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
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
        term.process(format!("\x1b[2;1Htop dirt {n}\x1b[6;1Hdeco row dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.word_decorations.push(deco);
        input
    };

    assert_damaged_frames_byte_stable(
        (&mut cpu, &mut gpu),
        theme,
        term,
        make,
        "CPU damaged path must be byte-stable — the settled \
         lifted Add deco's dy spill must land on a rebuilt neighbour row",
        "GPU scissored path must be byte-stable — the spill \
         neighbour row inside the scissor band must be rebuilt, not \
         Load-preserved under the row-gated wdeco re-stamp",
    );
}

#[test]
fn settled_translucent_cat_quad_inside_dirty_band_stays_byte_stable() {
    let theme = Theme::default();
    let (cpu, mut gpu) = backends(18.0, theme);
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
        let mut gpu2 = common::independent_gpu(18.0, theme);
        let mut win2 = aterm_gpu::WindowGpu::new();
        let gpu_fresh = gpu2.render_input(&mut win2, &input, None).pixels;
        assert_eq!(
            gpu_scissored, gpu_fresh,
            "frame {n}: a settled translucent cat quad inside the dirty \
             bounding band must be byte-stable on the GPU"
        );
    }
}

#[test]
fn free_scaled_sprite_samples_the_same_texel_as_the_cpu() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");

    let (nat_w, nat_h) = (40u16, 40u16);
    let mut scaled_seen = 0usize;
    for pct in [100u32, 50, 80, 90, 95, 105, 110, 112, 120, 125, 150, 200] {
        for flip in [false, true] {
            let dest =
                |n: u16| -> u16 { ((f64::from(n) * f64::from(pct) / 100.0).round() as u16).max(1) };
            let (dw, dh) = (dest(nat_w), dest(nat_h));
            if dw != nat_w {
                scaled_seen += 1;
            }
            let mut input = term.cell_frame(rows, cols);
            input.free_atlas = Some(Arc::new(free_atlas(1)));
            input.free_sprites = vec![FreeSprite {
                x: 20,
                y: 20,
                w: dw,
                h: dh,
                ax: 8,
                ay: 8, // the atlas's OPAQUE half: a wrong texel is a colour, not a blend
                aw: nat_w,
                ah: nat_h,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x: flip,
                z: FreeZ::OverText,
                sampler: FreeSampler::Nearest,
            }];
            let c = cpu.render_input(&input);
            let g = gpu.render_input(&mut win, &input, None);
            let mut bare = input.clone();
            bare.free_sprites.clear();
            assert_ne!(
                cpu.render_input(&bare).pixels,
                c.pixels,
                "scale {pct}% flip={flip}: the sprite painted nothing — the pin would be vacuous"
            );
            let delta = max_channel_delta(&c.pixels, &g.pixels);
            assert!(
                delta <= 2,
                "scale {pct}% flip={flip} (dest {dw}x{dh} from {nat_w}x{nat_h}): the GPU \
                 NEAREST sample diverged from the CPU stamp — max per-channel delta {delta} \
                 > 2. A pixel-centre tie resolved the other way; see `uv_tie_bias`."
            );
        }
    }
    assert!(
        scaled_seen >= 16,
        "the sweep must actually exercise the SCALED arm (saw {scaled_seen} scaled cases)"
    );
}
