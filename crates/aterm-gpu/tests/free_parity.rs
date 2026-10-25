// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU/GPU parity and damage-cache tests for the companion sprites (the
//! flying head, its notes, the motes, the dog, the nuke): Linear fx sprite
//! layers drawn from the one fx atlas.

use crate::common;

use aterm_core::terminal::Terminal;
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};
use aterm_render::{Theme, WindowCpu};

use crate::common::sprites::{Publisher, cut, layers_of, opaque, over, put, solid, under};
use crate::common::{backends, bb, gg, max_channel_delta, rr};

#[test]
fn companion_layers_match_the_cpu_across_bands_over_bg_and_text() {
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in common::arms(18.0, theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let (cw, ch) = cpu.cell_size();
        let (rows, cols) = (8usize, 16usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process("\x1b[?25l████████\x1b[7;3H████████".as_bytes()); // procedural: byte-exact base

        let base_input = term.cell_frame(rows, cols);
        let cpu_base = cpu.render_input(&base_input);
        let gpu_base = gpu.render_input(&mut win, &base_input, None);
        assert_eq!(
            max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
            0,
            "procedural-block base must be byte-exact so the sprite delta is effect-only"
        );

        let mh = ((ch + ch / 2) as u16).min(60); // > one band: every box crosses a boundary
        let opaque_set = [
            under(4, (2 * ch) as i32, opaque(2, 0, 40, mh)),
            under(2, (ch / 3) as i32, opaque(8, 0, 48, mh)),
            FreeSprite {
                flip_x: true,
                ..under(
                    (cols * cw - 44) as i32,
                    (5 * ch) as i32,
                    opaque(12, 4, 40, mh),
                )
            },
            over((10 * cw) as i32, (ch / 2) as i32, opaque(20, 0, 36, mh)),
        ];
        let mut input = term.cell_frame(rows, cols);
        put(&mut input, &opaque_set);
        let cpu_op = cpu.render_input(&input);
        let gpu_op = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_op.pixels, cpu_base.pixels,
            "the sprites must paint (non-vacuous)"
        );
        assert_eq!(
            max_channel_delta(&cpu_op.pixels, &gpu_op.pixels),
            0,
            "{arm}: opaque texels under an opaque white ink, across band boundaries: \
         byte-exact, or a band split or a texel mapping differs"
        );

        let translucent = [
            under(8, (4 * ch) as i32, cut(0, 33, 32, (ch as u16).min(30))),
            FreeSprite {
                flip_x: true,
                alpha: 140,
                ..under((cols * cw - 44) as i32, (5 * ch) as i32, cut(12, 4, 40, mh))
            },
            FreeSprite {
                tint: 0x0040_C080,
                ..under((6 * cw) as i32, (6 * ch + 3) as i32, cut(5, 2, 36, mh))
            },
            FreeSprite {
                z: FreeZ::OverText,
                alpha: 90,
                ..under((3 * cw) as i32, (6 * ch) as i32, cut(20, 10, 36, mh))
            },
        ];
        let all: Vec<FreeSprite> = opaque_set.iter().chain(&translucent).cloned().collect();
        let mut input = term.cell_frame(rows, cols);
        put(&mut input, &all);
        let cpu_t = cpu.render_input(&input);
        let gpu_t = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_t.pixels, cpu_op.pixels,
            "the translucent sprites must paint"
        );
        let delta = max_channel_delta(&cpu_t.pixels, &gpu_t.pixels);
        eprintln!(
            "{arm}: companion layers GPU vs CPU max per-channel delta = {delta} (Linear bar <= {})",
            SpriteLayers::PARITY
        );
        assert!(
            delta <= i32::from(SpriteLayers::PARITY),
            "{arm}: a translucent or tinted Linear sprite broke the companions' declared parity: \
         max per-channel delta {delta} > {}",
            SpriteLayers::PARITY
        );
        common::assert_arm(arm, &gpu);
    }
}

#[test]
fn an_over_text_sprite_covers_wdeco_and_additive_light_both_backends() {
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
    let mut input = term.cell_frame(rows, cols);
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
    let bare = input.clone();
    put(
        &mut input,
        &[FreeSprite {
            w: (2 * cw) as u16,
            h: (2 * ch) as u16,
            ..over(0, 0, solid([0xC8, 0x20, 0x20, 255], 8, 8))
        }],
    );

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
    assert_eq!(
        gpu_max, 0,
        "GPU FxLinearOver must cover glyph/wdeco/glow/nova with the opaque \
         sprite's own colour"
    );
}

