// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The client against a stand-in broker served on this thread's seam socket. The real broker,
//! and its own tests, are aterm-lan's.

use super::wire::{Frame, Reader, Request};
use super::*;
use crate::CtlStream;
use std::time::Instant;

/// A hang detector, never a latency budget.
const MUST_HAPPEN: Duration = Duration::from_secs(60);

#[cfg(target_os = "macos")]
fn answer_hello_at(path: &std::path::Path, protocol: u32) -> std::thread::JoinHandle<()> {
    let listener = crate::CtlListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        wire::send(
            &stream,
            &Frame::Hello {
                protocol,
                build: "arrival-fixture".into(),
            },
        )
        .unwrap();
        let hello = Reader::new(0)
            .frame(&stream, Some(Instant::now() + MUST_HAPPEN))
            .unwrap();
        assert!(matches!(hello, Some(Frame::Hello { .. })));
    })
}

/// A checked level read closes the startup race, and unrelated parent writes
/// neither redial nor announce an already observed broker generation.
#[cfg(target_os = "macos")]
#[test]
fn broker_arrivals_prime_once_and_ignore_unrelated_directory_changes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let served = answer_hello_at(&path, PROTOCOL + 7);
    let mut changes = seam_here(
        Seam {
            socket: Some(path),
            protocol: Some(PROTOCOL + 7),
            ..Seam::default()
        },
        Arrivals::new,
    );
    assert!(
        changes.poll().unwrap(),
        "the captured endpoint and protocol survive into the observer"
    );
    served.join().unwrap();
    assert!(!changes.poll().unwrap());
    std::fs::write(root.path().join("unrelated"), "noise").unwrap();
    assert!(!changes.poll().unwrap());
    assert_eq!(
        changes.wait_for(Duration::from_secs(60)),
        Duration::from_secs(60)
    );
}

/// Missing folders and replacement sockets are native events. File presence and
/// a wrong protocol grant no ready signal; the corrected replacement can do so.
#[cfg(target_os = "macos")]
#[test]
fn broker_arrivals_follow_missing_folders_and_verify_each_replacement() {
    let root = aterm_tempfile::tempdir().unwrap();
    let folder = root.path().join("n/d");
    let path = folder.join("s");
    let mut changes = seam_here(
        Seam {
            socket: Some(path.clone()),
            ..Seam::default()
        },
        Arrivals::new,
    );
    assert!(!changes.poll().unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let wrong = answer_hello_at(&path, PROTOCOL + 1);
    assert!(!changes.poll().unwrap(), "another protocol is not ready");
    wrong.join().unwrap();
    assert!(!changes.poll().unwrap());
    assert_eq!(
        changes.wait_for(MUST_HAPPEN),
        MUST_HAPPEN,
        "a rejected generation is not repeatedly probed"
    );
    std::fs::remove_file(&path).unwrap();
    let right = answer_hello_at(&path, PROTOCOL);
    assert!(changes.poll().unwrap());
    right.join().unwrap();
    assert!(!changes.poll().unwrap());
}

/// A socket left before listen is ready gets a bounded retry, while its new
/// listening generation is observed immediately rather than waiting that retry.
#[cfg(target_os = "macos")]
#[test]
fn broker_arrivals_recheck_an_unready_socket_and_skip_its_retry_on_replacement() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    drop(crate::CtlListener::bind(&path).unwrap());
    let mut changes = seam_here(
        Seam {
            socket: Some(path.clone()),
            ..Seam::default()
        },
        Arrivals::new,
    );
    assert!(!changes.poll().unwrap());
    assert!(changes.wait_for(MUST_HAPPEN) < MUST_HAPPEN);
    std::fs::remove_file(&path).unwrap();
    let ready = answer_hello_at(&path, PROTOCOL);
    assert!(changes.poll().unwrap());
    ready.join().unwrap();
    assert_eq!(changes.wait_for(MUST_HAPPEN), MUST_HAPPEN);
}

/// A direct path that macOS allowed and whose server answers a hello, as aterm-lan's does.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct AnswersHello(std::sync::atomic::AtomicUsize);

#[cfg(target_os = "macos")]
impl direct::Direct for AnswersHello {
    fn allowed(&self) -> Result<String, String> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok("a stand-in allowance".into())
    }

    fn record(&self) -> Option<std::path::PathBuf> {
        None
    }

    fn serve(&self, stream: CtlStream) {
        let hello = Frame::Hello {
            protocol: PROTOCOL,
            build: "direct-stand-in".into(),
        };
        if wire::send(&stream, &hello).is_ok() {
            let _ = Reader::new(0).frame(&stream, Some(Instant::now() + MUST_HAPPEN));
        }
    }
}

