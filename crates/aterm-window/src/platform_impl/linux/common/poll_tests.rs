// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use std::time::Instant;

use super::*;

#[test]
fn a_deadline_rounds_up_to_the_millisecond() {
    assert_eq!(timeout_millis(None), -1);
    assert_eq!(timeout_millis(Some(Duration::ZERO)), 0);
    assert_eq!(timeout_millis(Some(Duration::from_nanos(1))), 1);
    assert_eq!(timeout_millis(Some(Duration::from_micros(119_600))), 120);
    assert_eq!(timeout_millis(Some(Duration::from_millis(5))), 5);
    assert_eq!(timeout_millis(Some(Duration::MAX)), libc::c_int::MAX);
}

#[test]
fn a_wake_from_another_thread_ends_the_wait_and_drains() {
    let waker = Waker::new().expect("eventfd");
    let remote = waker.clone();
    let poke = std::thread::spawn(move || remote.wake());
    let mut fds = [readable(waker.as_raw_fd())];
    let until = Instant::now() + Duration::from_secs(60);
    while !ready(&fds[0]) {
        assert!(Instant::now() < until, "the wake never arrived");
        wait(&mut fds, Some(Duration::from_secs(60))).expect("poll");
    }
    poke.join().expect("the waking thread");
    waker.drain();
    let mut fds = [readable(waker.as_raw_fd())];
    wait(&mut fds, Some(Duration::ZERO)).expect("poll");
    assert!(!ready(&fds[0]), "a drained wake fired again");
}

#[test]
fn repeated_wakes_coalesce() {
    let waker = Waker::new().expect("eventfd");
    for _ in 0..1000 {
        waker.wake();
    }
    let mut fds = [readable(waker.as_raw_fd())];
    wait(&mut fds, Some(Duration::ZERO)).expect("poll");
    assert!(ready(&fds[0]));
    waker.drain();
    let mut fds = [readable(waker.as_raw_fd())];
    wait(&mut fds, Some(Duration::ZERO)).expect("poll");
    assert!(!ready(&fds[0]));
}