/// The rain's per-row layers and a companion's single layer are one law: a
/// sprite cut into its band slices (each a layer clipped to its band) draws
/// what the whole sprite draws, on the damaged path of both backends.
#[test]
fn a_multirow_sprite_is_its_band_slices_on_the_damaged_path_both_backends() {
    use aterm_core::render::PxRect;
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");

    let (x, w) = (4i32, 40u16);
    let h = (2 * ch + ch / 2) as u16;
    let (y_a, y_b) = ((ch + ch / 2) as i32, (2 * ch + ch / 2) as i32);
    let art = cut(2, 3, w, h);
    let sprite = |y: i32| under(x, y, art.clone());

    let mut whole = Publisher::default();
    let mut sliced = Publisher::default();
    let mut make_whole = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        whole.put(&mut input, &[sprite(y)]);
        input
    };
    let mut make_sliced = |term: &mut Terminal, y: i32| {
        let mut input = term.cell_frame(rows, cols);
        let s = sprite(y);
        let (y0, y1) = (y as usize, y as usize + h as usize);
        let slices: Vec<FreeSprite> = (y0 / ch..=(y1 - 1) / ch)
            .map(|r| FreeSprite {
                clip: PxRect::new(i32::MIN, (r * ch) as i32, i32::MAX, ((r + 1) * ch) as i32),
                ..s.clone()
            })
            .collect();
        assert!(slices.len() >= 3, "multi-row premise");
        sliced.put(&mut input, &slices);
        input
    };

    let (wa, wb) = (make_whole(&mut term, y_a), make_whole(&mut term, y_b));
    let (sa, sb) = (make_sliced(&mut term, y_a), make_sliced(&mut term, y_b));
    let mut wc_whole = WindowCpu::new();
    let mut wc_sliced = WindowCpu::new();
    let _ = cpu.render_input_cached(&mut wc_whole, &wa);
    let cpu_whole = cpu
        .render_input_cached(&mut wc_whole, &wb)
        .pixels()
        .to_vec();
    let _ = cpu.render_input_cached(&mut wc_sliced, &sa);
    let cpu_sliced = cpu
        .render_input_cached(&mut wc_sliced, &sb)
        .pixels()
        .to_vec();
    assert_eq!(
        cpu_whole, cpu_sliced,
        "CPU damaged path: a moved multi-row sprite must equal its moved band \
         slices byte-for-byte"
    );

    let mut win_whole = aterm_gpu::WindowGpu::new();
    let _ = gpu.render_input_cached(&mut win_whole, &wa);
    let gpu_whole = gpu
        .render_input_cached(&mut win_whole, &wb)
        .pixels()
        .to_vec();
    let mut gpu2 = common::independent_gpu(18.0, theme);
    let mut win_sliced = aterm_gpu::WindowGpu::new();
    let _ = gpu2.render_input_cached(&mut win_sliced, &sa);
    let gpu_sliced = gpu2
        .render_input_cached(&mut win_sliced, &sb)
        .pixels()
        .to_vec();
    assert_eq!(
        max_channel_delta(&gpu_whole, &gpu_sliced),
        0,
        "GPU damaged path: a moved multi-row sprite must equal its moved band slices"
    );
    assert!(
        max_channel_delta(&gpu_whole, &cpu_whole) <= i32::from(SpriteLayers::PARITY),
        "and the GPU's frame is the CPU's within the companions' declared parity"
    );
}

