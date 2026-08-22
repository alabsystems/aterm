// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Criterion benchmark for disk-backed `push_line` — the ST-1 pricing
//! instrument.
//!
//! The three memory-only scrollback benches (`push_line`, `truncate`,
//! `remove_newest`'s fixture aside) never priced the disk cold tier's append
//! path, which historically paid two `sync_data()` calls plus a whole-file
//! munmap/re-mmap per 100-line page — cost scaling with output rate and total
//! file size on the ingest path. Two workloads:
//!
//! - `disk_push_line_steady_growth`: unlimited-line store deep enough that
//!   every `block_size` pushes rotate a page into the cold file. Prices the
//!   pure append path (fsync policy + remap elimination); throughput should
//!   be flat across prefill depths.
//! - `disk_push_line_rotation`: `line_limit == prefill`, so every push also
//!   truncates the front — the steady state of a capped session. Reaches cold
//!   front-truncation and, across enough rotations, the dead>live compaction
//!   trigger, so regressions in either land in this number.
//!
//! Run with:
//!   cargo bench -p aterm-scrollback --bench disk_push_line --features disk-tier

use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig};
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use std::time::Duration;

const HOT_LIMIT: usize = 200;
const WARM_LIMIT: usize = 2_000;
const BLOCK_SIZE: usize = 100;
const PAYLOAD_LEN: usize = 120;
const PUSHES_PER_ITER: usize = 500;

/// Build a disk-backed store prefilled deep enough that pushes rotate pages
/// into the cold file. `limit: None` = unlimited (growth workload).
fn build(
    dir: &aterm_tempfile::TempDir,
    name: &str,
    prefill: usize,
    limit: Option<usize>,
) -> DiskBackedScrollback {
    let path = dir.path().join(format!("{name}.dtrm"));
    let mut config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(HOT_LIMIT)
        .with_warm_limit(WARM_LIMIT)
        .with_block_size(BLOCK_SIZE);
    config = match limit {
        Some(l) => config.with_line_limit(l),
        None => config.with_unlimited_lines(),
    };
    let mut sb = DiskBackedScrollback::with_config(config).expect("bench store should build");
    let payload = "x".repeat(PAYLOAD_LEN);
    for i in 0..prefill {
        sb.push_str(&format!("L{i:07}-{payload}"))
            .expect("prefill push should succeed");
    }
    assert!(
        sb.cold_line_count() > 0,
        "workload must reach the disk cold tier or it prices nothing"
    );
    sb
}

/// Pure append path: unlimited store, steady growth. Setup once per depth and
/// measure the steady state (the store keeps growing across samples, which is
/// exactly the regime where per-append remap cost would scale with file size).
fn bench_disk_push_line_steady_growth(c: &mut Criterion) {
    let mut group = c.benchmark_group("disk_push_line_steady_growth");
    group.throughput(Throughput::Elements(PUSHES_PER_ITER as u64));
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    let payload = "x".repeat(PAYLOAD_LEN);
    for prefill in [20_000_usize, 100_000] {
        let dir = aterm_tempfile::tempdir().expect("tempdir should succeed");
        let mut sb = build(&dir, "growth", prefill, None);
        group.bench_with_input(BenchmarkId::from_parameter(prefill), &prefill, |b, _| {
            b.iter(|| {
                for i in 0..PUSHES_PER_ITER {
                    sb.push_str(&format!("P{i:04}-{payload}"))
                        .expect("bench push should succeed");
                }
                black_box(sb.line_count())
            });
        });
    }

    group.finish();
}

/// Capped-session steady state: every push truncates the oldest line, so the
/// cold tier front-rotates and periodically crosses the dead>live compaction
/// trigger — the full ingest-path cost stack of a long-lived limited session.
fn bench_disk_push_line_rotation(c: &mut Criterion) {
    let mut group = c.benchmark_group("disk_push_line_rotation");
    group.throughput(Throughput::Elements(PUSHES_PER_ITER as u64));
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    let payload = "x".repeat(PAYLOAD_LEN);
    for prefill in [20_000_usize, 100_000] {
        let dir = aterm_tempfile::tempdir().expect("tempdir should succeed");
        let mut sb = build(&dir, "rotation", prefill, Some(prefill));
        group.bench_with_input(BenchmarkId::from_parameter(prefill), &prefill, |b, _| {
            b.iter(|| {
                for i in 0..PUSHES_PER_ITER {
                    sb.push_str(&format!("P{i:04}-{payload}"))
                        .expect("bench push should succeed");
                }
                black_box(sb.line_count())
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_disk_push_line_steady_growth,
    bench_disk_push_line_rotation,
);
criterion_main!(benches);
