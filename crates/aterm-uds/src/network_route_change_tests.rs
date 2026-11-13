// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::time::{Duration, Instant};

#[test]
fn poll_rounding_does_not_report_timeout_before_the_budget() {
    let routes = RouteChanges::open().expect("route socket");
    let start = Instant::now();
    let clock = Cell::new(start);
    let budget = Duration::from_micros(500);
    let ready = routes.readable_by(
        start + budget,
        || clock.get(),
        |_fd, millis| {
            let millis = u64::try_from(millis).expect("finite nonnegative poll timeout");
            clock.set(clock.get() + Duration::from_millis(millis));
            Ok(false)
        },
    );
    assert_eq!(ready, Some(false));
    assert!(clock.get().duration_since(start) >= budget);
}

#[test]
fn poll_rounding_expired_budget_does_not_poll_again() {
    let routes = RouteChanges::open().expect("network event socket");
    let start = Instant::now();
    for now in [start, start + Duration::from_nanos(1)] {
        assert_eq!(
            routes.readable_by(
                start,
                || now,
                |_fd, _millis| {
                    panic!("expired readiness must end even when messages remain queued")
                }
            ),
            Some(false)
        );
    }
}

#[test]
fn poll_rounding_interruption_at_the_deadline_stops_retrying() {
    let routes = RouteChanges::open().expect("network event socket");
    let start = Instant::now();
    let clock = Cell::new(start);
    let calls = Cell::new(0);
    let budget = Duration::from_millis(1);
    let ready = routes.readable_by(
        start + budget,
        || clock.get(),
        |_fd, _millis| {
            calls.set(calls.get() + 1);
            assert_eq!(calls.get(), 1, "no interrupted poll after expiry");
            clock.set(start + budget);
            Err(std::io::ErrorKind::Interrupted.into())
        },
    );
    assert_eq!(ready, Some(false));
    assert_eq!(calls.get(), 1);
}

#[test]
fn poll_rounding_interruption_does_not_restart_the_budget() {
    let routes = RouteChanges::open().expect("route socket");
    let start = Instant::now();
    let clock = Cell::new(start);
    let calls = Cell::new(0);
    let budget = Duration::from_micros(2500);
    let ready = routes.readable_by(
        start + budget,
        || clock.get(),
        |_fd, millis| {
            calls.set(calls.get() + 1);
            match calls.get() {
                1 => {
                    clock.set(clock.get() + Duration::from_micros(1500));
                    Err(std::io::ErrorKind::Interrupted.into())
                }
                2 => {
                    let millis = u64::try_from(millis).expect("finite nonnegative poll timeout");
                    clock.set(clock.get() + Duration::from_millis(millis));
                    Ok(false)
                }
                _ => panic!("unexpected extra poll"),
            }
        },
    );
    assert_eq!(ready, Some(false));
    assert_eq!(calls.get(), 2);
    assert_eq!(clock.get().duration_since(start), budget);
}

