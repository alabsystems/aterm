// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU/GPU byte-parity tests for `RenderInput.fire_patch`.

use aterm_core::terminal::Terminal;
use aterm_render::{FireMode, FirePatch, Theme, WindowCpu};

use crate::common::ByteExact;
use crate::common::{assert_byte_exact, backends, bb, block_text_term, gg, max_channel_delta, rr};

fn luma(p: u32) -> i32 {
    rr(p) + gg(p) + bb(p)
}

#[derive(Clone, Copy)]
struct Burn {
    x: usize,
    w: usize,
    base_y: usize,
    peak_h: u16,
    phase: u32,
    temp: u8,
    strength: u8,
    lean: i8,
    cov_cap: u8,
    mode: FireMode,
}

fn emit_burn(out: &mut Vec<FirePatch>, b: Burn, ch: usize, grid_w: usize, grid_h: usize) {
    let x0 = b.x.min(grid_w);
    let x1 = (b.x + b.w).min(grid_w);
    let reach = (b.peak_h as usize) * 6 / 5 + 2;
    let y0 = b.base_y.saturating_sub(reach);
    let y1 = (b.base_y + 1).min(grid_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut y = y0;
    while y < y1 {
        let row = y / ch;
        let band_end = ((row + 1) * ch).min(y1);
        out.push(FirePatch {
            row: row as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            base_y: b.base_y as u16,
            peak_h: b.peak_h,
            phase: b.phase,
            temp: b.temp,
            strength: b.strength,
            lean: b.lean,
            cov_cap: b.cov_cap,
            cell_h: ch as u16,
            mode: b.mode,
        });
        y = band_end;
    }
}

fn synthetic_burns(phase: u32, ch: usize, grid_w: usize, grid_h: usize) -> Vec<FirePatch> {
    let mut out = Vec::new();
    let burns = [
        Burn {
            x: ch,
            w: 8 * ch,
            base_y: grid_h - ch / 2,
            peak_h: (3 * ch) as u16,
            phase,
            temp: 230,
            strength: 240,
            lean: -48,
            cov_cap: 200,
            mode: FireMode::Add,
        },
        Burn {
            x: 10 * ch,
            w: 5 * ch,
            base_y: grid_h - ch,
            peak_h: (2 * ch) as u16,
            phase,
            temp: 51,
            strength: 160,
            lean: 90,
            cov_cap: 90,
            mode: FireMode::Add,
        },
        Burn {
            x: grid_w.saturating_sub(3 * ch),
            w: 4 * ch,
            base_y: grid_h - 2,
            peak_h: (5 * ch / 2) as u16,
            phase: phase.wrapping_add(7777),
            temp: 128,
            strength: 255,
            lean: 0,
            cov_cap: 160,
            mode: FireMode::Add,
        },
        Burn {
            x: 2 * ch,
            w: 3 * ch,
            base_y: 2 * ch - 1,
            peak_h: (3 * ch / 2) as u16,
            phase: phase.wrapping_add(191),
            temp: 180,
            strength: 90,
            lean: -120,
            cov_cap: 255,
            mode: FireMode::Add,
        },
    ];
    for b in burns {
        emit_burn(&mut out, b, ch, grid_w, grid_h);
    }
    out
}

#[test]
fn fire_patch_synthetic_field_is_byte_exact_over_text() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (10usize, 40usize);
    let mut term = block_text_term(rows, cols, 1..8);
    let (_cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cpu.cell_size().0, rows * ch);

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "procedural-block base must be byte-exact so the fire delta is effect-only"
    );

    for phase in [0u32, 91_750, 91_766, 1 << 20] {
        let mut input = term.cell_frame(rows, cols);
        input.fire_patch = synthetic_burns(phase, ch, grid_w, grid_h);
        let mut qrows: Vec<u16> = input.fire_patch.iter().map(|q| q.row).collect();
        qrows.sort_unstable();
        qrows.dedup();
        assert!(
            input.fire_patch.len() > qrows.len() && qrows.len() >= 4,
            "need a spanning multi-row field ({} patches on {} rows)",
            input.fire_patch.len(),
            qrows.len()
        );
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_f.pixels, cpu_base.pixels,
            "the synthetic burns must actually paint (non-vacuous, phase {phase})"
        );
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!(
            "fire_patch synthetic field (phase {phase}) GPU vs CPU max per-channel delta = {delta} ({} patches)",
            input.fire_patch.len()
        );
        if exact.holds(&gpu, "byte-exact fire gate") {
            assert_eq!(
                delta, 0,
                "fire field must be BYTE-EXACT CPU==GPU (got {delta} at phase {phase})"
            );
        }
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
fn over_ink_fire_byte_exact_over_dark_and_light_frames() {
    over_ink_fire_parity("over_ink_fire_byte_exact_over_dark_and_light_frames");
}

