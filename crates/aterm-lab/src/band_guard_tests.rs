// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

fn pair() -> (CtlStream, CtlStream, Instant) {
    let (left, right) = CtlStream::pair().unwrap();
    left.set_nonblocking(true).unwrap();
    right.set_nonblocking(true).unwrap();
    (left, right, Instant::now() + Duration::from_secs(60))
}

#[test]
fn descriptor_transfer_is_exact_bounded_and_keeps_original_identity() {
    let world = aterm_tempfile::tempdir().unwrap();
    let original = File::create(world.path().join("original")).unwrap();
    let (left, right, deadline) = pair();
    check_peer(&right, std::process::id()).unwrap();
    assert!(check_peer(&right, std::process::id() + 1).is_err());
    send(&left, b"original", &[original.as_fd()], deadline).unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    let (bytes, mut fds) = receive(&right, 1, deadline).unwrap();
    assert_eq!(bytes, b"original");
    assert_eq!(fds.len(), 1);
    let received = File::from(fds.pop().unwrap());
    let held = original.metadata().unwrap();
    let actual = received.metadata().unwrap();
    assert_eq!((held.dev(), held.ino()), (actual.dev(), actual.ino()));
    send(&right, b"ack", &[], deadline).unwrap();
    right.shutdown(Shutdown::Write).unwrap();
    let (ack, fds) = receive(&left, 0, deadline).unwrap();
    assert_eq!(ack, b"ack");
    assert!(fds.is_empty());
}

#[test]
fn surplus_descriptors_and_oversized_or_cut_messages_refuse() {
    let world = aterm_tempfile::tempdir().unwrap();
    let file = File::create(world.path().join("original")).unwrap();
    let (left, right, deadline) = pair();
    send(&left, b"surplus", &[file.as_fd(), file.as_fd()], deadline).unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    assert!(receive(&right, 1, deadline).is_err());
    let (left, right, deadline) = pair();
    send(&left, &[b'x'; MAX_MESSAGE + 1], &[], deadline).unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    assert!(receive(&right, 0, deadline).is_err());
    let (left, right, deadline) = pair();
    send(&left, b"short", &[], deadline).unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    assert!(
        receive_exact(
            &right,
            OFFER,
            "00000000000000000000000000000000",
            1,
            deadline
        )
        .is_err()
    );

    let (left, right, deadline) = pair();
    send(
        &left,
        &message(OFFER, "00000000000000000000000000000000"),
        &[file.as_fd()],
        deadline,
    )
    .unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    assert!(
        receive_exact(
            &right,
            OFFER,
            "11111111111111111111111111111111",
            1,
            deadline
        )
        .is_err()
    );

    let (left, right, deadline) = pair();
    send(
        &left,
        &message(OFFER, "00000000000000000000000000000000"),
        &[],
        deadline,
    )
    .unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    assert!(
        receive_exact(
            &right,
            OFFER,
            "00000000000000000000000000000000",
            1,
            deadline
        )
        .is_err()
    );
}

#[test]
fn request_and_launchd_identity_are_strict() {
    assert!(Request::parse("42", "0123456789abcdef0123456789abcdef", None).is_ok());
    for parent in ["", "+42", "-42", "1", "4294967296"] {
        assert!(Request::parse(parent, "0123456789abcdef0123456789abcdef", None).is_err());
    }
    assert!(Request::parse("42", "0123456789ABCDEF0123456789abcdef", None).is_err());
    assert_eq!(
        parse_launchd_pid("job = {\n\tpid = 42\n\tchild = {\n\t\tpid = 99\n\t}\n}").unwrap(),
        42
    );
    assert!(parse_launchd_pid("\tpid = 42\n\tpid = 43").is_err());
    assert!(parse_launchd_pid("\t\tpid = 42").is_err());
    assert!(parse_launchd_pid("\tpid = +42").is_err());
}

#[test]
fn functional_offer_binds_the_leaf_and_exact_second_descriptor_role() {
    let nonce = "0123456789abcdef0123456789abcdef";
    let request = Request::parse("42", nonce, Some("original.claim")).unwrap();
    let lane_only = Request::parse("42", nonce, None).unwrap();
    let other = Request::parse("42", nonce, Some("other.claim")).unwrap();
    assert_ne!(request.bound_nonce(), lane_only.bound_nonce());
    assert_ne!(request.bound_nonce(), other.bound_nonce());
    assert!(Request::parse("42", nonce, Some("../other.claim")).is_err());
    let world = aterm_tempfile::tempdir().unwrap();
    let lane = File::create(world.path().join("lane")).unwrap();
    let memory = File::create(world.path().join("memory")).unwrap();
    let (left, right, deadline) = pair();
    send(
        &left,
        &message(OFFER, &request.bound_nonce()),
        &[lane.as_fd(), memory.as_fd()],
        deadline,
    )
    .unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    let received = receive_exact(&right, OFFER, &request.bound_nonce(), 2, deadline).unwrap();
    assert_eq!(received.len(), 2);
    for (fd, original) in received.iter().zip([&lane, &memory]) {
        let held = aterm_uds::spawnfd::descriptor_metadata(fd.as_raw_fd()).unwrap();
        let original = original.metadata().unwrap();
        assert_eq!((held.dev(), held.ino()), (original.dev(), original.ino()));
    }
    let (left, right, deadline) = pair();
    send(
        &left,
        &message(OFFER, &request.bound_nonce()),
        &[lane.as_fd(), memory.as_fd()],
        deadline,
    )
    .unwrap();
    left.shutdown(Shutdown::Write).unwrap();
    assert!(receive_exact(&right, OFFER, &lane_only.bound_nonce(), 1, deadline).is_err());
}
