// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private local control fixtures exercise actual descriptor delivery.

use super::*;
use aterm_uds::CtlListener;
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt as _, time::Duration};

const HANG: Duration = Duration::from_secs(60);
const TOKEN: &str = "fixture-owner-token";

fn deadline() -> Instant {
    Instant::now() + HANG
}

struct World {
    _dir: aterm_tempfile::TempDir,
    path: String,
    listener: CtlListener,
}
impl World {
    fn new(token: bool) -> Self {
        Self::named(token, "m.sock")
    }

    fn named(token: bool, name: &str) -> Self {
        let dir = crate::test_scratch_dir("alloc");
        let path = dir.path().join(name);
        let listener = aterm_uds::kept_out(|| CtlListener::bind(&path)).unwrap();
        if token {
            let token = dir
                .path()
                .join(&crate::control_socket::token_names_for_sock(name)[0]);
            let mut token = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(token)
                .unwrap();
            token.write_all(TOKEN.as_bytes()).unwrap();
        }
        Self {
            _dir: dir,
            path: path.to_str().unwrap().into(),
            listener,
        }
    }
    fn accept(&self) -> CtlStream {
        aterm_uds::accept(
            &self.listener,
            aterm_uds::AcceptWait::Until(deadline()),
            aterm_uds::AcceptedMode::Nonblocking,
        )
        .unwrap()
        .0
    }
}

fn request_line(stream: &mut CtlStream) -> String {
    let until = deadline();
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) => return String::from_utf8(bytes).unwrap(),
            Ok(_) if byte[0] == b'\n' => return String::from_utf8(bytes).unwrap(),
            Ok(_) => {
                bytes.push(byte[0]);
                assert!(bytes.len() <= LINE_LIMIT);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, until).unwrap()
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => panic!("fixture read: {e}"),
        }
    }
}

fn auth(stream: &mut CtlStream) {
    assert_eq!(request_line(stream), format!("AUTH {TOKEN}"));
    assert_eq!(request_line(stream), "version");
    write_all(stream, b"OK version=fixture\n", deadline()).unwrap();
}

fn descriptor(stream: &mut CtlStream) -> OwnedFd {
    let until = deadline();
    loop {
        let mut byte = [0];
        match aterm_uds::fdpass::recv_with_fds(stream, &mut byte, 1) {
            Ok(received) => {
                assert_eq!(received.bytes, 1);
                assert_eq!(byte, [wire::FD_MARKER]);
                let [fd]: [_; 1] = received.fds.try_into().unwrap();
                return fd;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, until).unwrap()
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => panic!("fixture descriptor: {e}"),
        }
    }
}

fn monitor() -> (OwnedFd, CtlStream) {
    let (client, peer) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    peer.set_read_timeout(Some(HANG)).unwrap();
    (OwnedFd::from(client), peer)
}

#[test]
fn authenticated_admission_transfers_one_live_descriptor_and_returns_queued_only() {
    let world = World::new(true);
    let (client, mut peer) = monitor();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let mut stream = world.accept();
            auth(&mut stream);
            assert_eq!(request_line(&mut stream), wire::request([9; 16]).trim_end());
            write_all(
                &mut stream,
                format!("{}\n", wire::ADMISSION).as_bytes(),
                deadline(),
            )
            .unwrap();
            let received = descriptor(&mut stream);
            let mut monitor = CtlStream::from(received);
            monitor.write_all(b"actual read capability").unwrap();
            write_all(&mut stream, wire::queued(7).as_bytes(), deadline()).unwrap();
        });
        assert_eq!(attach(&world.path, client, [9; 16], deadline()).unwrap(), 7);
        let mut bytes = [0; 22];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"actual read capability");
        server.join().unwrap();
    });
}

#[test]
fn refusal_or_malformed_admission_never_delivers_the_capability() {
    for reply in [
        b"ERR denied\n".as_slice(),
        b"OK other\n".as_slice(),
        b"OK allocation monitor-fd\nextra\n".as_slice(),
    ] {
        let world = World::new(true);
        let (client, mut peer) = monitor();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let mut stream = world.accept();
                auth(&mut stream);
                assert!(request_line(&mut stream).starts_with("allocation attach generation="));
                write_all(&mut stream, reply, deadline()).unwrap();
                let mut buf = [0];
                loop {
                    match aterm_uds::fdpass::recv_with_fds(&stream, &mut buf, 1) {
                        Ok(received) => {
                            assert_eq!(received.bytes, 0);
                            assert!(received.fds.is_empty());
                            break;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            ready::until(stream.as_fd(), Interest::Read, deadline()).unwrap()
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            });
            assert!(attach(&world.path, client, [1; 16], deadline()).is_err());
            assert_eq!(
                peer.read(&mut [0]).unwrap(),
                0,
                "owned client was consumed without delegation"
            );
            server.join().unwrap();
        });
    }
}