fn over_ink_fire_parity(test: &str) {
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
            "({label}) block base must be byte-exact so the fire delta is effect-only"
        );

        let mut patches = Vec::new();
        let ember = Burn {
            x: 4 * ch,
            w: 6 * ch,
            base_y: grid_h - ch,
            peak_h: (2 * ch) as u16,
            phase: 55_555,
            temp: 200,
            strength: 220,
            lean: -40,
            cov_cap: 180,
            mode: FireMode::Add,
        };
        emit_burn(&mut patches, ember, ch, grid_w, grid_h);
        emit_burn(
            &mut patches,
            Burn {
                temp: 140,
                cov_cap: 200,
                mode: FireMode::Over,
                ..ember
            },
            ch,
            grid_w,
            grid_h,
        );
        let ink = Burn {
            x: grid_w - 4 * ch,
            w: 5 * ch,
            base_y: grid_h - 1,
            peak_h: (3 * ch) as u16,
            phase: 88_888,
            temp: 128,
            strength: 250,
            lean: 64,
            cov_cap: 220,
            mode: FireMode::Over,
        };
        emit_burn(&mut patches, ink, ch, grid_w, grid_h);
        let n_over = patches.iter().filter(|p| p.mode == FireMode::Over).count();
        assert!(
            n_over >= 5,
            "({label}) need a multi-patch, row-spanning Over field ({n_over})"
        );

        let mut input = term.cell_frame(rows, cols);
        input.fire_patch = patches;
        let cpu_f = cpu.render_input(&input);
        let gpu_f = gpu.render_input(&mut win, &input, None);
        assert_ne!(
            cpu_f.pixels, cpu_base.pixels,
            "({label}) the burns must actually paint (non-vacuous)"
        );
        if label == "light" {
            let pad = (cpu_f.width - grid_w) / 2;
            let probe_x = pad + grid_w - 2 * ch;
            let probe_y = pad + grid_h - ch / 2;
            let probe = probe_x + probe_y * cpu_f.width;
            assert!(
                luma(cpu_f.pixels[probe]) < luma(cpu_base.pixels[probe]),
                "an Over ink-burn must darken the white ground near its root"
            );
        }
        let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
        eprintln!(
            "fire Over ({label}) GPU vs CPU max per-channel delta = {delta} ({} patches)",
            input.fire_patch.len()
        );
        if exact.holds(&gpu, "byte-exact Over fire gate ({label})") {
            assert_byte_exact(
                test,
                &format!("fire Over ({label})"),
                &gpu,
                delta,
                format_args!("({label}) ink-fire must be BYTE-EXACT CPU==GPU (got {delta})"),
            );
        }
    }
}