/// The kernel gives a process its route-change socket, close-on-exec, and a wait on it ends
/// by its bound whether or not a change came meanwhile.
#[test]
fn a_route_change_socket_opens_close_on_exec_and_its_wait_ends() {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let changes = RouteChanges::open().expect("the kernel's route socket");
    // SAFETY: F_GETFD reads the live descriptor's flags and touches no memory.
    let flags = unsafe { libc::fcntl(changes.as_fd().as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0, "{}", std::io::Error::last_os_error());
    assert_ne!(flags & libc::FD_CLOEXEC, 0, "a child would inherit it");
    let _came = changes.wait(std::time::Duration::from_millis(50));
}

/// Address, router and link-state events wake a reader; unrelated network activity does not.
#[cfg(target_os = "macos")]
#[test]
fn network_notifications_ignore_unrelated_activity() {
    let message = |subclass: u32, code: u32| {
        [24_u32, 1, 1, subclass, 37, code]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect::<Vec<_>>()
    };
    for (subclass, code) in [
        (1, 1),
        (1, 3),
        (1, 6),
        (1, 9),
        (1, 10),
        (2, 1),
        (2, 9),
        (2, 11),
        (2, 12),
        (2, 13),
        (2, 16),
        (2, 25),
        (6, 1),
        (6, 3),
        (6, 6),
    ] {
        assert!(
            names_a_change(&message(subclass, code)),
            "subclass={subclass} code={code}"
        );
    }
    for (subclass, code) in [
        (1, 8),
        (2, 7),
        (2, 8),
        (2, 18),
        (2, 20),
        (2, 21),
        (2, 22),
        (6, 7),
        (3, 1),
    ] {
        assert!(
            !names_a_change(&message(subclass, code)),
            "subclass={subclass} code={code}"
        );
    }
    assert!(!names_a_change(&[]));
    for at in [0, 4, 8] {
        let mut invalid = message(1, 1);
        invalid[at..at + 4].copy_from_slice(&99_u32.to_ne_bytes());
        assert!(!names_a_change(&invalid));
    }
    assert!(!names_a_change(&message(1, 1)[..23]));
}

/// One netlink message of type `kind` whose fixed header of `fixed` bytes is followed by
/// `attributes` (each a type and its payload), padded as the kernel pads them.
#[cfg(target_os = "linux")]
fn netlink_message(kind: u16, fixed: usize, attributes: &[(u16, &[u8])]) -> Vec<u8> {
    let mut body = vec![0_u8; fixed];
    for (attribute, payload) in attributes {
        let length = u16::try_from(4 + payload.len()).expect("a short attribute");
        body.extend(length.to_ne_bytes());
        body.extend(attribute.to_ne_bytes());
        body.extend(*payload);
        body.resize(body.len().next_multiple_of(4), 0);
    }
    let length = u32::try_from(netlink::HEADER + body.len()).expect("a short message");
    let mut message = length.to_ne_bytes().to_vec();
    message.extend(kind.to_ne_bytes());
    message.extend([0_u8; 10]);
    message.extend(body);
    message
}

/// Linux: a wireless extensions event and an address's lifetime refresh, which the kernel
/// sends of a link and an address that stay as they were (a radio joined to nothing, every
/// router advertisement), wake no reader, alone or together in one datagram; a link's own
/// change, an address added (both stamps equal), one removed, a route, and a datagram that
/// does not parse each do, and so does any datagram holding one of them.
/// NEGATIVE CONTROLS: a read that counts every message (as before) wakes on the scan and
/// the refresh; one that drops every address message misses an address added; one that
/// drops every link message misses a carrier change.
#[cfg(target_os = "linux")]
#[test]
fn netlink_notifications_ignore_scans_and_lifetime_refreshes() {
    use netlink::{IFA_CACHEINFO, IFADDRMSG, IFINFOMSG, IFLA_WIRELESS, RTM_NEWADDR, RTM_NEWLINK};
    const IFLA_IFNAME: u16 = 3;
    const IFA_ADDRESS: u16 = 1;
    const RTM_DELADDR: u16 = 21;
    const RTM_NEWROUTE: u16 = 24;
    let cacheinfo = |created: u32, updated: u32| -> Vec<u8> {
        [1800_u32, 1800, created, updated]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect()
    };
    let address = [
        0x20_u8, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
    ];
    let scan = netlink_message(
        RTM_NEWLINK,
        IFINFOMSG,
        &[(IFLA_IFNAME, b"wlP9s9\0"), (IFLA_WIRELESS, &[0; 8])],
    );
    let refresh = netlink_message(
        RTM_NEWADDR,
        IFADDRMSG,
        &[
            (IFA_ADDRESS, &address),
            (IFA_CACHEINFO, &cacheinfo(1776, 479_815_188)),
        ],
    );
    let carrier = netlink_message(RTM_NEWLINK, IFINFOMSG, &[(IFLA_IFNAME, b"enP7s7\0")]);
    let added = netlink_message(
        RTM_NEWADDR,
        IFADDRMSG,
        &[
            (IFA_ADDRESS, &address),
            (IFA_CACHEINFO, &cacheinfo(5000, 5000)),
        ],
    );
    let unstamped = netlink_message(RTM_NEWADDR, IFADDRMSG, &[(IFA_ADDRESS, &address)]);
    let removed = netlink_message(
        RTM_DELADDR,
        IFADDRMSG,
        &[
            (IFA_ADDRESS, &address),
            (IFA_CACHEINFO, &cacheinfo(1776, 479_815_188)),
        ],
    );
    let route = netlink_message(RTM_NEWROUTE, 12, &[]);
    let both = [scan.clone(), refresh.clone()].concat();
    for quiet in [&scan, &refresh, &both] {
        assert!(!names_a_change(quiet), "{quiet:?}");
    }
    assert!(!names_a_change(&[]));
    for moved in [&carrier, &added, &unstamped, &removed, &route] {
        assert!(names_a_change(moved), "{moved:?}");
        assert!(
            names_a_change(&[both.clone(), moved.clone()].concat()),
            "{moved:?}"
        );
        assert!(
            names_a_change(&[moved.clone(), both.clone()].concat()),
            "{moved:?}"
        );
    }
    let mut long = scan.clone();
    long[..4].copy_from_slice(&u32::try_from(scan.len() + 4).unwrap().to_ne_bytes());
    let mut short = refresh.clone();
    short[..4].copy_from_slice(&8_u32.to_ne_bytes());
    for garbled in [&scan[..12], &long[..], &short[..]] {
        assert!(names_a_change(garbled), "{garbled:?}");
    }
}

/// A wait's length beside a flood: long against a poll's millisecond, short for a test.
#[cfg(target_os = "macos")]
const FLOODED: std::time::Duration = std::time::Duration::from_millis(300);

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a stand-in's send found its peer's queue full: macOS answers `ENOBUFS` for a
/// local datagram socket, blocking or not, where a stream says it would block.
#[cfg(target_os = "macos")]
fn queue_full(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::ENOBUFS)
}

