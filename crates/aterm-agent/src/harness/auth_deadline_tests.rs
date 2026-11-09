// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::auth_deadline::{Deadline, EXPIRED, Expired, Stream, io_with_budget};
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

#[cfg(unix)]
#[test]
fn original_wake_clock_domain_survives_limits_native_projection_and_restored_bounds() {
    use super::wake_deadline::{SleepSeam, WakeDeadline, host_slept};
    const HOUR: Duration = Duration::from_secs(3600);
    let _seam = SleepSeam::hold();
    host_slept(2 * HOUR); // A nonzero parent clock domain at birth.
    let awake = Instant::now();
    let clock = || awake;
    let parent = WakeDeadline::after(HOUR);
    let wrapped = Deadline::waking(parent, &clock);
    let limited = wrapped.limited(HOUR / 2);
    let native = limited
        .wake_parent()
        .expect("original parent is transferable");
    let bounds = limited.bounds();
    let raw_now = aterm_uds::process::continuous_ns();
    assert!(!wrapped.expired() && !limited.expired() && !native.passed());
    assert!(
        !bounds.remaining(awake, raw_now).is_zero(),
        "raw and parent clocks have separate domains"
    );
    assert!(native.remaining() <= HOUR / 2);
    let zero = wrapped.limited(Duration::ZERO).wake_parent().unwrap();
    assert!(
        zero.passed(),
        "a local zero limit cannot restart the parent"
    );
    host_slept(2 * HOUR);
    assert!(wrapped.expired() && limited.expired() && native.passed());
    assert!(
        bounds.remaining(awake, raw_now).is_zero(),
        "native authority retains the original parent"
    );
    assert!(
        limited.wake_parent().unwrap().passed(),
        "expired parent is not mistaken for absent parent"
    );
    let continuous = || raw_now;
    let injected = Deadline::with_clocks(HOUR, &clock, &continuous);
    assert!(
        injected.wake_parent().is_none(),
        "an arbitrary borrowed clock cannot be silently transferred"
    );
}

#[test]
fn an_external_monotonic_ceiling_is_never_restarted() {
    let end = Instant::now() + HANG;
    let deadline = Deadline::until(end);
    assert_eq!(deadline.end(), end);
    assert!(deadline.io_end() <= end);
    assert!(Deadline::until(Instant::now()).expired());
}

#[test]
fn sleep_expires_copies_and_limited_budgets_without_advancing_the_awake_clock() {
    let awake = Instant::now();
    let slept = Cell::new(Some(0));
    let clock = || awake;
    let continuous = || slept.get();
    let original = Deadline::with_clocks(Duration::from_secs(3600), &clock, &continuous);
    let copied = original;
    let limited = original.limited(Duration::from_secs(30));
    assert_eq!(original.remaining(), Ok(Duration::from_secs(3600)));
    assert_eq!(limited.remaining(), Ok(Duration::from_secs(30)));
    slept.set(Some(7200 * 1_000_000_000));
    for budget in [original, copied, limited, original.limited(HANG)] {
        assert_eq!(budget.remaining(), Err(Expired));
        assert!(budget.expired());
    }
    // Unsupported clocks retain the same awake-clock boundary, not a synthetic timeout.
    let unavailable = || None;
    let fallback = Deadline::with_clocks(HANG, &clock, &unavailable);
    assert_eq!(fallback.remaining(), Ok(HANG));
}

#[test]
fn a_socket_clock_sample_is_not_an_io_failure_and_sleep_ends_the_original_wait() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    for sleeps in [false, true] {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut client = TcpStream::connect_timeout(&listener.local_addr().unwrap(), HANG).unwrap();
        let (mut peer, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        checked_socket(&peer);
        let slept = Arc::new(AtomicU64::new(0));
        let clock = slept.clone();
        let (sampled, sample) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let continuous = || Some(clock.load(Ordering::Acquire));
            let deadline =
                Deadline::with_clocks(Duration::from_secs(3600), &Instant::now, &continuous);
            let mut notified = false;
            let answer = io_with_budget(Some(deadline), HANG, |within| {
                client.set_read_timeout(Some(within))?;
                let read = client.read(&mut [0]);
                if !notified
                    && read.as_ref().is_err_and(|e| {
                        matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                        )
                    })
                {
                    notified = true;
                    let _ = sampled.send(());
                }
                read
            });
            let _ = done.send(answer.map_err(|e| e.kind()));
        });
        let sampled = sample.recv_timeout(HANG);
        if sleeps {
            slept.store(7200 * 1_000_000_000, Ordering::Release);
        } else {
            let _ = peer.write_all(b"x");
        }
        let answer = result.recv_timeout(HANG);
        // EOF cooperatively releases the real operation before any failure assertion.
        drop(peer);
        worker.join().unwrap();
        assert!(sampled.is_ok(), "the real socket wait sampled its clock");
        assert_eq!(
            answer.unwrap(),
            if sleeps {
                Err(io::ErrorKind::TimedOut)
            } else {
                Ok(1)
            }
        );
    }
}

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
    let now = Instant::now();
    let clock = || now;
    let expired = Deadline::new(Duration::ZERO, &clock);
    let error = Stream::connect(&listener.local_addr().unwrap(), Some(expired), HANG, HANG)
        .err()
        .expect("an expired connection was started");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap_err()
        .kind(),
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
            let (mut socket, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
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
    let expiry_notified = Cell::new(false);
    let end = start + Duration::from_secs(10);
    let (expired, expiry) = mpsc::sync_channel(1);
    let clock = || now.get();
    let read = |n| {
        if n > 0 && !expiry_notified.replace(true) {
            now.set(end);
            expired.try_send(()).unwrap();
        }
    };
    let deadline = Deadline::new(Duration::from_secs(10), &clock);
    let (sent, receipt) = mpsc::sync_channel(1);
    let stream = Stream::connect(&addr, Some(deadline), HANG, HANG).unwrap();
    let (bytes, error, from_client) = std::thread::scope(|scope| {
        let mut stream = stream;
        stream.observe_read(&read);
        let server = scope.spawn(move || {
            let (mut socket, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            checked_socket(&socket);
            socket.write_all(b"x").unwrap();
            sent.send(()).unwrap();
            // The second byte only becomes available after the actual first read
            // advances the budget. Ignoring that expiry reads it, rather than merely
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
