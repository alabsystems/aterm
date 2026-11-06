// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::auth_deadline::{Deadline, EXPIRED, Expired, Stream};
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

fn checked_socket(socket: &TcpStream) {
    socket.set_read_timeout(Some(HANG)).unwrap();
    socket.set_write_timeout(Some(HANG)).unwrap();
}

#[test]
fn copies_and_local_limits_share_the_original_expiration() {
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let original = Deadline::new(Duration::from_secs(10), &clock);
    let copied = original;
    now.set(start + Duration::from_secs(4));
    let shorter = copied.limited(Duration::from_secs(2));
    let longer = copied.limited(Duration::from_secs(30));
    assert_eq!(original.end(), start + Duration::from_secs(10));
    assert_eq!(copied.end(), original.end());
    assert_eq!(longer.end(), original.end());
    assert_eq!(shorter.end(), start + Duration::from_secs(6));
    assert_eq!(original.remaining(), Ok(Duration::from_secs(6)));
    assert_eq!(
        original.capped(Duration::from_secs(3)),
        Ok(Duration::from_secs(3))
    );
    assert_eq!(
        original.capped(Duration::from_secs(30)),
        original.remaining()
    );
    now.set(shorter.end());
    assert_eq!(shorter.remaining(), Err(Expired));
    assert_eq!(original.remaining(), Ok(Duration::from_secs(4)));
    now.set(original.end());
    assert!(original.expired());
    assert_eq!(copied.left(), Err(EXPIRED));
    assert_eq!(longer.remaining(), Err(Expired));
    assert_eq!(
        original.limited(Duration::from_secs(30)).remaining(),
        Err(Expired)
    );
}

#[test]
fn zero_and_unrepresentable_budgets_are_already_expired() {
    let now = Instant::now();
    let clock = || now;
    let zero = Deadline::new(Duration::ZERO, &clock);
    assert_eq!(zero.end(), now);
    assert_eq!(zero.remaining(), Err(Expired));
    assert_eq!(zero.pause(Duration::ZERO), Err(Expired));

    assert!(now.checked_add(Duration::MAX).is_none());
    let overflow = Deadline::new(Duration::MAX, &clock);
    assert_eq!(overflow.end(), now);
    assert_eq!(overflow.remaining(), Err(Expired));
}

#[test]
fn an_expired_connect_does_not_reach_the_listener() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let now = Instant::now();
    let clock = || now;
    let expired = Deadline::new(Duration::ZERO, &clock);
    let error = Stream::connect(&listener.local_addr().unwrap(), Some(expired), HANG, HANG)
        .err()
        .expect("an expired connection was started");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn expired_stream_and_its_clone_do_not_send_more_bytes() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let start = Instant::now();
    let now = Cell::new(start);
    let clock = || now.get();
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let (received, receipt) = mpsc::sync_channel(1);
    let stream = Stream::connect(&addr, Some(deadline), HANG, HANG).unwrap();

    let (write_error, clone_error, read_error, flush_error, later) = std::thread::scope(|scope| {
        let mut stream = stream;
        let server = scope.spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            checked_socket(&socket);
            let mut first = [0; 4];
            socket.read_exact(&mut first).unwrap();
            socket.write_all(b"!").unwrap();
            received.send(first).unwrap();
            let mut later = Vec::new();
            // Closing the client with the deliberately unread byte can reset TCP.
            // Preserve any received bytes on that closure just as on ordinary EOF.
            if let Err(error) = socket.read_to_end(&mut later) {
                assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
            }
            later
        });
        let mut cloned = stream.try_clone().unwrap();
        stream.write_all(b"live").unwrap();
        assert_eq!(receipt.recv_timeout(HANG).unwrap(), *b"live");
        now.set(deadline.end());
        let write_error = stream.write_all(b"late").unwrap_err().kind();
        let clone_error = cloned.write_all(b"clone").unwrap_err().kind();
        let read_error = stream.read(&mut [0; 1]).unwrap_err().kind();
        let flush_error = stream.flush().unwrap_err().kind();
        drop(cloned);
        drop(stream);
        (
            write_error,
            clone_error,
            read_error,
            flush_error,
            server.join().unwrap(),
        )
    });
    for kind in [write_error, clone_error, read_error, flush_error] {
        assert_eq!(kind, io::ErrorKind::TimedOut);
    }
    assert!(
        later.is_empty(),
        "expired writes reached the peer: {later:?}"
    );
}

