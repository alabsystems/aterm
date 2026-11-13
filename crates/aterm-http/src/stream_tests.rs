// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// A wait for something that must happen is a hang detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

#[test]
fn cancellation_wakes_a_silent_socket_and_prevents_writes() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let mut stream = Stream::plain(
        tcp,
        Arc::new(AlwaysAuthorized),
        Deadline::after(HANG_DETECTOR),
    );
    let interrupt = stream.interruptor().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        let read = stream.read(&mut [0; 1]);
        let write = stream.write_all(b"must not reach the peer");
        done_tx.send((read, write)).unwrap();
    });
    ready_rx.recv_timeout(HANG_DETECTOR).unwrap();
    interrupt.interrupt();
    let (read, write) = done_rx.recv_timeout(HANG_DETECTOR).unwrap();
    assert!(
        matches!(read, Ok(0))
            || read.is_err_and(|error| matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
            ))
    );
    assert!(write.is_err());
    worker.join().unwrap();
}

#[derive(Debug)]
struct Revocable(AtomicBool);

impl Guard for Revocable {
    fn is_authorized(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn connected_pair() -> (TcpStream, TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let server = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap()
    .0;
    (client, server)
}

#[test]
fn plaintext_io_sets_only_its_socket_timeout_direction() {
    let (read_client, mut read_server) = connected_pair();
    read_server.write_all(b"r").unwrap();
    let mut read_stream = Stream::plain(
        read_client,
        Arc::new(AlwaysAuthorized),
        Deadline::after(HANG_DETECTOR),
    );
    let mut byte = [0_u8; 1];
    read_stream.read_exact(&mut byte).unwrap();
    let read_client = read_stream.into_tcp().unwrap();
    assert!(read_client.read_timeout().unwrap().is_some());
    assert_eq!(read_client.write_timeout().unwrap(), None);

    let (write_client, _write_server) = connected_pair();
    let mut write_stream = Stream::plain(
        write_client,
        Arc::new(AlwaysAuthorized),
        Deadline::after(HANG_DETECTOR),
    );
    write_stream.write_all(b"w").unwrap();
    write_stream.flush().unwrap();
    let write_client = write_stream.into_tcp().unwrap();
    assert_eq!(write_client.read_timeout().unwrap(), None);
    assert!(write_client.write_timeout().unwrap().is_some());
}

#[test]
fn a_revoked_guard_stops_writes_and_reads_on_an_open_socket() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = std::thread::spawn(move || {
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap()
        .0
    });
    let client = TcpStream::connect(addr).unwrap();
    let _server = accepted.join().unwrap();

    let guard = Arc::new(Revocable(AtomicBool::new(true)));
    let mut stream = Stream::plain(
        client,
        Arc::clone(&guard) as Arc<dyn Guard>,
        Deadline::after(HANG_DETECTOR),
    );
    assert!(stream.write(b"hello").is_ok());

    guard.0.store(false, Ordering::Release);
    let error = stream.write(b"secret").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let mut buf = [0u8; 4];
    assert_eq!(
        stream.read(&mut buf).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
}

/// Apple platforms refuse every `setsockopt` (EINVAL) on a socket a peer
/// reset, yet the bytes the peer sent before the reset stay readable: an
/// early answer (a 401 before the request was read) is read whole, and
/// only then the reset, never an invalid argument.
#[cfg(target_vendor = "apple")]
#[test]
fn an_answer_sent_before_a_reset_is_read_and_then_the_reset() {
    let (mut client, mut server) = connected_pair();
    client.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
    server.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
    client.write_all(b"unread").unwrap();
    let mut byte = [0_u8; 1];
    assert_eq!(server.peek(&mut byte).unwrap(), 1);
    let answer: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\n\r\nno";
    server.write_all(answer).unwrap();
    drop(server);
    let waited = Instant::now();
    while client.set_read_timeout(Some(HANG_DETECTOR)).is_ok() {
        assert!(waited.elapsed() < HANG_DETECTOR, "the reset never landed");
        aterm_uds::deadline::poll_yield();
    }

    let mut stream = Stream::plain(
        client,
        Arc::new(AlwaysAuthorized),
        Deadline::after(HANG_DETECTOR),
    );
    let mut got = Vec::new();
    let mut buf = [0_u8; 256];
    let end = loop {
        match stream.read(&mut buf) {
            Ok(0) => break None,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(error) => break Some(error),
        }
    };
    assert_eq!(got, answer);
    let error = end.expect("the reset after the answer, not an EOF");
    assert_eq!(error.kind(), io::ErrorKind::ConnectionReset, "{error:?}");
}

/// A failed lookup of a local name says what Linux resolves one with, keeping
/// its words and kind; a timeout and any other name keep the lookup's error.
#[cfg(unix)]
#[test]
fn a_local_name_linux_cannot_resolve_names_what_it_lacks() {
    let unknown = |host: &str| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("dns: {host}: failed to lookup address information: Name or service not known"),
        )
    };
    let named = without_mdns("alab-secrets.local", unknown("alab-secrets.local"));
    assert_eq!(named.kind(), io::ErrorKind::NotFound);
    assert_eq!(
        named.to_string(),
        "dns: alab-secrets.local: failed to lookup address information: Name or service not \
         known (this system resolves .local names over mDNS only through Avahi's nss-mdns or \
         systemd-resolved with MulticastDNS on; a URL naming the server's address needs \
         neither)"
    );
    let held = io::Error::new(io::ErrorKind::TimedOut, "dns: no answer within 8000ms");
    for (host, error) in [
        ("alab-secrets.local", held),
        ("github.com", unknown("github.com")),
    ] {
        let (kind, text) = (error.kind(), error.to_string());
        let kept = without_mdns(host, error);
        assert_eq!((kept.kind(), kept.to_string()), (kind, text), "{host}");
    }
}

