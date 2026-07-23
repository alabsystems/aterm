// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Criterion benchmarks for scroll damage tracking (#5742 ship-blocker #2).
//!
//! Proves that targeted row-level damage (marking only newly-exposed rows)
//! is faster than `mark_full()` for scroll operations. The renderer pays
//! O(damaged_rows) via `iter_bounds()`, so `mark_full()` forces O(all_rows)
//! iteration while targeted damage keeps it at O(scroll_delta).
//!
//! Run: cargo bench -p aterm-grid --bench scroll_damage

use aterm_grid::damage::{Damage, DamageTracker};
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use std::time::Duration;

const COLS: u16 = 120;

fn scroll_damage_criterion() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(3))
        .sample_size(200)
        .warm_up_time(Duration::from_secs(1))
}

/// Create a clean Damage tracker with partial tracking for `rows` rows.
fn fresh_damage(rows: u16) -> Damage {
    Damage::Partial(DamageTracker::new(rows))
}

/// Benchmark the cost of marking damage: mark_full() vs mark_rows() for
/// different scroll deltas on a 50-row and 200-row terminal.
///
/// mark_full() is O(1) but forces the renderer to iterate all rows.
/// mark_rows(n) is O(n) but limits renderer iteration to n rows.
fn bench_mark_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("scroll_damage/mark");

    for rows in [50u16, 200] {
        // mark_full: always O(1) regardless of terminal size
        group.throughput(Throughput::Elements(u64::from(rows)));
        group.bench_with_input(
            BenchmarkId::new("mark_full", format!("{rows}r")),
            &rows,
            |b, &rows| {
                b.iter_batched(
                    || fresh_damage(rows),
                    |mut damage| {
                        damage.mark_full();
                        black_box(&damage);
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // mark_rows for small scroll deltas (1, 3, 10 rows)
        for delta in [1u16, 3, 10] {
            group.throughput(Throughput::Elements(u64::from(delta)));
            group.bench_with_input(
                BenchmarkId::new(format!("mark_{delta}_rows"), format!("{rows}r")),
                &(rows, delta),
                |b, &(rows, delta)| {
                    b.iter_batched(
                        || fresh_damage(rows),
                        |mut damage| {
                            let start = rows.saturating_sub(delta);
                            damage.mark_rows(start, rows);
                            black_box(&damage);
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    group.finish();
}

/// Benchmark iter_bounds() cost: the price the renderer pays to find dirty rows.
///
/// This is the critical measurement: mark_full() forces the renderer to yield
/// `LineDamageBounds` for ALL rows, while targeted damage yields bounds for
/// only the scroll delta rows. The renderer then uploads only dirty row data
/// to the GPU vertex buffer.
fn bench_iterate_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("scroll_damage/iterate");

    for rows in [50u16, 200] {
        // Full damage iteration: yields all rows
        group.throughput(Throughput::Elements(u64::from(rows)));
        group.bench_with_input(
            BenchmarkId::new("full_damage", format!("{rows}r")),
            &rows,
            |b, &rows| {
                b.iter_batched(
                    || {
                        let mut damage = fresh_damage(rows);
                        damage.mark_full();
                        damage
                    },
                    |damage| {
                        let count: usize = damage.iter_bounds(rows, COLS).count();
                        black_box(count)
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // Targeted damage iteration: yields only delta rows
        for delta in [1u16, 3, 10] {
            group.throughput(Throughput::Elements(u64::from(delta)));
            group.bench_with_input(
                BenchmarkId::new(format!("targeted_{delta}_rows"), format!("{rows}r")),
                &(rows, delta),
                |b, &(rows, delta)| {
                    b.iter_batched(
                        || {
                            let mut damage = fresh_damage(rows);
                            let start = rows.saturating_sub(delta);
                            damage.mark_rows(start, rows);
                            damage
                        },
                        |damage| {
                            let count: usize = damage.iter_bounds(rows, COLS).count();
                            black_box(count)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    group.finish();
}

/// Benchmark the full mark-then-iterate cycle that the scroll+render pipeline
/// executes each frame. This is the end-to-end cost the GPU renderer pays.
///
/// Compares:
/// - mark_full() + iterate all rows (the old path)
/// - mark_rows(n) + iterate n rows (the dirty-rect path)
fn bench_mark_and_iterate(c: &mut Criterion) {
    let mut group = c.benchmark_group("scroll_damage/mark_and_iterate");

    for rows in [50u16, 200] {
        // Full damage cycle
        group.throughput(Throughput::Elements(u64::from(rows) * u64::from(COLS)));
        group.bench_with_input(
            BenchmarkId::new("full_damage", format!("{rows}r")),
            &rows,
            |b, &rows| {
                b.iter_batched(
                    || fresh_damage(rows),
                    |mut damage| {
                        damage.mark_full();
                        let count: usize = damage.iter_bounds(rows, COLS).count();
                        black_box(count)
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        // Targeted damage cycle for typical scroll deltas
        for delta in [1u16, 3, 10] {
            group.throughput(Throughput::Elements(u64::from(delta) * u64::from(COLS)));
            group.bench_with_input(
                BenchmarkId::new(format!("targeted_{delta}_rows"), format!("{rows}r")),
                &(rows, delta),
                |b, &(rows, delta)| {
                    b.iter_batched(
                        || fresh_damage(rows),
                        |mut damage| {
                            let start = rows.saturating_sub(delta);
                            damage.mark_rows(start, rows);
                            let count: usize = damage.iter_bounds(rows, COLS).count();
                            black_box(count)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = scroll_damage_criterion();
    targets = bench_mark_cost, bench_iterate_cost, bench_mark_and_iterate,
}
criterion_main!(benches);
