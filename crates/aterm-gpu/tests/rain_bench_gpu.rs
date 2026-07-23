// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PHOSPHOR perf gate, GPU renderer side (matrix-rain design §8): the
// steady-state animating GPU frame cost of a 120x40 transcript screen under a
// settled density-12 WORKING downpour, against the SAME frame with empty rain
// channels, measured in ONE run so the rain-under stream's added cost is the
// baseline-relative delta the bar names. Bar: raining median <= baseline
// median + 0.3 ms (both measured here). #[ignore]d release bench in the repo's
// manual-timing idiom (warm-up, ALTERNATING baseline/rain runs so drift hits
// both sides, sorted medians, printed numbers). Median of 3 runs lands in
// PROOF_CARRYING_PERFORMANCE.md ("PHOSPHOR"); transcript under
// proofs/phosphor/. No GPU / no font => the bench no-ops (returns), like the
// GPU parity gates.
//
// ```sh
// cargo test -p aterm-gpu --release --test rain_bench_gpu -- --ignored --nocapture
// ```

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainTickInput};
use aterm_render::Theme;

const ROWS: usize = 40;
const COLS: usize = 120;

/// A 120x40 agent-transcript-shaped screen: text on every 4th row (the gaps
/// are the rain-eligible negative space a real Claude Code session leaves),
/// cursor hidden.
fn transcript_terminal() -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l");
    let line = "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(3);
    for r in (0..ROWS).step_by(4) {
        term.process(format!("\x1b[{};1H{}", r + 1, &line[..COLS]).as_bytes());
    }
    term
}

/// Build an enabled engine over the terminal's REAL cell snapshot and drive it
/// through the public weather inputs into a settled WORKING downpour (~20 s of
/// simulated 30 Hz clock; sustained content deltas hold WORKING and push the
/// density EMA to its ceiling). Mirrors the aterm-render bench driver so the
/// two frame benches price the same field.
fn downpour(term: &mut Terminal, geom: EffectGeom, density: u8) -> MatrixRain {
    let mut e = MatrixRain::new(RainConfig {
        enabled: true,
        density,
        // This fixture prices the classic steady renderer path. Literal mode
        // needs a sampled material bank and intentionally emits nothing blank.
        output_material: false,
        seed: 7,
        ..RainConfig::default()
    });
    let input = term.cell_frame(ROWS, COLS);
    e.rescan_from_cells(
        &input.cells,
        &input.line_sizes,
        &input.images,
        ROWS,
        COLS,
        input.default_bg,
        1,
    );
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..600u64 {
        e.note_activity(i + 1);
        e.advance_ms(33);
        e.emit(geom, &RainTickInput::default(), &mut q, &mut a);
    }
    e
}

/// §8 perf gate — GPU `bench_rain_frame_steadystate`: full-frame GPU render
/// cost (encode + submit + GPU-side execution, no readback so the measured
/// delta is the rain-under stream's alone) of a 120x40 transcript screen under
/// a settled density-12 WORKING downpour, against the SAME frame with empty
/// rain channels. Baseline is measured in the SAME alternating loop. Bar:
/// raining median <= baseline median + 0.3 ms.
#[test]
#[ignore = "perf gate (design §8): run manually in --release with --ignored --nocapture"]
fn bench_rain_frame_steadystate_gpu() {
    let mut gpu = match aterm_gpu::GpuRenderer::new(18.0, Theme::default()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let mut win = aterm_gpu::WindowGpu::new();
    let (cw, ch) = gpu.cell_size();
    let mut term = transcript_terminal();
    let geom = EffectGeom {
        cell_w: cw as u16,
        cell_h: ch as u16,
        rows: ROWS as u16,
        cols: COLS as u16,
    };
    let mut engine = downpour(&mut term, geom, 12);
    let base_input = term.cell_frame(ROWS, COLS);
    let mut rain_input = base_input.clone();
    let tick_input = RainTickInput::default();

    // Warm both paths (also completes the engine's progressive atlas bake and
    // primes the GPU's resident offscreen + atlas bind group).
    for i in 0..8u64 {
        engine.note_activity(700 + i);
        engine.advance_ms(33);
        let fp = engine.emit(
            geom,
            &tick_input,
            &mut rain_input.rain_quads,
            &mut rain_input.rain_add,
        );
        assert_ne!(fp, 0, "the steady-state drive must actually rain");
        rain_input.rain_atlas = engine.rain_atlas();
        gpu.render_no_readback(&mut win, &base_input);
        gpu.render_no_readback(&mut win, &rain_input);
    }

    let iters = 60usize;
    let (mut t_base, mut t_rain): (Vec<Duration>, Vec<Duration>) =
        (Vec::with_capacity(iters), Vec::with_capacity(iters));
    let (mut qmin, mut qmax, mut amax) = (usize::MAX, 0usize, 0usize);
    for i in 0..iters as u64 {
        // Advance the ANIMATION between iterations (fresh quads, mutation ticks
        // included) — emission cost stays OUTSIDE the timed section (the
        // engine-side bench prices it separately).
        engine.note_activity(1000 + i);
        engine.advance_ms(33);
        engine.emit(
            geom,
            &tick_input,
            &mut rain_input.rain_quads,
            &mut rain_input.rain_add,
        );
        rain_input.rain_atlas = engine.rain_atlas();
        qmin = qmin.min(rain_input.rain_quads.len());
        qmax = qmax.max(rain_input.rain_quads.len());
        amax = amax.max(rain_input.rain_add.len());
        // ALTERNATE base/rain so device thermal/scheduler drift hits both sides.
        let s = Instant::now();
        gpu.render_no_readback(&mut win, &base_input);
        t_base.push(s.elapsed());
        let s = Instant::now();
        gpu.render_no_readback(&mut win, &rain_input);
        t_rain.push(s.elapsed());
    }
    t_base.sort();
    t_rain.sort();
    let (base, rain) = (t_base[iters / 2], t_rain[iters / 2]);
    let delta = rain.saturating_sub(base);
    println!(
        "bench_rain_frame_steadystate_gpu: cells {cw}x{ch} px — baseline median {base:?}, \
         raining median {rain:?} (delta {delta:?}), rain quads {qmin}..{qmax}, halo quads <= {amax} \
         (bar: baseline + 0.3 ms)"
    );
    assert!(qmax >= 256, "non-vacuity: downpour too thin ({qmax} quads)");
    assert!(
        delta.as_micros() < 300,
        "§8 gate: GPU raining frame {rain:?} exceeds baseline {base:?} + 0.3 ms (delta {delta:?})"
    );
}
