// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker served in-process on a test's own socket, its client the real one
//! (`aterm_uds::lan::broker`) pointed there by the seam, and its DNS-SD a fake.

use std::io::{Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use aterm_uds::CtlStream;
use aterm_uds::lan::wire::{Advert, Event, Instance, Request, Target};
use aterm_uds::lan::{ConnectOptions, LanError, Seam, UdpOptions, Unavailable, broker, seam_here};

use crate::bonjour::{Bonjour, Live};
use crate::server::{Admission, Failure, MAX_LIVE, Server, not_admitted, prepare};

/// A hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

/// One fake operation the server started, as the test drives it.
struct Started {
    request: Request,
    feed: mpsc::Sender<Event>,
    wake: CtlStream,
    withdrawn: mpsc::Receiver<()>,
}

impl Started {
    /// Have the operation hear `event`.
    fn hear(&mut self, event: Event) {
        self.feed.send(event).unwrap();
        self.wake.write_all(b"!").unwrap();
    }
}

/// DNS-SD as a test plays it: each start is handed to the test.
struct Fake(Mutex<mpsc::Sender<Started>>);

struct FakeLive {
    readable: CtlStream,
    events: mpsc::Receiver<Event>,
    withdrawn: mpsc::Sender<()>,
}

impl Bonjour for Fake {
    fn start(&self, request: &Request) -> Result<Box<dyn Live>, Failure> {
        let (readable, mut wake) = CtlStream::pair().map_err(|e| (-1, e.to_string()))?;
        let (feed, events) = mpsc::channel();
        let (gone, withdrawn) = mpsc::channel();
        if let Request::Register(advert) = request {
            feed.send(Event::Registered {
                name: advert.name.clone(),
                service: advert.service.clone(),
                domain: "local.".into(),
            })
            .unwrap();
            wake.write_all(b"!").unwrap();
        }
        self.0
            .lock()
            .unwrap()
            .send(Started {
                request: request.clone(),
                feed,
                wake,
                withdrawn,
            })
            .unwrap();
        Ok(Box::new(FakeLive {
            readable,
            events,
            withdrawn: gone,
        }))
    }
}

impl Live for FakeLive {
    fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.readable)
    }

    fn process(&mut self) -> Result<Vec<Event>, Failure> {
        let mut byte = [0u8; 64];
        match (&self.readable).read(&mut byte) {
            Ok(0) => return Err((-65537, "the fake's test went away".into())),
            Ok(_) => {}
            Err(error) => return Err((-65537, error.to_string())),
        }
        Ok(self.events.try_iter().collect())
    }
}

impl Drop for FakeLive {
    fn drop(&mut self) {
        let _ = self.withdrawn.send(());
    }
}

/// A broker served on a fresh socket, under `seam` (its classification), with the fake DNS-SD:
/// the seam that points a client at it, and where the fake's starts arrive.
fn serve(seam: Seam) -> (Seam, aterm_tempfile::TempDir, mpsc::Receiver<Started>) {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("run").join("broker.sock");
    let (started, starts) = mpsc::channel();
    let server = seam_here(seam.clone(), || {
        Server::bind(
            &socket,
            Admission::SameUid,
            Box::new(Fake(Mutex::new(started))),
        )
        .unwrap()
    });
    std::thread::spawn(move || {
        let _ = server.run();
    });
    let client = Seam {
        socket: Some(socket),
        ..seam
    };
    (client, root, starts)
}

fn loopback_local() -> Seam {
    Seam {
        loopback_is_local: true,
        ..Seam::default()
    }
}

#[test]
fn a_connect_is_made_by_the_broker_and_passed_back() {
    let (seam, _root, _) = serve(loopback_local());
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    let (mut stream, reached) = seam_here(seam, || {
        broker::connect(
            &Target::Addr(addr),
            addr.port(),
            MUST_HAPPEN,
            ConnectOptions::default(),
        )
    })
    .unwrap();
    assert_eq!(reached, addr);
    let (mut server, _) = aterm_uds::accept(
        &target,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    server.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    stream.write_all(b"x").unwrap();
    let mut byte = [0];
    server.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
    // SAFETY: F_GETFD reads the flags of a live descriptor.
    let flags = unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&stream), libc::F_GETFD) };
    assert!(
        flags >= 0 && flags & libc::FD_CLOEXEC != 0,
        "the passed socket closes on exec"
    );
}

#[test]
fn a_destination_off_the_local_network_is_refused() {
    // Without the seam, loopback is not local network, as a public address is not.
    let (seam, _root, _) = serve(Seam::default());
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    let refused = seam_here(seam, || {
        broker::connect(
            &Target::Addr(addr),
            addr.port(),
            MUST_HAPPEN,
            ConnectOptions::default(),
        )
    })
    .unwrap_err()
    .into_io();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{refused}"
    );
    assert!(
        refused.to_string().contains("not on the local network"),
        "{refused}"
    );
    assert_eq!(
        aterm_uds::accept(
            &target,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::WouldBlock,
        "the broker dialled nothing"
    );
}

