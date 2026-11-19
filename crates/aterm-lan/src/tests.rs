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
use crate::store::Store;

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
            None,
            Box::new(Fake(Mutex::new(started))),
            Store::Unserved,
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
            Store::Unserved,
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
            None,
            Box::new(Fake(Mutex::new(started))),
            Store::Unserved,
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
            None,
            Box::new(Fake(Mutex::new(started))),
            Store::Unserved,
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

/// A fixture Spotlight store: a volume root holding `.Spotlight-V100` with folders, files of
/// known lengths and a link to a large file outside it, and every path in it with what `lstat`
/// reads of it.
fn spotlight_fixture(
    root: &std::path::Path,
) -> (
    std::path::PathBuf,
    Vec<(std::path::PathBuf, std::fs::Metadata)>,
) {
    let store = root.join(crate::store::STORE_NAME);
    let shard = store.join("Store-V2").join("0A1B2C3D");
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::create_dir_all(store.join("VolumeConfiguration")).unwrap();
    std::fs::write(shard.join("0.indexHead"), vec![7u8; 10_000]).unwrap();
    std::fs::write(shard.join("live.0.indexIds"), vec![7u8; 100_000]).unwrap();
    std::fs::write(store.join("VolumeConfiguration.plist"), b"<plist/>").unwrap();
    // A link out of the store, to a file far larger than it: counted as the link, never followed.
    let outside = root.join("outside.bin");
    std::fs::write(&outside, vec![1u8; 1 << 20]).unwrap();
    std::os::unix::fs::symlink(&outside, store.join("Store-V2").join("elsewhere")).unwrap();
    let mut paths = vec![store.clone()];
    let mut i = 0;
    while i < paths.len() {
        if std::fs::symlink_metadata(&paths[i]).unwrap().is_dir() {
            let mut inside: Vec<_> = std::fs::read_dir(&paths[i])
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            inside.sort();
            paths.extend(inside);
        }
        i += 1;
    }
    let read = paths
        .into_iter()
        .map(|p| {
            let m = std::fs::symlink_metadata(&p).unwrap();
            (p, m)
        })
        .collect();
    (store, read)
}

