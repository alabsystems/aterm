// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::words::Platform;

const GIB: u64 = 1 << 30;

fn physical_sample() -> MemSample {
    MemSample {
        total: 32 * GIB,
        app: 12 * GIB,
        wired: 4 * GIB,
        compressed: Some(2 * GIB),
        cached: 8 * GIB,
        free: 4 * GIB,
        exact: true,
        ..MemSample::default()
    }
}

#[test]
fn physical_memory_exact_classes_and_positive_residual_conserve_capacity() {
    let classes = physical_sample().physical_classes(Some(Platform::MacOs));
    assert_eq!(classes.partition, MemoryPartition::Exact);
    assert_eq!(classes.total, 32 * GIB);
    assert_eq!(classes.counted, u128::from(30 * GIB));
    assert_eq!(classes.excess, 0);
    assert_eq!(
        classes.classes.map(|c| (c.kind, c.bytes)),
        [
            (MemoryClassKind::App, Some(12 * GIB)),
            (MemoryClassKind::Wired, Some(4 * GIB)),
            (MemoryClassKind::Compressed, Some(2 * GIB)),
            (MemoryClassKind::Cache, Some(8 * GIB)),
            (MemoryClassKind::Free, Some(4 * GIB)),
            (MemoryClassKind::Residual, Some(2 * GIB)),
        ]
    );
    assert_eq!(
        classes
            .classes
            .into_iter()
            .map(|c| u128::from(c.bytes.expect("complete exact class")))
            .sum::<u128>(),
        u128::from(classes.total)
    );
}

#[test]
fn physical_memory_available_stored_and_swap_are_not_extra_classes() {
    let mut sample = physical_sample();
    let before = sample.physical_classes(Some(Platform::MacOs));
    sample.available = u64::MAX;
    sample.stored = Some(16 * GIB);
    sample.swap_used = Some(3 * GIB);
    sample.swap_total = Some(6 * GIB);
    assert_eq!(sample.physical_classes(Some(Platform::MacOs)), before);
}

#[test]
fn physical_memory_unknown_compressed_keeps_its_bytes_unknown() {
    let mut sample = physical_sample();
    sample.compressed = None;
    let classes = sample.physical_classes(Some(Platform::MacOs));
    assert_eq!(classes.partition, MemoryPartition::Incomplete);
    assert_eq!(classes.classes[2].bytes, None);
    assert_eq!(classes.classes[5].bytes, Some(4 * GIB));
    assert_eq!(classes.counted, u128::from(28 * GIB));
    assert_eq!(classes.excess, 0);
}

#[test]
fn physical_memory_platform_and_reader_evidence_are_both_required_for_exactness() {
    let sample = physical_sample();
    let exact = sample.physical_classes(Some(Platform::MacOs));
    for platform in [Some(Platform::Linux), Some(Platform::Windows), None] {
        let classes = sample.physical_classes(platform);
        assert_eq!(classes.partition, MemoryPartition::Inexact);
        assert_eq!(classes.classes, exact.classes);
        assert_eq!(classes.counted, exact.counted);
    }
    assert_eq!(
        MemSample {
            exact: false,
            ..sample
        }
        .physical_classes(Some(Platform::MacOs))
        .partition,
        MemoryPartition::Inexact
    );
}

#[test]
fn physical_memory_oversum_preserves_the_readings_and_reports_excess() {
    let mut sample = physical_sample();
    sample.app = 64 * GIB;
    let classes = sample.physical_classes(Some(Platform::MacOs));
    assert_eq!(classes.partition, MemoryPartition::Overfull);
    assert_eq!(classes.total, 32 * GIB);
    assert_eq!(classes.classes[0].bytes, Some(64 * GIB));
    assert_eq!(classes.classes[5].bytes, Some(0));
    assert_eq!(classes.counted, u128::from(82 * GIB));
    assert_eq!(classes.excess, u128::from(50 * GIB));
}

#[test]
fn physical_memory_zero_capacity_never_establishes_a_partition() {
    for sample in [
        MemSample {
            compressed: Some(0),
            exact: true,
            ..MemSample::default()
        },
        MemSample {
            total: 0,
            ..physical_sample()
        },
    ] {
        let classes = sample.physical_classes(Some(Platform::MacOs));
        assert_eq!(classes.partition, MemoryPartition::NoCapacity);
        assert_eq!(classes.total, 0);
        assert_eq!(classes.classes[5].bytes, Some(0));
        assert_eq!(classes.excess, classes.counted);
    }
}

#[test]
fn physical_memory_sum_cannot_saturate_into_an_exact_partition() {
    let sample = MemSample {
        total: u64::MAX,
        app: u64::MAX,
        wired: 1,
        compressed: Some(0),
        exact: true,
        ..MemSample::default()
    };
    let classes = sample.physical_classes(Some(Platform::MacOs));
    assert_eq!(classes.partition, MemoryPartition::Overfull);
    assert_eq!(classes.counted, u128::from(u64::MAX) + 1);
    assert_eq!(classes.excess, 1);
    assert_eq!(classes.classes[5].bytes, Some(0));

    let classes = MemSample {
        wired: u64::MAX,
        compressed: Some(u64::MAX),
        cached: u64::MAX,
        free: u64::MAX,
        ..sample
    }
    .physical_classes(Some(Platform::MacOs));
    assert_eq!(classes.partition, MemoryPartition::Overfull);
    assert_eq!(classes.counted, 5 * u128::from(u64::MAX));
    assert_eq!(classes.excess, 4 * u128::from(u64::MAX));
}
