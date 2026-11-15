// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{LOOK, QueueClock, waits_in};

const fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// Step the clock a second at a time from `from` to `to` seconds after `t0`, the queue reading
/// `waiting` throughout; the second at which it first says over.
fn first_over(
    clock: &mut QueueClock,
    t0: Instant,
    from: u64,
    to: u64,
    waiting: bool,
) -> Option<u64> {
    (from..=to).find(|s| clock.over(t0 + secs(*s), &mut || waiting))
}

#[test]
fn time_a_build_waits_to_be_admitted_is_not_the_runs_own() {
    let t0 = Instant::now();
    let mut clock = QueueClock::start(secs(10), t0);
    // An hour in the queue, six times the bound: not over.
    assert_eq!(first_over(&mut clock, t0, 1, 3600, true), None);
    assert_eq!(clock.waited(), secs(3600));
    assert_eq!(clock.own(t0 + secs(3600)), Duration::ZERO);
    // Admitted: the bound counts from here, and ends the run ten seconds on.
    assert_eq!(first_over(&mut clock, t0, 3601, 3700, false), Some(3610));
    assert_eq!(clock.waited(), secs(3600));
}

#[test]
fn a_run_that_never_waits_is_over_at_its_bound() {
    let t0 = Instant::now();
    let mut clock = QueueClock::start(secs(10), t0);
    assert_eq!(first_over(&mut clock, t0, 1, 3600, false), Some(10));
    assert_eq!(clock.waited(), Duration::ZERO);
}

#[test]
fn the_queue_is_read_once_a_look_and_a_run_is_judged_only_then() {
    let t0 = Instant::now();
    let mut clock = QueueClock::start(Duration::ZERO, t0);
    let mut looks = 0;
    let mut waiting = || {
        looks += 1;
        false
    };
    // Past its bound of nothing, and still not judged before a look is due.
    assert!(!clock.over(t0 + LOOK / 2, &mut waiting));
    assert!(clock.over(t0 + LOOK, &mut waiting));
    assert!(!clock.over(t0 + LOOK + LOOK / 2, &mut waiting));
    assert_eq!(looks, 1);
}

#[test]
fn only_a_waiting_claim_of_the_runs_checkout_is_its_queue() {
    let here = [
        PathBuf::from("/lane/clone"),
        PathBuf::from("/private/lane/clone"),
    ];
    assert!(waits_in([(false, "/lane/clone")], &here));
    assert!(waits_in(
        [(true, "/lane/clone"), (false, "/private/lane/clone")],
        &here
    ));
    // Admitted: it builds, on the run's own time.
    assert!(!waits_in([(true, "/lane/clone")], &here));
    // Another checkout's wait is not this run's.
    assert!(!waits_in(
        [(false, "/lane/clone/sub"), (false, "/other")],
        &here
    ));
    assert!(!waits_in([], &here));
}
