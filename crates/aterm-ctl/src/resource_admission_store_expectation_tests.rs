// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{fs::File, os::fd::AsRawFd as _, time::Duration};

const HANG: Duration = Duration::from_secs(60);

fn request(stream: &CtlStream) -> wire::Submission {
    let peer = local_peer(stream).unwrap();
    wire::Submission {
        binding: wire::Binding {
            session: "s-acde".into(),
            attempt: [8; 16],
        },
        peer_pid: peer.pid,
        peer_birth: peer.birth,
        argv: vec![
            b"/sdk/bin/targo".to_vec(),
            b"--resource-workload-v1".to_vec(),
        ],
        cwd: b"/work".to_vec(),
        env: vec![],
        cpu_units: 1,
        memory_bytes: 1,
        label: "native original Store expectation".into(),
        timeout_ms: Some(1000),
    }
}

#[test]
fn foreign_directory_refuses_before_any_root_attachments_or_submit() {
    let selected = aterm_tempfile::tempdir().unwrap();
    let foreign = aterm_tempfile::tempdir().unwrap();
    let original = File::open(selected.path()).unwrap();
    let before = original.metadata().unwrap();
    let (client, host) = CtlStream::pair().unwrap();
    let request = request(&client);
    let deadline = Instant::now() + HANG;
    let server = std::thread::spawn(move || {
        use std::os::unix::fs::MetadataExt as _;
        receive_submission(
            &host,
            true,
            deadline,
            |_, _| true,
            |file| {
                let actual = aterm_uds::spawnfd::descriptor_metadata(file.as_raw_fd())?;
                Ok((actual.dev(), actual.ino()) == (before.dev(), before.ino()))
            },
        )
        .err()
        .unwrap()
        .kind()
    });
    assert_eq!(read_line(&client, deadline).unwrap(), PREPARE);
    write_frame(&client, &request.encode().unwrap(), deadline).unwrap();
    assert_eq!(read_line(&client, deadline).unwrap(), EXPECT);
    send(
        &client,
        File::open(foreign.path()).unwrap().as_fd(),
        deadline,
    )
    .unwrap();
    assert_eq!(server.join().unwrap(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        read_line(&client, deadline).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    // No stdio/events rights were sent, so no AuthorizedSubmission existed.
}

#[test]
fn exact_original_directory_is_retained_through_bound_accounted_attachment() {
    use std::os::unix::fs::MetadataExt as _;
    let selected = aterm_tempfile::tempdir().unwrap();
    let original = File::open(selected.path()).unwrap();
    let before = original.metadata().unwrap();
    let (client, host) = CtlStream::pair().unwrap();
    let request = request(&client);
    let deadline = Instant::now() + HANG;
    let server = std::thread::spawn(move || {
        let (submission, expected) = receive_submission(
            &host,
            true,
            deadline,
            |_, _| true,
            |file| {
                let actual = aterm_uds::spawnfd::descriptor_metadata(file.as_raw_fd())?;
                Ok((actual.dev(), actual.ino()) == (before.dev(), before.ino()))
            },
        )
        .unwrap();
        acknowledge(&host, deadline).unwrap();
        (submission, expected)
    });
    assert_eq!(read_line(&client, deadline).unwrap(), PREPARE);
    write_frame(&client, &request.encode().unwrap(), deadline).unwrap();
    assert_eq!(read_line(&client, deadline).unwrap(), EXPECT);
    send(&client, original.as_fd(), deadline).unwrap();
    assert_eq!(read_line(&client, deadline).unwrap(), ADMISSION);
    let (events, host_events) = CtlStream::pair().unwrap();
    let null = File::open("/dev/null").unwrap();
    let fds = Attachments {
        stdin: null.as_fd().try_clone_to_owned().unwrap(),
        stdout: null.as_fd().try_clone_to_owned().unwrap(),
        stderr: null.as_fd().try_clone_to_owned().unwrap(),
        events: host_events.into(),
    };
    send_attachments(&client, &fds, deadline).unwrap();
    assert_eq!(read_line(&client, deadline).unwrap(), ATTACHED);
    let (accepted, expected) = server.join().unwrap();
    assert!(accepted.request == request);
    assert_eq!(
        local_peer(&CtlStream::from(accepted.fds.events)).unwrap(),
        local_peer(&events).unwrap()
    );
    let held = aterm_uds::spawnfd::descriptor_metadata(expected.as_raw_fd()).unwrap();
    assert_eq!(
        (held.dev(), held.ino()),
        (
            original.metadata().unwrap().dev(),
            original.metadata().unwrap().ino()
        )
    );
}

#[test]
fn malformed_marker_extra_rights_and_expiry_cannot_be_store_expectations() {
    for extra in [false, true] {
        let (sender, receiver) = CtlStream::pair().unwrap();
        let file = File::open("/dev/null").unwrap();
        let rights = if extra {
            vec![file.as_fd(), file.as_fd()]
        } else {
            vec![file.as_fd()]
        };
        aterm_uds::fdpass::send_with_fds(&sender, &[if extra { MARKER } else { 0 }], &rights)
            .unwrap();
        assert_eq!(
            receive(&receiver, Instant::now() + HANG)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    let (_sender, receiver) = CtlStream::pair().unwrap();
    assert_eq!(
        receive(&receiver, Instant::now()).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}
