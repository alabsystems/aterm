// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release-build latency benchmarks for saturated rainbow-kitty cursor glow frames.

#[path = "../tests/common/mod.rs"]
mod common;
#[path = "../tests/cursor_bench.rs"]
mod suite;

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowStyle};
use common::{
    BUDGET_SCALE_VAR, budget_scale, caret_sample, note_blink, observe_caret_row,
    observe_ribbon_rows, scaled_budget_us,
};
use suite::config;

const ITERATIONS: usize = 300;

fn main() {
    if cfg!(debug_assertions) {
        panic!("the bounds are release-build numbers: run it with `targo --unverified bench`");
    }
    bench_cursor_rainbow_frozen_sweep_baseline();
    bench_cursor_rainbow_hot_ribbon_worstcase();
    bench_cursor_rainbow_populated_partial_custody();
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

/// Informational producer timing with a populated custody pool, including the
/// host's row observations. Fixture assertions check ownership, not latency.
fn bench_cursor_rainbow_populated_partial_custody() {
    const ROW: u16 = 12;
    const COL0: u16 = 2;
    const CELLS: u16 = 144;
    const RELEASES: u16 = 12;
    const WARM_FRAMES: usize = 256;

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
    let mut term = Terminal::new(26, 190);
    term.process(format!("\x1b[{};{}H", ROW + 1, COL0 + 1).as_bytes());
    let mut glow = CursorGlow::default();
    let mut quads = Vec::new();
    let mut row_buf = Vec::new();
    let mut blink_seen = 0;
    let mut now = Instant::now();
    let mut frame = |term: &Terminal, glow: &mut CursorGlow, now: Instant| {
        let (caret, cur) = caret_sample(term);
        note_blink(term, glow, &mut blink_seen, now);
        glow.note_context(term.is_alternate_screen());
        observe_caret_row(term, glow, &mut row_buf, caret, now);
        observe_ribbon_rows(term, glow, &mut row_buf);
        glow.tick(cur, now, &config, geometry, &mut quads)
    };
    let mut stamps = Vec::with_capacity(usize::from(RELEASES));
    let mut custody = |glow: &CursorGlow| {
        let ribbon = glow
            .v2_ribbon()
            .expect("the public host seam engages rainbow kitty");
        stamps.clear();
        stamps.extend(ribbon.cells().iter().filter_map(|cell| cell.released_at));
        let released = stamps.len();
        stamps.sort_unstable();
        stamps.dedup();
        (ribbon.cells().len(), released, stamps.len())
    };
    frame(&term, &mut glow, now);
    let text: Vec<u8> = (b'a'..=b'z').cycle().take(usize::from(CELLS)).collect();
    for byte in &text {
        now += Duration::from_millis(8);
        glow.note_typed_cells(now, 1);
        term.process(std::slice::from_ref(byte));
        frame(&term, &mut glow, now);
    }
    let last_key = now;
    assert_eq!(custody(&glow), (usize::from(CELLS), 0, 0));
    assert_eq!(glow.v2_ribbon().expect("typed ribbon").cohorts().len(), 1);
    for step in 0..RELEASES {
        now += Duration::from_millis(1);
        let cut = COL0 + CELLS - step - 1;
        term.process(
            format!(
                "\x1b[{};{}H\x1b[K\x1b[{};{}H",
                ROW + 1,
                cut + 1,
                ROW + 1,
                COL0 + CELLS + 1,
            )
            .as_bytes(),
        );
        frame(&term, &mut glow, now);
        assert_eq!(
            custody(&glow),
            (
                usize::from(CELLS),
                usize::from(step + 1),
                usize::from(step + 1)
            ),
            "each observed suffix clear contributes a distinct resident release stamp"
        );
    }
    for _ in 0..WARM_FRAMES {
        now += Duration::from_millis(1);
        frame(&term, &mut glow, now);
    }
    let populated = (
        usize::from(CELLS),
        usize::from(RELEASES),
        usize::from(RELEASES),
    );
    assert_eq!(custody(&glow), populated);
    assert!(
        (COL0 + CELLS - RELEASES..COL0 + CELLS).all(|col| {
            glow.v2_ribbon()
                .expect("dark custody")
                .field_at(ROW, col)
                .is_none()
        }),
        "all twelve held identities have finished their visual retract"
    );

    let mut samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        now += Duration::from_millis(1);
        let start = Instant::now();
        std::hint::black_box(frame(&term, &mut glow, now));
        samples.push(start.elapsed());
        assert_eq!(
            custody(&glow),
            populated,
            "every measured frame retains all release stamps"
        );
    }
    assert!(now.duration_since(last_key) < Duration::from_millis(900));
    assert!(!glow.v2_ribbon().expect("live original run").cohorts()[0].leaving_at(now));
    now += Duration::from_millis(1);
    term.process(format!("\x1b[{};{}H", ROW + 1, COL0 + 1).as_bytes());
    term.process(&text);
    frame(&term, &mut glow, now);
    assert_eq!(
        custody(&glow),
        (usize::from(CELLS), 0, 0),
        "identical text lifts every stamp"
    );
    assert!((COL0..COL0 + CELLS).all(|col| {
        glow.v2_ribbon()
            .expect("restored ribbon")
            .field_at(ROW, col)
            .is_some()
    }));
    samples.sort_unstable();
    let median = samples[ITERATIONS / 2];
    let p90 = samples[ITERATIONS * 9 / 10];
    println!(
        "bench_cursor_rainbow_populated_partial_custody (informational): \
         producer median {median:?} (p90 {p90:?}); {ITERATIONS} measured / \
         {WARM_FRAMES} warm frames, {CELLS} resident cells, {RELEASES} released cells / \
         {RELEASES} distinct stamps on every measured frame; {CELLS} cells restored"
    );
}