/// Each address the lookup gave gets an even share of what the deadline has left and the
/// last all of it: two stale answers that never answer leave the live one its turn, each
/// told as timed out.
/// NEGATIVE CONTROL: give each address the whole of what is left and the first takes it.
#[test]
fn each_address_gets_its_share_of_the_deadline_and_the_last_the_rest() {
    let original = Instant::now();
    let expired = Deadline::after(HANG_DETECTOR).until(original);
    assert!(
        expired.remaining().is_none(),
        "request construction cannot renew the original bound"
    );
    assert!(expired.connect_phase(HANG_DETECTOR).remaining().is_none());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let live = listener.local_addr().expect("addr");
    // TEST-NET-1: never dialled, a stale answer's stand-in.
    let stale: [SocketAddr; 2] = [
        "192.0.2.1:7484".parse().expect("addr"),
        "192.0.2.2:7484".parse().expect("addr"),
    ];
    let given = std::cell::RefCell::new(Vec::new());
    let mut told = Vec::new();
    let stream = connect_each(
        "alab-secrets.local",
        &[stale[0], stale[1], live],
        &Deadline::after(HANG_DETECTOR),
        |addr, within| {
            given.borrow_mut().push(within);
            if *addr == live {
                crate::connect::connect_timeout(addr, within)
            } else {
                Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
            }
        },
        &mut |addr, error| told.push((addr, error.kind())),
    )
    .expect("the live address connects");
    assert_eq!(stream.peer_addr().expect("a peer"), live);
    let given = given.into_inner();
    assert_eq!(given.len(), 3, "{given:?}");
    assert!(given[0] <= HANG_DETECTOR / 3, "a third: {given:?}");
    assert!(
        given[1] <= HANG_DETECTOR / 2,
        "half of what is left: {given:?}"
    );
    assert!(
        given[2] > HANG_DETECTOR / 2,
        "the last gets the rest: {given:?}"
    );
    assert_eq!(
        told,
        [
            (stale[0], io::ErrorKind::TimedOut),
            (stale[1], io::ErrorKind::TimedOut)
        ]
    );
}