#[test]
fn damaged_path_fire_patch_parity_cpu_matches_gpu() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false; // isolate the fire: no cursor pixels

    let burn_at = |base_y: usize, phase: u32| {
        let mut v = Vec::new();
        emit_burn(
            &mut v,
            Burn {
                x: 2 * ch,
                w: 6 * ch,
                base_y,
                peak_h: (2 * ch) as u16,
                phase,
                temp: 210,
                strength: 230,
                lean: -60,
                cov_cap: 190,
                mode: FireMode::Add,
            },
            ch,
            grid_w,
            grid_h,
        );
        v
    };

    let mut in_a = base_input.clone();
    in_a.fire_patch = burn_at(grid_h - 4, 70_000);
    let _ = cpu.render_input_cached(&mut win_cpu, &in_a);
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let mut in_b = base_input.clone();
    in_b.fire_patch = burn_at(grid_h - 2 * ch - 4, 70_016);
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
        "an animating fire_patch must MISS the GPU dirty gate (real re-render)"
    );

    let cpu_fresh = cpu.render_input(&in_b).pixels.clone();
    assert_eq!(
        cpu_b, cpu_fresh,
        "CPU cached-damaged fire frame must equal a fresh full render"
    );
    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path fire_patch CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "damaged-path byte-exact fire gate") {
        assert_eq!(
            delta, 0,
            "fire via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }
}

#[test]
fn seam_law_split_patches_byte_identical_to_one_wide() {
    let theme = Theme::default();
    let (mut cpu, gpu) = backends(18.0, theme);
    let mut gpu = gpu;
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 32usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = (cols * cw, rows * ch);

    for mode in [FireMode::Add, FireMode::Over] {
        let burn = Burn {
            x: ch,
            w: 20 * ch,
            base_y: grid_h - 3,
            peak_h: (3 * ch) as u16,
            phase: 424_242,
            temp: 220,
            strength: 245,
            lean: -70,
            cov_cap: 200,
            mode,
        };
        let mut wide = term.cell_frame(rows, cols);
        emit_burn(&mut wide.fire_patch, burn, ch, grid_w, grid_h);
        let mut split = term.cell_frame(rows, cols);
        let mut x = burn.x;
        while x < burn.x + burn.w {
            let w = 3.min(burn.x + burn.w - x);
            emit_burn(
                &mut split.fire_patch,
                Burn { x, w, ..burn },
                ch,
                grid_w,
                grid_h,
            );
            x += w;
        }
        assert!(
            split.fire_patch.len() > wide.fire_patch.len() * 10,
            "the split field must be genuinely fine-grained"
        );

        let cpu_wide = cpu.render_input(&wide).pixels.clone();
        let cpu_split = cpu.render_input(&split).pixels.clone();
        assert_ne!(
            cpu_wide,
            cpu.render_input(&term.cell_frame(rows, cols)).pixels,
            "({mode:?}) the wide burn must actually paint (non-vacuous)"
        );
        assert_eq!(
            cpu_wide, cpu_split,
            "({mode:?}) CPU: split patches must be byte-identical to one wide patch (zero seams)"
        );

        let gpu_wide = gpu.render_input(&mut win, &wide, None).pixels;
        let gpu_split = gpu.render_input(&mut win, &split, None).pixels;
        assert_eq!(
            gpu_wide, gpu_split,
            "({mode:?}) GPU: split patches must be byte-identical to one wide patch (zero seams)"
        );
    }
}

#[test]
fn fire_patch_head_band_parity_cpu_matches_gpu() {
    let exact = ByteExact::default();
    const P: usize = 6; // interior pad (px per edge)
    const H: usize = 20; // chrome head band (px above the padded grid)
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    cpu.set_pad(P);
    cpu.set_head(H);
    gpu.set_pad(P);
    gpu.set_head(H);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process("\x1b[?25l".as_bytes());
    let (cw, ch) = cpu.cell_size();
    let grid_top = P + H;

    let base_input = term.cell_frame(rows, cols);
    let cpu_base = cpu.render_input(&base_input);
    let gpu_base = gpu.render_input(&mut win, &base_input, None);
    assert_eq!(
        (cpu_base.width, cpu_base.height),
        (cols * cw + 2 * P, rows * ch + 2 * P + H),
        "unexpected head-band frame size"
    );
    assert_eq!(
        max_channel_delta(&cpu_base.pixels, &gpu_base.pixels),
        0,
        "head-band base must be byte-exact so the fire delta is effect-only"
    );

    let base_y = grid_top + ch / 2;
    let peak_h = (2 * ch) as u16;
    let reach = (peak_h as usize) * 6 / 5 + 2;
    let (x0, x1) = (P + 2 * cw, P + 14 * cw);
    let y0 = base_y.saturating_sub(reach); // above grid_top: head-band pixels
    assert!(y0 < grid_top, "premise: the burn must reach the head band");
    let mut patches = Vec::new();
    let mut y = y0;
    while y < base_y + 1 {
        let band = (y as i32 - grid_top as i32).div_euclid(ch as i32);
        let band_end = (grid_top as i32 + (band + 1) * ch as i32) as usize;
        let band_end = band_end.min(base_y + 1);
        patches.push(FirePatch {
            row: band.max(0) as u16,
            x: x0 as u16,
            y: y as u16,
            w: (x1 - x0) as u16,
            h: (band_end - y) as u16,
            base_y: base_y as u16,
            peak_h,
            phase: 70_000,
            temp: 240,
            strength: 250,
            lean: -40,
            cov_cap: 255,
            cell_h: ch as u16,
            mode: FireMode::Add,
        });
        y = band_end;
    }
    assert_eq!(patches[0].row, 0, "the above-grid slice must tag row 0");

    let mut input = term.cell_frame(rows, cols);
    input.fire_patch = patches;
    let cpu_f = cpu.render_input(&input);
    let gpu_f = gpu.render_input(&mut win, &input, None);
    assert_ne!(
        cpu_f.pixels, cpu_base.pixels,
        "the head-band burn must actually paint (non-vacuous)"
    );

    let band_lit =
        |painted: &[u32], base: &[u32], w: usize| (0..grid_top * w).any(|i| painted[i] != base[i]);
    assert!(
        band_lit(&cpu_f.pixels, &cpu_base.pixels, cpu_f.width),
        "CPU: the fire must draw inside the head band (y < pad + head)"
    );
    assert!(
        band_lit(&gpu_f.pixels, &gpu_base.pixels, gpu_f.width),
        "GPU: the fire must draw inside the head band (y < pad + head)"
    );

    let delta = max_channel_delta(&cpu_f.pixels, &gpu_f.pixels);
    eprintln!("fire_patch head-band GPU vs CPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "byte-exact head-band fire gate") {
        assert_eq!(
            delta, 0,
            "head-band fire must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }
}
