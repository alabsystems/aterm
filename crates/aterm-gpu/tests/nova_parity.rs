// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Parity gate for the SUPERNOVA additive light (Sparkle Words v2 `nova_add`):
// the second glow-shaped One/One stream over the linear Rgba8Unorm offscreen
// must match the CPU integer `add_sat` exactly like the LUMEN aurora —
// BYTE-EXACT (delta 0) over an opaque background, within the base glyph
// tolerance (<=8) over anti-aliased text, and an empty vec (the Settled nova,
// also `clear_overlays`) is byte-identical to the pre-nova path.
//
// SUPERNOVA arm (Sparkle Words v3 §3.2/§3.4): the FUCK SUPER NOVA rides the
// SAME channels (`nova_add` + `word_decorations`), driven here straight from
// the PURE emitters `supernova::emit_super`/`emit_super_decos` (clockless
// functions of (t, env) — no engine needed). Pinned: detonation-peak frame
// delta==0 over flat bg (gated `additive_is_byte_exact`) / <=8 over text;
// damaged-path wash-frame → shockwave-frame cached==fresh (the vacated-rows
// no-ghost case) on BOTH backends; settled-after-supernova gate-hit with the
// frozen rainbow ink still present.
//
// Gated: no GPU / no system font -> the test no-ops (returns); the byte-exact
// assertions additionally gate on `additive_is_byte_exact` (downlevel sRGB
// offscreens fold the add into linear — the accepted approximation).

use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;
use aterm_effects::supernova::{self, SuperEnv};
use aterm_render::{DamageOutcome, GlowQuad, Renderer, Theme, WindowCpu, premul_rgb};

mod common;
use common::{backends_fontdue as backends, bb, max_channel_delta};

/// A hand-built nova frame: a 3-row crown column plus per-row ring-band chord
/// quads (the §6.3 emitter shape: every quad in exactly one row band).
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
            });
        }
    }
}

/// (a) BLANK-TARGET additive validation: premultiplied nova light over pure
/// background must be BYTE-EXACT CPU==GPU (delta 0), and equal
/// min(255, bg+premul) per channel — the §8 "byte-exact additive" bar.
#[test]
fn nova_additive_is_byte_exact_over_background() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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

/// (b) FULL-FRAME over real text: the crown + ring composited over glyphs must
/// stay within the glyph tolerance (<=8) — additive light preserves, never
/// widens, the base AA divergence.
#[test]
fn nova_over_text_matches_within_tolerance() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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

/// (c) The EMPTY-nova code path is a TRUE no-op on both backends: a render with
/// an empty `nova_add` is byte-identical to one where quads were pushed,
/// RENDERED, and then cleared — including via `clear_overlays` (the `image
/// plain` contract).
///
/// THE RENDER BETWEEN THE PUSH AND THE CLEAR IS THE WHOLE TEST. All four legs
/// used to push and clear back-to-back, so no `render_input` ever received a
/// populated `nova_add` and the pixel assertions reduced to "the same empty
/// input renders the same twice". The CPU twin
/// (crates/aterm-render/tests/nova.rs) always had the honest form; this file
/// looked like a faithful mirror and was not. Each leg now renders the
/// populated frame and asserts it DIFFERS from base before draining, so a
/// silently-dropped nova stream cannot make the drain look clean.
#[test]
fn empty_nova_is_byte_identical_to_no_nova() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
    let (rows, cols) = (4usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hello aterm");
    let (cw, ch) = cpu.cell_size();
    let mut input = term.cell_frame(rows, cols);
    assert!(input.nova_add.is_empty());

    // CPU: baseline (empty) -> painted -> drained -> painted -> stripped.
    let cpu_base = cpu.render_input(&input).pixels.clone();
    push_nova(&mut input, cw, ch);
    let cpu_painted = cpu.render_input(&input).pixels.clone();
    assert_ne!(
        cpu_base, cpu_painted,
        "NON-VACUITY: a live nova_add frame must paint on the CPU"
    );
    input.nova_add.clear();
    let cpu_after = cpu.render_input(&input).pixels.clone();
    assert_eq!(
        max_channel_delta(&cpu_base, &cpu_after),
        0,
        "empty-nova path is not a no-op on the CPU"
    );
    push_nova(&mut input, cw, ch);
    let _ = cpu.render_input(&input);
    input.clear_overlays();
    assert!(
        input.nova_add.is_empty(),
        "clear_overlays must strip nova_add"
    );
    let cpu_stripped = cpu.render_input(&input).pixels.clone();
    assert_eq!(
        max_channel_delta(&cpu_base, &cpu_stripped),
        0,
        "clear_overlays must restore the bare frame on the CPU"
    );

    // GPU: the same painted-then-emptied-then-stripped invariant, on ONE
    // renderer+window so any per-frame instance-stream residue would survive
    // into the drained frame.
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_base = gpu.render_input(&mut win, &input, None).pixels;
    push_nova(&mut input, cw, ch);
    let gpu_painted = gpu.render_input(&mut win, &input, None).pixels;
    assert_ne!(
        gpu_base, gpu_painted,
        "NON-VACUITY: a live nova_add frame must paint on the GPU"
    );
    input.nova_add.clear();
    let gpu_after = gpu.render_input(&mut win, &input, None).pixels;
    assert_eq!(
        max_channel_delta(&gpu_base, &gpu_after),
        0,
        "empty-nova path is not a no-op on the GPU"
    );
    push_nova(&mut input, cw, ch);
    let _ = gpu.render_input(&mut win, &input, None);
    input.clear_overlays();
    let gpu_stripped = gpu.render_input(&mut win, &input, None).pixels;
    assert_eq!(
        max_channel_delta(&gpu_base, &gpu_stripped),
        0,
        "clear_overlays must restore the bare frame on the GPU"
    );
}

