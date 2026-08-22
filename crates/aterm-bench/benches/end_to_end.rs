// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// End-to-end SOFTWARE pipeline cost: bytes -> grid -> rasterized frame, on the
// CPU rasterizer (deterministic, no GPU/readback). Two views:
//   - cpu_render_frame:    ms/frame to rasterize a busy grid (the "to pixels" half).
//   - process_plus_render: throughput of (process a 64 KiB output chunk + render
//                          one frame) — the per-refresh work when output arrives.
// Complements `comparative` (engine only) and `aterm-gpu/gpu_frame` (GPU render).
// NOT a competitor comparison: other terminals' renderers are not in-process
// libraries, so an apples-to-apples end-to-end comparison needs an external
// app harness (out of scope here). Skips cleanly when no system font is found.
//   cargo bench -p aterm-bench --bench end_to_end

use aterm_core::render::{FrameRefill, RenderInput};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

/// A busy grid: every cell filled with cycling glyphs + a colour run per row.
fn busy_term(rows: usize, cols: usize) -> Terminal {
    let mut t = Terminal::new(rows as u16, cols as u16);
    let alpha = b"abcdefghijklmnopqrstuvwxyz0123456789 ";
    let mut line = Vec::with_capacity(cols + 16);
    for r in 0..rows {
        line.clear();
        line.extend_from_slice(b"\x1b[3");
        line.push(b'1' + (r % 6) as u8);
        line.push(b'm');
        for c in 0..cols {
            line.push(alpha[(r + c) % alpha.len()]);
        }
        line.extend_from_slice(b"\x1b[0m\r\n");
        t.process(&line);
    }
    t
}

/// ~64 KiB of realistic shell output (prompt + coloured ls + plain text).
fn corpus_64k() -> Vec<u8> {
    let unit = b"\x1b[1;32muser@host\x1b[0m:\x1b[34m~/src\x1b[0m$ ls -la\r\n\x1b[34mdrwxr-xr-x\x1b[0m  src  file.txt  12345 bytes\r\n";
    let mut v = Vec::with_capacity(64 * 1024 + unit.len());
    while v.len() < 64 * 1024 {
        v.extend_from_slice(unit);
    }
    v
}

fn end_to_end(c: &mut Criterion) {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font; CPU end-to-end bench not run");
        return;
    };
    let sizes = [(24usize, 80usize), (50, 200)];

    // --- to-pixels half: rasterize an already-populated grid ---
    let mut g = c.benchmark_group("cpu_render_frame");
    for (rows, cols) in sizes {
        let mut term = busy_term(rows, cols);
        // A-3: the engine builds the snapshot; the bench times the rasterize, so
        // build the (unchanging) snapshot once outside the measured loop.
        let input = term.cell_frame(rows, cols);
        g.bench_function(BenchmarkId::from_parameter(format!("{rows}x{cols}")), |b| {
            b.iter(|| {
                let f = r.render_input(black_box(&input));
                black_box(f.pixels.len());
            });
        });
    }
    g.finish();

    // --- full per-refresh pipeline: process 64 KiB + render one frame ---
    let corpus = corpus_64k();
    let mut g = c.benchmark_group("process_plus_render");
    g.throughput(Throughput::Bytes(corpus.len() as u64));
    for (rows, cols) in sizes {
        let mut term = Terminal::new(rows as u16, cols as u16);
        g.bench_function(BenchmarkId::from_parameter(format!("{rows}x{cols}")), |b| {
            b.iter(|| {
                term.process(black_box(&corpus));
                let f = r.render_input(&term.cell_frame(rows, cols));
                black_box(f.pixels.len());
            });
        });
    }
    g.finish();
}

