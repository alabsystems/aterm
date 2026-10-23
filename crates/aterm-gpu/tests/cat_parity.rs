// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::common;
use std::sync::Arc;

use aterm_core::terminal::Terminal;
use aterm_render::{SceneAtlas, SpriteQuad, Theme};

use crate::common::{backends, max_channel_delta};

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

#[test]
fn cat_effect_only_parity_pinned_over_bg_and_text() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l████████".as_bytes()); // procedural: byte-exact base

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
        quad_1to1(3, [4, (3 * ch) as u16, 40, hh], [2, 0], 255),
        quad_1to1(0, [2, 0, 48, hh], [8, 0], 255),
        quad_1to1(
            4,
            [8, (4 * ch) as u16, 32, (ch as u16).min(30)],
            [0, 33],
            255,
        ),
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

#[test]
fn cat_under_opaque_sixel_is_hidden_on_both_backends() {
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

    let base_input = term.cell_frame(rows, cols);
    let mut input = term.cell_frame(rows, cols);
    input.cat_atlas = Some(Arc::new(cat_atlas(1)));
    let hh = (ch as u16).min(32);
    input.cat_quads = vec![quad_1to1(0, [2, 0, (cw as u16) * 2 - 4, hh], [0, 0], 255)];

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
        "CPU: an opaque sixel must hide the cat sprite (the pass-1c sprite stamp \
         is under the inline-image stamp — pass 2b for this image's z >= 0)"
    );
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    let gpu_cat = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        gpu_base.pixels, gpu_cat.pixels,
        "GPU: an opaque sixel must hide the cat sprite (cat draws before images)"
    );

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

#[test]
fn damaged_path_cat_no_ghosting_and_settled_gate_hit() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (_, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 12usize);
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
    let (in_a, in_b, in_c) = (make(1), make(2), make(2));
    common::assert_moved_then_settled(
        (&mut cpu, &mut gpu),
        (18.0, theme),
        [&in_a, &in_b, &in_c],
        "cat",
        "cat (equal quads, same atlas version)",
    );
}

#[test]
fn a_malformed_sprite_quad_is_refused_on_both_backends() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (_, ch) = cpu.cell_size();
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

    let cases: [(&str, [u16; 4], u16, bool); 4] = [
        ("in-bounds control", [2, 0, 40, 20], 3, true),
        (
            "source rect overruns the atlas bottom",
            [2, 40, 40, 34],
            3,
            false,
        ),
        (
            "source rect overruns the atlas right",
            [40, 0, 40, 20],
            3,
            false,
        ),
        (
            "row tagged past the last grid row",
            [2, 0, 40, 20],
            99,
            false,
        ),
    ];
    for (name, [ax, ay, aw, ah], row, should_paint) in cases {
        for stream in ["cat", "rain"] {
            let quad = SpriteQuad {
                row,
                x: 4,
                y: (2 * ch) as u16,
                w: aw,
                h: ah,
                ax,
                ay,
                aw,
                ah,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x: false,
            };
            let mut input = term.cell_frame(rows, cols);
            if stream == "cat" {
                input.cat_atlas = Some(Arc::new(cat_atlas(1)));
                input.cat_quads = vec![quad];
            } else {
                input.rain_atlas = Some(Arc::new(cat_atlas(1)));
                input.rain_quads = vec![quad];
            }
            let c = cpu.render_input(&input).pixels;
            let g = gpu.render_input(&mut win, &input, None).pixels;
            assert_eq!(
                c == cpu_base,
                !should_paint,
                "{stream}: {name} — the CPU is the reference for what this quad may draw"
            );
            assert_eq!(
                g == gpu_base,
                !should_paint,
                "{stream}: {name} — the GPU must draw exactly what the CPU draws, \
                 not the ClampToEdge smear of an out-of-range rect"
            );
            assert_eq!(
                max_channel_delta(&c, &g),
                0,
                "{stream}: {name} — CPU and GPU must land the same pixels"
            );
        }
    }
}
