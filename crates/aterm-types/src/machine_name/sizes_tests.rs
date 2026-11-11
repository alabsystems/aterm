// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{disk_size, memory_size};

/// The largest reading names the top size, half units rounding up as for every other.
/// NEGATIVE CONTROL: the readings either side of a half unit round down and up.
#[test]
fn a_reading_near_the_top_of_u64_rounds_without_overflow() {
    assert_eq!(memory_size(u64::MAX), "17179869184GB");
    assert_eq!(disk_size(u64::MAX, true), "18446744.1TB");
    assert_eq!(disk_size(u64::MAX, false), "18446744.1TB");
    const GIB: u64 = 1 << 30;
    assert_eq!(memory_size(32 * GIB + GIB / 2 - 1), "32GB");
    assert_eq!(memory_size(32 * GIB + GIB / 2), "33GB");
    assert_eq!(disk_size(1_049_999_999_999, true), "1TB");
    assert_eq!(disk_size(1_050_000_000_000, true), "1.1TB");
    assert_eq!(disk_size(499_499_999_999, true), "499GB");
    assert_eq!(disk_size(499_500_000_000, true), "500GB");
}