/// The broker resolves only a local name (`aterm_uds::lan::is_local_name`): without the loopback
/// seam, a public name and `localhost` (RFC 6761 §6.3, never the local network's) are refused
/// unresolved; with the loopback seam, `localhost` resolves and connects through the broker while
/// a public name is still refused.
#[test]
fn a_host_connects_through_the_broker_and_only_a_local_name_resolves() {
    let (default_seam, _default_root, _) = serve(Seam::default());
    for name in ["example.com", "localhost"] {
        let refused = seam_here(default_seam.clone(), || broker::resolve(name, MUST_HAPPEN))
            .unwrap_err()
            .into_io();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{name}: {refused}"
        );
        assert!(
            refused.to_string().contains("not on the local network"),
            "{name}: {refused}"
        );
    }
    let (seam, _root, _) = serve(loopback_local());
    let addrs = seam_here(seam.clone(), || broker::resolve("localhost", MUST_HAPPEN)).unwrap();
    assert!(
        !addrs.is_empty() && addrs.iter().all(IpAddr::is_loopback),
        "{addrs:?}"
    );
    let refused = seam_here(seam.clone(), || broker::resolve("example.com", MUST_HAPPEN))
        .unwrap_err()
        .into_io();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{refused}"
    );
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = target.local_addr().unwrap().port();
    let (_stream, reached) = seam_here(seam, || {
        broker::connect(
            &Target::Host("localhost".into()),
            port,
            MUST_HAPPEN,
            ConnectOptions::default(),
        )
    })
    .unwrap();
    assert_eq!(reached, SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
}

#[test]
fn a_udp_socket_comes_back_bound() {
    let (seam, _root, _) = serve(Seam::default());
    let socket = seam_here(seam.clone(), || {
        broker::udp(UdpOptions {
            bind: "0.0.0.0:0".parse().unwrap(),
            reuse: true,
            groups: Vec::new(),
            multicast_if: None,
            ttl: Some(1),
        })
    })
    .unwrap();
    let port = socket.local_addr().unwrap().port();
    assert!(port != 0);
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    socket.send_to(b"ping", at).unwrap();
    socket.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    let mut buf = [0u8; 8];
    let (n, _) = socket.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"ping");
}

#[test]
fn no_broker_is_not_installed_and_another_protocol_is_outdated() {
    let root = aterm_tempfile::tempdir().unwrap();
    let absent = Seam {
        socket: Some(root.path().join("absent.sock")),
        ..Seam::default()
    };
    assert!(matches!(
        seam_here(absent, broker::hello),
        Err(LanError::Unavailable(Unavailable::NotInstalled))
    ));
    let (seam, _root, _) = serve(Seam::default());
    let hello = seam_here(seam.clone(), broker::hello).unwrap();
    assert_eq!(hello.protocol, aterm_uds::lan::PROTOCOL);
    let newer = Seam {
        protocol: Some(aterm_uds::lan::PROTOCOL + 1),
        ..seam
    };
    let outdated = seam_here(newer, broker::hello).unwrap_err();
    assert!(
        matches!(outdated, LanError::Unavailable(Unavailable::Outdated { theirs, .. }) if theirs == aterm_uds::lan::PROTOCOL),
        "{outdated:?}"
    );
}

#[test]
fn an_oversized_frame_ends_the_connection_unread() {
    let (seam, _root, _) = serve(Seam::default());
    let mut raw = aterm_uds::connect::until(
        seam.socket.as_deref().unwrap(),
        std::time::Instant::now() + MUST_HAPPEN,
    )
    .unwrap();
    let claimed = u32::try_from(aterm_uds::lan::wire::MAX_FRAME + 1).unwrap();
    raw.write_all(&claimed.to_be_bytes()).unwrap();
    #[cfg(unix)]
    {
        let deadline = std::time::Instant::now() + MUST_HAPPEN;
        while !aterm_uds::hangup::peer_closed(&raw) {
            assert!(
                std::time::Instant::now() < deadline,
                "the broker never closed"
            );
            aterm_uds::deadline::poll_yield();
        }
    }
    // The broker's hello, then its close: it read no body.
    let mut reader = aterm_uds::lan::wire::Reader::new(0);
    let deadline = Some(std::time::Instant::now() + MUST_HAPPEN);
    assert!(matches!(
        reader.frame(&raw, deadline).unwrap(),
        Some(aterm_uds::lan::wire::Frame::Hello { .. })
    ));
    assert_eq!(reader.frame(&raw, deadline).unwrap(), None);
}

