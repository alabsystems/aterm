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

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};
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

criterion_group!(benches, end_to_end);
criterion_main!(benches);
