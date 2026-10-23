// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release-build latency benchmarks for saturated rainbow-kitty cursor glow frames.

#[path = "../tests/common/mod.rs"]
mod common;
#[path = "../tests/cursor_bench.rs"]
mod suite;

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowStyle};
use common::{BUDGET_SCALE_VAR, budget_scale, scaled_budget_us};
use suite::config;

const ITERATIONS: usize = 300;

fn main() {
    if cfg!(debug_assertions) {
        panic!("the bounds are release-build numbers: run it with `targo --unverified bench`");
    }
    bench_cursor_rainbow_frozen_sweep_baseline();
    bench_cursor_rainbow_hot_ribbon_worstcase();
}

fn geometry() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 50,
        cols: 200,
        origin_x: 0,
        origin_y: 0,
        win_w: 2000,
        win_h: 1000,
        head: 0,
    }
}

fn saturated_sweep(style: GlowStyle) -> (CursorGlow, Instant, (u16, u16)) {
    let config = config(style);
    let geometry = geometry();
    let now = Instant::now();
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let row = 24;
    const BASE: u16 = 20;
    const SWEEP: usize = 160;
    let mut cursor = (row, BASE);
    glow.tick(Some(cursor), now, &config, geometry, &mut quads);
    for step in 0usize..1_200 {
        let phase = step % (2 * SWEEP);
        let off = if phase < SWEEP {
            phase + 1
        } else {
            2 * SWEEP - phase - 1
        };
        cursor.1 = BASE + off as u16;
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some(cursor), now, &config, geometry, &mut quads);
    }
    (glow, now, cursor)
}

type GlowFixture = fn(GlowStyle) -> (CursorGlow, Instant, (u16, u16));

fn benchmark_with(style: GlowStyle, label: &str, fixture: GlowFixture, min_total_quads: usize) {
    let config = config(style);
    let geometry = geometry();
    let (mut glow, now, cursor) = fixture(style);
    let mut quads = Vec::new();
    let mut samples = Vec::with_capacity(ITERATIONS);
    let mut max_over_quads = 0usize;
    let mut max_under_quads = 0usize;
    let mut max_total_quads = 0usize;
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let fingerprint = glow.tick(Some(cursor), now, &config, geometry, &mut quads);
        samples.push(start.elapsed());
        assert_ne!(fingerprint, 0, "saturated {label} fixture must emit");
        let under_quads = glow.under_quads().len();
        max_over_quads = max_over_quads.max(quads.len());
        max_under_quads = max_under_quads.max(under_quads);
        max_total_quads = max_total_quads.max(quads.len() + under_quads);
    }
    samples.sort();
    let median = samples[ITERATIONS / 2];
    let p90 = samples[ITERATIONS * 9 / 10];
    const P90_REFERENCE_US: u64 = 500;
    let scale = budget_scale();
    let p90_bound_us = scaled_budget_us(P90_REFERENCE_US, scale);
    println!(
        "{label}: median {median:?} (p90 {p90:?}), \
         max quads {max_total_quads} ({max_under_quads} under + {max_over_quads} over); \
         p90 bound {p90_bound_us} us (reference {P90_REFERENCE_US} × {BUDGET_SCALE_VAR} {scale})"
    );
    assert!(
        max_total_quads > min_total_quads,
        "fixture must exercise substantial geometry ({max_total_quads} quads, floor \
         {min_total_quads})"
    );
    assert!(
        p90.as_micros() < u128::from(p90_bound_us),
        "{label} cursor frame p90 {p90:?} >= {p90_bound_us} us \
         (reference {P90_REFERENCE_US} us × {BUDGET_SCALE_VAR} {scale}; median {median:?})"
    );
}

fn bench_cursor_rainbow_frozen_sweep_baseline() {
    benchmark_with(
        GlowStyle::RainbowKitty,
        "bench_cursor_rainbow_frozen_sweep_baseline",
        saturated_sweep,
        3_600,
    );
}

fn bench_cursor_rainbow_hot_ribbon_worstcase() {
    let config = config(GlowStyle::RainbowKitty);
    let geometry = Geom {
        cw: 18,
        ch: 40,
        rows: 26,
        cols: 190,
        origin_x: 0,
        origin_y: 0,
        win_w: 3420,
        win_h: 1040,
        head: 0,
    };
    let row = 12u16;
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    let mut col = 0u16;
    let mut dir: i32 = 1;
    glow.tick(Some((row, col)), now, &config, geometry, &mut quads);
    let mut step = |glow: &mut CursorGlow, quads: &mut Vec<_>, now: &mut Instant| {
        *now += Duration::from_millis(8);
        if (dir > 0 && col as usize + 2 >= geometry.cols) || (dir < 0 && col == 0) {
            dir = -dir;
        }
        col = (col as i32 + dir) as u16;
        glow.note_synthetic_typed(*now, 1);
        glow.tick(Some((row, col)), *now, &config, geometry, quads);
    };
    for _ in 0..1_200 {
        step(&mut glow, &mut quads, &mut now);
    }
    let mut samples = Vec::with_capacity(ITERATIONS);
    let mut max_over_quads = 0usize;
    let mut max_under_quads = 0usize;
    let mut max_total_quads = 0usize;
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        step(&mut glow, &mut quads, &mut now);
        samples.push(start.elapsed());
        let under_quads = glow.under_quads().len();
        max_over_quads = max_over_quads.max(quads.len());
        max_under_quads = max_under_quads.max(under_quads);
        max_total_quads = max_total_quads.max(quads.len() + under_quads);
    }
    samples.sort();
    let median = samples[ITERATIONS / 2];
    let p90 = samples[ITERATIONS * 9 / 10];
    const MEDIAN_REFERENCE_US: u64 = 500;
    const P90_REFERENCE_US: u64 = 1_000;
    let scale = budget_scale();
    let median_bound_us = scaled_budget_us(MEDIAN_REFERENCE_US, scale);
    let p90_bound_us = scaled_budget_us(P90_REFERENCE_US, scale);
    println!(
        "bench_cursor_rainbow_hot_ribbon_worstcase: median {median:?} (p90 {p90:?}), \
         max quads {max_total_quads} ({max_under_quads} under + {max_over_quads} over); \
         median bound {median_bound_us} us / p90 bound {p90_bound_us} us \
         (reference {MEDIAN_REFERENCE_US}/{P90_REFERENCE_US} × {BUDGET_SCALE_VAR} {scale})"
    );
    assert!(
        max_under_quads > 2_000,
        "fixture must exercise the hot tall under-ink path ({max_under_quads} quads)"
    );
    assert!(
        max_under_quads * 8 <= 16_384 * 5,
        "hot retina ribbon exceeded its dedicated share ({max_under_quads} quads)"
    );
    assert!(
        median.as_micros() < u128::from(median_bound_us),
        "worst-case hot nyan cursor frame median {median:?} >= {median_bound_us} us \
         (reference {MEDIAN_REFERENCE_US} us × {BUDGET_SCALE_VAR} {scale})"
    );
    assert!(
        p90.as_micros() < u128::from(p90_bound_us),
        "worst-case hot nyan cursor frame p90 {p90:?} >= {p90_bound_us} us \
         (reference {P90_REFERENCE_US} us × {BUDGET_SCALE_VAR} {scale}; median {median:?})"
    );
}
