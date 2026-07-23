// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig, ScrollbackStorage};
use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::time::Duration;

const BLOCK_SIZE: usize = 10;
const HOT_LIMIT: usize = 10;
const PAYLOAD_LEN: usize = 128;
const REMOVE_COUNT: usize = HOT_LIMIT + 1;
const WARM_LIMIT: usize = 100;

fn build_fixture(total_lines: usize) -> (aterm_tempfile::TempDir, ScrollbackStorage) {
    let dir = aterm_tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("remove-newest-bench.dtrm");
    let payload = "x".repeat(PAYLOAD_LEN);
    let config = DiskBackedScrollbackConfig::new(&path)
        .with_hot_limit(HOT_LIMIT)
        .with_warm_limit(WARM_LIMIT)
        .with_block_size(BLOCK_SIZE);
    let mut sb = DiskBackedScrollback::with_config(config).expect("fixture should build");
    for i in 0..total_lines {
        sb.push_str(&format!("Line {i:06}-{payload}"))
            .expect("fixture push should succeed");
    }
    (dir, ScrollbackStorage::from(sb))
}

fn bench_disk_backed_remove_newest_cross_tier(c: &mut Criterion) {
    let mut group = c.benchmark_group("disk_backed_remove_newest_cross_tier");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    group.warm_up_time(Duration::from_secs(1));

    for total_lines in [10_000usize, 100_000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(total_lines),
            &total_lines,
            |b, &total_lines| {
                b.iter_batched(
                    || build_fixture(total_lines),
                    |(_dir, mut sb)| {
                        sb.remove_newest(black_box(REMOVE_COUNT))
                            .expect("benchmark remove_newest should succeed");
                        black_box(sb.line_count())
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_disk_backed_remove_newest_cross_tier);
criterion_main!(benches);
