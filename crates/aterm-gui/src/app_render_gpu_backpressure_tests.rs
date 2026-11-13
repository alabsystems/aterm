// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::gpu_backpressure_excess_ns;
use std::time::Duration;

#[test]
fn backpressure_charges_only_the_wait_past_one_refresh() {
    let hz120 = Duration::from_micros(8333);
    let hz60 = Duration::from_micros(16667);
    for (wait_ns, fi, want) in [
        (0, hz120, 0),
        (1_000_000, hz120, 0),
        (8_333_000, hz120, 0),
        (10_333_000, hz120, 2_000_000),
        (25_000_000, hz120, 16_667_000),
        (25_000_000, hz60, 8_333_000),
    ] {
        assert_eq!(
            gpu_backpressure_excess_ns(wait_ns, fi),
            want,
            "{wait_ns} ns at {fi:?}"
        );
    }
}
