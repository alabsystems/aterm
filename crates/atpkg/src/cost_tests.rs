// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn human_bytes_scales_units() {
    assert_eq!(human_bytes(512), "512 B");
    assert_eq!(human_bytes(1 << 10), "1.0 KiB");
    assert_eq!(human_bytes(1 << 20), "1.0 MiB");
    assert_eq!(human_bytes(3 * (1 << 30)), "3.0 GiB");
    assert_eq!(human_bytes((3 << 30) / 2), "1.5 GiB");
    assert_eq!(human_bytes(1_310_720), "1.2 MiB");
    assert_eq!(human_bytes(u64::MAX), "17179869184.0 GiB");
}

#[test]
fn human_bytes_apart_never_prints_unequal_counts_alike() {
    let mib = 1u64 << 20;
    assert_eq!(
        human_bytes_apart(1400 * mib, 900 * mib),
        (human_bytes(1400 * mib), human_bytes(900 * mib))
    );
    assert_eq!(human_bytes(1174 * mib), human_bytes(1147 * mib));
    assert_eq!(
        human_bytes_apart(1174 * mib, 1147 * mib),
        ("1174.0 MiB".to_string(), "1147.0 MiB".to_string())
    );
    let (a, b) = human_bytes_apart(1174 * mib, 1174 * mib - 100);
    assert_eq!((a.as_str(), b.as_str()), ("1231028224 B", "1231028124 B"));
}

#[test]
fn disk_preflight_keeps_the_free_floor_and_cannot_overflow() {
    let gib = 1u64 << 30;
    assert!(disk_ok(3 * gib, 10 * gib, gib));
    assert!(!disk_ok(3 * gib, 7 * gib / 2, gib));
    assert!(!disk_ok(u64::MAX, 1000, 1));
    assert!(!disk_ok(u64::MAX, u64::MAX, 1));
    assert!(disk_ok(u64::MAX, u64::MAX, 0));
    assert!(disk_ok(3 * gib, 4 * gib, gib));
}
