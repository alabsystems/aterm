// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// CPU==GPU parity for N:1 merged ligatures (`admit_collapsed`).

use crate::common;
use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, Renderer, TextShapingConfig, Theme};

use crate::common::max_channel_delta_frame as max_channel_delta;

fn merged_ligature_font() -> &'static std::path::Path {
    std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aterm-render/assets/game/LuckiestGuy-Regular.ttf"
    ))
}

fn shaping(admit: bool) -> TextShapingConfig {
    TextShapingConfig {
        ligature_mode: LigatureMode::Enabled,
        admit_collapsed: admit,
        ..Default::default()
    }
}

#[test]
fn merged_ligature_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let font = merged_ligature_font();
    let (mut cpu, mut gpu) = common::backends_with_font(font, px, theme);
    let family = font.to_str().expect("the fixture path is UTF-8");
    let mut cpu_off = Renderer::from_system_with_family(Some(family), px, theme)
        .expect("the fixture face builds a renderer");
    cpu_off.debug_block_on_lazy_fallbacks();
    gpu.set_text_shaping(shaping(true));
    cpu.set_text_shaping(shaping(true));
    cpu_off.set_text_shaping(shaping(false));

    let (rows, cols) = (1usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25la fi b fl c ffi d");

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let cpu_off_frame = cpu_off.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    assert_eq!(
        (gpu_frame.width, gpu_frame.height),
        (cpu_frame.width, cpu_frame.height),
        "dimensions differ"
    );
    assert_ne!(
        cpu_frame.pixels, cpu_off_frame.pixels,
        "the merged ligatures did not slice — test would be vacuous (is this a merging font?)"
    );
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    eprintln!("merged-ligature GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU merged-ligature pixels diverge: max per-channel delta {delta} > 8"
    );
}

#[test]
fn merged_ligature_cursor_gpu_matches_cpu() {
    let theme = Theme::default();
    let px = 18.0;

    let font = merged_ligature_font();
    let (mut cpu, mut gpu) = common::backends_with_font(font, px, theme);
    gpu.set_text_shaping(shaping(true));
    cpu.set_text_shaping(shaping(true));

    let (rows, cols) = (1usize, 8usize);
    let mut win = aterm_gpu::WindowGpu::new();

    let mut hidden = Terminal::new(rows as u16, cols as u16);
    hidden.process(b"\x1b[?25lfi");
    let cpu_hidden = cpu.render_input(&hidden.cell_frame(rows, cols));

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[2 qfi\x1b[1;2H");
    let input = term.cell_frame(rows, cols);
    let cpu_cur = cpu.render_input(&input);
    let gpu_cur = gpu.render_input(&mut win, &input, None);

    assert_ne!(
        cpu_cur.pixels, cpu_hidden.pixels,
        "the block cursor over the merged ligature must change the CPU render"
    );
    assert_eq!(
        (gpu_cur.width, gpu_cur.height),
        (cpu_cur.width, cpu_cur.height),
        "dimensions differ"
    );
    let delta = max_channel_delta(&cpu_cur, &gpu_cur);
    eprintln!("merged-ligature cursor GPU vs CPU max per-channel delta = {delta}");
    assert!(
        delta <= 8,
        "GPU/CPU merged-ligature cursor pixels diverge: max per-channel delta {delta} > 8"
    );
}