/// A peer the broker did not admit is told whether its code was judged: a refusal stands for that
/// code, and a check that could not run or did not finish judged nothing.
#[test]
fn a_peer_not_admitted_is_told_whether_its_code_was_judged() {
    use aterm_keeper::identity::CheckError;
    use aterm_uds::lan::wire::Frame;
    assert_eq!(
        not_admitted(CheckError::Refused("not the team's code".into())),
        Frame::Refused {
            words: "not the team's code".into()
        }
    );
    for (unjudged, words) in [
        (
            CheckError::Unchecked("no audit token".into()),
            "no audit token",
        ),
        (
            CheckError::Unanswered("trustd did not answer".into()),
            "trustd did not answer",
        ),
    ] {
        assert_eq!(
            not_admitted(unjudged),
            Frame::Unchecked {
                words: words.into()
            }
        );
    }
}

/// A broker serving its most connections judges no other: the next is told it was not checked,
/// which its client reads apart from a refusal of its code.
#[test]
fn a_broker_serving_its_most_connections_tells_the_next_it_was_not_checked() {
    // Each held browse is two sockets here and the fake's pair.
    let room = aterm_uds::host::raise_open_files(4096);
    if room < 1024 {
        aterm_uds::host::could_not_run(format!(
            "{MAX_LIVE} held connections need about 1024 descriptors; this process has {room}"
        ));
    }
    let (seam, _root, starts) = serve(Seam::default());
    let held: Vec<_> = (0..MAX_LIVE)
        .map(|_| seam_here(seam.clone(), || broker::browse("_alab._tcp", "")).unwrap())
        .collect();
    // Each browse's operation, kept so that none ends.
    let started: Vec<_> = (0..MAX_LIVE)
        .map(|_| starts.recv_timeout(MUST_HAPPEN).unwrap())
        .collect();
    let next = seam_here(seam, broker::hello).unwrap_err();
    assert!(
        matches!(&next, LanError::Unavailable(Unavailable::Unchecked(why)) if why.contains("serving 128 connections")),
        "{next:?}"
    );
    drop((held, started));
}

#[test]
fn a_browse_streams_what_it_hears_and_ends_when_the_client_goes() {
    let (seam, _root, starts) = serve(Seam::default());
    let mut events = seam_here(seam, || broker::browse("_alab._tcp", "")).unwrap();
    let mut started = starts.recv_timeout(MUST_HAPPEN).unwrap();
    assert_eq!(
        started.request,
        Request::Browse {
            service: "_alab._tcp".into(),
            domain: String::new()
        }
    );
    let heard = Instance {
        name: "m11".into(),
        service: "_alab._tcp.".into(),
        domain: "local.".into(),
        interface: 4,
    };
    started.hear(Event::Added(heard.clone()));
    assert_eq!(
        events.next_within(MUST_HAPPEN).unwrap(),
        Some(Event::Added(heard.clone()))
    );
    started.hear(Event::Removed(heard.clone()));
    assert_eq!(events.next().unwrap().unwrap(), Event::Removed(heard));
    drop(events);
    started
        .withdrawn
        .recv_timeout(MUST_HAPPEN)
        .expect("the browse ended with its client");
}

#[test]
fn a_registration_lasts_until_it_is_dropped() {
    let (seam, _root, starts) = serve(Seam::default());
    let advert = Advert {
        name: "vault".into(),
        service: "_alab-vault._tcp".into(),
        port: 7100,
        txt: vec![b"v=1".to_vec()],
        host: Some("vault-host.local.".into()),
        addresses: vec!["192.168.1.9".parse().unwrap()],
        ..Advert::default()
    };
    let registration = seam_here(seam, || broker::register(&advert)).unwrap();
    assert_eq!(
        (
            registration.name(),
            registration.service(),
            registration.domain()
        ),
        ("vault", "_alab-vault._tcp", "local.")
    );
    let started = starts.recv_timeout(MUST_HAPPEN).unwrap();
    assert_eq!(started.request, Request::Register(advert));
    assert_eq!(
        started.withdrawn.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "registered while the registration lives"
    );
    drop(registration);
    started
        .withdrawn
        .recv_timeout(MUST_HAPPEN)
        .expect("the advert was withdrawn when its registration dropped");
}

