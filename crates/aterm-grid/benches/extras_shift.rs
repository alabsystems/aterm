// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Criterion benchmarks for CellExtras shift operations (#4542).
//!
//! Proves that full-screen scroll uses amortized O(1) via row_offset,
//! while partial-screen (region) scroll pays O(E) drain-rebuild.
//!
//! Run: cargo bench -p aterm-grid --bench extras_shift

use aterm_grid::extra::{CellCoord, CellExtra};
use aterm_grid::extra_collection::CellExtras;
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};

/// Populate extras with `count` entries across `rows` rows.
fn seed_extras(rows: u16, cols_per_row: u16) -> CellExtras {
    let mut extras = CellExtras::default();
    for row in 0..rows {
        for col in 0..cols_per_row {
            let mut extra = CellExtra::default();
            extra.set_fg_rgb(Some([0xFF, row as u8, col as u8]));
            extras.set(CellCoord::new(row, col), extra);
        }
    }
    extras
}

/// Benchmark full-screen shift_rows_up_by(0, 1) — the amortized O(1) path.
///
/// This is the hot path during normal terminal output scrolling. With
/// row_offset amortization (#4542), this increments a counter instead of
/// drain-rebuilding the entire HashMap.
fn bench_full_screen_scroll(c: &mut Criterion) {
    let mut group = c.benchmark_group("extras_shift/full_screen");

    let sizes: [(u16, u16); 3] = [(24, 10), (24, 40), (200, 40)];

    for (rows, cols) in sizes {
        let entry_count = u64::from(rows) * u64::from(cols);
        group.throughput(Throughput::Elements(entry_count));

        group.bench_with_input(
            BenchmarkId::new("amortized_o1", format!("{rows}r_{cols}c")),
            &(rows, cols),
            |b, &(rows, cols)| {
                b.iter_batched(
                    || seed_extras(rows, cols),
                    |mut extras| {
                        // 256 scrolls: all amortized O(1) via row_offset.
                        // One compaction at threshold.
                        for _ in 0..256 {
                            extras.shift_rows_up_by(0, 1);
                        }
                        black_box(extras.len())
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark partial-screen shift_rows_up_by(start > 0, 1) — the O(E) path.
///
/// This is the region-scroll path (DECSTBM scroll margins). Each call does
/// a full drain-rebuild of the HashMap. Measured as a baseline to compare
/// against the amortized full-screen path.
fn bench_region_scroll(c: &mut Criterion) {
    let mut group = c.benchmark_group("extras_shift/region_scroll");

    let sizes: [(u16, u16); 3] = [(24, 10), (24, 40), (200, 40)];

    for (rows, cols) in sizes {
        let entry_count = u64::from(rows) * u64::from(cols);
        group.throughput(Throughput::Elements(entry_count));

        group.bench_with_input(
            BenchmarkId::new("drain_rebuild", format!("{rows}r_{cols}c")),
            &(rows, cols),
            |b, &(rows, cols)| {
                b.iter_batched(
                    || seed_extras(rows, cols),
                    |mut extras| {
                        // 256 scrolls: each one is O(E) drain-rebuild.
                        for _ in 0..256 {
                            extras.shift_rows_up_by(1, 1);
                        }
                        black_box(extras.len())
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark shift_cols_right — single-row ICH operation, O(E) drain-rebuild.
///
/// Measures the cost of Insert Character (ICH), which currently does a
/// full HashMap drain-rebuild even though only one row's columns shift.
fn bench_col_shift(c: &mut Criterion) {
    let mut group = c.benchmark_group("extras_shift/col_shift");

    let sizes: [(u16, u16); 2] = [(24, 40), (200, 40)];

    for (rows, cols) in sizes {
        let entry_count = u64::from(rows) * u64::from(cols);
        group.throughput(Throughput::Elements(entry_count));

        group.bench_with_input(
            BenchmarkId::new("shift_cols_right", format!("{rows}r_{cols}c")),
            &(rows, cols),
            |b, &(rows, cols)| {
                b.iter_batched(
                    || seed_extras(rows, cols),
                    |mut extras| {
                        extras.shift_cols_right(rows / 2, 10, 5, cols);
                        black_box(extras.len())
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_full_screen_scroll,
    bench_region_scroll,
    bench_col_shift,
);
criterion_main!(benches);
