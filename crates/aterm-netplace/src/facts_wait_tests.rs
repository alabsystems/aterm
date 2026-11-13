// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

struct Waited {
    changed: bool,
    elapsed: Duration,
    polls: Vec<Duration>,
    path_waits: Vec<Duration>,
}

/// Drive the production wait loops with elapsed work and events. A path wait
/// advances this clock; no assertion depends on the host's scheduling speed.
fn drive(events: &[(u64, Woke)]) -> Waited {
    let start = Instant::now();
    let clock = Cell::new(start);
    let polls = RefCell::new(Vec::new());
    let path_waits = RefCell::new(Vec::new());
    let mut events = events.iter();
    let changed = wait_on_routes(
        &PathState::default(),
        &AtomicBool::new(false),
        Duration::from_secs(10),
        || clock.get(),
        |left| {
            polls.borrow_mut().push(left);
            let (work, event) = events.next().expect("unexpected extra poll");
            clock.set(clock.get() + Duration::from_secs(*work));
            *event
        },
        |_held, left| {
            path_waits.borrow_mut().push(left);
            clock.set(clock.get() + left);
        },
    );
    assert!(events.next().is_none(), "every scripted event was consumed");
    Waited {
        changed,
        elapsed: clock.get().duration_since(start),
        polls: polls.into_inner(),
        path_waits: path_waits.into_inner(),
    }
}

#[test]
fn a_late_failure_waits_only_until_the_original_deadline() {
    let waited = drive(&[(7, Woke::Failed)]);
    assert!(!waited.changed);
    assert_eq!(waited.polls, [Duration::from_secs(10)]);
    assert_eq!(waited.path_waits, [Duration::from_secs(3)]);
    assert_eq!(waited.elapsed, Duration::from_secs(10));
}

#[test]
fn ignored_traffic_and_failure_share_one_deadline() {
    let waited = drive(&[(3, Woke::Routes(false)), (4, Woke::Failed)]);
    assert!(!waited.changed);
    assert_eq!(
        waited.polls,
        [Duration::from_secs(10), Duration::from_secs(7)]
    );
    assert_eq!(waited.path_waits, [Duration::from_secs(3)]);
    assert_eq!(waited.elapsed, Duration::from_secs(10));
}

#[test]
fn failure_after_the_deadline_adds_no_more_sleep() {
    let waited = drive(&[(12, Woke::Failed)]);
    assert!(!waited.changed);
    assert!(waited.path_waits.is_empty());
    assert_eq!(waited.elapsed, Duration::from_secs(12));
}

#[test]
fn a_real_change_returns_without_fallback_wait() {
    let waited = drive(&[(2, Woke::Routes(true))]);
    assert!(waited.changed);
    assert!(waited.path_waits.is_empty());
    assert_eq!(waited.elapsed, Duration::from_secs(2));
}

#[test]
fn unrelated_traffic_keeps_waiting_to_the_same_deadline() {
    let waited = drive(&[(4, Woke::Routes(false)), (6, Woke::Deadline)]);
    assert!(!waited.changed);
    assert_eq!(
        waited.polls,
        [Duration::from_secs(10), Duration::from_secs(6)]
    );
    assert!(waited.path_waits.is_empty());
    assert_eq!(waited.elapsed, Duration::from_secs(10));
}

#[test]
fn a_path_change_latched_during_route_failure_is_acknowledged() {
    let path = PathState::default();
    let start = Instant::now();
    let fallback_calls = Cell::new(0);
    let changed = wait_on_routes(
        &path,
        &AtomicBool::new(false),
        Duration::from_secs(10),
        || start,
        |_| {
            path.changed.store(true, Ordering::SeqCst);
            Woke::Failed
        },
        |_held, _left| fallback_calls.set(fallback_calls.get() + 1),
    );
    assert!(changed);
    assert_eq!(fallback_calls.get(), 0);
    assert!(!path.changed.load(Ordering::SeqCst));
}

#[test]
fn a_path_change_arriving_in_the_fallback_ends_the_wait() {
    let path = PathState::default();
    let start = Instant::now();
    let clock = Cell::new(start);
    let polls = Cell::new(0);
    let fallback_calls = Cell::new(0);
    let changed = wait_on_routes(
        &path,
        &AtomicBool::new(false),
        Duration::from_secs(10),
        || clock.get(),
        |_| {
            polls.set(polls.get() + 1);
            assert_eq!(polls.get(), 1, "a failed route is never polled again");
            Woke::Failed
        },
        |_held, _left| {
            fallback_calls.set(fallback_calls.get() + 1);
            clock.set(clock.get() + Duration::from_secs(2));
            path.changed.store(true, Ordering::SeqCst);
        },
    );
    assert!(changed);
    assert_eq!(clock.get().duration_since(start), Duration::from_secs(2));
    assert_eq!(fallback_calls.get(), 1);
    assert!(!path.changed.load(Ordering::SeqCst));
}

