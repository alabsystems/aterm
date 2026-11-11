// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

/// The tests that fill the lane, one at a time: two of them each short of a place they are
/// waiting to see taken would wait for each other until the let-in.
static FILLING: Mutex<()> = Mutex::new(());

/// `then` with `holders` threads holding a place each.
fn with_places_held(holders: usize, then: impl FnOnce()) {
    let _alone = FILLING.lock().unwrap_or_else(PoisonError::into_inner);
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    std::thread::scope(|scope| {
        let (took, taken) = mpsc::channel();
        for n in 0..holders {
            let (took, released) = (took.clone(), &released);
            std::thread::Builder::new()
                .name(format!("holder::{n}"))
                .spawn_scoped(scope, move || {
                    join();
                    took.send(()).unwrap();
                    let _ = released.lock().unwrap().recv();
                })
                .unwrap();
        }
        for _ in 0..holders {
            taken.recv().unwrap();
        }
        then();
        drop(release);
    });
}

/// Two walks at once share the places, and each still answers in item order.
#[test]
fn walks_at_once_share_the_places_and_answer_in_item_order() {
    let items: Vec<usize> = (0..48).collect();
    let (running, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let walk = || {
        each(&items, |&item| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(3));
            running.fetch_sub(1, Ordering::SeqCst);
            item
        })
    };
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(walk);
        let second = scope.spawn(walk);
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(first, items);
    assert_eq!(second, items);
    assert!(most.load(Ordering::SeqCst) <= PLACES, "{most:?}");
}

/// A walk's caller gives its place up, so holding the last one cannot make its walk wait.
#[test]
fn a_walk_gives_its_callers_place_to_its_workers() {
    with_places_held(PLACES - 1, || {
        join();
        let walked = each(&[1, 2, 3], |&item| {
            let taken = *TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
            assert!(taken <= PLACES, "a worker went in over the places: {taken}");
            item
        });
        assert_eq!(walked, [1, 2, 3]);
    });
}

/// A thread with no use for a place leaves a full lane's queue holding none.
#[test]
fn a_thread_with_no_use_for_a_place_leaves_the_queue() {
    with_places_held(PLACES, || {
        let spent = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                take(&|| spent.load(Ordering::SeqCst));
                HELD.with(|held| held.borrow().is_some())
            });
            spent.store(true, Ordering::SeqCst);
            assert!(
                !waiting.join().unwrap(),
                "it took a place it had no use for"
            );
        });
    });
}
