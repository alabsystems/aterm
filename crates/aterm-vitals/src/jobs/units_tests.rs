// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::per_min;

#[test]
fn per_min_avoids_premature_saturation_on_large_counts() {
    let n = u64::MAX / 60_000 + 100;
    assert_eq!(per_min(n, 60_000), n);
    assert_eq!(per_min(u64::MAX, 1), u64::MAX);
}
