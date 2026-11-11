// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use std::cell::Cell;

struct FakeClock {
    now: Cell<Instant>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: Cell::new(Instant::now()),
        }
    }

    fn advance(&self, by: Duration) {
        self.now.set(self.now.get() + by);
    }
}

impl TimeSource for FakeClock {
    fn now(&self) -> Instant {
        self.now.get()
    }
}

#[test]
fn test_response_rate_above_limit_excess_dropped() {
    let mut limiter = ResponseRateLimiter::with_limits(100, 200);
    let clock = FakeClock::new();

    let mut allowed = 0;
    let mut denied = 0;
    for _ in 0..10 {
        if limiter.try_consume(40, &clock) {
            allowed += 1;
        } else {
            denied += 1;
        }
    }

    assert_eq!(
        allowed, 5,
        "burst capacity should permit exactly 5 x 40-byte responses"
    );
    assert_eq!(denied, 5, "excess over burst must be silently dropped");
}

#[test]
fn test_response_rate_replenishes_after_cooldown() {
    let mut limiter = ResponseRateLimiter::with_limits(1000, 100);
    let clock = FakeClock::new();

    assert!(limiter.try_consume(100, &clock));
    assert!(!limiter.try_consume(1, &clock));

    clock.advance(Duration::from_millis(50));
    assert!(
        limiter.try_consume(50, &clock),
        "tokens should replenish after cooldown"
    );
    assert!(!limiter.try_consume(1, &clock));
}

#[test]
fn test_response_burst_capacity_caps_at_configured_max() {
    let mut limiter = ResponseRateLimiter::with_limits(10_000, 100);
    let clock = FakeClock::new();

    assert!(limiter.try_consume(100, &clock));

    clock.advance(Duration::from_secs(10));
    assert!(
        limiter.try_consume(100, &clock),
        "full burst should be available after idle"
    );
    assert!(
        !limiter.try_consume(1, &clock),
        "bucket must cap at configured capacity"
    );
}

#[test]
fn test_response_oversized_always_dropped() {
    let mut limiter = ResponseRateLimiter::with_limits(1000, 100);
    let clock = FakeClock::new();

    assert!(
        !limiter.try_consume(101, &clock),
        "response larger than capacity must be dropped"
    );
    assert!(limiter.try_consume(100, &clock));
}