/// THE SPOTLIGHT STORE (SD-94): the broker answers each volume's Spotlight store size from its
/// own list of store folders, by a bounded walk that writes nothing. A fixture store, named by
/// the walker's injected roots and never by the request, reads its entries' allocated bytes
/// whole, the link out of it counted as itself; a volume with no store is not listed. Asked
/// again within ten minutes it answers the kept walk, and once ten minutes pass it walks again.
/// The walk leaves every entry as it was. A request that carries a path is refused unread and
/// walks nothing, as is a peer the broker does not admit. A walk that reaches its most entries,
/// or its longest time, is answered partial with what it counted, never as the whole store.
///
/// Negative controls: follow links in the walk (no `AtFlags::SYMLINK_NOFOLLOW` in `walk_one`'s
/// `fstatat`: the outside file's megabyte is counted); drop the kept answer (`fresh` always
/// false: two walks where one is kept); shorten the cadence (`every` a millisecond: the ask just
/// under ten minutes walks again); let the entry bound pass (`Budget::take` never
/// `Err(Entries)`: the bounded walk reads whole).
#[test]
fn the_broker_answers_the_spotlight_stores_size_read_only_and_bounded() {
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::Arc;
    use std::time::Instant;

    use aterm_keeper::identity::CheckError;
    use aterm_uds::lan::wire::{self, Frame, StoreBound, StoreVolume};

    use crate::counts::Count;
    use crate::store::{Bounds, MAX_WALK, WALK_EVERY};

    let root = aterm_tempfile::tempdir().unwrap();
    let volume = root.path().join("Fixture");
    let bare = root.path().join("Bare");
    std::fs::create_dir_all(&bare).unwrap();
    let (store, before) = spotlight_fixture(&volume);
    let whole_bytes: u64 = before.iter().map(|(_, m)| m.blocks() * 512).sum();
    let whole_entries = u64::try_from(before.len()).unwrap();
    assert!(
        whole_bytes >= 110_000,
        "CONTROL: the fixture holds its files"
    );
    assert!(whole_bytes < 1 << 20, "CONTROL: the outside file is larger");

    let now = Arc::new(Mutex::new(Instant::now()));
    let clock: Box<dyn Fn() -> Instant + Send + Sync> = {
        let now = Arc::clone(&now);
        Box::new(move || *now.lock().unwrap())
    };
    let roots = {
        let (volume, bare) = (volume.clone(), bare.clone());
        move || {
            Some(vec![
                ("/Volumes/Fixture".to_owned(), volume.clone()),
                ("/Volumes/Bare".to_owned(), bare.clone()),
            ])
        }
    };
    let served = std::sync::atomic::AtomicUsize::new(0);
    let serve_store =
        |admission: Admission, bounds: Bounds, clock: Box<dyn Fn() -> Instant + Send + Sync>| {
            let n = served.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let socket = root.path().join(format!("run{n}")).join("broker.sock");
            let (started, _starts) = mpsc::channel();
            let server = Server::bind(
                &socket,
                admission,
                None,
                Box::new(Fake(Mutex::new(started))),
                Store::walked(Box::new(roots.clone()), bounds, clock),
            )
            .unwrap();
            let shared = Arc::clone(&server.shared);
            std::thread::spawn(move || {
                let _ = server.run();
            });
            let seam = Seam {
                socket: Some(socket),
                ..Seam::default()
            };
            (seam, shared)
        };

    // The fixture's size, whole: the walk's own folder, every entry in it, and the link as
    // itself.
    let (seam, shared) = serve_store(Admission::SameUid, Bounds::BROKER, clock);
    let ask = |seam: &Seam| seam_here(seam.clone(), broker::spotlight_stores);
    let walks = |shared: &crate::server::Shared| shared.counts.get(Count::StoreWalks);
    let first = ask(&seam).unwrap();
    assert_eq!(
        first.volumes,
        [StoreVolume {
            volume: "/Volumes/Fixture".into(),
            bytes: whole_bytes,
            entries: whole_entries,
            partial: None,
        }]
    );
    assert_eq!(walks(&shared), 1);
    // Kept between walks: asked again, up to the last millisecond of ten minutes, the same walk
    // answers, its age the time since; at ten minutes it walks again.
    let again = ask(&seam).unwrap();
    assert_eq!((again.volumes, walks(&shared)), (first.volumes.clone(), 1));
    let just_under = WALK_EVERY - std::time::Duration::from_millis(1);
    *now.lock().unwrap() += just_under;
    let kept = ask(&seam).unwrap();
    assert_eq!(
        (kept.volumes, kept.age_ms, walks(&shared)),
        (
            first.volumes.clone(),
            u64::try_from(just_under.as_millis()).unwrap(),
            1
        ),
        "under ten minutes the kept walk answers"
    );
    *now.lock().unwrap() += std::time::Duration::from_millis(1);
    let walked = ask(&seam).unwrap();
    assert_eq!(walked.volumes, first.volumes);
    assert_eq!(walked.age_ms, 0);
    assert_eq!(walks(&shared), 2, "ten minutes on, it walks again");

    // Read-only: every entry is as it was, and nothing was added.
    for (path, was) in &before {
        let is = std::fs::symlink_metadata(path).unwrap();
        assert_eq!(
            (was.len(), was.modified().unwrap(), was.ino()),
            (is.len(), is.modified().unwrap(), is.ino()),
            "{}",
            path.display()
        );
    }
    let entries_now = |dir: &std::path::Path| std::fs::read_dir(dir).unwrap().count();
    assert_eq!(entries_now(&store), 3);
    assert_eq!(entries_now(&volume), 2);

    // A request that names a path, a write's target or a read's, is no request: refused before
    // it is read, its connection closed with no answer, nothing walked.
    let mut asked = wire::encode(&Frame::Request(Request::SpotlightStore)).unwrap();
    asked.extend_from_slice(b"/etc/sudoers");
    let body = u32::try_from(asked.len() - 4).unwrap();
    asked[..4].copy_from_slice(&body.to_be_bytes());
    assert!(wire::decode(&asked[4..]).is_err());
    let hello = wire::encode(&Frame::Hello {
        protocol: aterm_uds::lan::PROTOCOL,
        build: "law".into(),
    })
    .unwrap();
    let mut raw = aterm_uds::connect::until(
        seam.socket.as_deref().unwrap(),
        Instant::now() + MUST_HAPPEN,
    )
    .unwrap();
    raw.write_all(&[hello, asked].concat()).unwrap();
    raw.shutdown(std::net::Shutdown::Write).unwrap();
    let mut reader = wire::Reader::new(0);
    let deadline = Some(Instant::now() + MUST_HAPPEN);
    assert!(matches!(
        reader.frame(&raw, deadline).unwrap(),
        Some(Frame::Hello { .. })
    ));
    assert_eq!(reader.frame(&raw, deadline).unwrap(), None);
    assert_eq!(walks(&shared), 2, "a refused request walks nothing");
    assert_eq!(shared.counts.get(Count::SpotlightStore), 4);

    // A peer the broker does not admit is told so and walks nothing.
    let (refusing, refused_shared) = serve_store(
        Admission::Scripted(crate::server::Scripted {
            token: Box::new(|_| None),
            full_check: Box::new(|_| Err(CheckError::Refused("not aterm's code".into()))),
        }),
        Bounds::BROKER,
        Box::new(Instant::now),
    );
    assert!(matches!(
        ask(&refusing),
        Err(LanError::Unavailable(Unavailable::Refused(_)))
    ));
    assert_eq!(walks(&refused_shared), 0);

    // At its most entries the walk stops and says so: what it counted, a lower bound.
    let (bounded, _) = serve_store(
        Admission::SameUid,
        Bounds {
            entries: 2,
            time: MAX_WALK,
        },
        Box::new(Instant::now),
    );
    let cut = ask(&bounded).unwrap();
    assert_eq!(cut.volumes.len(), 1);
    let cut = &cut.volumes[0];
    assert_eq!(
        (cut.entries, cut.partial),
        (2, Some(StoreBound::Entries)),
        "{cut:?}"
    );
    assert!(cut.bytes < whole_bytes, "{cut:?}");
    // At its longest time likewise: here none, so nothing is counted, and it says so.
    let (timed, _) = serve_store(
        Admission::SameUid,
        Bounds {
            entries: u64::MAX,
            time: std::time::Duration::ZERO,
        },
        Box::new(Instant::now),
    );
    let timed = ask(&timed).unwrap();
    assert_eq!(
        timed.volumes,
        [StoreVolume {
            volume: "/Volumes/Fixture".into(),
            bytes: 0,
            entries: 0,
            partial: Some(StoreBound::Time),
        }]
    );
}

