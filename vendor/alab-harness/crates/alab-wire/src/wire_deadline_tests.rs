// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{deadline_at, remaining_at};
use std::io;
use std::time::{Duration, Instant};

#[test]
fn transaction_steps_share_the_original_remaining_budget() {
    let start = Instant::now();
    let until = deadline_at(Duration::from_millis(500), start).unwrap();
    for (elapsed_ms, remaining_ms) in [(0, 500), (120, 380), (499, 1)] {
        assert_eq!(
            remaining_at(until, start + Duration::from_millis(elapsed_ms)).unwrap(),
            Duration::from_millis(remaining_ms)
        );
    }
}

#[test]
fn the_exact_deadline_and_later_steps_have_no_budget() {
    let start = Instant::now();
    let until = deadline_at(Duration::from_millis(500), start).unwrap();
    for now in [until, until + Duration::from_millis(1)] {
        assert_eq!(
            remaining_at(until, now).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
