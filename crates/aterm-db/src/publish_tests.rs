// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Laws of [`Published`]: the revision is a frame key, and a read never waits.

use super::*;

#[test]
fn the_revision_moves_only_when_the_value_differs() {
    let published = Published::new(vec![1_u32, 2]);
    assert_eq!(published.current().revision, 0);
    // The same value again: nothing moves, and the reader's Arc is still the one held.
    let before = published.current();
    assert!(!published.publish(vec![1, 2]));
    let after = published.current();
    assert_eq!(after.revision, 0);
    assert!(Arc::ptr_eq(&before.value, &after.value));
    // A different value moves it once. The control: had `publish` counted every call, the
    // revision here would be 2, not 1.
    assert!(published.publish(vec![1, 3]));
    assert!(!published.publish(vec![1, 3]));
    let moved = published.current();
    assert_eq!(moved.revision, 1);
    assert_eq!(*moved.value, vec![1, 3]);
    // Back to an earlier value is a change too: equal revisions mean equal values, never the
    // reverse.
    assert!(published.publish(vec![1, 2]));
    assert_eq!(published.current().revision, 2);
}

#[test]
fn a_contended_read_answers_none_and_keeps_no_one_waiting() {
    let published = Published::new(7_u32);
    let reader = published.clone();
    let held = published
        .0
        .held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The lock is taken: a painting reader gets no new value and returns at once.
    assert!(reader.try_current().is_none());
    drop(held);
    // The control: the same read with the lock free answers.
    let current = reader.try_current().expect("an uncontended read answers");
    assert_eq!((current.revision, *current.value), (0, 7));
}

#[test]
fn every_read_is_an_ask_the_publisher_takes_once() {
    let published = Published::new(0_u8);
    assert!(!published.take_asked(), "nobody has read yet");
    // A waiting read is a worker's own and asks for nothing.
    let _ = published.current();
    assert!(!published.take_asked());
    let _ = published.try_current();
    let _ = published.try_current();
    assert!(published.take_asked());
    assert!(!published.take_asked(), "the ask was taken");
}
