// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Beam-rasterizer emission cost gate. `laser_beam_quads`/`comet_glow_quads`/
//! `phaser_streak_quads`/`beam_glow_quads` all fan a swept-sample run out over a
//! bloom-layer stack via `comet_beam`. The laser (6 layers) is the worst case:
//! the path RDP and the per-sample style are now computed ONCE per beam instead
//! of once per layer.
//! Window-space identity convention: `BeamClip::grid` (box == the old grid extents).
//!
//! ```sh
//! cargo test -p aterm-render --release --test beam_bench -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use aterm_render::{
    BeamClip, CometSample, GlowQuad, beam_glow_quads, comet_glow_quads, laser_beam_quads,
    phaser_streak_quads,
};

const ITERATIONS: usize = 2000;

/// A long, wiggly diagonal run (a real cursor jump) — enough samples that the
/// path RDP has work and every bloom layer would otherwise re-walk it.
fn run(n: usize) -> Vec<CometSample> {
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            CometSample {
                x: 6.0 + t * 900.0,
                y: 20.0 + (t * 11.0).sin() * 40.0,
                cov: (40.0 + 200.0 * t) as u8,
                pos: t,
            }
        })
        .collect()
}

/// A beam-style emitter under bench: fills `out` from the samples via the colour ramp.
type EmitFn = dyn Fn(&mut Vec<GlowQuad>, &[CometSample], &dyn Fn(f32) -> u32);

fn bench(label: &str, emit: &EmitFn) {
    let run = run(64);
    // A representative style ramp (a hue sweep + palette eval, like the real one).
    let color_at = |pos: f32| -> u32 {
        let r = (255.0 * pos) as u32;
        let g = (255.0 * (1.0 - pos)) as u32;
        (r << 16) | (g << 8) | 0x40
    };
    let mut out: Vec<GlowQuad> = Vec::with_capacity(4096);
    // Warm + prove it emits.
    out.clear();
    emit(&mut out, &run, &color_at);
    assert!(!out.is_empty(), "{label}: beam must emit quads");
    let mut samples = Vec::with_capacity(ITERATIONS);
    let mut sink = 0usize;
    for _ in 0..ITERATIONS {
        out.clear();
        let start = Instant::now();
        emit(&mut out, &run, &color_at);
        samples.push(start.elapsed());
        sink = sink.wrapping_add(out.len());
    }
    assert_ne!(sink, 0);
    samples.sort_unstable();
    let median = samples[ITERATIONS / 2];
    let p90 = samples[ITERATIONS * 9 / 10];
    println!(
        "bench_beam_{label}: median {median:?} (p90 {p90:?}), {} quads",
        out.len()
    );
    // Emitting one beam is trivial; this is a regression gate with a real number.
    assert!(
        median < Duration::from_micros(400),
        "beam {label} emit {median:?} >= 400 us"
    );
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_beam_laser() {
    bench("laser", &|out, run, c| {
        laser_beam_quads(out, BeamClip::grid(960, 400, 20), run, 3.0, 1.5, c)
    });
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_beam_comet() {
    bench("comet", &|out, run, c| {
        comet_glow_quads(out, BeamClip::grid(960, 400, 20), run, 3.0, 1.5, c)
    });
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_beam_phaser() {
    bench("phaser", &|out, run, c| {
        phaser_streak_quads(out, BeamClip::grid(960, 400, 20), run, 18.0, 1.5, c)
    });
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_beam_beam() {
    bench("beam", &|out, run, c| {
        beam_glow_quads(out, BeamClip::grid(960, 400, 20), run, 3.0, 1.5, c)
    });
}