/// The arrival watch hears a broker, never this process's own server: a socket file no broker
/// listens on (one that exited, crashed or was booted out) is not an arrival, though a LAN use
/// beside it is served directly. NEGATIVE CONTROL: the same seam's hello answers, from the direct
/// path, so a watch that took it would announce the stale socket as a broker.
#[cfg(target_os = "macos")]
#[test]
fn broker_arrivals_never_hear_the_direct_path() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    drop(crate::CtlListener::bind(&path).unwrap());
    let seam = Seam {
        socket: Some(path.clone()),
        ..Seam::default()
    };
    static DIRECT: AnswersHello = AnswersHello(std::sync::atomic::AtomicUsize::new(0));
    let direct = &DIRECT;
    let (mut changes, installed, hello) = seam_here(seam, || {
        direct::seam_here(direct, || {
            let installed = broker::installed_hello();
            assert_eq!(
                direct.0.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "installed readiness never even asks for the direct path"
            );
            (Arrivals::new(), installed, broker::hello())
        })
    });
    assert!(matches!(
        installed,
        Err(LanError::Unavailable(Unavailable::NotInstalled))
    ));
    let hello = hello.unwrap();
    assert_eq!(hello.direct.as_deref(), Some("a stand-in allowance"));
    let heard = direct::seam_here(direct, || changes.poll().unwrap());
    assert!(!heard, "a stale socket beside a direct path is no broker");
}

/// A stand-in broker at a fresh socket that answers one connection with `answer`, given the
/// request it read; the seam that points this thread's client at it.
fn stand_in(
    protocol: u32,
    answer: impl FnOnce(&CtlStream, Option<Request>) + Send + 'static,
) -> (Seam, aterm_tempfile::TempDir, std::thread::JoinHandle<()>) {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let listener = crate::CtlListener::bind(&path).unwrap();
    let served = std::thread::spawn(move || {
        let (stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        wire::send(
            &stream,
            &Frame::Hello {
                protocol,
                build: "stand-in".into(),
            },
        )
        .unwrap();
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + MUST_HAPPEN);
        let hello = reader.frame(&stream, deadline).unwrap();
        assert!(matches!(hello, Some(Frame::Hello { .. })), "{hello:?}");
        let request = match reader.frame(&stream, deadline) {
            Ok(Some(Frame::Request(request))) => Some(request),
            _ => None,
        };
        answer(&stream, request);
    });
    let seam = Seam {
        socket: Some(path),
        ..Seam::default()
    };
    (seam, root, served)
}

#[test]
fn no_broker_is_not_installed_and_says_what_installs_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let seam = Seam {
        socket: Some(root.path().join("absent.sock")),
        ..Seam::default()
    };
    let missing = seam_here(seam, broker::hello).unwrap_err();
    assert!(
        matches!(missing, LanError::Unavailable(Unavailable::NotInstalled)),
        "{missing:?}"
    );
    let io = missing.into_io();
    assert_eq!(io.kind(), std::io::ErrorKind::NotConnected);
    assert!(io.to_string().contains("not installed"), "{io}");
    // The io error keeps the typed refusal, so a caller above it names a held LAN, not a failed
    // dial; a plain NotConnected carries none.
    assert_eq!(Unavailable::of(&io), Some(&Unavailable::NotInstalled));
    assert_eq!(
        Unavailable::of(&std::io::Error::from(std::io::ErrorKind::NotConnected)),
        None
    );
    // A socket file nobody listens on refuses the connect: also not installed.
    let stale = root.path().join("stale.sock");
    drop(crate::CtlListener::bind(&stale).unwrap());
    let seam = Seam {
        socket: Some(stale),
        ..Seam::default()
    };
    assert!(matches!(
        seam_here(seam, broker::hello),
        Err(LanError::Unavailable(Unavailable::NotInstalled))
    ));
}

#[test]
fn a_broker_of_another_protocol_is_outdated() {
    let (seam, _root, served) = stand_in(PROTOCOL + 1, |_, _| {});
    let outdated = seam_here(seam, broker::hello).unwrap_err();
    assert!(
        matches!(
            outdated,
            LanError::Unavailable(Unavailable::Outdated { ours, theirs })
                if ours == PROTOCOL && theirs == PROTOCOL + 1
        ),
        "{outdated:?}"
    );
    served.join().unwrap();
}

