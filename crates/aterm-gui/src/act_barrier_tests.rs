// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Countdown;

fn waiter() -> (
    std::sync::mpsc::SyncSender<()>,
    std::sync::mpsc::Receiver<()>,
) {
    std::sync::mpsc::sync_channel(1)
}

#[test]
fn a_barrier_asked_over_an_empty_queue_goes_at_once() {
    let mut countdown = Countdown::default();
    let (tx, _rx) = waiter();
    assert_eq!(countdown.ask(vec![tx], Some(0)).map(|v| v.len()), Some(1));
    let (tx, _rx) = waiter();
    assert_eq!(
        countdown.ask(vec![tx], None).map(|v| v.len()),
        Some(1),
        "a queue that cannot be read owes nothing"
    );
    assert!(
        countdown.ask(Vec::new(), Some(9)).is_none(),
        "no ask, no barrier"
    );
}

#[test]
fn a_barrier_waits_for_the_bytes_queued_when_it_was_asked() {
    let mut countdown = Countdown::default();
    assert!(
        countdown
            .sent(100, || unreachable!("nothing owed"))
            .is_empty()
    );
    let (tx, _rx) = waiter();
    assert!(
        countdown.ask(vec![tx], Some(50)).is_none(),
        "50 bytes are owed first"
    );
    assert!(
        countdown.sent(49, || false).is_empty(),
        "one byte is still owed"
    );
    assert_eq!(
        countdown.sent(1, || false).len(),
        1,
        "its bytes have all gone"
    );
    assert!(
        countdown
            .sent(10, || unreachable!("nothing owed"))
            .is_empty()
    );
}

#[test]
fn an_emptied_queue_releases_every_barrier_owed() {
    let mut countdown = Countdown::default();
    let (a, _ra) = waiter();
    let (b, _rb) = waiter();
    assert!(countdown.ask(vec![a], Some(10)).is_none());
    assert!(countdown.ask(vec![b], Some(1_000)).is_none());
    assert_eq!(countdown.sent(10, || false).len(), 1, "the first is due");
    assert_eq!(
        countdown.sent(5, || true).len(),
        1,
        "bytes the kernel dropped never come: an empty master releases the rest"
    );
}
