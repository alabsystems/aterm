// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{nsevent_queue_age_ns, retained_key_queue_age_ns};

#[test]
fn a_retained_key_timestamp_counts_time_until_its_deferred_dispatch() {
    let original = Some((10, 1024.0));
    let at_capture = retained_key_queue_age_ns(original, || Some(1024.0));
    let at_dispatch = retained_key_queue_age_ns(original, || Some(1025.5));
    assert_eq!(at_capture, Some(0));
    assert_eq!(at_dispatch, Some(1_500_000_000));
    assert_ne!(
        at_dispatch,
        retained_key_queue_age_ns(Some((10, 1025.25)), || Some(1025.5)),
        "a later event's timestamp cannot stand for the captured key"
    );
}

#[test]
fn absent_or_non_key_clocks_never_ask_a_native_uptime_reader() {
    for native in [
        None,
        Some((11, 1024.0)),
        Some((12, 1024.0)),
        Some((2, 1024.0)),
        Some((10, 0.0)),
        Some((10, -1.0)),
        Some((10, f64::NAN)),
        Some((10, f64::INFINITY)),
    ] {
        assert_eq!(
            retained_key_queue_age_ns(native, || panic!("no native clock is needed")),
            None
        );
    }
    assert_eq!(retained_key_queue_age_ns(Some((10, 1024.0)), || None), None);
}

#[test]
fn unsupported_native_clock_values_remain_unavailable() {
    for (timestamp, uptime) in [
        (1024.0, f64::NAN),
        (1024.0, f64::INFINITY),
        (1024.0, f64::NEG_INFINITY),
        (1024.0, 1026.001),
        (1024.0, 1023.998),
    ] {
        assert_eq!(nsevent_queue_age_ns(timestamp, uptime), None);
        assert_eq!(
            retained_key_queue_age_ns(Some((10, timestamp)), || Some(uptime)),
            None
        );
    }
    assert_eq!(nsevent_queue_age_ns(1024.0, 1026.0), Some(2_000_000_000));
    assert_eq!(nsevent_queue_age_ns(1024.0, 1023.9995), Some(0));
}