/// A stand-in broker at `path` that admits nothing on its one connection: it sends `told` instead
/// of its hello, as the broker does, or (`None`) reads the client's hello and closes.
/// A sent verdict retains the connection until the client closes, so dialing and kernel peer
/// identification finish before this fixture retires its transport.
fn admitting_nothing_at(
    path: &std::path::Path,
    told: Option<Frame>,
) -> std::thread::JoinHandle<()> {
    let listener = crate::CtlListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        match told {
            Some(frame) => {
                use std::io::Read as _;
                wire::send(&stream, &frame).unwrap();
                stream.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
                let mut hello = Vec::new();
                (&stream).read_to_end(&mut hello).unwrap();
            }
            None => {
                let hello = Reader::new(0)
                    .frame(&stream, Some(Instant::now() + MUST_HAPPEN))
                    .unwrap();
                assert!(matches!(hello, Some(Frame::Hello { .. })), "{hello:?}");
            }
        }
    })
}

/// The seam that points this thread's client at `path`.
fn at(path: &std::path::Path) -> Seam {
    Seam {
        socket: Some(path.to_path_buf()),
        ..Seam::default()
    }
}

#[test]
fn a_broker_that_refuses_this_code_says_why() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let served = admitting_nothing_at(
        &path,
        Some(Frame::Refused {
            words: "not aterm's code".into(),
        }),
    );
    let refused = seam_here(at(&path), broker::hello).unwrap_err();
    assert!(
        matches!(&refused, LanError::Unavailable(Unavailable::Refused(why)) if why.contains("not aterm's code")),
        "{refused:?}"
    );
    served.join().unwrap();
}

/// A broker that did not judge this process, by saying so or by closing before its hello, reads
/// apart from a refusal of its code, in the broker's words.
#[test]
fn a_broker_that_did_not_judge_this_code_is_unchecked_not_refused() {
    let root = aterm_tempfile::tempdir().unwrap();
    let told = root.path().join("told.sock");
    let served = admitting_nothing_at(
        &told,
        Some(Frame::Unchecked {
            words: "trustd did not answer".into(),
        }),
    );
    let unchecked = seam_here(at(&told), broker::hello).unwrap_err();
    assert!(
        matches!(&unchecked, LanError::Unavailable(Unavailable::Unchecked(why)) if why.contains("trustd did not answer")),
        "{unchecked:?}"
    );
    assert!(
        unchecked
            .to_string()
            .contains("has not judged this process yet"),
        "{unchecked}"
    );
    served.join().unwrap();
    let closed = root.path().join("closed.sock");
    let served = admitting_nothing_at(&closed, None);
    let unchecked = seam_here(at(&closed), broker::hello).unwrap_err();
    assert!(
        matches!(&unchecked, LanError::Unavailable(Unavailable::Unchecked(why)) if why.contains("before its hello")),
        "{unchecked:?}"
    );
    served.join().unwrap();
}

/// A broker that did not judge this process is asked again on a back-off; one that refused its
/// code is not, until its socket is made again.
#[cfg(target_os = "macos")]
#[test]
fn broker_arrivals_ask_again_where_nothing_was_judged_and_not_after_a_refusal() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let unchecked = admitting_nothing_at(
        &path,
        Some(Frame::Unchecked {
            words: "the LAN broker is serving 128 connections".into(),
        }),
    );
    let mut changes = seam_here(at(&path), Arrivals::new);
    assert!(!changes.poll().unwrap());
    unchecked.join().unwrap();
    assert!(
        changes.wait_for(MUST_HAPPEN) < MUST_HAPPEN,
        "a broker that judged nothing is asked again"
    );
    std::fs::remove_file(&path).unwrap();
    let refused = admitting_nothing_at(
        &path,
        Some(Frame::Refused {
            words: "not aterm's code".into(),
        }),
    );
    assert!(!changes.poll().unwrap());
    refused.join().unwrap();
    assert_eq!(
        changes.wait_for(MUST_HAPPEN),
        MUST_HAPPEN,
        "a refusal of this code is not asked again"
    );
}

/// On macOS, a TCP dial of a local-network address goes to the broker, which hands back the
/// connection it made; loopback stands in for the LAN under the seam.
#[cfg(target_os = "macos")]
#[test]
fn a_local_network_dial_comes_back_from_the_broker() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    let target = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    let (seam, _root, served) = stand_in(PROTOCOL, move |stream, request| {
        let Some(Request::Connect {
            target: Target::Addr(asked),
            port,
            ..
        }) = request
        else {
            panic!("{request:?}");
        };
        assert_eq!((asked, port), (addr, addr.port()));
        let made = here::dial(
            &asked,
            Instant::now() + MUST_HAPPEN,
            ConnectOptions::default(),
        )
        .unwrap();
        wire::pass(
            stream,
            &Frame::Socket {
                reached: Some(asked),
            },
            made.as_fd(),
            Instant::now() + MUST_HAPPEN,
        )
        .unwrap();
    });
    let seam = Seam {
        loopback_is_local: true,
        ..seam
    };
    let mut client = seam_here(seam, || crate::connect_tcp(&addr, MUST_HAPPEN)).unwrap();
    served.join().unwrap();
    let (mut server, _) = crate::accept(
        &target,
        crate::AcceptWait::Forever,
        crate::AcceptedMode::Blocking,
    )
    .unwrap();
    server.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
    client.write_all(b"x").unwrap();
    let mut byte = [0];
    server.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
}

