// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WF-1 frame-gate pricing: what a web host's rAF loop pays per tick.
//!
//! The gate's claim is that a `render()` whose inputs are all unchanged can be
//! skipped entirely — no snapshot refill, no row diff, no cache clone, no
//! raster. This bench prices that against the ungated loop by measuring the
//! SAME source under two builds (A/B by extracting: the gated `aterm-wasm`,
//! then `git checkout -- crates/aterm-wasm/src/{lib,effects_api}.rs` for the
//! BEFORE arm), so it deliberately calls only APIs that exist in BOTH — no
//! `last_render_skipped`/`needs_frame` here.
//!
//! Three workloads, because a gate that only wins on a dead terminal is not
//! worth shipping:
//! - `settled`: the idle rAF tick (pump the effects clock, render). The gate's
//!   target case, and the overwhelmingly common one for a terminal a human is
//!   reading rather than typing into.
//! - `keystroke`: one echoed byte per tick. The gate can NEVER fire here, so
//!   this arm measures its pure OVERHEAD — the number that decides whether the
//!   skip is free or merely cheap.
//! - `mixed`: one byte every 8th tick (a realistic human typing cadence at
//!   60Hz), the blend that decides the real-world verdict.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use aterm_wasm::AtermTerminal;

/// A terminal with a screenful of real content, rendered once so the renderer's
/// caches are warm and the first-frame cost is out of the measurement.
fn warm_term(rows: usize, cols: usize) -> Option<AtermTerminal> {
    let mut t = AtermTerminal::new_from_system(rows as u16, cols as u16, 14.0)?;
    for r in 0..rows {
        t.process(format!("line {r} \x1b[3{}mcolored\x1b[0m text here\r\n", r % 8).as_bytes());
    }
    t.render();
    Some(t)
}

fn wasm_frame_gate(c: &mut Criterion) {
    let sizes = [(24usize, 80usize), (50, 200)];
    let mut g = c.benchmark_group("wasm_frame_gate");

    for (rows, cols) in sizes {
        let label = format!("{rows}x{cols}");

        // 1) SETTLED: the idle rAF tick. Nothing changes between frames.
        if let Some(mut t) = warm_term(rows, cols) {
            t.render(); // settle: this frame records the gate key
            g.bench_function(BenchmarkId::new("settled", &label), |b| {
                b.iter(|| {
                    t.advance_effects(black_box(16.0));
                    t.render();
                    black_box(t.width());
                });
            });
        }

        // 2) KEYSTROKE: a byte every tick — the gate can never fire. Pure
        //    overhead arm. Alternating glyph so the write is real damage, and
        //    `\r` keeps the cursor on one row (no wrap/scroll).
        if let Some(mut t) = warm_term(rows, cols) {
            let mut tick = 0u8;
            g.bench_function(BenchmarkId::new("keystroke", &label), |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    t.process(if tick.is_multiple_of(2) {
                        b"\rx"
                    } else {
                        b"\ry"
                    });
                    t.advance_effects(black_box(16.0));
                    t.render();
                    black_box(t.width());
                });
            });
        }

        // 3) MIXED: a byte every 8th tick — human typing against a 60Hz loop.
        if let Some(mut t) = warm_term(rows, cols) {
            let mut tick = 0u32;
            g.bench_function(BenchmarkId::new("mixed_1in8", &label), |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    if tick.is_multiple_of(8) {
                        t.process(if tick.is_multiple_of(16) {
                            b"\rx"
                        } else {
                            b"\ry"
                        });
                    }
                    t.advance_effects(black_box(16.0));
                    t.render();
                    black_box(t.width());
                });
            });
        }
    }
    g.finish();
}

criterion_group!(benches, wasm_frame_gate);
criterion_main!(benches);
