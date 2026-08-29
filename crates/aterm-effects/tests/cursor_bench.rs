// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LUMEN WAKE release performance gates.
//!
//! ```sh
//! cargo test -p aterm-effects --release --test cursor_bench -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};

const ITERATIONS: usize = 300;

fn config(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        ribbon_tall: false,
        enabled: true,
        dark_theme: true,
        // The documented default dark palette — a COHERENT pair, never 0/0
        // (`fg == bg` reads as a conceal-shaped theme and suppresses the tint).
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style,
        color: 0x00d0_d0d0,
        accent: 0x0048_c9ff,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        radius: 0.4,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
        wake_persist_s: aterm_effects::cursor_glow::RAINBOW_WAKE_PERSIST,
    }
}

fn geometry() -> Geom {
    // Identity layout: origin 0 + win == grid extents.
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

fn saturated(style: GlowStyle) -> (CursorGlow, Instant, (u16, u16)) {
    let config = config(style);
    let geometry = geometry();
    let now = Instant::now();
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let row = 24;
    let mut cursor = (row, 80);
    glow.tick(Some(cursor), now, &config, geometry, &mut quads);
    // Same-instant alternating moves are the hostile pre-decay case: resident
    // spark/particle storage reaches its hard cap while every emission remains
    // bounded. Normal human typing is far below this state. Authenticate each
    // synthetic move exactly like the shipping host; raw program deltas are
    // deliberately dark under the cursor-ownership gate and would make this
    // saturation/performance assertion vacuous.
    for step in 0usize..1_200 {
        cursor.1 = if step.is_multiple_of(2) { 81 } else { 80 };
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some(cursor), now, &config, geometry, &mut quads);
    }
    (glow, now, cursor)
}

/// the rainbow kitty's hostile pre-decay fixture: a same-instant ping-pong SWEEP across 160
/// distinct columns. The two-cell alternation above no longer saturates rainbow kitty —
/// its cell-ownership dedup collapses revisits into ONE live spark per cell
/// (exactly the stacking the audit caught), so alternating two cells now
/// measures ~2 sparks. Sweeping distinct cells fills the resident spark store
/// for real while the frozen clock keeps every emission on the cold path.
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
        // Synthetic typing must carry the same explicit provenance as the
        // shipping host; otherwise the anti-stray gate correctly rejects this
        // benchmark's raw cursor deltas and the saturation guard is vacuous.
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some(cursor), now, &config, geometry, &mut quads);
    }
    (glow, now, cursor)
}

fn benchmark(style: GlowStyle, label: &str) {
    benchmark_with(style, label, saturated);
}

/// A bench fixture: build a `CursorGlow` in the given style plus its start instant
/// and cursor cell.
type GlowFixture = fn(GlowStyle) -> (CursorGlow, Instant, (u16, u16));

