// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MAX_DRAIN_MESSAGES, RouteChanges};
use std::collections::VecDeque;
use std::io;

// A macOS address event; Linux's subscribed groups classify any nonempty read as a change.
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
