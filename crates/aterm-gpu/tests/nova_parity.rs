// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Parity for the nova additive light (`nova_add`), a second One/One glow stream
// on the Rgba8Unorm offscreen, held to the CPU `add_sat` like the aurora:
// byte-exact over an opaque background, within the glyph tolerance (<=8) over
// text, and an empty vec identical to no nova.
//
// Super nova arm: the super nova uses the same channels (`nova_add` and
// `word_decorations`), driven here from the pure emitters
// `supernova::emit_super`/`emit_super_decos`. Pinned: the detonation peak is
// delta 0 over flat bg and <=8 over text; the wash-to-shockwave frame on the
// damaged path equals a fresh render on both backends; a settled frame after
// the super nova, with its frozen rainbow ink, takes the dirty gate.
//
// A host without a GPU could not run it (`common::gpu`); byte-exact checks
// also gate on `additive_is_byte_exact`.

use crate::common;
use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;
use aterm_effects::supernova::{self, SuperEnv};
use aterm_render::{DamageOutcome, GlowQuad, Renderer, Theme, WindowCpu, premul_rgb};

use crate::common::{backends_fontdue as backends, bb, max_channel_delta};

/// A hand-built nova frame: a 3-row crown column plus per-row ring-band chord
/// quads (every quad in exactly one row band).
fn push_nova(input: &mut aterm_core::render::RenderInput, cw: usize, ch: usize) {
    let core = 0x00FF_F2C8; // solar core
    let fringe = 0x00FF_9A3C; // solar fringe
    // Crown: 3 stacked rects over rows 0..3 at column 6.
    for r in 0..3usize {
        input.nova_add.push(GlowQuad {
            row: r as u16,
            x: (6 * cw) as u16,
            y: (r * ch) as u16,
            w: (cw * 2) as u16,
            h: ch as u16,
            color: premul_rgb(core, 200),
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: premul_rgb(core, 200),
            alpha2: 0,
        });
    }
    // Ring chords: left + right chord slabs in rows 1..4 (the fixed-count
    // band idiom, one uniform-coverage quad per chord per band).
    for r in 1..4usize {
        for col in [3usize, 9] {
            input.nova_add.push(GlowQuad {
                row: r as u16,
                x: (col * cw) as u16,
                y: (r * ch + ch / 4) as u16,
                w: cw as u16,
                h: (ch / 2) as u16,
                color: premul_rgb(fringe, 120),
                // Additive light (see `GlowQuad::alpha`).
                alpha: 0,
                color2: premul_rgb(fringe, 120),
                alpha2: 0,
            });
        }
    }
}

/// Premultiplied nova light over pure background is byte-exact CPU==GPU and
/// equals `min(255, bg + premul)` per channel.
#[test]
fn nova_additive_is_byte_exact_over_background() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (5usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false; // isolate the nova: no cursor pixels

    // A spread of premultiplied coverages, plus the full ring/crown shape.
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
            // Additive light (see `GlowQuad::alpha`).
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
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "premultiplied nova light over a flat bg must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact additive gate: downlevel sRGB offscreen (linear add)");
    }

    // And the value is exactly min(255, bg + premul) at the a=255 quad centre.
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

/// Crown and ring over glyphs stay within the glyph tolerance (<=8).
#[test]
fn nova_over_text_matches_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (5usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"error[E0308]: fuck\r\n  --> src/main.rs:42:7\r\ncargo build failed");
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    push_nova(&mut input, cw, ch);

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("nova over-text max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert!(delta <= 8, "nova over text diverges: max delta {delta} > 8");
    } else {
        eprintln!("SKIP nova-over-text parity gate: downlevel sRGB offscreen (linear add)");
    }
}

/// Cached-path nova parity with a multi-row shape: frame A places the crown and
/// ring; frame B moves the ring down, and the shared dirty set must cover
/// previous and current rows on both backends with no stale light. Over a
/// glyph-free background frame B is byte-exact CPU vs GPU.
#[test]
fn damaged_path_multi_row_nova_parity_cpu_matches_gpu() {
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
    // A ring straddling `bands` row bands anchored at `top`: one quad per band.
    let ring_at = |top: u16, bands: u16| -> Vec<GlowQuad> {
        (top..top + bands)
            .map(|r| GlowQuad {
                row: r,
                x: (4 * cw) as u16,
                y: (r as usize * ch) as u16,
                w: (3 * cw) as u16,
                h: ch as u16,
                color,
                // Additive light (see `GlowQuad::alpha`).
                alpha: 0,
                color2: color,
                alpha2: 0,
            })
            .collect()
    };

    // Frame A: ring over rows 0..3.
    let mut in_a = base_input.clone();
    in_a.nova_add = ring_at(0, 3);
    let cpu_a = cpu
        .render_input_cached(&mut win_cpu, &in_a)
        .pixels()
        .to_vec();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_a);

    // Frame B: the ring expands downward to rows 3..7 (every prev row vacated).
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
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "multi-row nova over bg via the cached path must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP damaged-path byte-exact additive gate: downlevel sRGB offscreen");
    }
}

// ---------------------------------------------------------------------------
// Super nova cases, driven from the pure emitters.
// ---------------------------------------------------------------------------

/// The super nova word: row 2, cells 4..=7. Dark theme, so the detonation is
/// the full-viewport additive wash and star crown (pure `nova_add`).
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
        // The engine's reach clamp (min(6 rows, grid_h/2), 1 px floor) via the
        // shared helper.
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

