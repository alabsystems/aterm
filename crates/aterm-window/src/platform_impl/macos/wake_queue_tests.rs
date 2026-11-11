// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use std::sync::mpsc;
use std::time::Duration;

use super::*;

const MUST_HAPPEN: Duration = Duration::from_secs(60);

fn drain(rx: &Receiver<u32>) -> Vec<u32> {
    std::iter::from_fn(|| rx.try_next()).collect()
}

#[test]
fn events_arrive_oldest_first_across_senders_and_drains() {
    let (tx, rx) = channel();
    let other = tx.clone();
    tx.send(1).unwrap();
    other.send(2).unwrap();
    assert!(rx.has_pending());
    assert_eq!(rx.try_next(), Some(1));
    tx.send(3).unwrap();
    assert_eq!(drain(&rx), [2, 3]);
    assert!(!rx.has_pending());
    other.send(4).unwrap();
    assert_eq!(drain(&rx), [4]);
}

#[test]
fn only_the_send_that_arms_the_wake_owes_a_signal() {
    let (tx, rx) = channel();
    let other = tx.clone();
    let armed = rx.armed();
    assert!(!armed.is_set());
    assert_eq!(tx.send(1), Ok(true), "the first send signals");
    assert_eq!(other.send(2), Ok(false), "a drain is already owed");
    assert_eq!(tx.send(3), Ok(false));
    assert!(armed.is_set(), "owed a drain");
    assert_eq!(drain(&rx), [1, 2, 3]);
    assert!(!armed.is_set(), "the take cleared it");
    assert_eq!(other.send(4), Ok(true), "the next send after a take signals");
    assert_eq!(drain(&rx), [4]);
}

#[test]
fn a_drain_that_finds_the_lock_held_still_clears_the_flag() {
    let (tx, rx) = channel::<u32>();
    assert_eq!(tx.send(1), Ok(true));
    let (held_tx, held_rx) = mpsc::channel();
    let (drained_tx, drained_rx) = mpsc::channel::<()>();
    let queue = Arc::clone(&tx.queue);
    let holder = std::thread::spawn(move || {
        let mut guard = queue.lock();
        held_tx.send(()).unwrap();
        let _ = drained_rx.recv_timeout(MUST_HAPPEN);
        guard.events.push_back(7);
    });
    held_rx.recv_timeout(MUST_HAPPEN).expect("the holder took the lock");
    assert_eq!(drain(&rx), Vec::<u32>::new(), "nothing taken past the held lock");
    drained_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(tx.send(8), Ok(true), "the drain cleared the flag: this send signals");
    assert_eq!(drain(&rx), [1, 7, 8]);
}

#[test]
fn a_drain_stopped_at_a_held_lock_says_a_send_before_it_may_still_be_queued() {
    let (tx, rx) = channel();
    let drained = rx.drained();
    tx.send(1).unwrap();
    assert_eq!(drain(&rx), [1]);
    assert!(drained.emptied(), "the drain ended on an empty queue");
    tx.send(2).unwrap();
    let (held_tx, held_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let queue = Arc::clone(&tx.queue);
    let holder = std::thread::spawn(move || {
        let mut guard = queue.lock();
        held_tx.send(()).unwrap();
        let _ = go_rx.recv_timeout(MUST_HAPPEN);
        guard.events.push_back(3);
    });
    held_rx.recv_timeout(MUST_HAPPEN).expect("the holder took the lock");
    assert_eq!(drain(&rx), Vec::<u32>::new(), "2 was sent in full, yet this drain took nothing");
    assert!(!drained.emptied(), "so it does not claim to have emptied the queue");
    go_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(drain(&rx), [2, 3], "the next drain hands out what the stopped one left");
    assert!(drained.emptied());
    tx.send(4).unwrap();
    tx.send(5).unwrap();
    assert_eq!(rx.try_next(), Some(4), "a drain cut short after one event");
    assert!(!drained.emptied(), "5 is taken but not handed out");
    assert_eq!(drain(&rx), [5]);
    assert!(drained.emptied());
}

#[test]
fn a_send_after_the_receiver_is_gone_hands_its_event_back() {
    let (tx, rx) = channel();
    tx.send(1).unwrap();
    drop(rx);
    assert_eq!(tx.send(2), Err(2));
}

#[test]
fn a_drain_returns_at_once_while_a_sender_holds_the_lock() {
    let (tx, rx) = channel::<u32>();
    let (held_tx, held_rx) = mpsc::channel();
    let (drained_tx, drained_rx) = mpsc::channel::<()>();
    let queue = Arc::clone(&tx.queue);
    let holder = std::thread::spawn(move || {
        let mut guard = queue.lock();
        held_tx.send(()).unwrap();
        let _ = drained_rx.recv_timeout(MUST_HAPPEN);
        guard.events.push_back(7);
    });
    held_rx.recv_timeout(MUST_HAPPEN).expect("the holder took the lock");
    assert!(!rx.has_pending(), "the look waited for the held lock");
    assert_eq!(drain(&rx), Vec::<u32>::new(), "the drain waited for the held lock");
    drained_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(drain(&rx), [7]);
}
