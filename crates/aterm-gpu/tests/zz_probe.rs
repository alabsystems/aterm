// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// SCRATCH PROBE — delete before finishing. Its sibling
// `crates/aterm-core/tests/zz_mode_sweep.rs` was deleted in 1f08e39ba; this one was
// missed, and it fails `license_check.sh` for the missing Copyright line, which is a
// whole-tree gate stage — so every branch that merges main inherits a red it did not
// cause. The line is added here to unblock that; DELETING the file is still the right
// end state and belongs to whoever owns the effects work.

use std::sync::Arc;

use aterm_core::terminal::Terminal;
use aterm_render::{SceneAtlas, SpriteQuad, Theme};

mod common;
use common::backends;

fn atlas(version: u64, w: u32, h: u32) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
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

fn npaint(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
}

#[test]
fn zz_cat_quad_overrunning_source_rect() {
    println!("CONTROL {}", 1 + 1);
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        println!("SKIPPED-NO-GPU");
        return;
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (_cw, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    println!(
        "BASE diff px = {}",
        npaint(&cpu_base.pixels, &gpu_base.pixels)
    );

    // A 64x64 atlas, source rect ay=40 ah=34 -> ay+ah = 74 > 64. OVERRUN.
    let mut input = term.cell_frame(rows, cols);
    input.cat_atlas = Some(Arc::new(atlas(1, 64, 64)));
    let hh = 34u16;
    input.cat_quads = vec![SpriteQuad {
        row: 3,
        x: 4,
        y: (3 * ch) as u16,
        w: 40,
        h: hh,
        ax: 2,
        ay: 40,
        aw: 40,
        ah: hh,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }];

    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    println!(
        "CAT-OVERRUN: cpu painted {} px, gpu painted {} px, cpu-vs-gpu diff {} px",
        npaint(&cpu_f.pixels, &cpu_base.pixels),
        npaint(&gpu_f.pixels, &gpu_base.pixels),
        npaint(&cpu_f.pixels, &gpu_f.pixels)
    );

    // Same but X overrun: ax=40 aw=40 -> 80 > 64.
    let mut input2 = term.cell_frame(rows, cols);
    input2.cat_atlas = Some(Arc::new(atlas(1, 64, 64)));
    input2.cat_quads = vec![SpriteQuad {
        row: 3,
        x: 4,
        y: (3 * ch) as u16,
        w: 40,
        h: 20,
        ax: 40,
        ay: 0,
        aw: 40,
        ah: 20,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }];
    let cpu_f2 = cpu.render_input(&input2);
    let gpu_f2 = gpu.render_input(&mut win, &input2, None);
    println!(
        "CAT-OVERRUN-X: cpu painted {} px, gpu painted {} px, cpu-vs-gpu diff {} px",
        npaint(&cpu_f2.pixels, &cpu_base.pixels),
        npaint(&gpu_f2.pixels, &gpu_base.pixels),
        npaint(&cpu_f2.pixels, &gpu_f2.pixels)
    );

    // RAIN: same shape through the same build_sprites().
    let mut input3 = term.cell_frame(rows, cols);
    input3.rain_atlas = Some(Arc::new(atlas(1, 64, 64)));
    input3.rain_quads = vec![SpriteQuad {
        row: 3,
        x: 4,
        y: (3 * ch) as u16,
        w: 40,
        h: hh,
        ax: 2,
        ay: 40,
        aw: 40,
        ah: hh,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }];
    let cpu_f3 = cpu.render_input(&input3);
    let gpu_f3 = gpu.render_input(&mut win, &input3, None);
    println!(
        "RAIN-OVERRUN: cpu painted {} px, gpu painted {} px, cpu-vs-gpu diff {} px",
        npaint(&cpu_f3.pixels, &cpu_base.pixels),
        npaint(&gpu_f3.pixels, &gpu_base.pixels),
        npaint(&cpu_f3.pixels, &gpu_f3.pixels)
    );

    // CONTROL: an IN-BOUNDS quad must agree on both backends.
    let mut input4 = term.cell_frame(rows, cols);
    input4.cat_atlas = Some(Arc::new(atlas(1, 64, 64)));
    input4.cat_quads = vec![SpriteQuad {
        row: 3,
        x: 4,
        y: (3 * ch) as u16,
        w: 40,
        h: 20,
        ax: 2,
        ay: 0,
        aw: 40,
        ah: 20,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }];
    let cpu_f4 = cpu.render_input(&input4);
    let gpu_f4 = gpu.render_input(&mut win, &input4, None);
    println!(
        "IN-BOUNDS CONTROL: cpu painted {} px, gpu painted {} px, cpu-vs-gpu diff {} px",
        npaint(&cpu_f4.pixels, &cpu_base.pixels),
        npaint(&gpu_f4.pixels, &gpu_base.pixels),
        npaint(&cpu_f4.pixels, &gpu_f4.pixels)
    );
}
