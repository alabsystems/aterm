// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LUMEN WAKE release performance gates.
//!
//! ```sh
//! cargo test -p aterm-effects --release --test cursor_bench -- --ignored --nocapture --test-threads=1
//! # the same gates on a host with no measurement on record — scales the four
//! # ABSOLUTE wall-clock bounds only (p90 500 / median 500 / p90 1 000 /
//! # p90 2 000 µs); the quad caps, shares and fixture checks are never scaled
//! # (see `common::budget_scale`):
//! RK_FRAME_COST_BUDGET_SCALE=2 cargo test -p aterm-effects --release --test cursor_bench \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The knob is the one `tests/rainbow_kitty_v2_frame_cost.rs` reads — one name
//! across both gates, so a host's scale is set once; every red prints the
//! effective bound beside the reference and the scale it came from.

mod common;

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use common::{BUDGET_SCALE_VAR, budget_scale, scaled_budget_us};

const ITERATIONS: usize = 300;

fn config(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        // Shipping default: the tall body; explicit underline is exercised by
        // the workload matrix benchmark.
        ribbon_tall: true,
        ribbon_flat: false,
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
        audible: true,
        radius: 0.4,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
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

/// A bench fixture: build a `CursorGlow` in the given style plus its start instant
/// and cursor cell.
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
    // The reference machine's bound; this host's is it times the scale.
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
        "fixture must exercise substantial geometry ({max_total_quads} quads)"
    );
    assert!(
        p90.as_micros() < u128::from(p90_bound_us),
        "{label} cursor frame p90 {p90:?} >= {p90_bound_us} us \
         (reference {P90_REFERENCE_US} us × {BUDGET_SCALE_VAR} {scale}; median {median:?})"
    );
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_cursor_rainbow_frozen_sweep_baseline() {
    benchmark_with(
        GlowStyle::RainbowKitty,
        "bench_cursor_rainbow_frozen_sweep_baseline",
        saturated_sweep,
        5_000,
    );
}

/// The HOT ribbon gate the frozen-clock fixture above cannot reach: `saturated`
/// re-ticks one Instant, so `dt == 0` never runs the momentum integrator,
/// `rainbow.disp` stays 0, and only the cheaper resting path is measured. This
/// 125-key/s saturation stress warms the clock 8 ms per event and keeps typing
/// through the measured window, pinning the capped tall-ribbon worst-case cost;
/// it cannot observe geometry growth after the deliberate budget edge. It is
/// deliberately harsher than a claim about ordinary human cadence.
///
/// This is the deterministic half of that gate: a normal test run checks the
/// amount of real render work, not a host scheduler's clock. The release-only
/// timing probe below is deliberately supplementary.
#[test]
fn cursor_rainbow_hot_ribbon_work_stays_below_its_normalized_share() {
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
    let mut dir = 1i32;
    glow.tick(Some((row, col)), now, &config, geometry, &mut quads);
    for _ in 0..320 {
        now += Duration::from_millis(8);
        if (dir > 0 && col as usize + 2 >= geometry.cols) || (dir < 0 && col == 0) {
            dir = -dir;
        }
        col = (col as i32 + dir) as u16;
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some((row, col)), now, &config, geometry, &mut quads);
    }
    let under = glow.under_quads().len();
    let total = under + quads.len();
    println!(
        "hot rainbow normalized work: {under} under + {} over",
        quads.len()
    );
    assert!(
        under > 2_000,
        "fixture must reach the active tall ribbon ({under} quads)"
    );
    // The ribbon's DECLARED share of `MAX_QUADS`, restated here because a
    // `tests/` target cannot see the private `RAINBOW_RIBBON_QUAD_BUDGET` the
    // emitter solves against. It moved `1/2 -> 5/8` on 2026-08-31: at retina
    // metrics a full-width 100-cell tall line costs ~77 quads per cell even at
    // one slab, so half the budget made the cap shed the TAIL of exactly the
    // long lines the owner types (the missing red end). The law this gate
    // states is unchanged — the ribbon stays inside its own share, and the
    // frame-wide headroom assert below is what keeps the companions honest.
    assert!(
        under * 8 <= 16_384 * 5,
        "default tall ribbon spent more than its five-eighths normalized share: {under}"
    );
    assert!(
        total < 16_384,
        "active rim/ribbon frame must retain global quad headroom: {under} + {}",
        quads.len()
    );
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_cursor_rainbow_hot_ribbon_worstcase() {
    // The shipping-default tall presentation is the renderer's real
    // high-geometry rainbow path. The cold frozen-clock gate above cannot
    // reach its animated body, so this fixture warms it explicitly.
    let config = config(GlowStyle::RainbowKitty);
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
    // The reference machine's bounds; this host's are them times the scale.
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
    // The no-jump body has a dedicated share. Crossing it means a supposedly
    // ordinary typing frame borrowed jump/overflow capacity. The share is
    // `RAINBOW_RIBBON_QUAD_BUDGET`, restated here because a `tests/` target
    // cannot see it; it moved `1/2 -> 5/8` on 2026-08-31 so the cap stops
    // shedding the TAIL of a long line (see the emitter's own note).
    assert!(
        max_under_quads * 8 <= 16_384 * 5,
        "hot retina ribbon exceeded its dedicated share ({max_under_quads} quads)"
    );
    // Timing remains a release-only smoke. The non-ignored work/capacity
    // regression below owns deterministic enforcement. Five audit runs while
    // another crate compiled measured stable medians of 268–434 µs but p90
    // scheduler tails of 511–839 µs; a 500 µs p90 therefore rejected healthy
    // work for unrelated CPU contention. Keep the CPU median under the 500 µs
    // production budget and use one millisecond only as a generous tail smoke
    // (both the reference machine's numbers; `RK_FRAME_COST_BUDGET_SCALE`
    // scales them for a host with none on record, and only them).
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