/// A local name the broker resolves is cut like a named address: the partition refuses each
/// address it resolved to before the broker is asked to dial it.
#[cfg(target_os = "macos")]
#[test]
fn a_local_name_resolved_by_the_broker_is_cut_by_the_partition() {
    let (seam, _root, served) = stand_in(PROTOCOL, |stream, request| {
        assert!(
            matches!(&request, Some(Request::Resolve { name, .. }) if name == "vault-host.local"),
            "{request:?}"
        );
        wire::send(
            stream,
            &Frame::Addresses(vec!["192.168.1.9".parse().unwrap()]),
        )
        .unwrap();
    });
    let cut = crate::partition::cut_here("192.168.1.9:7100", || {
        seam_here(seam, || {
            connect(
                &Target::Host("vault-host.local".into()),
                7100,
                MUST_HAPPEN,
                ConnectOptions::default(),
            )
        })
    })
    .unwrap_err()
    .into_io();
    assert_eq!(cut.kind(), std::io::ErrorKind::HostUnreachable, "{cut}");
    // The stand-in served the resolve alone: no dial was asked of it.
    served.join().unwrap();
}

/// A broker's refusal of a destination comes back as the errno's kind, with its words.
#[test]
fn a_refused_connect_is_the_brokers_errno_and_words() {
    let (seam, _root, served) = stand_in(PROTOCOL, |stream, _| {
        wire::send(
            stream,
            &Frame::Error {
                code: libc::EACCES,
                words: "8.8.8.8 is not on the local network".into(),
            },
        )
        .unwrap();
    });
    let refused = seam_here(seam, || {
        broker::connect(
            &Target::Addr("8.8.8.8:53".parse().unwrap()),
            53,
            MUST_HAPPEN,
            ConnectOptions::default(),
        )
    })
    .unwrap_err()
    .into_io();
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(refused.to_string().contains("local network"), "{refused}");
    served.join().unwrap();
}

#[test]
fn events_wait_within_a_bound_and_end_when_the_broker_closes() {
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let (seam, _root, served) = stand_in(PROTOCOL, move |stream, request| {
        assert!(
            matches!(request, Some(Request::Browse { .. })),
            "{request:?}"
        );
        wait.recv().unwrap();
        wire::send(
            stream,
            &Frame::Event(Event::Added(Instance {
                name: "m11".into(),
                service: "_alab._tcp.".into(),
                domain: "local.".into(),
                interface: 4,
            })),
        )
        .unwrap();
    });
    let mut events = seam_here(seam, || broker::browse("_alab._tcp", "")).unwrap();
    assert!(matches!(
        events.next_within(Duration::from_millis(1)),
        Ok(None)
    ));
    go.send(()).unwrap();
    let heard = events.next_within(MUST_HAPPEN).unwrap();
    assert!(
        matches!(heard, Some(Event::Added(ref i)) if i.name == "m11"),
        "{heard:?}"
    );
    served.join().unwrap();
    assert!(
        events.next().is_none(),
        "the stream ended with the broker's close"
    );
}

#[test]
fn a_build_id_is_named_in_each_hello() {
    assert!(!hello_build().is_empty());
}

/// A dial that reached no peer reads as one whether it went through the broker or not, the
/// host-down errno std leaves uncategorized included; a held LAN or a local refusal is not one.
#[test]
fn a_dial_that_reached_no_peer_reads_as_one_through_the_broker_too() {
    assert!(reached_no_peer(&std::io::Error::from_raw_os_error(
        libc::EHOSTDOWN
    )));
    for code in [libc::EHOSTDOWN, libc::ECONNREFUSED, libc::ETIMEDOUT] {
        assert!(reached_no_peer(&broker::failed(code, "fixture")), "{code}");
    }
    assert!(!reached_no_peer(&broker::failed(libc::EACCES, "fixture")));
    assert!(!reached_no_peer(
        &LanError::Unavailable(Unavailable::NotInstalled).into_io()
    ));
}
