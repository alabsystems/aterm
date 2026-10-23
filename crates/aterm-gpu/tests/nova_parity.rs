// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::common;
use crate::common::ByteExact;
use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;
use aterm_effects::supernova::{self, SuperEnv};
use aterm_render::{DamageOutcome, GlowQuad, Renderer, Theme, WindowCpu, premul_rgb};

use crate::common::{backends_fontdue as backends, bb, max_channel_delta};

/// A hand-built nova frame.
fn push_nova(input: &mut aterm_core::render::RenderInput, cw: usize, ch: usize) {
    let core = 0x00FF_F2C8; // solar core
    let fringe = 0x00FF_9A3C; // solar fringe
    for r in 0..3usize {
        input.nova_add.push(GlowQuad {
            row: r as u16,
            x: (6 * cw) as u16,
            y: (r * ch) as u16,
            w: (cw * 2) as u16,
            h: ch as u16,
            color: premul_rgb(core, 200),
            alpha: 0,
            color2: premul_rgb(core, 200),
            alpha2: 0,
        });
    }
    for r in 1..4usize {
        for col in [3usize, 9] {
            input.nova_add.push(GlowQuad {
                row: r as u16,
                x: (col * cw) as u16,
                y: (r * ch + ch / 4) as u16,
                w: cw as u16,
                h: (ch / 2) as u16,
                color: premul_rgb(fringe, 120),
                alpha: 0,
                color2: premul_rgb(fringe, 120),
                alpha2: 0,
            });
        }
    }
}

