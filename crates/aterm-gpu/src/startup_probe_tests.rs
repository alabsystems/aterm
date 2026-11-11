// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Duration, slot_ns};

#[test]
fn slot_ns_avoids_the_sentinel_and_saturates() {
    assert_eq!(slot_ns(Duration::ZERO), 1);
    assert_eq!(slot_ns(Duration::from_nanos(7)), 7);
    assert_eq!(slot_ns(Duration::MAX), u64::MAX);
}
