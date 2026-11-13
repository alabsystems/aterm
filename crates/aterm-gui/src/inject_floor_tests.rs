// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

#[test]
fn floor_admits_under_cap_and_refuses_a_burst_storm() {
    let s = 9_999_001;
    assert!(allow(s, 64), "a small write is under the floor");
    assert!(
        !allow(s, (CAP_BYTES as usize) + 1),
        "a > cap single write is refused"
    );
    forget(s);
}

#[test]
fn floor_throttles_a_tight_loop_then_recovers() {
    let chunk = (CAP_BYTES as usize) / 4;
    let t0 = Instant::now();
    let mut bucket = Bucket {
        tokens: CAP_BYTES,
        last: t0,
    };
    let admitted = (0..8).filter(|_| step(&mut bucket, t0, chunk)).count();
    assert_eq!(admitted, 4, "a full bucket admits four quarters at once");
    let later = t0 + Duration::from_millis(250);
    assert!(
        step(&mut bucket, later, chunk),
        "a quarter second refills a quarter"
    );
    assert!(!step(&mut bucket, later, chunk), "and only a quarter");

    let s = 9_999_002;
    let t0 = Instant::now();
    let admitted = (0..8).filter(|_| allow(s, chunk)).count();
    let credit = t0.elapsed().as_secs_f64() * REFILL_BYTES_PER_SEC;
    assert!(
        admitted >= 4,
        "a full bucket admits four quarters: {admitted}"
    );
    assert!(
        (admitted * chunk) as f64 <= CAP_BYTES + credit,
        "allow keeps the session's bucket: {admitted} admits in {credit} bytes of refill"
    );
    forget(s);
}
