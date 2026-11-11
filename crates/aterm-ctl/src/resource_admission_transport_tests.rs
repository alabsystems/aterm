// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;
fn request(peer: Peer) -> wire::Submission {
    wire::Submission {
        binding: wire::Binding {
            session: "s-a912afd80a5a598f9ace".into(),
            attempt: [2; 16],
        },
        peer_pid: peer.pid,
        peer_birth: peer.birth,
        argv: vec![b"/bin/x".to_vec()],
        cwd: b"/tmp".to_vec(),
        env: vec![],
        cpu_units: 1,
        memory_bytes: 1,
        label: "test".into(),
        timeout_ms: None,
    }
}
#[test]
fn wrong_birth_and_session_are_refused_before_admission() {
    for wrong_birth in [true, false] {
        let (a, mut b) = CtlStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        b.set_nonblocking(true).unwrap();
        let mut req = request(local_peer(&a).unwrap());
        if wrong_birth {
            req.peer_birth += 1;
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        write_frame(&b, &req.encode().unwrap(), deadline).unwrap();
        let e = receive(&a, true, deadline, |_, _| false).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let mut byte = [0];
        assert_eq!(read_line(&b, deadline).unwrap(), wire::PREPARE);
        assert_eq!(
            b.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
#[test]
fn exactly_four_rights_are_owned_and_short_rights_are_refused() {
    for (count, payload) in [
        (3, &b"r"[..]),
        (4, &b"r"[..]),
        (5, &b"r"[..]),
        (4, &b"rx"[..]),
    ] {
        let (a, b) = CtlStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        b.set_nonblocking(true).unwrap();
        let req = request(local_peer(&a).unwrap());
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        write_frame(&b, &req.encode().unwrap(), deadline).unwrap();
        let server = std::thread::spawn(move || receive(&a, true, deadline, |_, _| true));
        assert_eq!(read_line(&b, deadline).unwrap(), wire::PREPARE);
        assert_eq!(read_line(&b, deadline).unwrap(), wire::ADMISSION);
        let (event, _client) = CtlStream::pair().unwrap();
        let descriptors = vec![event.as_fd(); count];
        assert_eq!(
            aterm_uds::fdpass::send_with_fds(&b, payload, &descriptors).unwrap(),
            payload.len()
        );
        let result = server.join().unwrap();
        if count == 4 && payload.len() == 1 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
        }
    }
}
#[test]
fn owner_refusal_precedes_any_input_or_rights() {
    let (a, _b) = CtlStream::pair().unwrap();
    let e = receive(
        &a,
        false,
        Instant::now() + std::time::Duration::from_secs(1),
        |_, _| panic!("unauthorized callback"),
    )
    .err()
    .unwrap();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied)
}
#[test]
fn truncated_and_oversized_frames_fail_closed() {
    for bytes in [&[0, 0, 0, 3, 1][..], &[255, 255, 255, 255][..]] {
        let (a, mut b) = CtlStream::pair().unwrap();
        b.write_all(bytes).unwrap();
        drop(b);
        a.set_nonblocking(true).unwrap();
        assert!(read_frame(&a, Instant::now() + std::time::Duration::from_secs(1)).is_err())
    }
}