/// (d) DAMAGED/CACHED-PATH nova parity with a MULTI-ROW shape: frame A places
/// the crown+ring (priming both caches); frame B advances the ring downward —
/// a real change whose prev∪cur rows the shared dirty set must cover on both
/// backends (CPU damage rows == GPU scissor band), with no stale light left on
/// any vacated band. Over a glyph-free background the additive light is
/// byte-exact, so CPU frame B must equal GPU frame B with delta 0.
#[test]
fn damaged_path_multi_row_nova_parity_cpu_matches_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some((mut cpu, mut gpu)) = backends(px, theme) else {
        return;
    };
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
// SUPERNOVA arm (Sparkle Words v3 §3.2) — the design-promised nova_parity
// cases, driven straight from the pure emitters.
// ---------------------------------------------------------------------------

/// The supernova word: row 2, cells 4..=7 ("fuck" is 4 lead cells). Dark theme
/// (`Theme::default`), so the detonation is the full-viewport additive wash +
/// star crown — pure `nova_add`.
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
        // FIX 9: the ENGINE's reach clamp (min(6 rows, grid_h/2), 1 px floor)
        // via the shared helper — the former min(grid_w, grid_h) clamp ran
        // these pins at a radius the engine can never produce.
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

/// Fold one supernova frame at `t_ms` into `input`'s channels, exactly as the
/// engine does: additive quads into `nova_add`, deco stamps (debris motes /
/// light-theme veil) into `word_decorations`.
fn push_super(input: &mut aterm_core::render::RenderInput, env: &SuperEnv, t_ms: u64) {
    supernova::emit_super(
        t_ms,
        env,
        supernova::MAX_SUPER_QUADS_PER,
        &mut input.nova_add,
    );
    supernova::emit_super_decos(t_ms, env, &mut input.word_decorations, 256);
}

/// The §3.1 frozen rainbow ink: the static per-lead-cell gradient the episode
/// settles to after the supernova — byte-stable across frames (sorted by
/// (row, col), unique cells, per the `RenderInput::ink` invariant).
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

/// (e) SUPERNOVA detonation peak (t=500 ms, `sin(π·e)` == 1) over a FLAT
/// background: the wash + crown are premultiplied One/One quads, so the frame
/// must be BYTE-EXACT CPU==GPU (delta 0, gated on `additive_is_byte_exact`).
#[test]
fn supernova_detonation_peak_is_byte_exact_over_background() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
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

/// (f) SUPERNOVA detonation peak over REAL text (the charge-lit prompt): the
/// full-viewport wash composited over glyphs must stay within the base glyph
/// tolerance (<=8) — additive light preserves, never widens, the AA divergence.
#[test]
fn supernova_detonation_peak_over_text_matches_within_tolerance() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
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

