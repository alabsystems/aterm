// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::*;

/// The hang detector: a wait for what must happen fails after a minute, never sooner.
const HANG: Duration = Duration::from_secs(60);

fn queue() -> SerialQueue {
    SerialQueue::new(c"aterm-objc test queue").expect("a queue")
}

#[test]
fn work_runs_one_at_a_time_in_submission_order_and_sync_waits_for_it() {
    let q = queue();
    let order = Arc::new(Mutex::new(Vec::new()));
    let running = Arc::new(AtomicUsize::new(0));
    for i in 0..200 {
        let order = Arc::clone(&order);
        let running = Arc::clone(&running);
        q.spawn(move || {
            assert_eq!(
                running.fetch_add(1, Ordering::SeqCst),
                0,
                "overlapping work"
            );
            order.lock().expect("order").push(i);
            running.fetch_sub(1, Ordering::SeqCst);
        });
    }
    let seen = q.sync(|| order.lock().expect("order").clone());
    assert_eq!(seen, (0..200).collect::<Vec<_>>());
    assert_eq!(q.sync(|| 7_u32), 7);
}

#[test]
fn the_queue_knows_its_own_work_and_no_other() {
    let q = queue();
    let other = queue();
    assert!(!q.is_current());
    assert!(q.sync(|| q.is_current()));
    assert!(!q.sync(|| other.is_current()));
    let (tx, rx) = mpsc::channel();
    let q = Arc::new(q);
    let inner = Arc::clone(&q);
    q.spawn(move || {
        let _ = tx.send(inner.is_current());
    });
    assert_eq!(rx.recv_timeout(HANG), Ok(true));
}

#[test]
fn a_sync_from_the_queues_own_work_is_refused_not_deadlocked() {
    let q = queue();
    let refused = std::panic::catch_unwind(AssertUnwindSafe(|| q.sync(|| q.sync(|| 1))));
    let payload = refused.expect_err("refused");
    let text = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(text.contains("would deadlock"), "{text}");
    // The queue is still usable.
    assert_eq!(q.sync(|| 2), 2);
}

#[test]
fn a_panic_in_sync_reaches_the_caller_and_the_queue_goes_on() {
    let q = queue();
    let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
        q.sync(|| -> u8 { panic!("on the queue") })
    }));
    assert!(caught.is_err());
    assert_eq!(q.sync(|| 3), 3);
}

#[test]
fn a_panic_in_spawned_work_is_reported_and_later_work_still_runs() {
    let q = queue();
    q.spawn(|| panic!("spawned work panics"));
    let (tx, rx) = mpsc::channel();
    q.spawn(move || {
        let _ = tx.send(5);
    });
    assert_eq!(rx.recv_timeout(HANG), Ok(5));
}

#[test]
fn pending_work_outlives_the_dropped_holder() {
    let q = queue();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    q.spawn(move || {
        let _ = gate_rx.recv_timeout(HANG);
    });
    q.spawn(move || {
        let _ = done_tx.send(());
    });
    drop(q);
    gate_tx.send(()).expect("gate");
    assert_eq!(done_rx.recv_timeout(HANG), Ok(()));
}

#[test]
fn the_queue_is_an_object_the_runtime_can_message() {
    let q = queue();
    let id = q.as_id();
    assert!(!id.is_null());
    // SAFETY: a dispatch queue is an Objective-C object here; `-description` is `@16@0:8`
    // and answers an autoreleased NSString, read inside the pool.
    let text = autoreleasepool(|_| unsafe {
        let description: unsafe extern "C-unwind" fn(Id, crate::Sel) -> Id = crate::checked_msg();
        crate::ns_string_to_rust(description(id, crate::sel!(description)))
    });
    assert!(text.contains("aterm-objc test queue"), "{text}");
}