#[test]
fn nova_additive_is_byte_exact_over_background() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (5usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false; // isolate the nova: no cursor pixels

    let base = 0x00A0_5CFF; // violet palette
    for (i, a) in [40u8, 90, 160, 220, 255].iter().enumerate() {
        let col = i + 1;
        input.nova_add.push(GlowQuad {
            row: 4,
            x: (col * cw) as u16,
            y: (4 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(base, *a),
            alpha: 0,
            color2: premul_rgb(base, *a),
            alpha2: 0,
        });
    }
    push_nova(&mut input, cw, ch);

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    assert_eq!(
        (cpu_frame.width, cpu_frame.height),
        (gpu_frame.width, gpu_frame.height)
    );
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("nova additive-over-bg max per-channel delta = {delta}");
    if exact.holds(&gpu, "byte-exact additive gate") {
        assert_eq!(
            delta, 0,
            "premultiplied nova light over a flat bg must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }

    let bg = theme.bg;
    let premul = premul_rgb(base, 255);
    let want_b = ((bg & 0xff) + (premul & 0xff)).min(255) as i32;
    let cx = 5 * cw + cw / 2; // 5th quad (a=255), centre
    let cy = 4 * ch + ch / 2;
    let got = cpu_frame.pixels[cy * cpu_frame.width + cx];
    assert_eq!(
        bb(got),
        want_b,
        "additive blue channel must be min(255, bg+premul)"
    );
}

#[test]
fn damaged_path_multi_row_nova_parity_cpu_matches_gpu() {
    let exact = ByteExact::default();
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut base_input = term.cell_frame(rows, cols);
    base_input.cursor_visible = false;

    let color = premul_rgb(0x003C_E8A0, 180); // aurora palette
    let ring_at = |top: u16, bands: u16| -> Vec<GlowQuad> {
        (top..top + bands)
            .map(|r| GlowQuad {
                row: r,
                x: (4 * cw) as u16,
                y: (r as usize * ch) as u16,
                w: (3 * cw) as u16,
                h: ch as u16,
                color,
                alpha: 0,
                color2: color,
                alpha2: 0,
            })
            .collect()
    };

    let mut in_a = base_input.clone();
    in_a.nova_add = ring_at(0, 3);
    let cpu_a = cpu
        .render_input_cached(&mut win_cpu, &in_a)
        .pixels()
        .to_vec();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    let mut in_b = base_input.clone();
    in_b.nova_add = ring_at(3, 4);
    let misses_before = gpu.gate_misses();
    let cpu_b = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_view = gpu.render_input_cached(&mut win_gpu, &in_b);
    let gpu_b = gpu_view.pixels().to_vec();
    drop(gpu_view);

    assert!(
        gpu.gate_misses() > misses_before,
        "an advancing ring must MISS the GPU dirty gate (real re-render)"
    );
    assert!(
        cpu_a != cpu_b,
        "the ring did not move between frames A and B"
    );

    let delta = max_channel_delta(&cpu_b, &gpu_b);
    eprintln!("damaged-path nova CPU vs GPU max per-channel delta = {delta}");
    if exact.holds(&gpu, "damaged-path byte-exact additive gate") {
        assert_eq!(
            delta, 0,
            "multi-row nova over bg via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    }
}

/// The super nova word.
fn super_env(cpu: &Renderer, rows: usize, cols: usize) -> SuperEnv {
    let (cw, ch) = cpu.cell_size();
    let (grid_w, grid_h) = ((cols * cw) as i32, (rows * ch) as i32);
    SuperEnv {
        grid_w,
        grid_h,
        cell_w: cw as i32,
        cell_h: ch as i32,
        cx: 6 * cw as i32,
        cy: (2 * ch + ch / 2) as i32,
        r_max: supernova::r_max_for(ch as i32, grid_h),
        row: 2,
        start_col: 4,
        end_col: 7,
        cols: cols as u16,
        light: false,
        intensity: 1.0,
        seed: 0x5EED_F0CC,
        base_hue: 200.0,
    }
}

/// Fold one super nova frame at `t_ms` into `input` as the engine does: additive quads into
/// `nova_add`, deco stamps into `word_decorations`.
fn push_super(input: &mut aterm_core::render::RenderInput, env: &SuperEnv, t_ms: u64) {
    supernova::emit_super(
        t_ms,
        env,
        supernova::MAX_SUPER_QUADS_PER,
        &mut input.nova_add,
    );
    supernova::emit_super_decos(t_ms, env, &mut input.word_decorations, 256);
}

/// The frozen rainbow ink the episode settles to: a static per-cell gradient.
fn frozen_rainbow_ink() -> Vec<InkCell> {
    [
        (4u16, [230u8, 60u8, 60u8]),
        (5, [225, 180, 40]),
        (6, [60, 200, 120]),
        (7, [90, 110, 235]),
    ]
    .into_iter()
    .map(|(col, color)| InkCell { row: 2, col, color })
    .collect()
}

#[test]
fn supernova_detonation_peak_over_text_matches_within_tolerance() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"error[E0308]: fuck\r\n  --> src/main.rs:42:7\r\ncargo build failed");
    let mut input = term.cell_frame(rows, cols);
    let env = super_env(&cpu, rows, cols);
    push_super(&mut input, &env, 500);

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("supernova detonation-peak over-text max per-channel delta = {delta}");
    crate::common::byte_exact_or_could_not_run(&gpu, "supernova-over-text parity gate");
    assert!(
        delta <= 8,
        "supernova detonation over text diverges: max delta {delta} > 8"
    );
}

#[test]
fn supernova_damaged_path_wash_to_shockwave_cached_equals_fresh() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let env = super_env(&cpu, rows, cols);
    let mut make = |t_ms: u64| {
        let mut input = term.cell_frame(rows, cols);
        input.cursor_visible = false;
        input.ink = frozen_rainbow_ink(); // the word's ink rides along
        push_super(&mut input, &env, t_ms);
        input
    };

    let in_a = make(500);
    assert!(!in_a.nova_add.is_empty(), "wash frame premise");
    let cpu_a = cpu
        .render_input_cached(&mut win_cpu, &in_a)
        .pixels()
        .to_vec();
    let _ = gpu.present_input_readback(&mut win_gpu, &in_a);

    let in_b = make(1000);
    assert!(!in_b.nova_add.is_empty(), "shockwave frame premise");
    assert_ne!(
        in_a.nova_add, in_b.nova_add,
        "the nova must actually advance"
    );
    let scissors_before = gpu.scissor_taken();
    let cpu_b_cached = cpu
        .render_input_cached(&mut win_cpu, &in_b)
        .pixels()
        .to_vec();
    let gpu_b_scissored = gpu.present_input_readback(&mut win_gpu, &in_b).pixels;
    assert_ne!(cpu_a, cpu_b_cached, "wash → shockwave must change pixels");
    assert!(
        gpu.scissor_taken() > scissors_before,
        "the wash → shockwave frame must take the GPU scissored path"
    );

    let cpu_b_fresh = cpu.render_input(&in_b).pixels;
    assert_eq!(
        cpu_b_cached, cpu_b_fresh,
        "CPU damaged path: the shockwave frame must equal a fresh render \
         (no wash ghost on any vacated row)"
    );
    let mut gpu2 = common::independent_gpu(18.0, theme);
    gpu2.set_bloom(false);
    gpu2.set_shimmer(false);
    let mut win2 = aterm_gpu::WindowGpu::new();
    let gpu_b_fresh = gpu2.render_input(&mut win2, &in_b, None).pixels;
    assert_eq!(
        gpu_b_scissored, gpu_b_fresh,
        "GPU scissored path: the shockwave frame must equal a fresh render \
         (no wash ghost on any vacated row)"
    );
}

#[test]
fn settled_after_supernova_gate_hits_with_frozen_rainbow_ink() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    term.process(b"\x1b[3;5Hfuck");
    let (cw, ch) = cpu.cell_size();
    let env = super_env(&cpu, rows, cols);

    let mut in_debris = term.cell_frame(rows, cols);
    in_debris.ink = frozen_rainbow_ink();
    push_super(&mut in_debris, &env, 2000);
    assert!(
        !in_debris.word_decorations.is_empty(),
        "t=2000 must still carry rainbow-debris decos (non-vacuous transition)"
    );
    let mut settled = term.cell_frame(rows, cols);
    settled.ink = frozen_rainbow_ink();
    push_super(&mut settled, &env, supernova::SUPER_TOTAL_MS);
    assert!(
        settled.nova_add.is_empty() && settled.word_decorations.is_empty(),
        "t >= SUPER_TOTAL_MS must emit nothing (the one-shot is over)"
    );
    assert!(
        !settled.ink.is_empty(),
        "the frozen rainbow ink must be present"
    );

    let mut settled_bare = settled.clone();
    settled_bare.ink.clear();
    let mut win_fresh = aterm_gpu::WindowGpu::new();
    let cpu_frame = cpu.render_input(&settled);
    assert_eq!(
        (cpu_frame.width, cpu_frame.height),
        (cols * cw, rows * ch),
        "these renderers must be unpadded for the cell math below"
    );
    let width = cpu_frame.width;
    let cpu_settled = cpu_frame.pixels;
    let cpu_bare = cpu.render_input(&settled_bare).pixels;
    let gpu_settled = gpu.render_input(&mut win_fresh, &settled, None).pixels;
    let gpu_bare = gpu.render_input(&mut win_fresh, &settled_bare, None).pixels;
    for (backend, inked, bare) in [
        ("CPU", &cpu_settled, &cpu_bare),
        ("GPU", &gpu_settled, &gpu_bare),
    ] {
        for (i, (&a, &b)) in inked.iter().zip(bare.iter()).enumerate() {
            let (row, col) = ((i / width) / ch, (i % width) / cw);
            let is_lead = row == 2 && (4..=7).contains(&col);
            assert!(
                is_lead || a == b,
                "{backend}: the frozen ink must touch ONLY the word's lead cells — \
                 pixel {i} (row {row}, col {col}) changed {b:#08x} -> {a:#08x}"
            );
        }
        for cell in frozen_rainbow_ink() {
            let (r, c) = (cell.row as usize, cell.col as usize);
            let want = (u32::from(cell.color[0]) << 16)
                | (u32::from(cell.color[1]) << 8)
                | u32::from(cell.color[2]);
            let (mut changed, mut exact) = (false, false);
            for y in r * ch..(r + 1) * ch {
                for x in c * cw..(c + 1) * cw {
                    let i = y * width + x;
                    changed |= inked[i] != bare[i];
                    exact |= inked[i] & 0x00ff_ffff == want;
                }
            }
            assert!(
                changed,
                "{backend}: lead cell (row {r}, col {c}) must be recoloured by the frozen ink \
                 (is the word actually on screen?)"
            );
            assert!(
                exact,
                "{backend}: lead cell (row {r}, col {c}) must carry its ink colour \
                 {want:#08x} verbatim on at least one fully-covered glyph pixel"
            );
        }
    }

    let cpu_debris = cpu
        .render_input_cached(&mut win_cpu, &in_debris)
        .pixels()
        .to_vec();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_debris);
    let cpu_settled_cached = cpu
        .render_input_cached(&mut win_cpu, &settled)
        .pixels()
        .to_vec();
    assert_ne!(
        win_cpu.last_damage(),
        DamageOutcome::GateHit,
        "debris → settled is a real change (decos vanish)"
    );
    assert_ne!(
        max_channel_delta(&cpu_debris, &cpu_settled_cached),
        0,
        "debris → settled must change the image (the decos really vanish)"
    );
    let misses_before = gpu.gate_misses();
    let _ = gpu.render_input_cached(&mut win_gpu, &settled);
    assert!(
        gpu.gate_misses() > misses_before,
        "debris → settled must miss the GPU gate"
    );

    let settled_again = settled.clone();
    let cpu_hit = cpu
        .render_input_cached(&mut win_cpu, &settled_again)
        .pixels()
        .to_vec();
    assert_eq!(
        win_cpu.last_damage(),
        DamageOutcome::GateHit,
        "a settled post-supernova frame with frozen rainbow ink must take the CPU dirty gate"
    );
    let hits_before = gpu.gate_hits();
    let gpu_hit = gpu
        .render_input_cached(&mut win_gpu, &settled_again)
        .pixels()
        .to_vec();
    assert!(
        gpu.gate_hits() > hits_before,
        "a settled post-supernova frame with frozen rainbow ink must take the GPU dirty gate"
    );

    same_image("CPU", &cpu_settled, &cpu_hit, width);
    same_image("GPU", &gpu_settled, &gpu_hit, width);
}

/// Byte-for-byte frame equality that names the first divergent pixel.
fn same_image(backend: &str, want: &[u32], got: &[u32], width: usize) {
    assert_eq!(
        want.len(),
        got.len(),
        "{backend}: gate-hit frame length {} != fresh render {}",
        got.len(),
        want.len()
    );
    if let Some((i, (&w, &g))) = want
        .iter()
        .zip(got.iter())
        .enumerate()
        .find(|&(_, (&a, &b))| a != b)
    {
        let (x, y) = (i % width, i / width);
        panic!(
            "{backend} gate hit did not hand back the settled frame's own pixels — \
             first mismatch at pixel {i} (x={x}, y={y}): fresh render {w:#08x}, \
             gate-hit frame {g:#08x}"
        );
    }
}
