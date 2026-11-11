// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH, duration_from_millis_f64};

#[test]
fn millisecond_conversion_is_total() {
    for ms in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.0,
        -1.0,
        -1e300,
        0.0,
        f64::MIN_POSITIVE,
        1e300,
        f64::MAX,
    ] {
        let _ = duration_from_millis_f64(ms);
    }
    assert_eq!(duration_from_millis_f64(f64::NAN), Duration::ZERO);
    assert_eq!(duration_from_millis_f64(f64::INFINITY), Duration::ZERO);
    assert_eq!(duration_from_millis_f64(-1.0), Duration::ZERO);
    assert_eq!(duration_from_millis_f64(0.0), Duration::ZERO);
    assert_eq!(
        duration_from_millis_f64(f64::MAX),
        Duration::from_millis(u64::MAX)
    );
}

#[test]
fn millisecond_conversion_keeps_the_fraction() {
    assert_eq!(duration_from_millis_f64(1.0), Duration::from_millis(1));
    assert_eq!(
        duration_from_millis_f64(1500.0),
        Duration::from_millis(1500)
    );
    assert_eq!(duration_from_millis_f64(0.5), Duration::from_nanos(500_000));
    assert_eq!(
        duration_from_millis_f64(1.25),
        Duration::from_nanos(1_250_000)
    );
}

#[test]
fn resolution_survives_an_aged_time_origin() {
    let hour_ms = 3_600_000.0_f64;
    let a = duration_from_millis_f64(hour_ms);
    let b = duration_from_millis_f64(hour_ms + 0.1);
    assert!(
        b > a && b - a >= Duration::from_nanos(99_998),
        "0.1ms apart after an hour resolved as {:?}",
        b.saturating_sub(a)
    );
}

#[test]
fn instant_differences_saturate_backwards() {
    let a = Instant::from_js_millis(1000.0);
    let b = Instant::from_js_millis(1500.0);
    assert_eq!(b.duration_since(a), Duration::from_millis(500));
    assert_eq!(b.saturating_duration_since(a), Duration::from_millis(500));
    assert_eq!(b - a, Duration::from_millis(500));
    assert_eq!(a.duration_since(b), Duration::ZERO);
    assert_eq!(a.saturating_duration_since(b), Duration::ZERO);
    assert_eq!(a.checked_duration_since(b), None);
    assert_eq!(
        b.checked_duration_since(a),
        Some(Duration::from_millis(500))
    );
    assert!(b > a);
}

#[test]
fn instant_arithmetic_round_trips_and_checks() {
    let a = Instant::from_js_millis(1000.0);
    let mut b = a;
    b += Duration::from_secs(1);
    assert_eq!(b, a + Duration::from_secs(1));
    b -= Duration::from_secs(1);
    assert_eq!(b, a);
    assert_eq!(
        a - Duration::from_millis(1000),
        Instant::from_js_millis(0.0)
    );
    assert_eq!(a.checked_sub(Duration::from_secs(60)), None);
    assert!(a.checked_add(Duration::from_secs(60)).is_some());
    assert_eq!(a.checked_add(Duration::MAX), None);
}

#[test]
fn system_time_reports_backwards_steps() {
    let t0 = SystemTime::from_js_millis(1_700_000_000_000.0);
    let t1 = SystemTime::from_js_millis(1_700_000_001_000.0);
    assert_eq!(
        t1.duration_since(t0).expect("forward"),
        Duration::from_secs(1)
    );
    assert_eq!(
        t0.duration_since(UNIX_EPOCH).expect("after the epoch"),
        Duration::from_millis(1_700_000_000_000)
    );
    assert_eq!(SystemTime::UNIX_EPOCH, UNIX_EPOCH);
    let err: SystemTimeError = t0.duration_since(t1).expect_err("backwards");
    assert_eq!(err.duration(), Duration::from_secs(1));
    assert_eq!(err.to_string(), "second time provided was later than self");
    const fn assert_error<E: core::error::Error>() {}
    assert_error::<SystemTimeError>();
}

#[test]
fn system_time_arithmetic_round_trips() {
    let t = SystemTime::from_js_millis(1_700_000_000_000.0);
    let mut u = t;
    u += Duration::from_secs(10);
    assert_eq!(u, t + Duration::from_secs(10));
    u -= Duration::from_secs(10);
    assert_eq!(u, t);
    assert_eq!(
        t - Duration::from_secs(1),
        t.checked_sub(Duration::from_secs(1)).expect("in range")
    );
    assert_eq!(UNIX_EPOCH.checked_sub(Duration::from_secs(1)), None);
    assert_eq!(t.checked_add(Duration::MAX), None);
}