#[cfg(target_os = "macos")]
#[test]
fn a_team_requirement_names_aterm_and_refuses_a_team_that_could_rewrite_it() {
    assert_eq!(
        crate::server::team_requirement("ABCDE12345").unwrap(),
        "identifier \"com.aterm.aterm\" and anchor apple generic and \
         certificate leaf[subject.OU] = \"ABCDE12345\""
    );
    for bad in ["", "A\" or anchor apple", "ABC DEF", &"A".repeat(21)] {
        assert!(crate::server::team_requirement(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_served_socket_is_left_to_its_broker_and_a_stale_one_cleared() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("run").join("broker.sock");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let serving = aterm_uds::CtlListener::bind(&socket).unwrap();
    assert_eq!(
        prepare(&socket).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    assert!(socket.exists(), "the serving broker's socket stays");
    drop(serving);
    prepare(&socket).unwrap();
    assert!(!socket.exists(), "the stale socket is cleared");
}

#[test]
fn an_actual_stale_broker_socket_survives_scheduling_delay() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("broker.sock");
    let serving = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&socket)).unwrap();
    drop(serving);
    crate::server::prepare_at(&socket, std::time::Instant::now() - Duration::from_secs(11))
        .unwrap();
    assert!(!socket.exists());
    let serving = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&socket)).unwrap();
    assert_eq!(
        crate::server::prepare_at(&socket, std::time::Instant::now() - Duration::from_secs(11))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AddrInUse
    );
    assert!(socket.exists());
    drop(serving);
    assert_eq!(
        crate::server::prepare_at(&socket, std::time::Instant::now() - Duration::from_secs(61))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(
        socket.exists(),
        "an expired probe never establishes staleness"
    );
}

#[test]
fn an_actual_socket_handoff_survives_scheduling_delay() {
    use std::os::fd::AsFd as _;
    for (age, error) in [
        (31, None),
        (31, Some(std::io::ErrorKind::InvalidData)),
        (61, Some(std::io::ErrorKind::TimedOut)),
    ] {
        let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let (kept, _other) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let worker = std::thread::spawn(move || {
            crate::server::pass_socket(
                &server,
                &aterm_uds::lan::wire::Frame::Socket { reached: None },
                kept.as_fd(),
                std::time::Instant::now() - Duration::from_secs(age),
            )
        });
        let mut reader = aterm_uds::lan::wire::Reader::new(1);
        assert!(matches!(
            reader
                .frame(&client, Some(std::time::Instant::now() + MUST_HAPPEN))
                .unwrap(),
            Some(aterm_uds::lan::wire::Frame::Socket { reached: None })
        ));
        let received = reader.take_fd().unwrap();
        let ack = if error == Some(std::io::ErrorKind::InvalidData) {
            (&client).write_all(&[0xff])
        } else {
            aterm_uds::lan::wire::took(&client)
        };
        let result = worker.join().unwrap();
        if let Some(error) = error {
            assert_eq!(result.unwrap_err().kind(), error);
        } else {
            result.unwrap();
            ack.unwrap();
        }
        drop(received);
    }
}

#[test]
fn an_admitted_native_request_survives_scheduling_delay() {
    use aterm_uds::lan::wire::{self, Frame, Reader};
    for (age, protocol, expected_socket) in [
        (11, aterm_uds::lan::PROTOCOL, true),
        (61, aterm_uds::lan::PROTOCOL, false),
        (0, aterm_uds::lan::PROTOCOL + 1, false),
    ] {
        let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        assert_eq!(
            aterm_uds::peer::peer_uid(&server),
            Some(aterm_uds::peer::our_uid())
        );
        let (send, _starts) = mpsc::channel();
        let shared = crate::server::Shared::new(
            Admission::SameUid,
            Box::new(Fake(Mutex::new(send))),
            |words| eprintln!("native request: {words}"),
        );
        wire::send(
            &client,
            &Frame::Hello {
                protocol,
                build: "native-request".into(),
            },
        )
        .unwrap();
        wire::send(
            &client,
            &Frame::Request(Request::Udp {
                bind: "0.0.0.0:0".parse().unwrap(),
                reuse: false,
                groups: Vec::new(),
                multicast_if: None,
                ttl: None,
            }),
        )
        .unwrap();
        let worker = std::thread::spawn(move || {
            seam_here(loopback_local(), || {
                crate::server::connection_at(
                    &shared,
                    &server,
                    std::time::Instant::now() - Duration::from_secs(age),
                )
            });
        });
        let mut reader = Reader::new(1);
        let deadline = Some(std::time::Instant::now() + MUST_HAPPEN);
        assert!(matches!(
            reader.frame(&client, deadline).unwrap(),
            Some(Frame::Hello {
                protocol: aterm_uds::lan::PROTOCOL,
                ..
            })
        ));
        // The stale request's broker refuses before it reads, so it closes with this client's
        // hello and request unread in its queue: macOS reads that close as the end, Linux as a
        // reset (`unix_release_sock` resets a peer whose close dropped bytes).
        let reply = match reader.frame(&client, deadline) {
            Err(error)
                if !expected_socket && error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                None
            }
            read => read.unwrap(),
        };
        let received = reader.take_fd();
        let acknowledged = if received.is_some() {
            wire::took(&client).is_ok()
        } else {
            false
        };
        drop(client);
        worker.join().unwrap();
        if expected_socket {
            assert!(
                matches!(reply, Some(Frame::Socket { reached: None })),
                "reply={reply:?}"
            );
            assert!(acknowledged);
            let socket = std::net::UdpSocket::from(received.unwrap());
            assert!(socket.local_addr().unwrap().port() > 0);
        } else {
            assert!(reply.is_none() && received.is_none());
            assert!(!acknowledged);
        }
    }
}