#[test]
fn poll_rounding_does_not_turn_a_remaining_budget_into_a_probe() {
    let routes = aterm_uds::network::RouteChanges::open().expect("route socket");
    let start = Instant::now();
    let clock = Cell::new(start);
    let calls = Cell::new(0);
    let budget = Duration::from_micros(500);
    let changed = wait_on_routes(
        &PathState::default(),
        &AtomicBool::new(false),
        budget,
        || clock.get(),
        |left| {
            poll_both_by(&routes, None, left, |_fds, millis| {
                calls.set(calls.get() + 1);
                assert!(
                    millis > 0,
                    "a positive budget must not become a zero-time probe"
                );
                assert_eq!(calls.get(), 1, "no immediate retry before the deadline");
                let millis = u64::try_from(millis).expect("finite nonnegative poll timeout");
                clock.set(clock.get() + Duration::from_millis(millis));
                Ok(())
            })
        },
        |_held, _left| panic!("a timeout needs no failed-route fallback"),
    );
    assert!(!changed);
    assert_eq!(calls.get(), 1);
    assert!(clock.get().duration_since(start) >= budget);
}

/// A route socket over a datagram pair a test writes, and a second handle on it that sees what
/// its reads left queued.
fn stand_in_routes() -> (
    aterm_uds::network::RouteChanges,
    std::os::unix::net::UnixDatagram,
    std::os::unix::net::UnixDatagram,
) {
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let witness = heard.try_clone().expect("a second handle");
    witness
        .set_nonblocking(true)
        .expect("a witness that never blocks");
    (
        aterm_uds::network::RouteChanges::over(heard.into()),
        kernel,
        witness,
    )
}

/// An address change in the kernel's format: macOS's network-class `KEV_INET_NEW_ADDR`, and
/// on Linux any message of the groups joined.
fn address_change() -> Vec<u8> {
    [24_u32, 1, 1, 1, 37, 1]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

/// Linux reports a netlink overflow (ENOBUFS) as POLLERR until a read takes it, with or without
/// POLLIN, and a socket nothing reads stays in error. Whatever the route socket's poll reports,
/// an error or a hangup alone included, the keeper's wait reads it and the read decides: what it
/// holds ends the wait as a change at the first poll, never a fall back to the path's timer and
/// never a poll again at once that leaves the report unread (a busy spin to the deadline), and
/// leaves nothing queued to end the next wait. Only a read that fails is a failure, and the
/// bell's hangup stays one.
#[test]
fn a_route_socket_polled_in_error_is_read_not_given_up() {
    for report in [libc::POLLERR, libc::POLLHUP, libc::POLLNVAL] {
        let polls = Cell::new(0);
        let reporting = |fds: &mut [libc::pollfd; 2], _millis| {
            polls.set(polls.get() + 1);
            assert_eq!(
                polls.get(),
                1,
                "a route socket that polled {report:#x} was polled again unread"
            );
            fds[0].revents = report;
            Ok(())
        };
        let (routes, kernel, witness) = stand_in_routes();
        kernel.send(&address_change()).expect("a change told");
        let start = Instant::now();
        let clock = Cell::new(start);
        let path_waits = RefCell::new(Vec::new());
        let changed = wait_on_routes(
            &PathState::default(),
            &AtomicBool::new(false),
            Duration::from_secs(10),
            || clock.get(),
            |left| poll_both_by(&routes, None, left, reporting),
            |_held, left| {
                path_waits.borrow_mut().push(left);
                clock.set(clock.get() + left);
            },
        );
        assert!(
            changed,
            "a route socket that polled {report:#x} was given up for {:?} of path waits",
            path_waits.borrow()
        );
        assert!(path_waits.borrow().is_empty(), "no fall back to the path");
        assert_eq!(
            witness.recv(&mut [0; 64]).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::WouldBlock),
            "the read took what the socket held"
        );
    }
    let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
    drop(peer);
    let gone = aterm_uds::network::RouteChanges::over(heard.into());
    let hung_up = |fds: &mut [libc::pollfd; 2], _millis| {
        fds[0].revents = libc::POLLHUP;
        Ok(())
    };
    assert_eq!(
        poll_both_by(&gone, None, Duration::ZERO, hung_up),
        Woke::Failed,
        "a socket whose read fails"
    );
    let (routes, _kernel, _witness) = stand_in_routes();
    let (hear, ring) = aterm_uds::pipe().expect("a bell");
    drop(ring);
    let bell_gone = |fds: &mut [libc::pollfd; 2], _millis| {
        fds[1].revents = libc::POLLHUP;
        Ok(())
    };
    assert_eq!(
        poll_both_by(
            &routes,
            Some(std::os::fd::AsFd::as_fd(&hear)),
            Duration::ZERO,
            bell_gone
        ),
        Woke::Failed,
        "a bell that hung up"
    );
}
