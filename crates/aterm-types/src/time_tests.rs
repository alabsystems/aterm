// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn duration_to_nanos_cases() {
    for (what, duration, nanos) in [
        ("zero", Duration::ZERO, 0),
        ("one second", Duration::from_secs(1), 1_000_000_000),
        ("subsec nanos only", Duration::from_nanos(42), 42),
        (
            "mixed secs and nanos",
            Duration::new(2, 500_000_000),
            2_500_000_000,
        ),
        ("saturates at max", Duration::from_secs(u64::MAX), u64::MAX),
    ] {
        assert_eq!(duration_to_nanos(duration), nanos, "{what}");
    }
}