/// Every passed socket arrives with its receive side whole: the broker holds its copy until the
/// client took it in ([`aterm_uds::lan::wire::pass`]), so bytes sent to it after the pass are read.
#[test]
fn passed_sockets_read_what_arrives_after_the_pass_every_time() {
    let (seam, _root, _) = serve(loopback_local());
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    for round in 0..200 {
        let (mut stream, _) = seam_here(seam.clone(), || {
            broker::connect(
                &Target::Addr(addr),
                addr.port(),
                MUST_HAPPEN,
                ConnectOptions::default(),
            )
        })
        .unwrap();
        let (mut accepted, _) = aterm_uds::accept(
            &target,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        accepted.write_all(b"hi").unwrap();
        stream.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let mut heard = [0u8; 2];
        stream
            .read_exact(&mut heard)
            .unwrap_or_else(|error| panic!("TCP round {round}: {error}"));
        assert_eq!(&heard, b"hi");

        let socket = seam_here(seam.clone(), || {
            broker::udp(UdpOptions {
                bind: "0.0.0.0:0".parse().unwrap(),
                reuse: false,
                groups: Vec::new(),
                multicast_if: None,
                ttl: None,
            })
        })
        .unwrap();
        let at = SocketAddr::from((Ipv4Addr::LOCALHOST, socket.local_addr().unwrap().port()));
        socket.send_to(b"ping", at).unwrap();
        socket.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let mut buf = [0u8; 8];
        let (n, from) = socket
            .recv_from(&mut buf)
            .unwrap_or_else(|error| panic!("UDP round {round}: {error}"));
        assert_eq!((&buf[..n], from), (&b"ping"[..], at), "UDP round {round}");
    }
}

fn counts_clock() -> u64 {
    static NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NOW.fetch_add(1000, std::sync::atomic::Ordering::Relaxed) + 1000
}

fn counts_cpu() -> u64 {
    7
}

/// The law of the broker's counts: each connection the accept loop takes is counted once, each
/// full check where it is called, each requesting process by its whole token with what its
/// check said, each request by its kind, each name resolution's thread and each connection by
/// the one way it ended; and the counts file is written when they are due, after the connection
/// they were due at was handed on, holding counts and pids and nothing a request carried.
///
/// The connections are real ones to a served socket, taken by the production accept loop's turn
/// ([`Server::step`]) and served by the production connection path on this thread where that
/// turn hands them on, so every count is in before it is read. Admission is
/// [`Admission::Scripted`]: the token and the verdict are the script's, the path that counts
/// them the one [`Admission::Code`] takes (which
/// `a_connection_admitted_by_code_is_counted_with_its_check` drives with a real check). Not
/// driven here: `no_thread` (no thread could be started), `hello_unsent` (a peer gone before
/// the hello), and the CPU totals, which are clocks' readings.
///
/// Negative controls: delete `counts.add(Count::ChecksRun)` in `full_check` (`checks_run` reads
/// 0, wants 22); return `Count::ClosedUnsaid` for the hello alone (`hello_only` reads 0, wants
/// 2); delete `shared.counts.add(ended)` in `connection_clock` (every ending reads 0); write
/// the counts before the hand-off in `Server::step` (the file's previous snapshot reads
/// `checks_run` 7, wants 8); resolve a `Resolve` request's name with `here::resolve` (the
/// resolutions' threads counted read 0, wants 5).
#[test]
fn the_broker_counts_each_connection_once_by_how_it_ended() {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use aterm_keeper::identity::CheckError;
    use aterm_uds::lan::wire::{self, Frame};
    use aterm_uds::peer::AuditToken;

    use crate::counts::{Count, Sink};
    use crate::server::{Scripted, connection_at};

    let token = |pid: u32, version: u32| AuditToken([0, 501, 20, 501, 20, pid, 0, version]);
    let (first, exec, second) = (token(500, 1), token(500, 2), token(600, 1));
    let (refused, unchecked, unanswered) = (token(900, 1), token(901, 1), token(902, 1));
    // The token the kernel is said to give each connection, in the order they are served.
    let tokens: Arc<Mutex<VecDeque<Option<AuditToken>>>> = Arc::default();
    let said = Arc::clone(&tokens);

    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("run").join("broker.sock");
    let (started, starts) = mpsc::channel();
    let served_under = loopback_local();
    let mut server = seam_here(served_under.clone(), || {
        Server::bind(
            &socket,
            Admission::Scripted(Scripted {
                token: Box::new(move |_| said.lock().unwrap().pop_front().unwrap()),
                full_check: Box::new(|token| match token.map(|token| token.pid()) {
                    Some(900) => Err(CheckError::Refused("scripted".into())),
                    Some(901) => Err(CheckError::Unchecked("scripted".into())),
                    Some(902) => Err(CheckError::Unanswered("scripted".into())),
                    _ => Ok(()),
                }),
            }),
            Box::new(Fake(Mutex::new(started))),
        )
        .unwrap()
    });
    server.sink = Sink::beside_with(&socket, counts_clock, counts_cpu);
    let client_seam = Seam {
        socket: Some(socket.clone()),
        ..served_under.clone()
    };

    // One turn of the accept loop: the one connection waiting is served here, where the turn
    // hands it on, as the process `token` names.
    let serve_one = |server: &mut Server, token: Option<AuditToken>| {
        tokens.lock().unwrap().push_back(token);
        let mut served = false;
        server
            .step(|shared, stream| {
                seam_here(served_under.clone(), || {
                    connection_at(shared, &stream, std::time::Instant::now());
                });
                shared.live.fetch_sub(1, Ordering::AcqRel);
                served = true;
            })
            .unwrap();
        assert!(served, "the broker was not serving its most");
    };
    // A real client's one call, on a thread of its own, served here.
    fn client<T: Send + 'static>(
        seam: &Seam,
        call: impl FnOnce() -> T + Send + 'static,
    ) -> std::thread::JoinHandle<T> {
        let seam = seam.clone();
        std::thread::spawn(move || seam_here(seam, call))
    }
    // A client that writes `bytes` and stays open, its writing end closed: served here.
    let raw = |bytes: &[u8]| {
        let mut raw =
            aterm_uds::connect::until(&socket, std::time::Instant::now() + MUST_HAPPEN).unwrap();
        raw.write_all(bytes).unwrap();
        raw.shutdown(std::net::Shutdown::Write).unwrap();
        raw
    };
    let hello = |protocol: u32| {
        wire::encode(&Frame::Hello {
            protocol,
            build: "law".into(),
        })
        .unwrap()
    };
    let resolve = wire::encode(&Frame::Request(Request::Resolve {
        name: "localhost".into(),
        within_ms: 1000,
    }))
    .unwrap();
    let ours = aterm_uds::lan::PROTOCOL;

    // 1 to 5: five resolves from one process.
    for _ in 0..5 {
        let asked = client(&client_seam, || broker::resolve("localhost", MUST_HAPPEN));
        serve_one(&mut server, Some(first));
        asked.join().unwrap().unwrap();
    }
    // 6 to 8: three connects from another.
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    for _ in 0..3 {
        let asked = client(&client_seam, move || {
            broker::connect(
                &Target::Addr(addr),
                addr.port(),
                MUST_HAPPEN,
                ConnectOptions::default(),
            )
        });
        serve_one(&mut server, Some(second));
        asked.join().unwrap().unwrap();
    }
    // 9 and 10: a hello alone from the first process, then from its pid after an exec.
    for token in [first, exec] {
        let asked = client(&client_seam, broker::hello);
        serve_one(&mut server, Some(token));
        asked.join().unwrap().unwrap();
    }
    // 11 to 13: one refused, one that could not be checked, one trustd did not answer.
    for token in [refused, unchecked, unanswered] {
        let asked = client(&client_seam, broker::hello);
        serve_one(&mut server, Some(token));
        let told = asked.join().unwrap().unwrap_err();
        assert_eq!(
            matches!(told, LanError::Unavailable(Unavailable::Refused(_))),
            token == refused,
            "{told:?}"
        );
    }
    // 14 to 20: the endings a well-behaved client never makes, from clients the kernel gave no
    // token for.
    let mut held = Vec::new();
    // Another protocol.
    held.push(raw(&hello(ours + 1)));
    serve_one(&mut server, None);
    // A first frame that is no hello.
    held.push(raw(&resolve));
    serve_one(&mut server, None);
    // A hello that cannot be read: a frame longer than any.
    let oversized = u32::try_from(wire::MAX_FRAME + 1).unwrap().to_be_bytes();
    held.push(raw(&oversized));
    serve_one(&mut server, None);
    // Closed before its hello.
    held.push(raw(&[]));
    serve_one(&mut server, None);
    // A second hello where the request belongs.
    held.push(raw(&[hello(ours), hello(ours)].concat()));
    serve_one(&mut server, None);
    // A request that cannot be read: closed inside its frame.
    held.push(raw(
        &[&hello(ours)[..], &resolve[..resolve.len() - 1]].concat()
    ));
    serve_one(&mut server, None);
    // A byte past the one request.
    held.push(raw(&[&hello(ours)[..], &resolve[..], b"x"].concat()));
    serve_one(&mut server, None);
    // 21: a request that fails (a name off the local network).
    let asked = client(&client_seam, || broker::resolve("example.com", MUST_HAPPEN));
    serve_one(&mut server, None);
    asked.join().unwrap().unwrap_err();
    // 22: one more than the broker serves at once.
    server.shared.live.fetch_add(MAX_LIVE, Ordering::AcqRel);
    let asked = client(&client_seam, broker::hello);
    server
        .step(|_, _| panic!("one more than the most is handed on"))
        .unwrap();
    assert!(matches!(
        asked.join().unwrap().unwrap_err(),
        LanError::Unavailable(Unavailable::Unchecked(_))
    ));
    server.shared.live.fetch_sub(MAX_LIVE, Ordering::AcqRel);
    // 23: a standing browse that hears one instance, then its client goes.
    let asked = client(&client_seam, move || {
        let mut events = broker::browse("_alab._tcp", "").unwrap();
        let mut started = starts.recv_timeout(MUST_HAPPEN).unwrap();
        started.hear(Event::Added(Instance {
            name: "m11".into(),
            service: "_alab._tcp.".into(),
            domain: "local.".into(),
            interface: 4,
        }));
        events.next_within(MUST_HAPPEN).unwrap().unwrap();
        // The client goes as this thread ends; the fake's operation is handed back, so the
        // drive ends on the client's hang-up and never on the fake's.
        started
    });
    serve_one(&mut server, Some(second));
    let _started = asked.join().unwrap();
    drop(held);

    let counts = &server.shared.counts;
    let read: Vec<(&str, u64)> = Count::ALL
        .into_iter()
        .map(|what| (what.name(), counts.get(what)))
        .filter(|(_, n)| *n != 0)
        .collect();
    assert_eq!(
        read,
        [
            ("accepted", 23),
            ("busy", 1),
            ("checks_run", 22),
            ("new_requesters", 6),
            ("refused", 1),
            ("unchecked", 1),
            ("unanswered", 1),
            ("admitted", 19),
            ("closed_unsaid", 1),
            ("wrong_protocol", 1),
            ("wrong_first_frame", 1),
            ("read_error", 2),
            ("hello_only", 2),
            ("wrong_request_frame", 1),
            ("trailing_bytes", 1),
            ("connect", 3),
            ("resolve", 6),
            ("browse", 1),
            ("answered", 9),
            ("failed", 1),
        ]
    );
    let listed = |pid, connections, admitted, since_accepted| crate::counts::Listed {
        pid,
        connections,
        admitted,
        since_accepted,
    };
    assert_eq!(
        counts.requesters(),
        [
            listed(500, 6, 6, 1),
            listed(600, 4, 4, 6),
            listed(500, 1, 1, 10),
            listed(900, 1, 0, 11),
            listed(901, 1, 0, 12),
            listed(902, 1, 0, 13),
        ]
    );
    // The five resolves of a name each ran on a thread of their own, counted as its lookup
    // returned; the name off the local network was never looked up.
    assert_eq!(counts.resolve_threads.ended.load(Ordering::Relaxed), 5);

    // The file was last written at the 16th connection, once that one was handed on and (here)
    // served: its previous snapshot is the 8th's, a second of the scripted clock earlier.
    let file = std::fs::read_to_string(socket.with_file_name(aterm_uds::lan::COUNTS_NAME)).unwrap();
    let lines: Vec<&str> = file.lines().collect();
    let field = |line: &str, name: &str| -> u64 {
        line.split(' ')
            .find_map(|field| field.strip_prefix(name)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("no {name} in {line}"))
            .parse()
            .unwrap()
    };
    assert_eq!(lines[0], crate::counts::HEAD);
    assert_eq!(lines[1], "previous");
    assert_eq!(field(lines[2], "accepted"), 8);
    assert_eq!(field(lines[2], "checks_run"), 8);
    let current = lines.iter().position(|line| *line == "current").unwrap();
    assert_eq!(field(lines[current + 1], "accepted"), 16);
    assert_eq!(field(lines[current + 1], "checks_run"), 16);
    assert_eq!(field(lines[current + 1], "wrong_first_frame"), 1);
    assert_eq!(field(lines[current + 1], "resolve_threads"), 5);
    assert_eq!(field(lines[current + 1], "process_cpu_ns"), 7);
    assert_eq!(
        field(lines[current + 1], "at_ms") - field(lines[2], "at_ms"),
        1000
    );
    assert_eq!(
        lines[current + 2..],
        [
            "requester pid=500 connections=6 admitted=6 since_accepted=1",
            "requester pid=600 connections=3 admitted=3 since_accepted=6",
            "requester pid=500 connections=1 admitted=1 since_accepted=10",
            "requester pid=900 connections=1 admitted=0 since_accepted=11",
            "requester pid=901 connections=1 admitted=0 since_accepted=12",
            "requester pid=902 connections=1 admitted=0 since_accepted=13",
        ]
    );
    // Counts and pids only: every word is a bare word or a name and a number.
    for word in file.split_whitespace() {
        let numbered = word
            .split_once('=')
            .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let bare = word.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') || word == "1";
        assert!(numbered || bare, "{word:?} in the counts file");
    }
}