/// DMG-1/WF-1 pricing: the DAMAGE-LIGHT per-tick pipeline — one echoed
/// keystroke between frames (not a 64 KiB feed), through the PERSISTENT
/// damage-tracked raster (`WindowCpu` + `render_input_cached`, the shipping
/// cached path — NOT `render_input`, whose throwaway cache full-repaints).
/// Two arms per size so the boundary cost is priced A/B on identical work:
///
/// - `full_extract`: `cell_frame_into` + `take_damage` (the historical
///   unconditional O(rows x cols) engine resolve per tick).
/// - `scoped_extract`: `cell_frame_damage_scoped_into` (the DMG-1 carrier:
///   only the damaged row re-resolves; fill-and-consume).
///
/// Everything downstream (dirty diff, cache clone, dirty-row raster) is
/// IDENTICAL between arms, so the delta isolates the extraction boundary.
/// Sizes include 135x480 (~4K fullscreen, ~65k cells) — the regime where the
/// per-tick full-grid resolve dominates a one-row change.
///
/// TWO-SIDED REACH GUARDS (asserted in setup, so a silent degradation to the
/// full arm fails the bench run rather than quietly mis-pricing): the scoped
/// arm's warmup asserts the echo tick actually takes `FrameRefill::Scoped`,
/// and the first fill asserts `Full`. The content-equality of the two arms is
/// pinned by aterm-core's
/// `damage_scoped_extraction_matches_full_extract_over_mutation_corpus`.
/// One keystroke-echo tick: overwrite column 0 of the bottom row with an
/// ALTERNATING glyph — real damage every tick (the byte changes), exactly one
/// damaged row, and NO wrap/scroll, so the scoped arm's continuity holds every
/// iteration (a wrap would advance base_y and honestly force the full arm).
fn echo_tick(term: &mut Terminal, tick: &mut u8) {
    *tick = tick.wrapping_add(1);
    term.process(if tick.is_multiple_of(2) {
        b"\rx"
    } else {
        b"\ry"
    });
}

fn keystroke_tick(c: &mut Criterion) {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font; keystroke_tick bench not run");
        return;
    };
    let sizes = [(24usize, 80usize), (50, 200), (135, 480)];
    let mut g = c.benchmark_group("keystroke_tick");
    for (rows, cols) in sizes {
        {
            let mut term = busy_term(rows, cols);
            let mut wc = WindowCpu::new();
            let mut input = term.cell_frame(rows, cols);
            let _ = r.render_input_cached(&mut wc, &input);
            let mut tick = 0u8;
            g.bench_function(
                BenchmarkId::new("full_extract", format!("{rows}x{cols}")),
                |b| {
                    b.iter(|| {
                        echo_tick(&mut term, &mut tick);
                        term.cell_frame_into(&mut input, rows, cols);
                        term.take_damage();
                        let v = r.render_input_cached(&mut wc, black_box(&input));
                        black_box(v.width());
                    });
                },
            );
        }
        {
            let mut term = busy_term(rows, cols);
            let mut wc = WindowCpu::new();
            let mut input = RenderInput::empty();
            // Reach guard, side 1: the baseline fill is the full arm.
            let first = term.cell_frame_damage_scoped_into(&mut input, rows, cols);
            assert!(
                matches!(first, FrameRefill::Full),
                "first fill must take the full arm"
            );
            // Reach guard, side 2: the echo tick takes the SCOPED arm — the
            // path this bench exists to price.
            let mut tick = 0u8;
            echo_tick(&mut term, &mut tick);
            let warm = term.cell_frame_damage_scoped_into(&mut input, rows, cols);
            assert!(
                matches!(warm, FrameRefill::Scoped { .. }),
                "echo tick must take the scoped arm (got {warm:?})"
            );
            let _ = r.render_input_cached(&mut wc, &input);
            g.bench_function(
                BenchmarkId::new("scoped_extract", format!("{rows}x{cols}")),
                |b| {
                    b.iter(|| {
                        echo_tick(&mut term, &mut tick);
                        let refill = term.cell_frame_damage_scoped_into(&mut input, rows, cols);
                        black_box(matches!(refill, FrameRefill::Scoped { .. }));
                        let v = r.render_input_cached(&mut wc, black_box(&input));
                        black_box(v.width());
                    });
                },
            );
        }
    }
    g.finish();
}

criterion_group!(benches, end_to_end, keystroke_tick);
criterion_main!(benches);
