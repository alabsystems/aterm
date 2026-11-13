// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_messages::{Severity, tags};

fn msg(title: &str) -> Message {
    Message::new(tags::CONFIG, Severity::Warn, title).line("a detail")
}

/// Bounded, deduplicated, drained in order, and poison-tolerant: once a producer panics
/// holding the lock, a queue is refused and a drain answers empty, never a second panic.
#[test]
fn the_pre_app_inbox_is_poison_tolerant_and_bounded() {
    if !crate::test_alone::alone() {
        return;
    }
    whole_inbox_for_test();
    assert!(!INBOX_PENDING.load(Ordering::Acquire));
    for i in 0..(INBOX_CAP + 8) {
        queue_message(msg(&format!("warning {i}")));
    }
    queue_message(msg("warning 0"));
    assert!(INBOX_PENDING.load(Ordering::Acquire));
    let taken = take_queued();
    assert_eq!(taken.len(), INBOX_CAP, "the cap holds");
    assert_eq!(taken[0].message.title, "warning 0");
    assert_eq!(
        taken[INBOX_CAP - 1].message.title,
        format!("warning {}", INBOX_CAP - 1)
    );
    assert!(taken[0].stamp.unix_ms > 0, "stamped at queue time");
    assert!(take_queued().is_empty(), "drained");
    assert!(!INBOX_PENDING.load(Ordering::Acquire));
    queue_message(msg("again"));
    queue_message(msg("again"));
    queue_message(Message::new(tags::CONFIG, Severity::Warn, "again").line("other"));
    let taken = take_queued();
    assert_eq!(taken.len(), 2);
    queue_message(Message::new(tags::CONFIG, Severity::Warn, "bad\u{1b}[31m"));
    let taken = take_queued();
    assert_eq!(taken[0].message.title, "bad[31m");

    queue_message(msg("before the poison"));
    let poisoned = std::panic::catch_unwind(|| {
        let _held = INBOX.lock().unwrap();
        panic!("a producer panicked with the inbox held");
    });
    assert!(poisoned.is_err());
    assert!(INBOX.is_poisoned());
    queue_message(msg("into the poison"));
    assert!(
        take_queued().is_empty(),
        "a poisoned inbox drains empty rather than panicking"
    );
    assert!(!INBOX_PENDING.load(Ordering::Acquire));
    INBOX.clear_poison();
    assert!(
        take_queued().is_empty(),
        "the flag is down: nothing is read until a queue raises it"
    );
    queue_message(msg("after the poison"));
    let taken = take_queued();
    assert_eq!(
        taken
            .iter()
            .map(|m| m.message.title.as_str())
            .collect::<Vec<_>>(),
        ["before the poison", "after the poison"],
        "what the poison stranded comes out ahead of what followed it"
    );
}

/// A test that reads lane `0` runs where no other test queued into it: a message another
/// test of the lib-test process queued on lane `0` meanwhile is not in its drain.
#[test]
fn a_lane_zero_test_drains_no_message_another_test_queued() {
    const STRAY: &str = "another test's message on lane 0";
    if !crate::test_alone::in_child() {
        queue_message(msg(STRAY));
    }
    if !lane_zero_alone() {
        return;
    }
    assert!(
        take_queued().iter().all(|m| m.message.title != STRAY),
        "a lane 0 drain took another test's message"
    );
}

fn row(seq: u64) -> crate::harness_host::RestoredRow {
    crate::harness_host::RestoredRow {
        seq,
        lines: Vec::new(),
        log: None,
    }
}

fn rows_in(taken: &[InboxMessage]) -> Vec<u64> {
    taken
        .iter()
        .filter_map(|m| m.restored.as_ref().map(|r| r.seq))
        .collect()
}

/// However many rows are said between two parks, and however full the lane is, the window
/// gets the latest, the one that names every tab, and no other message gives way to it.
#[test]
fn the_latest_restored_row_always_reaches_the_window() {
    let _lane = own_lane_for_test();
    let lane = current_lane();
    let many = u64::try_from(INBOX_CAP * 2).expect("small");
    for seq in 1..=many {
        queue_restored_row(lane, row(seq));
    }
    let taken = take_queued();
    assert_eq!(
        rows_in(&taken).last(),
        Some(&many),
        "more rows than the cap"
    );
    assert!(taken.len() <= INBOX_CAP, "the cap holds: {}", taken.len());

    for i in 0..INBOX_CAP {
        queue_message(msg(&format!("warning {i}")));
    }
    queue_restored_row(lane, row(20));
    queue_restored_row(lane, row(22));
    queue_restored_row(lane, row(21));
    let taken = take_queued();
    assert_eq!(
        rows_in(&taken),
        [22],
        "a lane full of other messages keeps only the latest, whichever was said last"
    );
    assert_eq!(taken.len(), INBOX_CAP + 1, "no message gave way to the row");
}