/// Fold one super nova frame at `t_ms` into `input` as the engine does: additive
/// quads into `nova_add`, deco stamps into `word_decorations`.
fn push_super(input: &mut aterm_core::render::RenderInput, env: &SuperEnv, t_ms: u64) {
    supernova::emit_super(
        t_ms,
        env,
        supernova::MAX_SUPER_QUADS_PER,
        &mut input.nova_add,
    );
    supernova::emit_super_decos(t_ms, env, &mut input.word_decorations, 256);
}

/// The frozen rainbow ink the episode settles to: a static per-cell gradient,
/// sorted by (row, col) with unique cells (the `RenderInput::ink` invariant).
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

/// Detonation peak (t=500 ms) over a flat background: premultiplied One/One
/// quads, byte-exact CPU==GPU (gated on `additive_is_byte_exact`).
#[test]
fn supernova_detonation_peak_is_byte_exact_over_background() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16); // all background
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false; // isolate the supernova light
    let env = super_env(&cpu, rows, cols);
    push_super(&mut input, &env, 500);
    assert!(
        !input.nova_add.is_empty(),
        "detonation peak must emit wash/crown quads (non-vacuous)"
    );
    assert!(
        input.word_decorations.is_empty(),
        "dark-theme detonation is pure additive (the veil is light-theme only)"
    );

    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame.pixels, &gpu_frame.pixels);
    eprintln!("supernova detonation-peak over-bg max per-channel delta = {delta}");
    if gpu.additive_is_byte_exact() {
        assert_eq!(
            delta, 0,
            "detonation-peak wash+crown over a flat bg must be BYTE-EXACT CPU==GPU (got {delta})"
        );
    } else {
        eprintln!("SKIP byte-exact additive gate: downlevel sRGB offscreen (linear add)");
    }
}

/// Detonation peak over text: the wash over glyphs stays within the glyph
/// tolerance (<=8).
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
    if gpu.additive_is_byte_exact() {
        assert!(
            delta <= 8,
            "supernova detonation over text diverges: max delta {delta} > 8"
        );
    } else {
        eprintln!("SKIP supernova-over-text parity gate: downlevel sRGB offscreen");
    }
}

/// Damaged path, wash frame to shockwave frame: the wash lights every row, the
/// shockwave only the ring's, so most rows are vacated and must be rebuilt.
/// Cached equals fresh byte for byte on the CPU damaged path and the GPU
/// scissored path.
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

    // Frame A: mid-detonation wash (every row lit) primes both caches.
    let in_a = make(500);
    assert!(!in_a.nova_add.is_empty(), "wash frame premise");
    let cpu_a = cpu
        .render_input_cached(&mut win_cpu, &in_a)
        .pixels()
        .to_vec();
    let _ = gpu.present_input_readback(&mut win_gpu, &in_a);

    // Frame B: shockwave ring (t=1000) — the wash rows are vacated.
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

/// Settled after the super nova, with the frozen rainbow ink: once the window
/// closes (empty `nova_add` and decos) a byte-equal settled frame takes the
/// dirty gate on both backends. Counters alone would pass a wrong image, so a
/// pixel oracle also checks the ink lands on exactly the word's four lead cells
/// and the gate hit hands back a fresh render's pixels.
#[test]
fn settled_after_supernova_gate_hits_with_frozen_rainbow_ink() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    // The word at row 2 col 4 (CUP is 1-based), so the ink is visible.
    term.process(b"\x1b[3;5Hfuck");
    let (cw, ch) = cpu.cell_size();
    let env = super_env(&cpu, rows, cols);

    // A late animating frame (debris motes still live), then the settled one.
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

    // Pixel oracle 1: against a no-ink twin only the four lead cells change, each
    // to its own colour. Fresh renders on throwaway windows.
    let mut settled_bare = settled.clone();
    settled_bare.ink.clear();
    let mut win_fresh = aterm_gpu::WindowGpu::new();
    let cpu_frame = cpu.render_input(&settled);
    // The cell arithmetic below is grid-relative, so pin the framing: no
    // interior pad, no head band (both default to 0 in these backends).
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
        // Every pixel outside the four lead cells is untouched by the ink.
        for (i, (&a, &b)) in inked.iter().zip(bare.iter()).enumerate() {
            let (row, col) = ((i / width) / ch, (i % width) / cw);
            let is_lead = row == 2 && (4..=7).contains(&col);
            assert!(
                is_lead || a == b,
                "{backend}: the frozen ink must touch ONLY the word's lead cells — \
                 pixel {i} (row {row}, col {col}) changed {b:#08x} -> {a:#08x}"
            );
        }
        // ... and each lead cell is inked, with its own exact colour.
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

    // The gate decisions (counters).
    let cpu_debris = cpu
        .render_input_cached(&mut win_cpu, &in_debris)
        .pixels()
        .to_vec();
    let _ = gpu.render_input_cached(&mut win_gpu, &in_debris);
    // Debris → settled: a real change (decos vanish) — both must re-render.
    let cpu_settled_cached = cpu
        .render_input_cached(&mut win_cpu, &settled)
        .pixels()
        .to_vec();
    assert_ne!(
        win_cpu.last_damage(),
        DamageOutcome::GateHit,
        "debris → settled is a real change (decos vanish)"
    );
    // The same claim in pixels, not just in the damage outcome.
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

    // Settled → settled (byte-equal ink, empty overlays): the gate must hit.
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

    // Pixel oracle 2: the gate hit's pixels equal a fresh full render.
    same_image("CPU", &cpu_settled, &cpu_hit, width);
    same_image("GPU", &gpu_settled, &gpu_hit, width);
}

/// Byte-for-byte frame equality that names the first divergent pixel (and its
/// cell) instead of dumping two whole frames into the log.
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
