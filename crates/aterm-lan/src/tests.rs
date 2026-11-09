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
    raw.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
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