/// A lookup's addresses take the port asked for; a failed one is named as the resolution's
/// with its kind kept, and a missing LAN broker reads as not connected, its words naming what
/// installs it.
#[cfg(unix)]
#[test]
fn a_lookup_takes_the_port_and_names_a_failure_by_its_host() {
    let ip: IpAddr = "192.0.2.7".parse().unwrap();
    assert_eq!(
        lookup("alab-secrets.local", 7484, HANG_DETECTOR, |_, _| Ok(vec![
            ip
        ]))
        .unwrap(),
        [SocketAddr::new(ip, 7484)]
    );
    let error = lookup("example.invalid", 443, HANG_DETECTOR, |_, _| {
        Err(io::Error::new(io::ErrorKind::NotFound, "no such host"))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .starts_with("dns: example.invalid: no such host"),
        "{error}"
    );
    #[cfg(unix)]
    {
        let error = lookup("alab-secrets.local", 7484, HANG_DETECTOR, |_, _| {
            Err(
                aterm_uds::lan::LanError::Unavailable(aterm_uds::lan::Unavailable::NotInstalled)
                    .into_io(),
            )
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotConnected, "{error}");
    }
}

/// The connector dials what its resolver gave, within what the deadline has left.
/// `localhost` is the loopback's, so a connect to it needs no LAN broker: with this thread's
/// broker a socket nothing serves, it still reaches a loopback listener.
#[cfg(unix)]
#[test]
#[cfg(unix)]
fn localhost_connects_with_no_broker() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = aterm_tempfile::tempdir().unwrap();
    let absent = aterm_uds::lan::Seam {
        socket: Some(root.path().join("absent.sock")),
        ..aterm_uds::lan::Seam::default()
    };
    let stream = aterm_uds::lan::seam_here(absent, || {
        TcpConnector.connect_noting(
            "localhost",
            port,
            Deadline::after(HANG_DETECTOR),
            &mut |_, _| {},
        )
    })
    .unwrap();
    assert!(stream.peer_addr().unwrap().ip().is_loopback());
    drop(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap(),
    );
}

#[test]
fn a_resolved_address_connects() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stream = connect_with(
        "loopback.test",
        addr.port(),
        Deadline::after(HANG_DETECTOR),
        |host, within| {
            assert_eq!(host, "loopback.test");
            assert!(within <= HANG_DETECTOR);
            Ok(vec![addr.ip()])
        },
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(stream.peer_addr().unwrap(), addr);
    drop(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap(),
    );
}

#[test]
fn a_failed_connect_names_the_address_it_went_to() {
    // A port nothing listens on: bound, then released.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let mut told = Vec::new();
    let error = TcpConnector
        .connect_noting(
            "127.0.0.1",
            addr.port(),
            Deadline::after(HANG_DETECTOR),
            &mut |at, error| told.push((at, error.kind())),
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(told, [(addr, io::ErrorKind::ConnectionRefused)]);
    // A connect that succeeds tells nothing.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let open = listener.local_addr().unwrap();
    let mut quiet = true;
    TcpConnector
        .connect_noting(
            "127.0.0.1",
            open.port(),
            Deadline::after(HANG_DETECTOR),
            &mut |_, _| quiet = false,
        )
        .unwrap();
    assert!(quiet);
    drop(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap(),
    );
}

#[test]
fn an_idle_cap_shorter_than_the_budget_bounds_each_call_and_names_itself() {
    let deadline = Deadline::after(HANG_DETECTOR).with_idle(Duration::from_millis(200));
    assert_eq!(deadline.idle(), Some(Duration::from_millis(200)));
    let (budget, cap) = deadline.per_call().unwrap();
    assert_eq!(budget, Duration::from_millis(200));
    assert_eq!(deadline.expired(cap).to_string(), "no bytes for 200ms");
    let short = Deadline::after(HANG_DETECTOR).with_idle(2 * HANG_DETECTOR);
    let (budget, cap) = short.per_call().unwrap();
    assert!(budget <= HANG_DETECTOR);
    assert_eq!(cap, Cap::Total);
    let expired = short.expired(cap);
    assert_eq!(expired.kind(), io::ErrorKind::TimedOut);
    assert!(
        !expired.to_string().starts_with("no bytes for"),
        "{expired}"
    );
    assert_eq!(Deadline::after(HANG_DETECTOR).idle(), None);
}

#[test]
fn the_connect_phase_takes_the_sooner_end() {
    let total = Deadline::after(2 * HANG_DETECTOR);
    let connect = total.connect_phase(HANG_DETECTOR);
    assert!(
        connect
            .remaining()
            .is_some_and(|left| left <= HANG_DETECTOR)
    );
    assert_eq!(connect.budget, Budget::Connect);
    assert_eq!(connect.expired(Cap::Total).kind(), io::ErrorKind::TimedOut);
    let kept = Deadline::after(HANG_DETECTOR).connect_phase(2 * HANG_DETECTOR);
    assert_eq!(kept.budget, Budget::Request);
}

#[test]
fn a_budget_too_long_for_the_clock_is_far_off_rather_than_a_panic() {
    let unbounded = Deadline::after(Duration::MAX);
    assert!(
        unbounded
            .remaining()
            .is_some_and(|left| left > HANG_DETECTOR)
    );
    let connect = Deadline::after(HANG_DETECTOR).connect_phase(Duration::MAX);
    assert_eq!(connect.budget, Budget::Request);
    let connect = unbounded.connect_phase(Duration::MAX);
    assert!(connect.remaining().is_some_and(|left| left > HANG_DETECTOR));
}

#[test]
fn an_expired_deadline_is_a_timeout_not_a_hang() {
    let deadline = Deadline::after(Duration::ZERO);
    assert!(deadline.remaining().is_none());
    assert_eq!(
        deadline.remaining_or_timeout().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}
