// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The growth bell on instances of its own, so no other test's appends or rings reach it. Each
//! wait for something that must happen is bounded by the one-minute hang detector.

use super::Growth;
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

/// A wait on `growth` in a thread of its own, woken once the count leaves `seen` or once `flag`
/// rises. It says `id` on `ready`, once, when it has read both under the bell's lock and found
/// neither, so what the test does after hearing it comes after that read. Its count comes back on
/// the channel returned.
fn waiter(
    growth: &Arc<Growth>,
    seen: Option<u64>,
    flag: &Arc<AtomicBool>,
    ready: &mpsc::Sender<usize>,
    id: usize,
) -> mpsc::Receiver<u64> {
    let (done, back) = mpsc::channel();
    let (growth, flag, ready) = (Arc::clone(growth), Arc::clone(flag), ready.clone());
    thread::spawn(move || {
        let said = Cell::new(false);
        let count = growth.wait(None, seen, &|| {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            if !said.replace(true) {
                let _ = ready.send(id);
            }
            false
        });
        let _ = done.send(count);
    });
    back
}

/// Three appends from another thread while this one holds the bell's lock: they finish only if no
/// append reaches for it.
fn append_three_with_the_lock_held(growth: &Growth) {
    thread::scope(|s| {
        let held = growth.held();
        let appender = s.spawn(|| (0..3).for_each(|_| growth.grew()));
        until_within(MUST_HAPPEN, "three appends, the bell's lock held", || {
            appender.is_finished().then_some(())
        });
        drop(held);
    });
}

/// ATERM-KI-1108-2: AN APPEND NO WAIT ASKED FOR TAKES NO LOCK AND RINGS NOTHING. With no wait, and
/// with a wait resting on its flag alone (the materializer inside its settle, or resting while
/// another process owns Commons), appends finish while the bell's lock is held: a burst of them
/// costs that wait no wake and the store's locks no broadcast.
/// NEGATIVE CONTROLS (each run, then undone): `grew` ringing whatever the ask says (its
/// `self.ring()` out of the `if`) blocks the first appends on the held lock; a wait asking for
/// growth whatever `seen` says (`armed.store` out of its `if`) blocks the second; the hang detector
/// fails each.
#[test]
fn an_append_no_wait_asked_for_takes_no_lock() {
    let growth = Arc::new(Growth::new());
    append_three_with_the_lock_held(&growth);

    let flag = Arc::new(AtomicBool::new(false));
    let (ready, heard) = mpsc::channel();
    let back = waiter(&growth, None, &flag, &ready, 0);
    until_within(MUST_HAPPEN, "the resting wait's read", || {
        heard.try_recv().ok()
    });
    append_three_with_the_lock_held(&growth);
    flag.store(true, Ordering::SeqCst);
    growth.ring();
    let count = until_within(MUST_HAPPEN, "the resting wait woken by its ring", || {
        back.try_recv().ok()
    });
    assert_eq!(count, 6);
}

/// A GROWTH OR A RING AFTER A WAIT'S READ WAKES IT: a wait for growth that read the count wakes on
/// the next append, and a wait resting on its flag wakes on the ring made once the flag rose. A
/// wait whose deadline has come returns the count it read.
/// NEGATIVE CONTROLS (each run, then undone): `grew` that never rings (its `self.ring()` dropped)
/// leaves the growth's wait to the hang detector; `ring` without its `notify_all` leaves it too.
#[test]
fn a_growth_or_a_ring_after_a_waits_read_wakes_it() {
    let growth = Arc::new(Growth::new());
    assert_eq!(growth.wait(Some(Instant::now()), Some(0), &|| false), 0);

    let flag = Arc::new(AtomicBool::new(false));
    let (ready, heard) = mpsc::channel();
    let back = waiter(&growth, Some(0), &flag, &ready, 0);
    until_within(MUST_HAPPEN, "the growth wait's read", || {
        heard.try_recv().ok()
    });
    growth.grew();
    let count = until_within(MUST_HAPPEN, "the wait woken by the growth", || {
        back.try_recv().ok()
    });
    assert_eq!(count, 1);

    let back = waiter(&growth, None, &flag, &ready, 1);
    until_within(MUST_HAPPEN, "the resting wait's read", || {
        heard.try_recv().ok()
    });
    flag.store(true, Ordering::SeqCst);
    growth.ring();
    let count = until_within(MUST_HAPPEN, "the wait woken by the ring", || {
        back.try_recv().ok()
    });
    assert_eq!(count, 1);
}

/// ONE APPEND WAKES EVERY WAIT THAT ASKED FOR IT: two materializers of one process (an owner's
/// restart overlapping its predecessor's join) both wait for growth, both asleep, and the one
/// append that takes the ask wakes both.
/// NEGATIVE CONTROL: `ring` waking one waiter (`notify_one`) leaves the other to the hang detector.
#[test]
fn one_append_wakes_every_wait_that_asked_for_it() {
    let growth = Arc::new(Growth::new());
    let flag = Arc::new(AtomicBool::new(false));
    let (ready, heard) = mpsc::channel();
    let backs = [
        waiter(&growth, Some(0), &flag, &ready, 0),
        waiter(&growth, Some(0), &flag, &ready, 1),
    ];
    let mut asleep =
        [0, 1].map(|_| until_within(MUST_HAPPEN, "a wait's read", || heard.try_recv().ok()));
    asleep.sort_unstable();
    assert_eq!(asleep, [0, 1]);
    growth.grew();
    for back in &backs {
        let count = until_within(MUST_HAPPEN, "each wait woken by the one append", || {
            back.try_recv().ok()
        });
        assert_eq!(count, 1);
    }
}
