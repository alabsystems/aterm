// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Criterion benchmark for full-history sequential walks — the ST-6 pricing
//! instrument.
//!
//! Bulk consumers (checkpoint snapshots, reflow input, search feeding) walk
//! all N lines oldest→newest. Routed through the random-access `get_line`,
//! every line paid an O(log P) binary search, a cache probe, and a full
//! `Line` clone; the streaming iterator decodes each block/page once and
//! MOVES its lines out. This workload prices exactly that difference and
//! doubles as the flatness fence: per-line cost must not grow with depth.

use aterm_scrollback::Scrollback;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use std::time::Duration;

const HOT_LIMIT: usize = 1_000;
const WARM_LIMIT: usize = 10_000;
const MEMORY_BUDGET: usize = 400 * 1024 * 1024;
const BLOCK_SIZE: usize = 64;
const PAYLOAD_LEN: usize = 120;

fn build_scrollback(prefill: usize) -> Scrollback {
    let payload = "x".repeat(PAYLOAD_LEN);
    let mut sb = Scrollback::with_block_size(HOT_LIMIT, WARM_LIMIT, MEMORY_BUDGET, BLOCK_SIZE);
    // Deep histories are the point: lift the default 100k line cap.
    sb.set_line_limit(None);
    for i in 0..prefill {
        sb.push_str(&format!("L{i:07}-{payload}"));
    }
    assert_eq!(sb.line_count(), prefill, "prefill must be fully retained");
    assert!(sb.cold_line_count() > 0, "walk must cross the cold tier");
    sb
}

/// Iterate the complete history and fold line lengths (defeats dead-code
/// elimination while staying allocation-free per line on the streaming path).
fn bench_iterate_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("scrollback_iterate_all");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));
    group.warm_up_time(Duration::from_secs(1));

    for prefill in [50_000_usize, 200_000] {
        group.throughput(Throughput::Elements(prefill as u64));
        let sb = build_scrollback(prefill);
        group.bench_with_input(BenchmarkId::from_parameter(prefill), &prefill, |b, _| {
            b.iter(|| {
                let (count, total_len) = sb.iter().fold((0usize, 0usize), |(c, t), line| {
                    (c + 1, t.wrapping_add(line.to_string().len()))
                });
                assert_eq!(count, sb.line_count(), "walk must be complete");
                black_box(total_len)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_iterate_all);
criterion_main!(benches);