#[test]
fn damaged_path_sprite_no_ghosting_and_settled_gate_hit_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let art = cut(2, 0, 40, (2 * ch + ch / 2) as u16);

    let mut publisher = Publisher::default();
    let mut make = |y: i32| {
        let mut input = term.cell_frame(rows, cols);
        publisher.put(&mut input, &[under(2, y, art.clone())]);
        input
    };
    let in_a = make((ch + ch / 2) as i32); // bands 1..=3
    let in_b = make((2 * ch + ch / 2) as i32); // bands 2..=4
    let in_c = make((2 * ch + ch / 2) as i32); // settled: B again
    common::assert_moved_then_settled(
        (&mut cpu, &mut gpu),
        (18.0, theme),
        [&in_a, &in_b, &in_c],
        "multi-row sprite",
        "sprite (the same layer: same id, same content)",
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

    let layers = layers_of(&[FreeSprite {
        alpha: 140,
        ..under(
            6,
            (3 * ch + ch / 4) as i32,
            cut(0, 33, 48, (3 * ch + ch / 2) as u16),
        )
    }]);
    let make = |term: &mut Terminal, n: usize| {
        term.process(format!("\x1b[3;1Htop dirt {n}\x1b[11;1Hbottom dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.fx_layers.extend(layers.iter().cloned());
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

    let layers = layers_of(&[FreeSprite {
        alpha: 140,
        ..over(
            6,
            (3 * ch + ch / 4) as i32,
            cut(0, 33, 48, (3 * ch + ch / 2) as u16),
        )
    }]);
    let make = |term: &mut Terminal, n: usize| {
        term.process(format!("\x1b[3;1Htop dirt {n}\x1b[11;1Hbottom dirt {n}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        input.fx_layers.extend(layers.iter().cloned());
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
         rebuilt, not Load-preserved under the FxLinearOver redraw",
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

/// The integer nearest map (`docs/DESIGN-card-engine-2026-10-19.md` §4.1):
/// the GPU picks the CPU's texel at every scale and mirror, so an opaque art
/// under an opaque white ink lands byte-exact; the old float sampler needed
/// a tie bias and still parted at some scales.
#[test]
fn a_scaled_sprite_samples_the_cpus_texel_at_every_scale() {
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in common::arms(18.0, theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let (rows, cols) = (8usize, 20usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25l");

        let (nat_w, nat_h) = (40u16, 40u16);
        let art = opaque(8, 8, nat_w, nat_h);
        let mut scaled_seen = 0usize;
        for pct in [100u32, 50, 80, 90, 95, 105, 110, 112, 120, 125, 150, 200] {
            for flip in [false, true] {
                let dest = |n: u16| -> u16 {
                    ((f64::from(n) * f64::from(pct) / 100.0).round() as u16).max(1)
                };
                let (dw, dh) = (dest(nat_w), dest(nat_h));
                if dw != nat_w {
                    scaled_seen += 1;
                }
                let bare = term.cell_frame(rows, cols);
                let mut input = bare.clone();
                put(
                    &mut input,
                    &[FreeSprite {
                        w: dw,
                        h: dh,
                        flip_x: flip,
                        ..over(20, 20, art.clone())
                    }],
                );
                let c = cpu.render_input(&input);
                let g = gpu.render_input(&mut win, &input, None);
                assert_ne!(
                    cpu.render_input(&bare).pixels,
                    c.pixels,
                    "scale {pct}% flip={flip}: the sprite painted nothing — the pin would be vacuous"
                );
                let delta = max_channel_delta(&c.pixels, &g.pixels);
                assert_eq!(
                    delta, 0,
                    "{arm}: scale {pct}% flip={flip} (dest {dw}x{dh} from {nat_w}x{nat_h}): the GPU \
                 picked another texel than the CPU's integer nearest map"
                );
            }
        }
        assert!(
            scaled_seen >= 16,
            "the sweep must actually exercise the SCALED arm (saw {scaled_seen} scaled cases)"
        );
        common::assert_arm(arm, &gpu);
    }
}

/// A COMPANION STANDS IN FRONT OF THE LIGHT UNDER THE WORDS, ON BOTH BACKENDS. A knock layer's
/// body sits on the under-glyph light (`glow_under`: the rainbow ribbon's body, the meteor's
/// train, the fire's flame body), never in it, and under the ink; its letters' knock-out ring
/// of page lies on the body only. The GPU's `FxKnockUnder` and `GlyphKnock` open its base-fg
/// group after the Unorm interpose, as the CPU draws them after `draw_fire_patch`. Drawn under
/// that light, a cat whose head reached into the typed row wore the ribbon's colours (GitHub
/// Light, round 14's open K5 item).
#[test]
fn a_companion_sits_on_the_under_glyph_light_both_backends() {
    use crate::common::sprites::art_of;
    use aterm_core::render::GlowQuad;
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in common::arms(18.0, theme) {
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        let mut win = aterm_gpu::WindowGpu::new();
        let (cw, ch) = cpu.cell_size();
        let (rows, cols) = (3usize, 8usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        // A full block at col 0 and a letter at col 1, both in the body's box.
        term.process("\x1b[?25l█x".as_bytes());
        let red = 0x00C8_2020u32;
        // The body's box spans cols 0..4 of row 0; its art is clear over col 1.
        let art = art_of((4 * cw) as u16, ch as u16, |x, _| {
            if (cw..2 * cw).contains(&usize::from(x)) {
                [0, 0, 0, 0]
            } else {
                [0xC8, 0x20, 0x20, 255]
            }
        });
        let mut input = term.cell_frame(rows, cols);
        // The ribbon's body across row 0: source-over as light themes lay it, and additive.
        for (alpha, color) in [(160u8, 0x0040_C0A0u32), (0, 0x0060_6000)] {
            let premul = aterm_render::premul_rgb(color, if alpha == 0 { 200 } else { alpha });
            input.glow_under.push(GlowQuad {
                row: 0,
                x: 0,
                y: 0,
                w: (cols * cw) as u16,
                h: ch as u16,
                color: premul,
                alpha,
                color2: premul,
                alpha2: alpha,
            });
        }
        let lit = gpu.render_input(&mut win, &input, None).pixels.clone();
        put(&mut input, &[under(0, 0, art)]);
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        common::assert_arm(arm, &gpu);
        let w = gpu_f.width;
        let cell = |px: &[u32], col: usize| -> Vec<u32> {
            (0..ch)
                .flat_map(|y| (col * cw..(col + 1) * cw).map(move |x| px[y * w + x]))
                .collect()
        };
        let body = cell(&gpu_f.pixels, 3);
        let off = body
            .iter()
            .map(|&g| {
                (rr(g) - rr(red))
                    .abs()
                    .max((gg(g) - gg(red)).abs())
                    .max((bb(g) - bb(red)).abs())
            })
            .max()
            .unwrap_or(0);
        assert!(
            off <= 2,
            "{arm}: GPU body over the under-glyph light: max delta {off} from its own colour"
        );
        let clear = max_channel_delta(&cell(&gpu_f.pixels, 1), &cell(&lit, 1));
        assert!(
            clear <= 2,
            "{arm}: where the art is clear the letter keeps the light round it on the GPU: \
             max delta {clear}"
        );
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        assert!(
            delta <= 2,
            "{arm}: and the backends agree over the whole frame: max delta {delta}"
        );
    }
}
