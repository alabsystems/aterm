// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Instant;

/// A connected worker, and a lost one with nothing to tell a keeper, wait with no deadline; a
/// lost one holding a master or owing a RELEASE waits for its next try, never less than `retry`.
#[test]
fn the_link_waits_with_no_deadline_but_for_a_keeper_it_lost_and_has_news_for() {
    let retry = LINK_POLL;
    let now = Instant::now();
    let past = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
    let later = now + Duration::from_secs(4);
    for (connected, dial_for, next_try) in [(true, true, past), (true, false, later)] {
        assert_eq!(
            wait_for(connected, dial_for, retry, next_try, now),
            None,
            "connected: every duty comes with a keeper frame's wake"
        );
    }
    assert_eq!(
        wait_for(false, false, retry, past, now),
        None,
        "lost with nothing held or owed: a wake would dial nobody"
    );
    assert_eq!(
        wait_for(false, true, retry, past, now),
        Some(retry),
        "lost just after a connection ended: the first try a full pause after its wake"
    );
    assert_eq!(
        wait_for(false, true, retry, later, now),
        Some(Duration::from_secs(4)),
        "lost after failed dials: the backoff's try, with no wake before it"
    );
    assert_eq!(
        wait_for(false, true, retry, now + retry / 2, now),
        Some(retry),
        "never two tries closer than the pace"
    );
}

/// A poll's timeout: none (-1) for no deadline, else the wait rounded up to whole milliseconds.
#[test]
fn a_wait_polls_for_its_deadline_rounded_up_and_with_none_for_none() {
    assert_eq!(
        poll_millis(None),
        -1,
        "no deadline is a poll with no timeout"
    );
    assert_eq!(poll_millis(Some(LINK_POLL)), 500);
    assert_eq!(
        poll_millis(Some(Duration::from_micros(499_001))),
        500,
        "rounded up"
    );
    assert_eq!(
        poll_millis(Some(Duration::from_micros(1))),
        1,
        "under a millisecond is not a poll that returns at once"
    );
    assert_eq!(poll_millis(Some(Duration::ZERO)), 0);
    assert_eq!(poll_millis(Some(Duration::from_secs(u64::MAX))), i32::MAX);
}

/// A wait with no deadline polls with none (-1), and still returns for the keeper's frame. The
/// waits run on a helper thread, so a wait the frame fails to end fails this test at the
/// one-minute hang detector rather than hanging it.
#[test]
fn a_wait_with_no_deadline_polls_with_none_and_wakes_for_the_keeper() {
    let (near, far) = aterm_uds::kept_out(CtlStream::pair).expect("a socket pair");
    let (tx, rx) = mpsc::sync_channel::<Queued>(1);
    let (bell, mut door) = doorbell().expect("a doorbell");
    // The keeper's frame is there before the wait begins: the poll returns at once, timing
    // nothing.
    let sent = aterm_uds::fdpass::send_vectored(&far, &[io::IoSlice::new(&[0])]).expect("a byte");
    assert_eq!(sent, 1);
    let (woke, waits) = mpsc::channel::<(bool, Option<i32>)>();
    let waiter = std::thread::spawn(move || {
        let keeper = KeeperClient {
            stream: near,
            frame_wait: aterm_spec::wait::MUST_HAPPEN,
            policy: IdentityPolicy::SameUid,
            keeper_caps: Cell::new(0),
            keeper_version: OnceCell::new(),
            syncs: Cell::new(0),
            synced: Cell::new(0),
            retiring: RefCell::new(Vec::new()),
        };
        for wait in [None, Some(LINK_POLL)] {
            POLLED_MS.with(|polled| polled.set(None));
            let keeper_woke = matches!(door.next(&rx, Some(&keeper), wait), Woke::Keeper);
            let polled = POLLED_MS.with(std::cell::Cell::get);
            if woke.send((keeper_woke, polled)).is_err() {
                return;
            }
        }
    });
    let wait = || {
        waits
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the keeper's frame ended the wait")
    };
    assert_eq!(
        wait(),
        (true, Some(-1)),
        "no deadline is a poll with no timeout, which the keeper's frame ends"
    );
    let (keeper_woke, polled) = wait();
    assert!(
        keeper_woke,
        "the keeper's frame ends a wait with a deadline"
    );
    // Zero included: a helper thread held off the CPU past the whole deadline before the poll
    // polls for none of it, and still hears the frame.
    assert!(
        polled.is_some_and(|ms| (0..=500).contains(&ms)),
        "control: a deadline is the poll's timeout, at most LINK_POLL: {polled:?}"
    );
    waiter.join().expect("the waiter");
    drop((tx, bell, far));
}