/// (g) DAMAGED-PATH wash-frame → shockwave-frame, the vacated-rows no-ghost
/// case: the detonation wash lights EVERY viewport row; the shockwave frame
/// lights only the ring's rows, so most rows are VACATED between the frames
/// and must be rebuilt fresh (additive light never survives on — or
/// re-accumulates onto — a preserved row). Cached==fresh byte-for-byte on
/// BOTH backends: the CPU damaged path and the GPU SCISSORED present path.
#[test]
fn supernova_damaged_path_wash_to_shockwave_cached_equals_fresh() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
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
    let mut gpu2 = aterm_gpu::GpuRenderer::new(18.0, theme).expect("GPU was available above");
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

/// (h) SETTLED-AFTER-SUPERNOVA gate-hit with the frozen rainbow ink present:
/// once the supernova window closes (t >= SUPER_TOTAL_MS: empty `nova_add`,
/// empty decos) the episode's settled state is the STATIC rainbow ink — and a
/// byte-equal settled frame must take the dirty gate on BOTH backends
/// (`is_active == false` with non-empty settled ink is the §3.1 gate-hit
/// path; 0% idle).
///
/// WITH A PIXEL ORACLE, because counters alone would pass a renderer that hit
/// the gate at the right moments and drew the wrong image. Two additions, and
/// the FIRST is what made the second worth having:
///   * THE WORD IS ON SCREEN. `ink` is a per-cell FOREGROUND override, and the
///     grid this test used to render was BLANK. MEASURED on that old fixture:
///     rendering the settled frame with the ink and without it differs in
///     ZERO pixels, on both backends — the "frozen rainbow ink must be
///     present" assertion was about the input vec, never about the frame. The
///     terminal now prints `fuck` at the episode's own cells (row 2, cols
///     4..=7 — the `super_env` word), which moves 244 CPU pixels.
///   * THE SETTLED IMAGE IS CHECKED. Against a no-ink twin: EXACTLY the four
///     lead cells may differ, each must differ, and each must contain its own
///     ink colour verbatim. Then the pixels the GATE HIT hands back must equal
///     a fresh full render of the settled frame, byte-for-byte, on both
///     backends — a hit that replayed the debris frame, or any stale buffer,
///     fails here even though every counter is right.
#[test]
fn settled_after_supernova_gate_hits_with_frozen_rainbow_ink() {
    let theme = Theme::default();
    let Some((mut cpu, mut gpu)) = backends(18.0, theme) else {
        return;
    };
    let mut win_cpu = WindowCpu::new();
    let mut win_gpu = aterm_gpu::WindowGpu::new();
    let (rows, cols) = (8usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    // The supernova word itself, at the cells `super_env`/`frozen_rainbow_ink`
    // name: CUP is 1-based, so row 2 col 4 is `\x1b[3;5H`. Without glyphs here
    // the ink is invisible and every pixel assertion below would be vacuous.
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

    // ---- PIXEL ORACLE 1: the settled frame IS the inked word ---------------
    //
    // The same settled input minus the ink is the differential twin: the ink
    // must change the four lead cells and NOTHING else, and each lead cell must
    // carry its own colour verbatim (per-cell distinct colours, so a swapped,
    // smeared or dropped entry is caught). Fresh full renders on throwaway
    // windows, independent of the cached path exercised below.
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
        // Every pixel OUTSIDE the four lead cells is untouched by the ink.
        for (i, (&a, &b)) in inked.iter().zip(bare.iter()).enumerate() {
            let (row, col) = ((i / width) / ch, (i % width) / cw);
            let is_lead = row == 2 && (4..=7).contains(&col);
            assert!(
                is_lead || a == b,
                "{backend}: the frozen ink must touch ONLY the word's lead cells — \
                 pixel {i} (row {row}, col {col}) changed {b:#08x} -> {a:#08x}"
            );
        }
        // ... and each lead cell IS inked, with its own exact colour.
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

    // ---- The gate decisions (counters), unchanged ---------------------------
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
    // The same claim in PIXELS, not just in the damage outcome.
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

    // Settled → settled (byte-equal ink, empty overlays): the gate must HIT.
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

    // ---- PIXEL ORACLE 2: what the gate handed back is the settled IMAGE ----
    //
    // A gate hit returns cached pixels without rendering. They must equal a
    // FRESH full render of the same input, byte-for-byte, on both backends —
    // the assertion the counters above cannot make.
    same_image("CPU", &cpu_settled, &cpu_hit, width);
    same_image("GPU", &gpu_settled, &gpu_hit, width);
}

/// Byte-for-byte frame equality that names the FIRST divergent pixel (and its
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
