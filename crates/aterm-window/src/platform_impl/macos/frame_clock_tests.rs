// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

#[test]
fn host_seconds_convert_to_the_instants_they_name() {
    // Compare conversions of one sample; scheduling between separate clock reads
    // is not a property of the timestamp conversion.
    let seconds = host_seconds();
    let at = instant_of(seconds);
    let later = instant_of(seconds + 0.5);
    assert!(later > at, "half a second on is later");
    assert_eq!(instant_of(f64::NAN), instant_of(f64::INFINITY));
    assert_eq!(instant_of(f64::MAX), instant_of(f64::NAN), "a value past an Instant's range");
    assert_eq!(instant_of(-f64::MAX), instant_of(f64::NAN));
}

#[test]
fn the_calibration_keeps_the_read_no_preemption_split() {
    // The host clock and `Instant` are one clock here: `base` at host second 100. The first
    // read was preempted for 5 ms between its host read and its `Instant`; the next two were
    // not, and the second of them is the tightest (200 ns between its host reads).
    let base = Instant::now();
    let at = |s: f64| base + Duration::from_secs_f64(s - 100.0);
    let reads = [
        (100.000_000, at(100.005_000), 100.005_000_5),
        (100.006_000, at(100.006_000_4), 100.006_000_8),
        (100.007_000, at(100.007_000_1), 100.007_000_2),
    ];
    let (host, instant) = tightest_pair(reads).expect("three reads");
    let skew = (instant - base).as_secs_f64() - (host - 100.0);
    assert!(
        skew.abs() < 1e-6,
        "the pair maps the host clock onto Instant within a microsecond, not the preempted \
         read's 5 ms: {:.3} ms off",
        skew * 1e3
    );
    assert_eq!(tightest_pair([]), None);
}
