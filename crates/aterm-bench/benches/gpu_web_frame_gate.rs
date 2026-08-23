// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the WebGL browser backend pays per rAF tick — and what a settled-frame
//! gate would give back.
//!
//! `aterm-gpu-web` is the default browser backend on capable hardware and, at
//! the revision this bench was written against, had NO settled-frame gate: every
//! rAF tick ran the whole pump -> refill -> effects -> stamp -> spill build and
//! then an unconditional swapchain acquire + blit + present, forever, on a pane
//! where nothing changed. Its CPU twin (`aterm-wasm`) has had that gate since
//! WF-1.
//!
//! WHAT THIS BENCH DOES AND DOES NOT COVER — read before quoting a number.
//! `render()` is a `wasm32`-only export (it needs a browser canvas swapchain),
//! so this drives [`AtermGpuTerminal::render_headless`], the native seam that
//! calls the SAME gate and the SAME `build_frame` in the same order, minus the
//! GL present. So every number here is the ENGINE-SIDE half only. The GL half
//! the gate also elides — swapchain acquire, letterbox blit, submit, present —
//! cannot be measured anywhere in this repo (node has no WebGL and there is no
//! headless WebGL2 on this box), so treat these numbers as a LOWER bound on a
//! settled browser tick, never as the whole prize.
//!
//! The A/B is by BUILD (the same source against the two `aterm-gpu-web`
//! revisions), so the in-bench assertions are only the ones true of BOTH
//! builds. They are the two-sided reach guard on the interesting state:
//! `settled` asserts the visible grid is genuinely unchanged across a tick,
//! `keystroke` asserts it genuinely changed. Gate BEHAVIOUR (settled must skip
//! after the fix, must not before) is pinned by the permanent unit tests in
//! `crates/aterm-gpu-web/src/lib.rs`, which is the only place that assertion
//! can be true.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use aterm_gpu_web::AtermGpuTerminal;

/// A terminal with a screenful of real content, built once so the first-frame
/// cost is out of the measurement.
fn warm_term(rows: usize, cols: usize) -> Option<AtermGpuTerminal> {
    let mut t = AtermGpuTerminal::new_from_system(rows as u16, cols as u16, 14.0)?;
    for r in 0..rows {
        t.process(format!("line {r} \x1b[3{}mcolored\x1b[0m text here\r\n", r % 8).as_bytes());
    }
    t.render_headless();
    Some(t)
}

/// The whole visible grid as text — the cheapest thing that is genuinely a
/// function of every cell, used only by the reach guards (never in a hot loop).
fn screen(t: &AtermGpuTerminal, rows: usize, cols: usize) -> String {
    let mut out = String::with_capacity(rows * cols);
    for r in 0..rows {
        for c in 0..cols {
            out.push_str(&t.cell_text(r as u16, c as u16));
        }
        out.push('\n');
    }
    out
}

fn gpu_web_frame_gate(c: &mut Criterion) {
    let sizes = [(24usize, 80usize), (50, 200)];
    let mut g = c.benchmark_group("gpu_web_frame_gate");

    for (rows, cols) in sizes {
        let label = format!("{rows}x{cols}");

        // 1) SETTLED: the idle rAF tick. Nothing changes between frames — the
        //    gate's whole target population.
        if let Some(mut t) = warm_term(rows, cols) {
            t.render_headless(); // settle: this frame records the gate key
            let before = screen(&t, rows, cols);
            t.advance_effects(16.0);
            t.render_headless();
            // REACH GUARD (true of BOTH builds): this arm really is settled.
            assert_eq!(
                screen(&t, rows, cols),
                before,
                "settled arm must not change the grid"
            );
            g.bench_function(BenchmarkId::new("settled", &label), |b| {
                b.iter(|| {
                    t.advance_effects(black_box(16.0));
                    black_box(t.render_headless());
                });
            });
        }

        // 2) KEYSTROKE: a byte every tick — the gate can NEVER fire. The
        //    two-sided control, and the arm that prices the gate's OVERHEAD.
        if let Some(mut t) = warm_term(rows, cols) {
            let before = screen(&t, rows, cols);
            t.process(b"\rx");
            t.render_headless();
            // REACH GUARD (true of BOTH builds): this arm really does change.
            assert_ne!(
                screen(&t, rows, cols),
                before,
                "keystroke arm must change the grid"
            );
            let mut tick = 0u8;
            g.bench_function(BenchmarkId::new("keystroke", &label), |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    t.process(if tick.is_multiple_of(2) { b"\rx" } else { b"\ry" });
                    t.advance_effects(black_box(16.0));
                    black_box(t.render_headless());
                });
            });
        }

        // 3) MIXED: a byte every 8th tick — human typing against a 60Hz loop,
        //    the blend that decides the real-world verdict.
        if let Some(mut t) = warm_term(rows, cols) {
            let mut tick = 0u32;
            g.bench_function(BenchmarkId::new("mixed_1in8", &label), |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    if tick.is_multiple_of(8) {
                        t.process(if tick.is_multiple_of(16) { b"\rx" } else { b"\ry" });
                    }
                    t.advance_effects(black_box(16.0));
                    black_box(t.render_headless());
                });
            });
        }
    }
    g.finish();
}

criterion_group!(benches, gpu_web_frame_gate);
criterion_main!(benches);
