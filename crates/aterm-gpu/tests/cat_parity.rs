// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The word cats, the pet and the kitty cursor as Linear fx sprite layers
//! (`docs/DESIGN-card-engine-2026-10-19.md` §6, lane B7): the GPU draws
//! what the CPU draws, byte for byte where every texel and ink is opaque.

use aterm_core::terminal::Terminal;
use aterm_render::Theme;
use aterm_render::sprites::{FreeSprite, SpriteLayers};

use crate::common::sprites::{Publisher, cut, over, put, under};
use crate::common::{self, backends, max_channel_delta};

#[test]
fn cat_layers_match_the_cpu_over_bg_and_text() {
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in common::arms(18.0, theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let (cw, ch) = cpu.cell_size();
        let (rows, cols) = (6usize, 16usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process("\x1b[?25l████████\x1b[4;1H████████".as_bytes()); // procedural: byte-exact base

        let base_input = term.cell_frame(rows, cols);
        let cpu_base = cpu.render_input(&base_input);
        let gpu_base = gpu.render_input(&mut win, &base_input, None);
        assert_eq!(
            max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
            0,
            "procedural-block base must be byte-exact so the sprite delta is effect-only"
        );

        let hh = (ch as u16).min(32); // inside one row band and the opaque half of the sheet
        let opaque = [
            under(4, (3 * ch) as i32, cut(2, 0, 40, hh)),
            over(2, 0, cut(8, 0, 48, hh)),
            FreeSprite {
                flip_x: true,
                ..under(60, ch as i32 + 3, cut(5, 1, 33, hh))
            },
        ];
        let mut input = term.cell_frame(rows, cols);
        put(&mut input, &opaque);
        let cpu_op = cpu.render_input(&input);
        let gpu_op = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_op.pixels, cpu_base.pixels,
            "the sprites must paint (non-vacuous)"
        );
        let delta = max_channel_delta(&cpu_op.pixels, &gpu_op.pixels);
        assert_eq!(
            delta, 0,
            "{arm}: opaque texels under an opaque white ink are the texels themselves: \
         any delta is a texel the GPU's integer mapping picked differently"
        );

        let mut input = term.cell_frame(rows, cols);
        let translucent = [
            under(8, (4 * ch) as i32, cut(0, 33, 32, (ch as u16).min(30))),
            FreeSprite {
                flip_x: true,
                alpha: 140,
                tint: 0x00C0_FF80,
                ..over((cols * cw - 44) as i32, (5 * ch) as i32, cut(12, 4, 40, hh))
            },
        ];
        let all: Vec<FreeSprite> = opaque.iter().chain(&translucent).cloned().collect();
        put(&mut input, &all);
        let cpu_t = cpu.render_input(&input);
        let gpu_t = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_t.pixels, cpu_op.pixels,
            "the translucent sprites must paint"
        );
        let delta = max_channel_delta(&cpu_t.pixels, &gpu_t.pixels);
        eprintln!(
            "{arm}: cat layers GPU vs CPU max per-channel delta = {delta} (Linear bar <= {})",
            SpriteLayers::PARITY
        );
        assert!(
            delta <= i32::from(SpriteLayers::PARITY),
            "{arm}: a translucent Linear sprite broke the companions' declared parity: max per-channel \
         delta {delta} > {}",
            SpriteLayers::PARITY
        );
        common::assert_arm(arm, &gpu);
    }
}

#[test]
fn an_under_text_cat_under_an_opaque_sixel_is_hidden_on_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

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

    let hh = (ch as u16).min(32);
    let art = cut(0, 0, (cw as u16) * 2 - 4, hh);
    let base_input = term.cell_frame(rows, cols);
    let mut input = term.cell_frame(rows, cols);
    put(&mut input, &[under(2, 0, art.clone())]);

    let cpu_base = cpu.render_input(&base_input);
    let cpu_cat = cpu.render_input(&input);
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
        "CPU: an opaque sixel must hide an under-text sprite (the Linear Under \
         layers draw before the inline-image stamp)"
    );
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    let gpu_cat = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        gpu_base.pixels, gpu_cat.pixels,
        "GPU: an opaque sixel must hide an under-text sprite (FxLinearUnder \
         draws before the images)"
    );

    let mut moved = term.cell_frame(rows, cols);
    put(&mut moved, &[under(2, (2 * ch) as i32, art)]);
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

#[test]
fn damaged_path_cat_no_ghosting_and_settled_gate_hit() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    let art = cut(2, 0, 40, (ch as u16).min(32));

    let mut publisher = Publisher::default();
    let mut make = |row: usize| {
        let mut input = term.cell_frame(rows, cols);
        publisher.put(&mut input, &[under(2, (row * ch) as i32, art.clone())]);
        input
    };
    let (in_a, in_b, in_c) = (make(1), make(2), make(2));
    common::assert_moved_then_settled(
        (&mut cpu, &mut gpu),
        (18.0, theme),
        [&in_a, &in_b, &in_c],
        "cat",
        "cat (the same layer: same id, same content)",
    );
}

/// A sprite reaching past an edge of the frame, or cut to nothing by its
/// clip, lands the same pixels on both backends: the clip and the frame
/// bound every sprite, and none smears an edge texel.
#[test]
fn a_sprite_at_or_past_an_edge_lands_the_same_pixels_on_both_backends() {
    use aterm_core::render::PxRect;
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");

    let base = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base).pixels;
    let gpu_base = gpu.render_input(&mut win, &base, None).pixels;
    assert_eq!(
        max_channel_delta(&cpu_base, &gpu_base),
        0,
        "blank base must be byte-exact so the sprite delta is effect-only"
    );

    let (gw, gh) = ((cols * cw) as i32, (rows * ch) as i32);
    let art = cut(2, 0, 40, 20);
    let cases: [(&str, FreeSprite, bool); 7] = [
        (
            "in-bounds control",
            under(4, 2 * ch as i32, art.clone()),
            true,
        ),
        (
            "past the right edge",
            over(gw - 13, 2 * ch as i32, art.clone()),
            true,
        ),
        (
            "past the left and top edges",
            over(-17, -9, art.clone()),
            true,
        ),
        (
            "below the last grid row",
            under(4, gh + 64, art.clone()),
            false,
        ),
        ("left of the frame", over(-400, 0, art.clone()), false),
        (
            "clipped to a window",
            FreeSprite {
                clip: PxRect::new(10, 2 * ch as i32 + 3, 31, 2 * ch as i32 + 11),
                ..under(4, 2 * ch as i32, art.clone())
            },
            true,
        ),
        (
            "a stretched cut past the bottom edge",
            FreeSprite {
                w: 80,
                h: 60,
                ..over(30, gh - 25, art.clone())
            },
            true,
        ),
    ];
    for (name, sprite, should_paint) in cases {
        let mut input = term.cell_frame(rows, cols);
        put(&mut input, std::slice::from_ref(&sprite));
        let c = cpu.render_input(&input).pixels;
        let g = gpu.render_input(&mut win, &input, None).pixels;
        assert_eq!(
            c == cpu_base,
            !should_paint,
            "{name}: the CPU is the reference for what this sprite may draw"
        );
        assert_eq!(
            g == gpu_base,
            !should_paint,
            "{name}: the GPU must draw exactly where the CPU draws"
        );
        assert_eq!(
            max_channel_delta(&c, &g),
            0,
            "{name}: CPU and GPU must land the same pixels"
        );
    }
}
