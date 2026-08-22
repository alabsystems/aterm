// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Criterion benchmark for `truncate` throughput.
//!
//! Measures the cost of cross-tier truncation at varying scrollback depths.
//! The current implementation decompresses a warm-tier boundary block (LZ4)
//! when truncation spans the warm tier. The theme goal is to keep tier
//! structure intact during truncate, avoiding unnecessary decompression.
//!
//! Run with: cargo bench --package aterm-scrollback --bench truncate

use aterm_scrollback::Scrollback;
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use std::time::Duration;

const HOT_LIMIT: usize = 1_000;
const WARM_LIMIT: usize = 10_000;
const MEMORY_BUDGET: usize = 100 * 1024 * 1024; // 100 MB
const BLOCK_SIZE: usize = 64;
const PAYLOAD_LEN: usize = 120;

/// Build a scrollback pre-filled to `prefill` lines.
fn build_scrollback(prefill: usize) -> Scrollback {
    let payload = "x".repeat(PAYLOAD_LEN);
    let mut sb = Scrollback::with_block_size(HOT_LIMIT, WARM_LIMIT, MEMORY_BUDGET, BLOCK_SIZE);
    for i in 0..prefill {
        sb.push_str(&format!("L{i:06}-{payload}"));
    }
    sb
}

/// Benchmark: truncate removing lines only from the cold tier.
///
/// Cold truncation uses O(1) front_offset adjustment with no decompression.
/// This is the fast path — establishing a baseline for cross-tier comparison.
fn bench_truncate_cold_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncate_cold_only");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    for prefill in [20_000_usize, 50_000] {
        // Remove 500 lines from cold tier (well within cold capacity).
        let remove_count = 500_usize;
        let keep = prefill - remove_count;
        group.throughput(Throughput::Elements(remove_count as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(prefill),
            &prefill,
            |b, &prefill| {
                b.iter_batched(
                    || build_scrollback(prefill),
                    |mut sb| {
                        sb.truncate(black_box(keep))
                            .expect("cold-only truncate should succeed");
                        black_box(sb.line_count())
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: truncate spanning cold + warm tiers (cross-tier).
///
/// Since the `front_offset` optimization, cross-tier truncation is O(1) per
/// tier — no decompression is needed. This benchmark verifies that cross-tier
/// truncation cost is independent of data volume.
fn bench_truncate_cross_tier(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncate_cross_tier");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    for prefill in [20_000_usize, 50_000] {
        // Remove enough lines to exhaust cold and cut into warm tier.
        // With HOT_LIMIT=1000, WARM_LIMIT=10000, BLOCK_SIZE=64:
        //   At 50K lines: cold ~= 39K, warm ~= 10K, hot ~= 1K
        //   Cross-tier truncation advances front_offset in each tier.
        group.throughput(Throughput::Elements(1));

        group.bench_with_input(
            BenchmarkId::from_parameter(prefill),
            &prefill,
            |b, &prefill| {
                b.iter_batched(
                    || {
                        let sb = build_scrollback(prefill);
                        let cold = sb.cold_line_count();
                        let warm_cut = 500.min(sb.warm_line_count());
                        let keep = prefill - cold - warm_cut;
                        (sb, keep)
                    },
                    |(mut sb, keep)| {
                        sb.truncate(black_box(keep))
                            .expect("cross-tier truncate should succeed");
                        black_box(sb.line_count())
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: truncate scaling — same fraction removed at different depths.
///
/// Removes 10% of lines at each depth. If truncate is O(tiers) not O(lines),
/// cost should be roughly constant regardless of total line count.
fn bench_truncate_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncate_scaling");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    for prefill in [5_000_usize, 20_000, 50_000, 100_000] {
        let keep = prefill * 9 / 10; // Keep 90%, remove 10%
        let remove_count = prefill - keep;
        group.throughput(Throughput::Elements(remove_count as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(prefill),
            &prefill,
            |b, &prefill| {
                b.iter_batched(
                    || build_scrollback(prefill),
                    |mut sb| {
                        sb.truncate(black_box(keep))
                            .expect("scaling truncate should succeed");
                        black_box(sb.line_count())
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark: push + truncate cycle (line_limit enforcement path).
///
/// Simulates the hot path where `push_line` triggers `truncate` on every
/// batch because a `line_limit` is set. This is the real-world pattern
/// for bounded scrollback.
fn bench_push_truncate_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("push_truncate_cycle");
    let pushes_per_iter = 500_usize;
    group.throughput(Throughput::Elements(pushes_per_iter as u64));
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    let payload = "x".repeat(PAYLOAD_LEN);

    for prefill in [10_000_usize, 50_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(prefill),
            &prefill,
            |b, &prefill| {
                b.iter_batched_ref(
                    || {
                        let mut sb = build_scrollback(prefill);
                        sb.set_line_limit(Some(prefill));
                        sb
                    },
                    |sb| {
                        for i in 0..pushes_per_iter {
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

/// Benchmark: deep-history push + truncate cycle (ST-3 pricing instrument).
///
/// Same steady-state rotation as `push_truncate_cycle`, but at depths where
/// the cold tier holds thousands of pages (~15.5k at 1M lines with the 64-line
/// block size). Before the base-offset redesign, every 100-line block rotation
/// drained AND rebased the whole cold cumulative index — an O(total pages)
/// memmove + rewrite per drop that the 50k-line shallow fence cannot see. The
/// flat-throughput claim of `push_line` is the fence: throughput here must
/// stay level from 200k to 1M prefill, not degrade with depth.
///
/// The store is built ONCE per depth and reused across sampling iterations:
/// the workload is a steady-state rotation (mutation between samples is the
/// point), and a per-batch million-line rebuild would dominate wall time.
fn bench_push_truncate_cycle_deep(c: &mut Criterion) {
    let mut group = c.benchmark_group("push_truncate_cycle_deep");
    let pushes_per_iter = 500_usize;
    group.throughput(Throughput::Elements(pushes_per_iter as u64));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    let payload = "x".repeat(PAYLOAD_LEN);

    for prefill in [200_000_usize, 1_000_000] {
        let mut sb = build_scrollback(prefill);
        sb.set_line_limit(Some(prefill));
        assert!(
            sb.cold_line_count() > 0,
            "deep rotation must reach the cold tier or it prices nothing"
        );
        group.bench_with_input(BenchmarkId::from_parameter(prefill), &prefill, |b, _| {
            b.iter(|| {
                for i in 0..pushes_per_iter {
                    sb.push_str(&format!("P{i:04}-{}", payload));
                }
                black_box(sb.line_count())
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_truncate_cold_only,
    bench_truncate_cross_tier,
    bench_truncate_scaling,
    bench_push_truncate_cycle,
    bench_push_truncate_cycle_deep,
);
criterion_main!(benches);