/// One kernel event message as `PF_SYSTEM` delivers it: Apple's, the network class's. On
/// Linux it reads as a netlink message of neither type that can report no change, so each
/// stands for a change there.
fn kernel_event(subclass: u32, code: u32) -> Vec<u8> {
    [24_u32, 1, 1, subclass, 37, code]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

/// The wait's contract over a socket a test writes: a flood of messages that name no change
/// is read and passed over to the deadline; a change ends the wait as one, the other
/// descriptor as itself, and a socket whose peer is gone as a failure, never a busy poll.
#[cfg(target_os = "macos")]
#[test]
fn a_route_wait_ends_on_a_change_the_other_descriptor_or_its_deadline() {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    kernel
        .set_nonblocking(true)
        .expect("a writer that never blocks");
    let changes = RouteChanges::over(heard.into());
    let (quiet, mut ring) = std::io::pipe().expect("the other descriptor");
    let flooding = std::sync::Arc::new(AtomicBool::new(true));
    let (flooded, floods) = std::sync::mpsc::channel();
    let (begun, beginning) = std::sync::mpsc::channel();
    let flood = {
        let flooding = std::sync::Arc::clone(&flooding);
        let kernel = kernel.try_clone().expect("the flood's end");
        std::thread::spawn(move || {
            let unrelated = kernel_event(3, 1);
            let mut sent = 0_u64;
            while flooding.load(Ordering::Acquire) {
                match kernel.send(&unrelated) {
                    Ok(_) => {
                        if sent == 0 {
                            let _ = begun.send(());
                        }
                        sent += 1;
                    }
                    Err(error) if queue_full(&error) => crate::deadline::poll_yield(),
                    Err(error) => panic!("the flood's send: {error}"),
                }
            }
            let _ = flooded.send(sent);
        })
    };
    beginning.recv_timeout(HANG).expect("the flood begins");
    let began = std::time::Instant::now();
    let woke = changes.wait_beside(FLOODED, Some(quiet.as_fd()));
    let waited = began.elapsed();
    flooding.store(false, Ordering::Release);
    let sent = floods.recv_timeout(HANG).expect("the flood ends");
    flood.join().expect("the flood ends cleanly");
    assert_eq!(woke, RouteWake::Timeout, "{sent} unrelated messages");
    assert!(
        waited >= FLOODED,
        "{sent} unrelated messages ended a {FLOODED:?} wait after {waited:?}"
    );
    // The flood may have filled the queue: the change waits for the room the wait's reads
    // make.
    let (told, tells) = std::sync::mpsc::channel();
    let teller = std::thread::spawn(move || {
        let change = kernel_event(1, 1);
        let sent = loop {
            match kernel.send(&change) {
                Err(error) if queue_full(&error) => crate::deadline::poll_yield(),
                sent => break sent,
            }
        };
        let _ = told.send(sent.map(|_| kernel));
    });
    assert_eq!(
        changes.wait_beside(HANG, Some(quiet.as_fd())),
        RouteWake::Change
    );
    let kernel = tells
        .recv_timeout(HANG)
        .expect("the change is told")
        .expect("an address change");
    teller.join().expect("the teller ends cleanly");
    ring.write_all(&[1]).expect("ring the other descriptor");
    assert_eq!(
        changes.wait_beside(HANG, Some(quiet.as_fd())),
        RouteWake::Also
    );
    // A zero wait looks once: at nothing waiting, then at a change already queued.
    assert_eq!(
        changes.wait_beside(std::time::Duration::ZERO, None),
        RouteWake::Timeout
    );
    kernel.send(&kernel_event(2, 13)).expect("a link coming up");
    assert_eq!(
        changes.wait_beside(std::time::Duration::ZERO, None),
        RouteWake::Change
    );
    let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
    drop(peer);
    let failed = RouteChanges::over(heard.into());
    assert_eq!(failed.wait_beside(HANG, None), RouteWake::Failed);
    let began = std::time::Instant::now();
    assert!(!failed.wait(FLOODED), "a failed socket tells no change");
    assert!(
        began.elapsed() >= FLOODED,
        "a failed socket's wait is waited out"
    );
}

/// Whatever poll(2) reports of either descriptor, an error, a hangup or an invalid
/// descriptor with no POLLIN included, the wait takes at the poll that reported it, and never
/// polls again at once with it untaken, which would spin to the deadline: the route socket is
/// read and the read decides (a change queued is `Change`, its peer gone is `Failed`), and
/// the other descriptor ends the wait as `Also`.
#[test]
fn a_wait_takes_any_report_of_either_descriptor_at_once() {
    use std::os::fd::AsFd as _;
    // One poll that reports `route` and `also`; a second means the first was passed over.
    let once = |route: libc::c_short, also: libc::c_short| {
        let polls = Cell::new(0);
        move |fds: &mut [libc::pollfd; 2], _millis: libc::c_int| -> std::io::Result<()> {
            polls.set(polls.get() + 1);
            assert_eq!(
                polls.get(),
                1,
                "polled again at once after reports {route:#x} and {also:#x}"
            );
            fds[0].revents = route;
            fds[1].revents = also;
            Ok(())
        }
    };
    let (quiet, _writer) = std::io::pipe().expect("the other descriptor");
    for report in [libc::POLLERR, libc::POLLHUP, libc::POLLNVAL] {
        let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
        let witness = heard.try_clone().expect("a second handle");
        witness
            .set_nonblocking(true)
            .expect("a witness that never blocks");
        let changes = RouteChanges::over(heard.into());
        kernel.send(&kernel_event(1, 1)).expect("an address change");
        assert_eq!(
            changes.until_by(Instant::now() + HANG, None, once(report, 0)),
            RouteWake::Change,
            "a route socket that reported {report:#x} beside a change queued"
        );
        assert_eq!(
            witness.recv(&mut [0; 64]).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::WouldBlock),
            "the read took what the socket held"
        );
        let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
        drop(peer);
        let gone = RouteChanges::over(heard.into());
        assert_eq!(
            gone.until_by(Instant::now() + HANG, None, once(report, 0)),
            RouteWake::Failed,
            "a route socket that reported {report:#x} and whose read fails"
        );
        assert_eq!(
            changes.until_by(Instant::now() + HANG, Some(quiet.as_fd()), once(0, report)),
            RouteWake::Also,
            "the other descriptor that reported {report:#x}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn kernel_notification_socket_filters_before_delivery() {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let changes = RouteChanges::open().expect("kernel network notification socket");
    let mut filter = [0_u32; 3];
    // SAFETY: SIOCGKEVFILT writes kern_event.h's three-u32 kev_request to this live buffer.
    let read = unsafe {
        libc::ioctl(
            changes.as_fd().as_raw_fd(),
            0x400c_6503 as libc::c_ulong,
            filter.as_mut_ptr(),
        )
    };
    assert_eq!(read, 0, "{}", std::io::Error::last_os_error());
    assert_eq!(filter, [1, 1, 0]);
}