/// A duty that excludes one folder a request.
struct OneFolder;

impl aterm_uds::lan::ScopeDuty for OneFolder {
    fn plan(&self, uid: u32) -> Result<aterm_uds::lan::ScopePlan, aterm_uds::lan::ScopeAnswer> {
        Ok(aterm_uds::lan::ScopePlan {
            uid,
            folders: [std::path::PathBuf::from("/Users//alice/trust")].into(),
        })
    }

    fn apply(&self, plan: &aterm_uds::lan::ScopePlan) -> aterm_uds::lan::ScopeAnswer {
        aterm_uds::lan::ScopeAnswer::Excluded(u32::try_from(plan.folders.len()).unwrap())
    }

    fn now(&self) -> u64 {
        0
    }

    fn sleep(&self, period: Duration) {
        panic!("a first request waits no pace: {period:?}");
    }
}

/// A `ScopeHome` over the real wire reaches the serving broker's duty and its answer comes
/// back; a broker with no Spotlight duty (the direct path, a test's broker of the LAN alone)
/// answers that it does not serve the request.
/// NEGATIVE CONTROL: a `serving` that drops the duty answers the first case unserved.
#[test]
fn a_scope_request_is_answered_by_the_duty_and_none_is_unserved() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("run").join("broker.sock");
    let (started, _starts) = mpsc::channel();
    let server = Server::bind(
        &socket,
        Admission::SameUid,
        None,
        Box::new(Fake(Mutex::new(started))),
        Store::Unserved,
    )
    .unwrap()
    .serving(crate::Duties {
        scope: Some(std::sync::Arc::new(OneFolder)),
    });
    std::thread::spawn(move || {
        let _ = server.run();
    });
    let served = Seam {
        socket: Some(socket),
        ..Seam::default()
    };
    assert_eq!(
        seam_here(served, broker::scope_home),
        aterm_uds::lan::ScopeAnswer::Excluded(1)
    );

    let (lan_alone, _root, _) = serve(Seam::default());
    assert!(matches!(
        seam_here(lan_alone, broker::scope_home),
        aterm_uds::lan::ScopeAnswer::Unserved(words) if words.contains("no Spotlight duty")
    ));
}
