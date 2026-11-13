// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn positive_fragments_round_up_and_long_waits_saturate() {
    for (duration, expected) in [
        (Duration::ZERO, 0),
        (Duration::from_nanos(1), 1),
        (Duration::from_nanos(999_999), 1),
        (Duration::from_millis(1), 1),
        (Duration::from_nanos(1_000_001), 2),
        (Duration::MAX, i32::MAX),
    ] {
        assert_eq!(poll_timeout_ms(duration), expected);
    }
}

#[test]
fn a_poll_timeout_is_the_smallest_millisecond_ceiling() {
    let limit_ns = u64::try_from(i32::MAX).unwrap() * 1_000_000;
    for nanos in [
        0,
        1,
        999_999,
        1_000_000,
        1_000_001,
        199_999_999,
        200_000_000,
        200_000_001,
        limit_ns - 1,
        limit_ns,
    ] {
        let budget = Duration::from_nanos(nanos);
        let millis = poll_timeout_ms(budget);
        let represented = Duration::from_millis(u64::try_from(millis).unwrap());
        if budget.is_zero() {
            assert!(represented.is_zero());
        } else {
            assert!(represented >= budget, "{budget:?}: {millis}");
            assert!(represented - budget < Duration::from_millis(1));
        }
    }
}

#[test]
fn a_poll_timeout_saturates_at_the_signed_limit() {
    let limit = Duration::from_millis(u64::try_from(i32::MAX).unwrap());
    for budget in [limit, limit + Duration::from_nanos(1), Duration::MAX] {
        assert_eq!(poll_timeout_ms(budget), i32::MAX);
    }
}
