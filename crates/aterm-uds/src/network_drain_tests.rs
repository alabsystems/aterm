// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MAX_DRAIN_MESSAGES, RouteChanges};
use std::collections::VecDeque;
use std::io;

// A macOS address event; on Linux a netlink message of neither type that can report no change.
fn change_message() -> Vec<u8> {
    [24_u32, 1, 1, 1, 37, 1]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

#[cfg(target_os = "macos")]
fn unrelated_message() -> Vec<u8> {
    [24_u32, 1, 1, 3, 37, 1]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

fn deliver(buffer: &mut [u8], message: &[u8]) -> io::Result<usize> {
    buffer[..message.len()].copy_from_slice(message);
    Ok(message.len())
}

#[test]
fn a_drain_yields_with_the_next_message_still_queued() {
    let cases = [
        (change_message(), true),
        #[cfg(target_os = "macos")]
        (unrelated_message(), false),
    ];
    for (message, changed) in cases {
        let mut queue = VecDeque::from(vec![message; MAX_DRAIN_MESSAGES + 1]);
        let mut calls = 0;
        let first = RouteChanges::drain_by(|buffer| {
            calls += 1;
            assert!(calls <= MAX_DRAIN_MESSAGES, "a busy stream must yield");
            deliver(buffer, &queue.pop_front().expect("queued message"))
        });
        assert_eq!(first, Some(changed));
        assert_eq!(calls, MAX_DRAIN_MESSAGES);
        assert_eq!(queue.len(), 1);

        let mut calls = 0;
        let next = RouteChanges::drain_by(|buffer| {
            calls += 1;
            assert!(calls <= 2, "stop when the remaining queue is empty");
            queue.pop_front().map_or_else(
                || Err(io::Error::from_raw_os_error(libc::EAGAIN)),
                |message| deliver(buffer, &message),
            )
        });
        assert_eq!(next, Some(changed));
        assert_eq!(calls, 2);
        assert!(queue.is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn unrelated_reads_do_not_erase_an_earlier_change() {
    let mut queue = VecDeque::from([change_message(), unrelated_message()]);
    let changed = RouteChanges::drain_by(|buffer| {
        queue.pop_front().map_or_else(
            || Err(io::Error::from_raw_os_error(libc::EAGAIN)),
            |message| deliver(buffer, &message),
        )
    });
    assert_eq!(changed, Some(true));
    assert!(queue.is_empty());
}

#[test]
fn transient_receive_errors_preserve_the_change_already_read() {
    for errno in [libc::EAGAIN, libc::EINTR] {
        for prior_change in [false, true] {
            let mut calls = 0;
            let changed = RouteChanges::drain_by(|buffer| {
                calls += 1;
                if prior_change && calls == 1 {
                    deliver(buffer, &change_message())
                } else {
                    assert_eq!(calls, 1 + usize::from(prior_change));
                    Err(io::Error::from_raw_os_error(errno))
                }
            });
            assert_eq!(changed, Some(prior_change), "errno {errno}");
            assert_eq!(calls, 1 + usize::from(prior_change));
        }
    }
}

#[test]
fn a_receive_overflow_is_a_change_even_without_a_message() {
    for prior_change in [false, true] {
        let mut calls = 0;
        let changed = RouteChanges::drain_by(|buffer| {
            calls += 1;
            if prior_change && calls == 1 {
                deliver(buffer, &change_message())
            } else {
                assert_eq!(calls, 1 + usize::from(prior_change));
                Err(io::Error::from_raw_os_error(libc::ENOBUFS))
            }
        });
        assert_eq!(changed, Some(true));
        assert_eq!(calls, 1 + usize::from(prior_change));
    }
}

#[test]
fn eof_and_fatal_receive_errors_keep_a_change_or_report_failure() {
    for errno in [None, Some(libc::EBADF), Some(libc::EIO)] {
        for prior_change in [false, true] {
            let mut calls = 0;
            let changed = RouteChanges::drain_by(|buffer| {
                calls += 1;
                if prior_change && calls == 1 {
                    deliver(buffer, &change_message())
                } else {
                    assert_eq!(calls, 1 + usize::from(prior_change));
                    errno.map_or(Ok(0), |errno| Err(io::Error::from_raw_os_error(errno)))
                }
            });
            assert_eq!(changed, prior_change.then_some(true), "errno {errno:?}");
            assert_eq!(calls, 1 + usize::from(prior_change));
        }
    }
}

/// An ask without waiting ([`RouteChanges::changed_now`]) reads one bounded batch and says what
/// it found: nothing queued and messages that name no change are no change, a change is one,
/// and a socket at its end is a failure. A batch that stopped at its bound with the queue not
/// yet empty is a change too, since one may wait among what it left: the owner that keeps an
/// answer while no change came must not keep it past a queue it did not finish reading.
///
/// NEGATIVE CONTROL (2026-11-12): with the full batch judged by its messages alone
/// (`Some(changed)` in `changed_now_by`), the first ask of the flooded queue says no change
/// (`a batch that left the queue unread`).
#[test]
fn an_ask_without_waiting_counts_a_queue_it_did_not_empty_as_a_change() {
    let ask = |queue: &mut VecDeque<Vec<u8>>, end: Option<i32>| {
        RouteChanges::changed_now_by(|buffer| {
            queue.pop_front().map_or_else(
                || end.map_or(Ok(0), |errno| Err(io::Error::from_raw_os_error(errno))),
                |message| deliver(buffer, &message),
            )
        })
    };
    let empty = Some(libc::EAGAIN);
    assert_eq!(
        ask(&mut VecDeque::new(), empty),
        Some(false),
        "nothing queued"
    );
    assert_eq!(
        ask(&mut VecDeque::from([change_message()]), empty),
        Some(true),
        "a change queued"
    );
    assert_eq!(ask(&mut VecDeque::new(), None), None, "a socket at its end");
    assert_eq!(
        ask(&mut VecDeque::new(), Some(libc::ENOBUFS)),
        Some(true),
        "an overflow lost messages"
    );
    #[cfg(target_os = "macos")]
    {
        let mut one_short = VecDeque::from(vec![unrelated_message(); MAX_DRAIN_MESSAGES - 1]);
        assert_eq!(
            ask(&mut one_short, empty),
            Some(false),
            "unrelated messages read to the end of the queue"
        );
        let mut flooded = VecDeque::from(vec![unrelated_message(); MAX_DRAIN_MESSAGES]);
        flooded.push_back(change_message());
        assert_eq!(
            ask(&mut flooded, empty),
            Some(true),
            "a batch that left the queue unread"
        );
        assert_eq!(flooded.len(), 1, "one batch, and no more, is read");
        assert_eq!(ask(&mut flooded, empty), Some(true), "the change it left");
        assert_eq!(
            ask(&mut flooded, empty),
            Some(false),
            "and nothing after it"
        );
    }
}
