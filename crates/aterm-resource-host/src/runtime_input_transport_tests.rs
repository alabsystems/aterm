// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use trust_resource_provenance::process;

fn connected() -> (aterm_tempfile::TempDir, UnixStream, UnixStream) {
    let directory = aterm_tempfile::Builder::new()
        .prefix("support-peer-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let path = directory.path().join("socket");
    let listener = UnixListener::bind(path).unwrap();
    let caller =
        UnixStream::connect(listener.local_addr().unwrap().as_pathname().unwrap()).unwrap();
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    (directory, caller, accepted)
}

#[test]
fn support_peer_stays_native_authenticated_after_reply_eof_and_ack() {
    // These are transport-only frames, not a fabricated context or SDK.
    for requires_ack in [true, false] {
        let (_directory, caller, accepted) = connected();
        let own = process::read_identity(std::process::id()).unwrap();
        let peer = caller.try_clone().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (done, returned) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let result = {
                let mut channel = Channel::new(accepted).unwrap();
                channel.send(b"transport-only reply", deadline).unwrap();
                finish_support_reply(&mut channel, requires_ack, deadline)
            };
            // The result is sent only after every server socket owner drops.
            done.send(result).unwrap();
        });
        let mut client = Channel::new(caller).unwrap();
        assert_eq!(client.receive(deadline).unwrap(), b"transport-only reply");
        wire::transport::require_reply_eof(&mut client, deadline).unwrap();
        process::check_peer(&peer, own).unwrap();
        if requires_ack {
            client.send(&[1], deadline).unwrap();
        }
        assert!(matches!(
            returned.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        process::check_peer(&peer, own).unwrap();
        let mut changed = own;
        changed.birth ^= 1;
        assert!(process::check_peer(&peer, changed).is_err());

        wire::transport::finish_reply(&mut client).unwrap();
        returned
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        server.join().unwrap();
        #[cfg(target_os = "macos")]
        assert!(process::check_peer(&peer, own).is_err());
    }
}

#[test]
fn support_close_refuses_wrong_ack_and_trailing_frames() {
    for wrong_ack in [true, false] {
        let (_directory, caller, accepted) = connected();
        let deadline = Instant::now() + Duration::from_secs(60);
        // A refusal can precede the caller's remaining writes. Keep the accepted
        // endpoint alive until the caller ends its send half, independently of the
        // server task.
        let accepted_owner = accepted.try_clone().unwrap();
        let server = std::thread::spawn(move || {
            let mut channel = Channel::new(accepted).unwrap();
            channel.send(b"transport-only reply", deadline).unwrap();
            finish_support_reply(&mut channel, true, deadline)
        });
        let mut client = Channel::new(caller).unwrap();
        client.receive(deadline).unwrap();
        wire::transport::require_reply_eof(&mut client, deadline).unwrap();
        client
            .send(if wrong_ack { &[2] } else { &[1] }, deadline)
            .unwrap();
        if !wrong_ack {
            client.send(b"trailing frame", deadline).unwrap();
        }
        wire::transport::finish_reply(&mut client).unwrap();
        assert_eq!(
            server.join().unwrap().unwrap_err().kind(),
            if wrong_ack {
                io::ErrorKind::PermissionDenied
            } else {
                io::ErrorKind::InvalidData
            }
        );
        drop(accepted_owner);
    }
}

#[test]
fn support_close_keeps_the_original_deadline_when_client_remains_open() {
    let (_directory, caller, accepted) = connected();
    let deadline = Instant::now() + Duration::from_secs(5);
    let expired = Instant::now();
    let server = std::thread::spawn(move || {
        let mut channel = Channel::new(accepted).unwrap();
        channel.send(b"transport-only reply", deadline).unwrap();
        finish_support_reply(&mut channel, false, expired)
    });
    let mut client = Channel::new(caller).unwrap();
    client.receive(deadline).unwrap();
    wire::transport::require_reply_eof(&mut client, deadline).unwrap();
    assert_eq!(
        server.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}