#[test]
fn read_to_end_checks_the_deadline_again_after_partial_input() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let start = Instant::now();
    let now = Cell::new(start);
    let expire_after_check = Cell::new(false);
    let expiry_notified = Cell::new(false);
    let end = start + Duration::from_secs(10);
    let (expired, expiry) = mpsc::sync_channel(1);
    let clock = || {
        let observed = now.get();
        if expire_after_check.replace(false) {
            now.set(end);
        }
        if observed == end && !expiry_notified.replace(true) {
            expired.try_send(()).unwrap();
        }
        observed
    };
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let (sent, receipt) = mpsc::sync_channel(1);
    let stream = Stream::connect(&addr, Some(deadline), HANG, HANG).unwrap();
    let (bytes, error, from_client) = std::thread::scope(|scope| {
        let mut stream = stream;
        let server = scope.spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            checked_socket(&socket);
            socket.write_all(b"x").unwrap();
            sent.send(()).unwrap();
            // The second byte only becomes available when the next clock check
            // observes expiry. Ignoring that expiry reads it, rather than merely
            // reaching a later socket timeout with the same error kind.
            expiry.recv_timeout(HANG).unwrap();
            let _ = socket.write_all(b"y");
            let _ = socket.shutdown(std::net::Shutdown::Write);
            let mut from_client = Vec::new();
            if let Err(error) = socket.read_to_end(&mut from_client) {
                assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
            }
            from_client
        });
        receipt.recv_timeout(HANG).unwrap();
        expire_after_check.set(true);
        let mut bytes = Vec::new();
        let error = stream.read_to_end(&mut bytes).unwrap_err().kind();
        drop(stream);
        (bytes, error, server.join().unwrap())
    });
    assert_eq!(bytes, b"x");
    assert_eq!(error, io::ErrorKind::TimedOut);
    assert!(from_client.is_empty());
    assert_eq!(deadline.remaining(), Err(Expired));
}

/// A POLL'S DEADLINE MADE FROM A SIGN-IN'S PASSES WHEN THE SIGN-IN'S DOES (KI-1105-14): from an
/// hour's sign-in deadline whose host then slept two hours, it has expired, leaves nothing,
/// pauses for nothing, and hands a mail leaf no later end than now; so does a sub-limit taken
/// from it. CONTROL: before the sleep it leaves most of the hour and its end is about an hour
/// off. NEGATIVE CONTROL: a deadline that measured what the sign-in's left when made on the clock
/// that stops in sleep alone left the hour, and kept handing leaves an end an hour away.
#[cfg(unix)]
#[test]
fn a_deadline_made_from_a_sign_ins_passes_when_the_host_slept_past_it() {
    use super::wake_deadline::{SleepSeam, WakeDeadline, host_slept};
    const HOUR: Duration = Duration::from_secs(3600);
    let _seam = SleepSeam::hold();
    let clock = Instant::now;
    let poll = Deadline::waking(WakeDeadline::after(HOUR), &clock);
    let sub = poll.limited(2 * HOUR);
    assert!(!poll.expired(), "CONTROL: an hour's, just taken");
    assert!(poll.remaining().is_ok_and(|left| left > HOUR / 2));
    assert!(poll.end() > Instant::now() + HOUR / 2, "CONTROL: its end");
    assert!(poll.counts_sleep());

    host_slept(2 * HOUR);
    assert!(poll.expired(), "the host slept past the sign-in's deadline");
    assert_eq!(poll.remaining(), Err(Expired));
    assert_eq!(poll.left(), Err(EXPIRED));
    assert_eq!(poll.pause(Duration::from_secs(2)), Err(Expired));
    assert!(poll.end() <= Instant::now(), "a leaf gets no time");
    assert!(sub.expired() && sub.end() <= Instant::now(), "a sub-limit");
}