/// A standing operation's thread may never end, so [`crate::bonjour::drive`] says each time the
/// operation heard something, which is where the accept loop's thread adds its CPU time: once
/// for each batch heard, before the batch is sent.
///
/// Negative control: delete `heard();` in `drive` (reads 0, wants 1).
#[test]
fn a_standing_operation_says_each_time_it_heard() {
    use aterm_uds::lan::wire::{Frame, Reader};
    let (started, starts) = mpsc::channel();
    let fake = Fake(Mutex::new(started));
    let mut live = fake
        .start(&Request::Browse {
            service: "_alab._tcp".into(),
            domain: String::new(),
        })
        .unwrap();
    let mut started = starts.recv_timeout(MUST_HAPPEN).unwrap();
    let (client, server) = CtlStream::pair().unwrap();
    let heard = Instance {
        name: "m11".into(),
        service: "_alab._tcp.".into(),
        domain: "local.".into(),
        interface: 4,
    };
    started.hear(Event::Added(heard.clone()));
    // The client reads the one event and goes, which ends the drive.
    let reads = std::thread::spawn(move || {
        let mut reader = Reader::new(0);
        let deadline = Some(std::time::Instant::now() + MUST_HAPPEN);
        reader.frame(&client, deadline).unwrap()
    });
    let mut times = 0;
    crate::bonjour::drive(&server, live.as_mut(), &mut || times += 1).unwrap();
    assert_eq!(
        reads.join().unwrap(),
        Some(Frame::Event(Event::Added(heard)))
    );
    assert_eq!(times, 1);
}