fn benchmark_with(style: GlowStyle, label: &str, fixture: GlowFixture) {
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
    println!(
        "bench_cursor_{label}_worstcase: median {median:?} (p90 {p90:?}), \
         max quads {max_total_quads} ({max_under_quads} under + {max_over_quads} over)"
    );
    assert!(
        max_total_quads > 100,
        "fixture must exercise substantial geometry"
    );
    assert!(
        p90.as_micros() < 500,
        "worst-case {label} cursor frame p90 {p90:?} >= 500 us (median {median:?})"
    );
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_cursor_water_worstcase() {
    benchmark(GlowStyle::Water, "water");
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_cursor_rainbow_worstcase() {
    benchmark_with(GlowStyle::RainbowKitty, "nyan", saturated_sweep);
}

/// The HOT ribbon gate the frozen-clock fixture above cannot reach: `saturated`
/// re-ticks one Instant, so `dt == 0` never runs the momentum integrator,
/// `rainbow.disp` stays 0, and only the cold 1-strip path is measured — while the
/// real worst case is the per-strip wave at RETINA cell metrics under sustained
/// typing (the audited ~6× cost / quad-budget saturation). This fixture warms
/// the clock 8 ms per keystroke and keeps typing THROUGH the measured window,
/// pinning both the frame cost and the no-truncation budget headroom.
#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_cursor_rainbow_hot_ribbon_worstcase() {
    // The default RainbowKitty presentation is the restored v0.43 underline
    // and is covered by `bench_cursor_rainbow_worstcase`. This gate is for the
    // explicitly selected TALL presentation whose animated per-strip body is
    // the renderer's real high-geometry rainbow path.
    let mut config = config(GlowStyle::RainbowKitty);
    config.ribbon_tall = true;
    // 2× retina cell metrics — the hot path's cost scales with device pixels.
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
    // A ping-pong sweep keeps every move a 1-cell TYPING advance (a row jump
    // would reset + fast-fade the ribbon): momentum pins hot and the resident
    // spark population reaches its steady-state worst case.
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
    println!(
        "bench_cursor_rainbow_hot_ribbon_worstcase: median {median:?} (p90 {p90:?}), \
         max quads {max_total_quads} ({max_under_quads} under + {max_over_quads} over)"
    );
    assert!(
        max_under_quads > 6_000,
        "fixture must exercise the hot under-ink per-strip path ({max_under_quads} quads)"
    );
    // Each stream's MAX_QUADS is 16_384; at the under-ink cap the ribbon tail
    // visibly pops off every frame, so the ribbon stream must keep real headroom.
    assert!(
        max_under_quads <= 14_384,
        "hot retina ribbon saturates its quad budget ({max_under_quads} quads)"
    );
    assert!(
        p90.as_micros() < 500,
        "worst-case hot nyan cursor frame p90 {p90:?} >= 500 us (median {median:?})"
    );
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_cursor_water_outlier_jump() {
    const JUMP_ITERATIONS: usize = 200;
    // A 4,095-cell diagonal is tail-capped at MAX_SPARKS=512 resident path
    // samples; appending the live destination makes 512 adjacent segments.
    // Water rasterizes both its undertow and crest across every segment, so
    // even the 1 px hostile geometry must produce at least one clipped quad
    // per layer per segment. The shared per-stream upload ceiling is
    // CursorGlow::MAX_QUADS=16,384 (private production constants, repeated
    // here deliberately so a cap change must be reviewed against this gate).
    const MIN_OVER_QUADS: usize = 2 * 512;
    const MAX_OVER_QUADS: usize = 16_384;
    let config = config(GlowStyle::Water);
    let geometry = Geom {
        cw: 1,
        ch: 1,
        rows: 4096,
        cols: 4096,
        origin_x: 0,
        origin_y: 0,
        win_w: 4096,
        win_h: 4096,
        head: 0,
    };
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    let mut cursor = (0, 0);
    glow.tick(Some(cursor), now, &config, geometry, &mut quads);

    let mut samples = Vec::with_capacity(JUMP_ITERATIONS);
    let mut min_over_quads = usize::MAX;
    let mut max_over_quads = 0usize;
    let mut min_under_quads = usize::MAX;
    let mut max_under_quads = 0usize;
    for iteration in 0..JUMP_ITERATIONS {
        now += Duration::from_secs(1);
        cursor = if iteration.is_multiple_of(2) {
            (4095, 4095)
        } else {
            (0, 0)
        };
        // Raw cursor deltas are deliberately dark under the ownership gate.
        // Authenticate this benchmark's scripted jump through the same
        // explicit synthetic seam as the other hostile fixtures above, or the
        // timing loop measures the rejected/no-output path and passes
        // vacuously.
        glow.note_synthetic_move(now);
        let start = Instant::now();
        let fingerprint = glow.tick(Some(cursor), now, &config, geometry, &mut quads);
        samples.push(start.elapsed());
        assert_ne!(
            fingerprint, 0,
            "outlier-jump fixture must emit authenticated water geometry"
        );
        min_over_quads = min_over_quads.min(quads.len());
        max_over_quads = max_over_quads.max(quads.len());
        min_under_quads = min_under_quads.min(glow.under_quads().len());
        max_under_quads = max_under_quads.max(glow.under_quads().len());
    }
    samples.sort();
    let median = samples[JUMP_ITERATIONS / 2];
    let p90 = samples[JUMP_ITERATIONS * 9 / 10];
    println!(
        "bench_cursor_water_outlier_jump: median {median:?} (p90 {p90:?}), \
         over quads {min_over_quads}..={max_over_quads}, \
         under quads {min_under_quads}..={max_under_quads}"
    );
    assert!(
        min_over_quads >= MIN_OVER_QUADS,
        "authenticated 4,095-cell water jump emitted too little wake: \
         over quads {min_over_quads}..={max_over_quads}, expected every frame >= \
         {MIN_OVER_QUADS}"
    );
    assert!(
        max_over_quads <= MAX_OVER_QUADS,
        "authenticated 4,095-cell water jump exceeded the per-stream cap: \
         over quads {min_over_quads}..={max_over_quads}, expected every frame <= \
         {MAX_OVER_QUADS}"
    );
    assert_eq!(
        (min_under_quads, max_under_quads),
        (0, 0),
        "Water owns only the over-ink stream; unexpected under-ink quads across \
         the run (min, max)"
    );
    assert!(
        p90.as_micros() < 2_000,
        "bounded 4095-cell jump p90 {p90:?} >= 2 ms"
    );
}
