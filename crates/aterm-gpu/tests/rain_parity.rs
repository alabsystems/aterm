// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phosphor rain CPU/GPU parity tests.

use crate::common;
use crate::common::ByteExact;
use crate::rain_common::RainScene;
use aterm_core::terminal::Terminal;
use aterm_effects::matrix_rain::RainLayers;
use aterm_render::{Theme, WindowCpu};

use crate::common::{backends, max_channel_delta};

#[test]
fn rain_effect_only_parity_pinned_over_bg_and_text() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    for (arm, mut cpu, mut gpu) in common::arms(18.0, theme) {
        let mut win = aterm_gpu::WindowGpu::new();
        let (rows, cols) = (10usize, 40usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process("\x1b[?25l████████████".as_bytes());

        let base_input = term.cell_frame(rows, cols);
        let cpu_base = cpu.render_input(&base_input);
        let gpu_base = gpu.render_input(&mut win, &base_input, None);
        assert_eq!(
            max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
            0,
            "procedural-block base must be byte-exact so the rain delta is effect-only"
        );

        let mut scene = RainScene::new(rows, cols, cpu.cell_size(), &base_input);
        scene.drive_until_raining();

        assert!(
            scene.cells.iter().any(|q| q.tint != 0x00FF_FFFF),
            "rain emission carries only trivial (white) tints — the tint path is unexercised"
        );
        let mut tints: Vec<u32> = scene.cells.iter().map(|q| q.tint).collect();
        tints.sort_unstable();
        tints.dedup();
        assert!(
            tints.len() >= 2,
            "rain emission carries a single tint level; the ramp should light several \
         trail levels (got {tints:?})"
        );

        let mut cells_in = term.cell_frame(rows, cols);
        scene.apply_cells(&mut cells_in);
        let cpu_q = cpu.render_input(&cells_in);
        let gpu_q = gpu.render_input(&mut win, &cells_in, None);
        assert_ne!(
            cpu_q.pixels, cpu_base.pixels,
            "the rain sprites must actually paint (non-vacuous)"
        );
        let delta_q = max_channel_delta(&cpu_q.pixels, &gpu_q.pixels);
        eprintln!(
            "{arm}: rain layers effect-only GPU vs CPU max per-channel delta = {delta_q} (the rain's declared parity <= {}, \
         {} cells, {} tint levels)",
            RainLayers::PARITY,
            scene.cells.len(),
            tints.len(),
        );
        assert!(
            delta_q <= i32::from(RainLayers::PARITY),
            "{arm}: rain layer parity broke its declared bar: max per-channel delta {delta_q} \
         — the tinted 1:1 tile path diverged"
        );

        let mut add_in = term.cell_frame(rows, cols);
        add_in.rain_add = scene.add.clone();
        let cpu_a = cpu.render_input(&add_in);
        let gpu_a = gpu.render_input(&mut win, &add_in, None);
        assert_ne!(
            cpu_a.pixels, cpu_base.pixels,
            "the bright-head halos must actually paint (non-vacuous)"
        );
        let delta_a = max_channel_delta(&cpu_a.pixels, &gpu_a.pixels);
        eprintln!(
            "rain_add halo GPU vs CPU max per-channel delta = {delta_a} ({} halo quads)",
            scene.add.len()
        );
        if exact.holds(&gpu, "byte-exact rain_add gate") {
            assert_eq!(
                delta_a, 0,
                "{arm}: rain_add premultiplied additive must be BYTE-EXACT CPU==GPU (got {delta_a})"
            );
        }

        if exact.holds(&gpu, "the full rain frame within the sprite bar") {
            let mut full_in = term.cell_frame(rows, cols);
            scene.apply(&mut full_in);
            let cpu_f = cpu.render_input(&full_in);
            let gpu_f = gpu.render_input(&mut win, &full_in, None);
            let delta_f = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
            eprintln!("rain full-frame GPU vs CPU max per-channel delta = {delta_f}");
            assert!(
                delta_f <= i32::from(RainLayers::PARITY),
                "{arm}: combined rain frame diverged past the rain's declared parity: max delta {delta_f}"
            );
        }
        common::assert_arm(arm, &gpu);
    }
}

#[test]
fn scissored_path_rain_row_filter_matches_full_render() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l████████████".as_bytes());
    let base_a = term.cell_frame(rows, cols);
    let mut scene = RainScene::new(rows, cols, cpu.cell_size(), &base_a);
    scene.drive_until_raining();
    let mut qrows: Vec<u16> = scene.cells.iter().map(|q| q.row).collect();
    qrows.sort_unstable();
    qrows.dedup();
    assert!(
        scene.cells.len() >= 10 && qrows.len() >= 3,
        "need a multi-row field to make the filter non-vacuous ({} cells on {} rows)",
        scene.cells.len(),
        qrows.len()
    );
    let hit_row = qrows[1];
    assert!(
        qrows.first() < Some(&hit_row) && qrows.last() > Some(&hit_row),
        "picked band row must have rain strictly above and below (rows {qrows:?})"
    );

    let mut in_a = base_a.clone();
    scene.apply(&mut in_a);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.present_input_readback(&mut win_gpu, &in_a);

    term.process(format!("\x1b[{};1H▒▒▒▒▒▒", hit_row + 1).as_bytes());
    let mut in_b = term.cell_frame(rows, cols);
    scene.apply(&mut in_b);

    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let scissors_before = gpu.scissor_taken();
    let gpu_b = gpu.present_input_readback(&mut win_gpu, &in_b).pixels;
    assert!(
        gpu.scissor_taken() > scissors_before,
        "the one-row change must take the GPU scissored path (Full would \
         bypass the row filter and pass vacuously)"
    );

    let mut gpu2 = common::independent_gpu(18.0, theme);
    let mut win_fresh = aterm_gpu::WindowGpu::new();
    let gpu_fresh = gpu2.render_input(&mut win_fresh, &in_b, None);
    assert_eq!(
        gpu_b, gpu_fresh.pixels,
        "scissored GPU frame (per-row rain layers) must be \
         byte-identical to a fresh full render of the same input"
    );

    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("scissored-path rain CPU vs GPU max per-channel delta = {delta}");
    assert!(
        delta <= i32::from(RainLayers::PARITY),
        "scissored-path rain parity broke its declared bar: {delta}"
    );
}
