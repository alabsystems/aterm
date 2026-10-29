// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glow-halo (`RenderInput.glow_halo`) CPU/GPU byte parity tests.

use aterm_core::terminal::Terminal;
use aterm_render::{HaloMode, RainHalo, Theme, WindowCpu};

use crate::common::ByteExact;
use crate::common::{assert_byte_exact, backends, bb, block_text_term, gg, max_channel_delta, rr};

#[allow(clippy::too_many_arguments)]
fn emit_halo(
    out: &mut Vec<RainHalo>,
    cx: usize,
    cy: usize,
    rx: u16,
    ry: u16,
    color: u32,
    ch: usize,
    grid_w: usize,
    grid_h: usize,
) {
    assert!(cx < grid_w && cy < grid_h, "centre must be grid-interior");
    let x0 = cx.saturating_sub(rx as usize);
    let x1 = (cx + rx as usize + 1).min(grid_w);
    let y0 = cy.saturating_sub(ry as usize);
    let y1 = (cy + ry as usize + 1).min(grid_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut y = y0;
    while y < y1 {
        let row = y / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            color,
            cx: cx as u16,
            cy: cy as u16,
            rx: rx.max(1),
            ry: ry.max(1),
            mode: HaloMode::Add,
        });
        y = band_end;
    }
}

#[test]
fn glow_halo_synthetic_field_is_byte_exact_over_text() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = block_text_term(rows, cols, 1..8);
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the halo delta is effect-only"
    );

    let mut halos = Vec::new();
    emit_halo(
        &mut halos,
        6 * cw + cw / 2,
        2 * ch + ch / 3,
        (2 * cw) as u16,
        (3 * ch / 2) as u16,
        0x00FF_6018,
        ch,
        grid_w,
        grid_h,
    );
    emit_halo(
        &mut halos,
        12 * cw,
        4 * ch + ch / 2,
        (3 * cw) as u16,
        (ch / 2).max(1) as u16,
        0x0030_C0FF,
        ch,
        grid_w,
        grid_h,
    );
    emit_halo(
        &mut halos,
        2,
        5 * ch + ch / 2,
        (2 * cw) as u16,
        ch as u16,
        0x0080_FF80,
        ch,
        grid_w,
        grid_h,
    );
    emit_halo(
        &mut halos,
        grid_w - 3,
        grid_h - 2,
        (2 * cw) as u16,
        ch as u16,
        0x00FF_FFFF,
        ch,
        grid_w,
        grid_h,
    );
    emit_halo(
        &mut halos,
        20 * cw,
        2,
        (3 * cw) as u16,
        ch as u16,
        0x00C8_40E0,
        ch,
        grid_w,
        grid_h,
    );
    let mut qrows: Vec<u16> = halos.iter().map(|q| q.row).collect();
    qrows.sort_unstable();
    qrows.dedup();
    assert!(
        halos.len() > qrows.len() && qrows.len() >= 4,
        "need a spanning multi-row field ({} quads on {} rows)",
        halos.len(),
        qrows.len()
    );

    let mut input = term.cell_frame(rows, cols);
    input.glow_halo = halos;
    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_base.pixels,
        "the synthetic halos must actually paint (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!(
        "glow_halo synthetic-field GPU vs CPU max per-channel delta = {delta} ({} quads)",
        input.glow_halo.len()
    );
    if exact.holds(&gpu, "byte-exact glow_halo gate") {
        assert_eq!(
            delta, 0,
            "glow_halo radial additive must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }
}

#[test]
fn damaged_path_glow_halo_parity_cpu_matches_gpu() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the halo: no cursor pixels

    let halo_at = |col: usize, row: u16| RainHalo {
        row,
        x: (col * cw) as u16,
        y: (row as usize * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: 0x0060_FF30,
        cx: (col * cw + cw) as u16,
        cy: (row as usize * ch + ch / 2) as u16,
        rx: cw as u16,
        ry: (ch / 2).max(1) as u16,
        mode: HaloMode::Add,
    };

    let mut in_a = base_input.clone();
    in_a.glow_halo.push(halo_at(2, 1));
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let mut in_b = base_input.clone();
    in_b.glow_halo.push(halo_at(9, 4));
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved glow_halo must MISS the GPU dirty gate (real re-render)"
    );

    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged glow_halo frame must equal a fresh full render"
    );
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path glow_halo CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "damaged-path byte-exact glow_halo gate") {
        assert_eq!(
            delta, 0,
            "glow_halo over bg via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }
}

fn light_theme() -> Theme {
    Theme {
        fg: 0x0020_2020,
        bg: 0x00FF_FFFF,
        ..Theme::default()
    }
}

#[test]
fn over_veils_byte_exact_over_dark_and_light_frames() {
    over_veils_parity("over_veils_byte_exact_over_dark_and_light_frames");
}

fn over_veils_parity(test: &str) {
    let exact = ByteExact::default();
    for (label, theme) in [("dark", Theme::default()), ("light", light_theme())] {
        let (mut cpu, mut gpu) = backends(18.0, theme);
        let mut win = aterm_gpu::WindowGpu::new();
        let (rows, cols) = (10usize, 40usize);
        let mut term = block_text_term(rows, cols, 1..8);
        let (cw, ch) = cpu.cell_size();
        let (grid_w, grid_h) = (cols * cw, rows * ch);

        let base_input = term.cell_frame(rows, cols);
        let cpu_base = cpu.render_input(&base_input);
        let gpu_base = gpu.render_input(&mut win, &base_input, None);
        assert_eq!(
            max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
            0,
            "({label}) block base must be byte-exact so the veil delta is effect-only"
        );

        let mut halos = Vec::new();
        let (ecx, ecy) = (6 * cw + cw / 2, 3 * ch + ch / 2);
        emit_halo(
            &mut halos,
            ecx,
            ecy,
            (2 * cw) as u16,
            ch as u16,
            0x00FF_6018,
            ch,
            grid_w,
            grid_h,
        );
        let over_from = halos.len();
        emit_halo(
            &mut halos,
            ecx,
            ecy,
            (2 * cw) as u16,
            ch as u16,
            0x0030_3038,
            ch,
            grid_w,
            grid_h,
        );
        emit_halo(
            &mut halos,
            14 * cw,
            4 * ch + ch / 3,
            (2 * cw) as u16,
            (3 * ch / 2) as u16,
            0x0018_1810,
            ch,
            grid_w,
            grid_h,
        );
        emit_halo(
            &mut halos,
            grid_w - 3,
            grid_h - 2,
            (2 * cw) as u16,
            ch as u16,
            0x0040_2020,
            ch,
            grid_w,
            grid_h,
        );
        for q in &mut halos[over_from..] {
            q.mode = HaloMode::Over;
        }
        assert!(
            halos[over_from..].len() >= 5,
            "({label}) need a multi-quad, row-spanning Over field"
        );

        let mut input = term.cell_frame(rows, cols);
        input.glow_halo = halos;
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_f.pixels, cpu_base.pixels,
            "({label}) the veils must actually paint (non-vacuous)"
        );
        if label == "light" {
            let pad = (cpu_f.width - grid_w) / 2;
            let smoke_centre = (pad + 14 * cw) + (pad + 4 * ch + ch / 3) * cpu_f.width;
            assert!(
                luma(cpu_f.pixels[smoke_centre]) < luma(cpu_base.pixels[smoke_centre]),
                "an Over veil must darken the white ground at its centre"
            );
        }
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!(
            "glow_halo Over ({label}) GPU vs CPU max per-channel delta = {delta} ({} quads)",
            input.glow_halo.len()
        );
        if exact.holds(&gpu, "byte-exact Over gate ({label})") {
            assert_byte_exact(
                test,
                &format!("glow_halo Over ({label})"),
                &gpu,
                delta,
                format_args!("({label}) Over veils must be BYTE-EXACT CPU==GPU (got {delta})"),
            );
        }
    }
}

fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
}

#[test]
fn damaged_path_over_veil_parity_cpu_matches_gpu() {
    let exact = ByteExact::default();
    let (mut cpu, mut gpu) = backends(18.0, light_theme());
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (6usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the veil: no cursor pixels

    let veil_at = |col: usize, row: u16| RainHalo {
        row,
        x: (col * cw) as u16,
        y: (row as usize * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: 0x0030_3038,
        cx: (col * cw + cw) as u16,
        cy: (row as usize * ch + ch / 2) as u16,
        rx: cw as u16,
        ry: (ch / 2).max(1) as u16,
        mode: HaloMode::Over,
    };

    let mut in_a = base_input.clone();
    in_a.glow_halo.push(veil_at(2, 1));
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let mut in_b = base_input.clone();
    in_b.glow_halo.push(veil_at(9, 4));
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b = gpu
        .render_input_cached(&mut win_gpu, &in_b)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_misses() > misses_before,
        "a moved Over veil must MISS the GPU dirty gate (real re-render)"
    );

    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged Over frame must equal a fresh full render"
    );
    assert_ne!(
        cpu_b,
        cpu.render_input(&base_input).pixels,
        "the moved veil must actually darken the white ground (non-vacuous)"
    );
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path Over veil CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "damaged-path byte-exact Over gate") {
        assert_byte_exact(
            "damaged_path_over_veil_parity_cpu_matches_gpu",
            "damaged-path Over veil",
            &gpu,
            delta,
            format_args!("Over veil via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"),
        );
    }
}
