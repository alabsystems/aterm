// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn production_is_the_constant_and_fast_a_thousandth_floored() {
    let ten = Duration::from_secs(10);
    assert_eq!(Timing::PRODUCTION.of(ten), ten);
    assert_eq!(Timing::FAST.of(ten), Duration::from_millis(10));
    assert_eq!(Timing::FAST.of(Duration::from_millis(40)), FAST_FLOOR);
    assert_eq!(Timing::FAST.of(Duration::ZERO), Duration::ZERO);
    assert_eq!(
        Timing::FAST.of(Duration::from_secs(3600)),
        Duration::from_millis(3600)
    );
    assert!(Timing::FAST.is_fast() && !Timing::PRODUCTION.is_fast());
}

#[test]
fn a_floored_wait_keeps_production_and_its_floor() {
    let (five, second) = (Duration::from_secs(5), Duration::from_secs(1));
    assert_eq!(Timing::PRODUCTION.of_at_least(five, second), five);
    assert_eq!(Timing::FAST.of_at_least(five, second), second);
    assert_eq!(
        Timing::FAST.of_at_least(Duration::from_secs(3600), second),
        Duration::from_millis(3600)
    );
    assert_eq!(
        Timing::FAST.of_at_least(Duration::from_millis(400), second),
        Duration::from_millis(400)
    );
}

#[test]
fn select_fast_wins_for_the_rest_of_the_process() {
    select_fast();
    assert_eq!(current(), Timing::FAST);
    assert_eq!(current(), Timing::FAST);
}

#[test]
fn scale_millis_backoff_and_cadence_wire_round_trip() {
    assert_eq!(Timing::PRODUCTION.divisor(), 1);
    assert_eq!(Timing::FAST.divisor(), 1000);
    assert_eq!(Timing::PRODUCTION.scale_millis(10_000), 10_000);
    assert_eq!(Timing::FAST.scale_millis(10_000), 10);
    assert_eq!(Timing::FAST.scale_millis(50), 1);
    assert_eq!(Timing::FAST.scale_millis(0), 0);
    assert_eq!(Timing::FAST.scale_millis_at_least(5_000, 1_000), 1_000);
    assert_eq!(
        Timing::PRODUCTION.exponential_backoff_ms(100, 3, 5_000),
        800
    );
    assert_eq!(Timing::FAST.exponential_backoff_ms(1_000, 4, 10_000), 10);

    let wire = Timing::FAST.encode_cadence_wire_le(10_000, 250);
    assert_eq!(
        Timing::decode_cadence_wire_le(&wire),
        Some((Timing::FAST, 10_000, 250))
    );
}