#[test]
fn malformed_zero_overflow_or_observed_ack_is_not_queued_success() {
    for reply in [
        "OK allocation queued attachment=0\n",
        "OK allocation queued attachment=18446744073709551616\n",
        "OK allocation observed attachment=1\n",
        "OK allocation queued attachment=1\nextra\n",
    ] {
        let world = World::new(true);
        let (client, _peer) = monitor();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let mut stream = world.accept();
                auth(&mut stream);
                request_line(&mut stream);
                write_all(
                    &mut stream,
                    format!("{}\n", wire::ADMISSION).as_bytes(),
                    deadline(),
                )
                .unwrap();
                drop(descriptor(&mut stream));
                write_all(&mut stream, reply.as_bytes(), deadline()).unwrap();
            });
            assert!(attach(&world.path, client, [2; 16], deadline()).is_err());
            server.join().unwrap();
        });
    }
}

#[test]
fn missing_owner_token_and_expired_deadline_do_not_send_auth_or_descriptor() {
    let world = World::new(false);
    let (client, mut peer) = monitor();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let mut stream = world.accept();
            assert_eq!(request_line(&mut stream), "");
        });
        assert_eq!(
            attach(&world.path, client, [3; 16], deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        server.join().unwrap();
    });
    let (client, mut peer) = monitor();
    assert_eq!(
        attach(&world.path, client, [3; 16], Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    assert!(matches!(
        aterm_uds::accept(
            &world.listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock
    ));
}

#[test]
fn absolute_deadline_refuses_queued_partial_line_without_consuming_it() {
    let (mut reader, mut writer) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    reader.set_nonblocking(true).unwrap();
    writer.write_all(b"OK allocation").unwrap();
    assert_eq!(
        read_line(&mut reader, Instant::now()).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    let mut first = [0];
    reader.read_exact(&mut first).unwrap();
    assert_eq!(first, [b'O']);
}

#[test]
fn auth_refusal_sends_neither_attachment_request_nor_descriptor() {
    let world = World::new(true);
    let (client, mut peer) = monitor();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let mut stream = world.accept();
            assert_eq!(request_line(&mut stream), format!("AUTH {TOKEN}"));
            assert_eq!(request_line(&mut stream), "version");
            write_all(&mut stream, b"ERR auth\n", deadline()).unwrap();
            assert_eq!(request_line(&mut stream), "");
        });
        assert_eq!(
            attach(&world.path, client, [4; 16], deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        server.join().unwrap();
    });
}

#[test]
fn changing_latest_alias_does_not_retarget_a_pinned_authenticated_exchange() {
    let world = World::named(true, "aterm-1001.sock");
    let other = World::named(true, "aterm-1002.sock");
    let alias = world._dir.path().join("aterm.sock");
    std::os::unix::fs::symlink("aterm-1001.sock", &alias).unwrap();
    let (client, _peer) = monitor();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let mut stream = world.accept();
            assert_eq!(request_line(&mut stream), format!("AUTH {TOKEN}"));
            assert_eq!(request_line(&mut stream), "version");
            std::fs::remove_file(&alias).unwrap();
            std::os::unix::fs::symlink(&other.path, &alias).unwrap();
            write_all(&mut stream, b"OK version=fixture\n", deadline()).unwrap();
            assert_eq!(request_line(&mut stream), wire::request([5; 16]).trim_end());
            write_all(
                &mut stream,
                format!("{}\n", wire::ADMISSION).as_bytes(),
                deadline(),
            )
            .unwrap();
            drop(descriptor(&mut stream));
            write_all(&mut stream, wire::queued(11).as_bytes(), deadline()).unwrap();
        });
        assert_eq!(
            attach(alias.to_str().unwrap(), client, [5; 16], deadline()).unwrap(),
            11
        );
        server.join().unwrap();
    });
    assert!(matches!(
        aterm_uds::accept(
            &other.listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock
    ));
}
