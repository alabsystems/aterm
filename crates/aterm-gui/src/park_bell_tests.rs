// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::time::Duration;

use super::*;

/// A wait that takes the ring it hears.
fn take(bell: &ParkBell<bool>, within: Duration) -> bool {
    bell.wait(within, |rung| std::mem::take(rung).then_some(()))
        .is_some()
}

/// A RING BEFORE THE WAIT IS HEARD AT ONCE AND TAKEN: an hour's wait after it returns what it
/// heard, and the next wait, given no time, hears nothing and ends at its deadline.
#[test]
fn a_ring_before_the_wait_is_heard_and_taken() {
    let bell = ParkBell::new(false);
    bell.ring(|rung| *rung = true);
    assert!(take(&bell, Duration::from_secs(3600)), "heard");
    assert!(!take(&bell, Duration::ZERO), "taken");
    assert_eq!(bell.parks(), 0, "neither wait parked");
}

/// A RING REACHES A PARKED WAITER: the ring is made only once the waiter has parked on the
/// condition variable (its count, made under the lock the park releases), so it can end the
/// hour's wait only through the condition variable (a one-minute hang detector, not a budget).
/// NEGATIVE CONTROL: a ring that tells the state without notifying the condition variable leaves
/// the waiter parked for its hour, and the hang detector fails the test.
#[test]
fn a_ring_reaches_a_parked_waiter() {
    let bell = Arc::new(ParkBell::new(false));
    let (said, heard) = std::sync::mpsc::channel();
    let waiting = Arc::clone(&bell);
    let waiter = std::thread::spawn(move || said.send(take(&waiting, Duration::from_secs(3600))));
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the waiter parks", || {
        (bell.parks() > 0).then_some(())
    });
    bell.ring(|rung| *rung = true);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(true),
        "the ring ends the parked wait"
    );
    waiter.join().expect("waiter").expect("sent");
}
