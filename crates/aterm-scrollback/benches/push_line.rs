// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Criterion benchmark for `push_line` throughput.
//!
//! Validates that `push_line` is O(1) amortized in release builds by measuring
//! throughput at varying scrollback depths. The incremental memory tracking
//! (running totals updated on push/evict) should keep per-push cost constant
//! regardless of total line count.

use aterm_scrollback::Scrollback;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use std::time::Duration;

const HOT_LIMIT: usize = 1_000;
const WARM_LIMIT: usize = 10_000;
const MEMORY_BUDGET: usize = 100 * 1024 * 1024; // 100 MB
const BLOCK_SIZE: usize = 64;
const PAYLOAD_LEN: usize = 120;
const PUSHES_PER_ITER: usize = 500;

/// Build a scrollback pre-filled to `prefill` lines.
fn build_scrollback(prefill: usize) -> Scrollback {
    let payload = "x".repeat(PAYLOAD_LEN);
    let mut sb = Scrollback::with_block_size(HOT_LIMIT, WARM_LIMIT, MEMORY_BUDGET, BLOCK_SIZE);
    for i in 0..prefill {
        sb.push_str(&format!("L{i:06}-{payload}"));
    }
    sb
}

/// Benchmark: push_line throughput at varying scrollback depths.
///
/// If push_line is truly O(1) amortized, throughput should be roughly constant
/// across prefill sizes (500, 5K, 50K lines).
fn bench_push_line_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("push_line_throughput");
    group.throughput(Throughput::Elements(PUSHES_PER_ITER as u64));
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    let payload = "x".repeat(PAYLOAD_LEN);

    for prefill in [500_usize, 5_000, 50_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(prefill),
            &prefill,
            |b, &prefill| {
                b.iter_batched_ref(
                    || build_scrollback(prefill),
                    |sb| {
                        for i in 0..PUSHES_PER_ITER {
                            sb.push_str(&format!("P{i:04}-{}", payload));
                        }
                        black_box(sb.line_count())
                    },
                    criterion::BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: push_line under memory pressure (eviction path).
///
/// Uses a small memory budget to force warm→cold eviction on every push batch.
/// Measures the overhead of the eviction path vs. the non-pressure path.
fn bench_push_line_memory_pressure(c: &mut Criterion) {
    let mut group = c.benchmark_group("push_line_memory_pressure");
    group.throughput(Throughput::Elements(PUSHES_PER_ITER as u64));
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    let payload = "x".repeat(PAYLOAD_LEN);
    // Small budget forces frequent eviction.
    let small_budget = 512 * 1024; // 512 KB

    for prefill in [500_usize, 5_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(prefill),
            &prefill,
            |b, &prefill| {
                b.iter_batched_ref(
                    || {
                        let mut sb = Scrollback::with_block_size(
                            HOT_LIMIT,
                            WARM_LIMIT,
                            small_budget,
                            BLOCK_SIZE,
                        );
                        let payload_inner = "x".repeat(PAYLOAD_LEN);
                        for i in 0..prefill {
                            sb.push_str(&format!("L{i:06}-{payload_inner}"));
                        }
                        sb
                    },
                    |sb| {
                        for i in 0..PUSHES_PER_ITER {
                            sb.push_str(&format!("P{i:04}-{}", payload));
                        }
                        black_box(sb.line_count())
                    },
                    criterion::BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_push_line_throughput,
    bench_push_line_memory_pressure,
);
criterion_main!(benches);
