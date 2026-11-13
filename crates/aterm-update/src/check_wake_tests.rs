// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

#[test]
fn scheduler_delays_consume_the_wait_budget_instead_of_extending_it() {
    let elapsed = Cell::new(Duration::ZERO);
    let mut slices = Vec::new();
    let result = wait_slices(
        Duration::from_secs(20),
        || elapsed.get(),
        || true,
        |slice| {
            slices.push(slice);
            // The first five-second request resumes twelve seconds late, below
            // the system-sleep gap detector's threshold.
            elapsed.set(
                elapsed.get()
                    + if slices.len() == 1 {
                        Duration::from_secs(17)
                    } else {
                        slice
                    },
            );
            Waited::Elapsed
        },
    );
    assert_eq!(result, Waited::Elapsed);
    assert_eq!(slices, [Duration::from_secs(5), Duration::from_secs(3)]);
    assert_eq!(elapsed.get(), Duration::from_secs(20));
}

#[test]
fn an_expired_budget_does_not_wait_and_a_wake_still_ends_the_wait() {
    assert_eq!(
        wait_slices(
            Duration::from_secs(20),
            || Duration::from_secs(21),
            || true,
            |_| panic!("budget already expired")
        ),
        Waited::Elapsed
    );
    let mut calls = 0;
    let woke = Waited::Woke(Duration::from_secs(150));
    assert_eq!(
        wait_slices(
            Duration::from_secs(20),
            || Duration::ZERO,
            || true,
            |slice| {
                calls += 1;
                assert_eq!(slice, WAKE_SLICE);
                woke
            }
        ),
        woke
    );
    assert_eq!(calls, 1);
}

#[test]
fn lost_ownership_cancels_before_sleep_and_before_reporting_a_wake() {
    assert_eq!(
        wait_slices(
            Duration::from_secs(86_400),
            || Duration::ZERO,
            || false,
            |_| panic!("retired wait slept")
        ),
        Waited::Cancelled
    );
    let current = Cell::new(true);
    let mut calls = 0;
    let result = wait_slices(
        Duration::from_secs(86_400),
        || Duration::ZERO,
        || current.get(),
        |_| {
            calls += 1;
            current.set(false);
            Waited::Woke(Duration::ZERO)
        },
    );
    assert_eq!(result, Waited::Cancelled);
    assert_eq!(calls, 1);
}
