// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;

const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// Rings wake a nap or remain queued. Distinct causes survive an earlier
/// route hint; repeats coalesce, and an empty queue waits for its timer.
#[test]
fn a_nap_ends_with_the_first_ring_or_its_timer() {
    let bell = Arc::new(Bell::default());
    assert_eq!(bell.nap(Duration::from_millis(10)), Wake::Timer);
    bell.ring(Wake::Route);
    bell.ring(Wake::Browse);
    bell.ring(Wake::Broker);
    bell.ring(Wake::Route);
    assert_eq!(
        bell.nap(HANG_DETECTOR),
        Wake::Route,
        "kept for the next nap"
    );
    assert_eq!(bell.nap(HANG_DETECTOR), Wake::Browse);
    assert_eq!(bell.nap(HANG_DETECTOR), Wake::Broker);
    assert_eq!(bell.nap(Duration::from_millis(10)), Wake::Timer, "taken");
    let ringer = {
        let bell = Arc::clone(&bell);
        std::thread::spawn(move || bell.ring(Wake::Browse))
    };
    assert_eq!(bell.nap(HANG_DETECTOR), Wake::Browse);
    ringer.join().expect("rang");
    bell.ring(Wake::Put);
    assert_eq!(bell.clear(), Some(Wake::Put));
    assert_eq!(bell.nap(Duration::from_millis(10)), Wake::Timer);
    let max_ringer = {
        let bell = Arc::clone(&bell);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            bell.ring(Wake::Put);
        })
    };
    assert_eq!(bell.nap(Duration::MAX), Wake::Put);
    max_ringer.join().expect("rang max");
}

/// A source heard rings its bell each time it says a change came, and its thread ends once
/// the bell is gone.
/// NEGATIVE CONTROL: a hearer that never rings leaves the nap to its timer.
#[test]
fn a_heard_source_rings_its_bell_and_ends_with_it() {
    let bell = Arc::new(Bell::default());
    let (tx, rx) = mpsc::channel::<()>();
    let (looked, looks) = mpsc::channel::<()>();
    assert!(hear(
        &bell,
        move |most| {
            let _ = looked.send(());
            rx.recv_timeout(most).is_ok()
        },
        Wake::Route,
    ));
    tx.send(()).expect("a change");
    assert_eq!(bell.nap(HANG_DETECTOR), Wake::Route);
    drop(bell);
    tx.send(()).expect("one more change");
    // The thread sees its bell gone after this change and stops looking: its waiter (and
    // with it this channel's sender) is dropped.
    let deadline = Instant::now() + HANG_DETECTOR;
    loop {
        match looks.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the hearer outlived its bell"),
        }
    }
}
