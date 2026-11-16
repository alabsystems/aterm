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
        move |most, _| {
            let _ = looked.send(());
            match rx.recv_timeout(most) {
                Ok(()) => Heard::Change,
                Err(_) => Heard::Failed,
            }
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

/// A hearer of route changes waits with no timer: each wait it makes is given
/// [`Duration::MAX`], and dropping its bell, with no change, ends the thread at once, its route
/// wait having heard the bell's [`Closing`] reach its end.
/// NEGATIVE CONTROL: a hearer that waits in steps (`HEAR_STEP`, 5 s, the code before) is given
/// 5 s; one that waits beside no [`Closing`] outlives its bell to the hang detector.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_route_hearer_waits_with_no_timer_and_ends_with_its_bell() {
    let (heard, _kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let mut routes = route_waiter(crate::network::RouteChanges::over(heard.into()));
    let bell = Arc::new(Bell::default());
    let (gave, given) = mpsc::channel::<Duration>();
    let mut first = true;
    assert!(hear(
        &bell,
        move |most, closing| {
            let _ = gave.send(most);
            if std::mem::take(&mut first) {
                return Heard::Change;
            }
            routes(most, closing)
        },
        Wake::Route,
    ));
    assert_eq!(bell.nap(HANG_DETECTOR), Wake::Route, "the change rang");
    let waits: Vec<Duration> = (0..2)
        .map(|_| given.recv_timeout(HANG_DETECTOR).expect("a wait began"))
        .collect();
    assert_eq!(
        waits,
        [Duration::MAX, Duration::MAX],
        "every wait is unbounded"
    );
    drop(bell);
    match given.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Ok(most) => panic!("the hearer waited again ({most:?}) after its bell was dropped"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the hearer outlived its bell"),
    }
}

/// A route hearer whose socket failed waits on it again after a pause beside its bell's
/// [`Closing`] ([`route_waiter`]): a failure that passes (here an empty datagram, which a read
/// takes for a failed socket) loses no later change, and the bell dropped during the pause ends
/// the thread at once.
/// NEGATIVE CONTROLS: end the hearer at a failure (`Heard::Failed`, the code before) and the
/// later change never rings (`the change after a failure rang`); pause with a sleep that waits
/// beside nothing and the thread outlives its bell (`the hearer outlived its bell`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_route_hearer_outlasts_a_failure_and_ends_with_its_bell_as_it_pauses() {
    // An address change as the kernel words it on macOS; any message of it reads as a change on
    // Linux.
    let change: Vec<u8> = [24_u32, 1, 1, 1, 37, 1]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect();
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let bell = Arc::new(Bell::default());
    assert!(hear(
        &bell,
        route_waiter_pausing(
            crate::network::RouteChanges::over(heard.into()),
            Duration::ZERO
        ),
        Wake::Route,
    ));
    kernel.send(&[]).expect("an empty datagram");
    kernel.send(&change).expect("a change");
    assert_eq!(
        bell.nap(HANG_DETECTOR),
        Wake::Route,
        "the change after a failure rang"
    );
    drop(bell);

    // A pause past the hang detector: only the bell's end cuts it short.
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let bell = Arc::new(Bell::default());
    let (began, waits) = mpsc::channel::<()>();
    let mut routes = route_waiter_pausing(
        crate::network::RouteChanges::over(heard.into()),
        Duration::MAX,
    );
    assert!(hear(
        &bell,
        move |most, closing| {
            let _ = began.send(());
            routes(most, closing)
        },
        Wake::Route,
    ));
    waits.recv_timeout(HANG_DETECTOR).expect("a wait began");
    kernel.send(&[]).expect("an empty datagram");
    drop(bell);
    match waits.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Ok(()) => panic!("the hearer waited again after its bell was dropped"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the hearer outlived its bell"),
    }
}