/// The live law of the broker's own admission ([`Admission::Code`], what `aterm lan serve`
/// admits by): a real connection from this process, taken by the accept loop's turn and served
/// by the production connection path, is admitted by a real check of this process's code
/// against this image's own requirement (macOS's `SecCodeCopyGuestWithAttributes` and
/// `SecCodeCheckValidity`), and is counted as one call of the full check, one check whose
/// helper returned, and one connection of one requester, this pid. The first connection's
/// counts are in the file the turn writes. It counts; `check_cpu_ns` is a clock's reading and
/// is asserted nowhere.
///
/// Negative control: have the `Code` arm of `Admission::check` call `identity.check_named`
/// (the check runs and admits, its cost counted nowhere: `checks_returned` reads 0, wants 1).
#[cfg(target_os = "macos")]
#[test]
fn a_connection_admitted_by_code_is_counted_with_its_check() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use aterm_keeper::identity::{Identity, IdentityPolicy};

    use crate::counts::{Count, Listed};
    use crate::server::connection_at;

    aterm_uds::deadline::require_trust_service();
    let identity = Identity::for_self(IdentityPolicy::Designated)
        .expect("own requirement")
        .regardless_of_uid();
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("run").join("broker.sock");
    let (started, _starts) = mpsc::channel();
    let served_under = loopback_local();
    let mut server = seam_here(served_under.clone(), || {
        Server::bind(
            &socket,
            Admission::Code(Arc::new(identity)),
            Box::new(Fake(Mutex::new(started))),
        )
        .unwrap()
    });
    let client_seam = Seam {
        socket: Some(socket.clone()),
        ..served_under.clone()
    };
    let asked = std::thread::spawn(move || seam_here(client_seam, broker::hello));
    server
        .step(|shared, stream| {
            seam_here(served_under.clone(), || {
                connection_at(shared, &stream, std::time::Instant::now());
            });
            shared.live.fetch_sub(1, Ordering::AcqRel);
        })
        .unwrap();
    asked
        .join()
        .unwrap()
        .expect("the broker admits this process's own code");

    let counts = &server.shared.counts;
    let read: Vec<(&str, u64)> = Count::ALL
        .into_iter()
        .map(|what| (what.name(), counts.get(what)))
        .filter(|(_, n)| *n != 0)
        .collect();
    assert_eq!(
        read,
        [
            ("accepted", 1),
            ("checks_run", 1),
            ("new_requesters", 1),
            ("admitted", 1),
            ("hello_only", 1),
        ]
    );
    assert_eq!(counts.check_cost.checks.load(Ordering::Relaxed), 1);
    assert_eq!(
        counts.requesters(),
        [Listed {
            pid: std::process::id(),
            connections: 1,
            admitted: 1,
            since_accepted: 1,
        }]
    );
    let file = std::fs::read_to_string(socket.with_file_name(aterm_uds::lan::COUNTS_NAME)).unwrap();
    assert!(
        file.contains(" checks_run=1 ") && file.contains(" checks_returned=1 "),
        "{file}"
    );
    assert!(
        file.contains(&format!(
            "requester pid={} connections=1 admitted=1 since_accepted=1",
            std::process::id()
        )),
        "{file}"
    );
}
