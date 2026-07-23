// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-pixel FIRE FIELD render cost gate — the half `cursor_bench` never
//! measured. `draw_fire_patch` evaluates [`fire_field_add`]/[`fire_field_over`]
//! at EVERY covered device pixel, EVERY frame, for the whole blaze lifetime, so
//! a worst-case (12-cell, full-height, full-blaze) frame is the real hot loop.
//!
//! ```sh
//! cargo test -p aterm-render --release --test fire_bench -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use aterm_render::fire_field::{
    FireFieldParams, FireRow, fire_precomp, fire_shade_add, fire_shade_over, fire_top_fade,
};

const ITERATIONS: usize = 200;

/// One worst-case blaze frame in ONE mode: the 12 hottest cells (the
/// `emit_flames` cap), each a full-height (`~4·cell_h`) tongue rooted mid-screen,
/// rasterised EXACTLY like `draw_fire_patch` — the patch precompute hoisted once
/// per cell, the top-fade once per row, a single field call per covered pixel.
/// A real frame is one mode (Add on dark themes, Over on light), never both, so
/// timing modes separately is the true single-frame cost. Returns a non-zero
/// accumulator so the optimiser can't elide the loop.
fn render_frame(cw: i32, ch: i32, phase: u32, over: bool) -> u64 {
    const CELLS: i32 = 12;
    let peak = ch * 4; // full blaze (0.55 + 3.60·1²)·ch ≈ 4·ch
    let row = 20; // mid-screen root
    let base_y = row * ch;
    let lean = -((ch as f32 * 0.30) as i32) * 4; // full-rise lean
    let mut acc: u64 = 0;
    for cell in 0..CELLS {
        let p = FireFieldParams {
            base_y,
            peak_h: peak,
            phase,
            temp: 255,
            strength: 235,
            lean,
            cov_cap: 44,
            cell_h: ch,
            top_fade_y: 0,
        };
        let pc = fire_precomp(&p);
        let lean_px = lean.abs() / 4 + 1;
        let x0 = cell * cw - lean_px;
        let x1 = (cell + 1) * cw + lean_px;
        let y0 = base_y - peak - 2;
        // Sweep each scanline with the incremental FireRow sampler — EXACTLY the
        // draw_fire_patch hot loop, so the bench tracks the real render cost.
        for y in y0..base_y {
            let tf = fire_top_fade(y, &p);
            let mut sampler = FireRow::new(y, x0, &p, &pc);
            for x in x0..x1 {
                let c = sampler.core(x);
                if over {
                    let (rgb, a) = fire_shade_over(&c, &p, tf);
                    acc = acc.wrapping_add(u64::from(rgb) ^ u64::from(a));
                } else {
                    acc = acc.wrapping_add(u64::from(fire_shade_add(&c, &p, tf)));
                }
            }
        }
    }
    acc
}

fn median_frame(cw: i32, ch: i32, over: bool) -> Duration {
    let mut samples = Vec::with_capacity(ITERATIONS);
    let mut sink: u64 = 0;
    for i in 0..ITERATIONS {
        let start = Instant::now();
        sink = sink.wrapping_add(render_frame(cw, ch, 4096 + i as u32 * 97, over));
        samples.push(start.elapsed());
    }
    assert_ne!(sink, 0);
    samples.sort_unstable();
    samples[ITERATIONS / 2]
}

fn bench(cw: i32, ch: i32, label: &str) {
    // Prove the frame actually lights (non-zero work), each mode.
    assert_ne!(
        render_frame(cw, ch, 4096, false),
        0,
        "{label}: Add must light"
    );
    assert_ne!(
        render_frame(cw, ch, 4096, true),
        0,
        "{label}: Over must light"
    );
    let add = median_frame(cw, ch, false);
    let over = median_frame(cw, ch, true);
    let median = add.max(over); // the worst single real frame is one mode
    let px = 12 * (cw + (((ch as f32 * 0.30) as i32) * 4).abs() / 4 * 2 + 2) * (ch * 4 + 2);
    println!(
        "bench_fire_render_{label}: median {median:?} (add {add:?} / over {over:?}) over ~{px} px/frame"
    );
    // A 12-cell full blaze must render well inside a 60fps frame's compose slice.
    // Generous ceiling (it runs on the UI thread before present); the point is a
    // REGRESSION GATE with a real number, not a tight bound.
    assert!(
        median < Duration::from_millis(3),
        "worst-case {label} fire render frame {median:?} >= 3 ms"
    );
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_fire_render_small() {
    bench(10, 20, "cw10_ch20");
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_fire_render_medium() {
    bench(16, 32, "cw16_ch32");
}

#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_fire_render_retina() {
    bench(20, 40, "cw20_ch40");
}
